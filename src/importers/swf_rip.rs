use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use swf::{AudioCompression, SoundFormat, Tag};

use super::ImportedButton;

pub struct SwfRipResult {
    pub tab_name: String,
    pub buttons: Vec<ImportedButton>,
    pub skipped: usize,
    pub skipped_codecs: Vec<String>,
}

/// Pulls every embedded sound out of a Flash `.swf` file. This ONLY walks
/// the SWF's static tag structure to copy out raw audio payloads -- it
/// never runs an ActionScript interpreter (the `swf` crate is a pure
/// parser, not a player), so there is no code-execution surface for
/// whatever a malicious old soundboard's script might otherwise try to
/// do. Supports the two codecs that cover the vast majority of real
/// Flash soundboards, MP3 and raw/uncompressed PCM; anything else
/// (ADPCM/Nellymoser/AAC/Speex -- rare in this genre) is counted and
/// skipped rather than attempted.
pub fn extract_sounds(path: &Path) -> Result<SwfRipResult> {
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let swf_buf = swf::decompress_swf(&data[..]).context("not a valid .swf (bad header/compression)")?;
    let parsed = swf::parse_swf(&swf_buf).context("failed to parse .swf tags")?;
    let encoding = swf::SwfStr::encoding_for_version(swf_buf.header.version());

    // id -> friendly name, when the SWF's author assigned one.
    let mut names: HashMap<u16, String> = HashMap::new();
    for tag in &parsed.tags {
        match tag {
            Tag::ExportAssets(assets) => {
                for a in assets {
                    names.insert(a.id, a.name.to_str_lossy(encoding).into_owned());
                }
            }
            Tag::SymbolClass(links) => {
                for l in links {
                    names.entry(l.id).or_insert_with(|| l.class_name.to_str_lossy(encoding).into_owned());
                }
            }
            _ => {}
        }
    }

    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("swf-soundboard").to_string();
    let out_dir = crate::persistence::data_dir().join("swf-sounds").join(unique_dir_name(&stem));
    fs::create_dir_all(&out_dir).context("creating output directory")?;

    let mut buttons = Vec::new();
    let mut skipped = 0usize;
    let mut skipped_codecs: Vec<String> = Vec::new();
    let mut counter = 0u32;

    let mut stream_format: Option<SoundFormat> = None;
    let mut stream_buf: Vec<u8> = Vec::new();

    for tag in &parsed.tags {
        match tag {
            Tag::DefineSound(sound) => {
                counter += 1;
                let label = names.get(&sound.id).cloned().unwrap_or_else(|| format!("Sound {counter}"));
                match extract_define_sound(&out_dir, &label, sound) {
                    Ok(Some(file)) => buttons.push(ImportedButton { label, file }),
                    Ok(None) => {
                        skipped += 1;
                        skipped_codecs.push(format!("{:?}", sound.format.compression));
                    }
                    Err(e) => log::warn!("failed to extract sound {}: {e}", sound.id),
                }
            }
            Tag::SoundStreamHead(head) | Tag::SoundStreamHead2(head) => {
                flush_stream(&out_dir, &mut stream_format, &mut stream_buf, &mut counter, &mut buttons, &mut skipped, &mut skipped_codecs);
                stream_format = Some(head.stream_format.clone());
            }
            Tag::SoundStreamBlock(block) => {
                if let Some(fmt) = &stream_format {
                    if fmt.compression == AudioCompression::Mp3 && block.len() > 4 {
                        stream_buf.extend_from_slice(&block[4..]);
                    }
                }
            }
            _ => {}
        }
    }
    flush_stream(&out_dir, &mut stream_format, &mut stream_buf, &mut counter, &mut buttons, &mut skipped, &mut skipped_codecs);

    Ok(SwfRipResult { tab_name: stem, buttons, skipped, skipped_codecs })
}

fn flush_stream(
    out_dir: &Path,
    stream_format: &mut Option<SoundFormat>,
    stream_buf: &mut Vec<u8>,
    counter: &mut u32,
    buttons: &mut Vec<ImportedButton>,
    skipped: &mut usize,
    skipped_codecs: &mut Vec<String>,
) {
    if let Some(fmt) = stream_format.take() {
        if !stream_buf.is_empty() {
            if fmt.compression == AudioCompression::Mp3 {
                *counter += 1;
                let label = format!("Stream {counter}");
                match write_mp3(out_dir, &label, stream_buf) {
                    Ok(file) => buttons.push(ImportedButton { label, file }),
                    Err(e) => log::warn!("failed to write extracted stream audio: {e}"),
                }
            } else {
                *skipped += 1;
                skipped_codecs.push(format!("{:?}", fmt.compression));
            }
        }
    }
    stream_buf.clear();
}

fn extract_define_sound(out_dir: &Path, label: &str, sound: &swf::Sound) -> Result<Option<String>> {
    match sound.format.compression {
        AudioCompression::Mp3 => {
            if sound.data.len() < 2 {
                return Ok(None);
            }
            // First 2 bytes are the SWF-specific "SeekSamples" field, not
            // part of the MP3 stream itself.
            Ok(Some(write_mp3(out_dir, label, &sound.data[2..])?))
        }
        AudioCompression::Uncompressed | AudioCompression::UncompressedUnknownEndian => {
            Ok(Some(write_wav(out_dir, label, sound.data, &sound.format)?))
        }
        _ => Ok(None),
    }
}

fn write_mp3(out_dir: &Path, label: &str, bytes: &[u8]) -> Result<String> {
    let name = unique_filename(out_dir, label, "mp3");
    let path = out_dir.join(&name);
    let mut f = fs::File::create(&path)?;
    f.write_all(bytes)?;
    Ok(path.to_string_lossy().to_string())
}

/// Wraps raw PCM in a minimal WAV header. Flash's raw sample layout
/// already matches WAV's convention (8-bit unsigned, 16-bit signed
/// little-endian), so no sample conversion is needed -- just framing.
fn write_wav(out_dir: &Path, label: &str, pcm: &[u8], format: &SoundFormat) -> Result<String> {
    let channels: u16 = if format.is_stereo { 2 } else { 1 };
    let bits_per_sample: u16 = if format.is_16_bit { 16 } else { 8 };
    let sample_rate: u32 = format.sample_rate as u32;
    let block_align: u16 = channels * (bits_per_sample / 8);
    let byte_rate: u32 = sample_rate * block_align as u32;
    let data_len: u32 = pcm.len() as u32;

    let name = unique_filename(out_dir, label, "wav");
    let path = out_dir.join(&name);
    let mut f = fs::File::create(&path)?;

    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&sample_rate.to_le_bytes())?;
    f.write_all(&byte_rate.to_le_bytes())?;
    f.write_all(&block_align.to_le_bytes())?;
    f.write_all(&bits_per_sample.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    f.write_all(pcm)?;

    Ok(path.to_string_lossy().to_string())
}

fn unique_filename(out_dir: &Path, label: &str, ext: &str) -> String {
    let base = crate::util::sanitize_label(label);
    let base = if base.is_empty() { "sound".to_string() } else { base };
    let mut candidate = format!("{base}.{ext}");
    let mut i = 1;
    while out_dir.join(&candidate).exists() {
        candidate = format!("{base}-{i}.{ext}");
        i += 1;
    }
    candidate
}

/// A distinct output dir per rip, so re-dropping the same file doesn't
/// collide with (or silently reuse stale files from) a previous
/// extraction.
fn unique_dir_name(stem: &str) -> String {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    format!("{stem}-{ts}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Builds the smallest valid SWF that exercises the parts of the
    /// pipeline this module cares about: an uncompressed (`FWS`) file
    /// containing one `DefineSound` tag with a tiny fake MP3 payload.
    /// Hand-assembled per the SWF19 tag format spec so this test stays
    /// fully offline (no real downloaded `.swf` needed).
    fn build_minimal_swf_with_mp3_sound() -> Vec<u8> {
        // DefineSound tag body: id(u16) + format(u8) + num_samples(u32) + data
        let mut define_sound_body = Vec::new();
        define_sound_body.extend_from_slice(&1u16.to_le_bytes()); // character id
        // compression=Mp3(2)<<4 | rate=44100(3)<<2 | 16-bit<<1 | stereo
        define_sound_body.push((2u8 << 4) | (3 << 2) | (1 << 1) | 1);
        define_sound_body.extend_from_slice(&4u32.to_le_bytes()); // num_samples
        define_sound_body.extend_from_slice(&0u16.to_le_bytes()); // SeekSamples header
        define_sound_body.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]); // fake mp3 bytes

        let tag_code_and_length = (14u16 << 6) | (define_sound_body.len() as u16 & 0x3F);
        let mut tags = Vec::new();
        tags.extend_from_slice(&tag_code_and_length.to_le_bytes());
        tags.extend_from_slice(&define_sound_body);
        tags.extend_from_slice(&0u16.to_le_bytes()); // End tag

        // Body after the 8-byte FWS header: rect(minimal) + frame_rate(u16) + frame_count(u16) + tags
        let mut body = Vec::new();
        body.push(0x00); // RECT: nbits=0 -> single zero byte
        body.extend_from_slice(&0u16.to_le_bytes()); // frame rate (fixed8, here just 0)
        body.extend_from_slice(&1u16.to_le_bytes()); // frame count
        body.extend_from_slice(&tags);

        let mut file = Vec::new();
        file.extend_from_slice(b"FWS");
        file.push(6); // version
        let file_len = 8 + body.len() as u32;
        file.extend_from_slice(&file_len.to_le_bytes());
        file.extend_from_slice(&body);
        file
    }

    #[test]
    fn extracts_mp3_sound_and_strips_seek_samples_header() {
        let data = build_minimal_swf_with_mp3_sound();
        let swf_buf = swf::decompress_swf(Cursor::new(data)).expect("decompress");
        let parsed = swf::parse_swf(&swf_buf).expect("parse");

        let mut found = false;
        for tag in &parsed.tags {
            if let Tag::DefineSound(sound) = tag {
                assert_eq!(sound.format.compression, AudioCompression::Mp3);
                assert_eq!(sound.format.sample_rate, 44100);
                assert!(sound.format.is_stereo);
                assert!(sound.format.is_16_bit);
                // 2-byte SeekSamples header + 4 fake mp3 bytes
                assert_eq!(sound.data, &[0, 0, 0xAA, 0xBB, 0xCC, 0xDD]);
                found = true;
            }
        }
        assert!(found, "expected to find the DefineSound tag");
    }

    #[test]
    fn wav_header_round_trips_format_fields() {
        let dir = std::env::temp_dir().join(format!("ultimate-soundboard-swftest-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();

        let format = SoundFormat { compression: AudioCompression::Uncompressed, sample_rate: 22050, is_stereo: true, is_16_bit: true };
        let pcm = vec![0u8; 400]; // 100 stereo 16-bit frames
        let path = write_wav(&dir, "Test Sound", &pcm, &format).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        let channels = u16::from_le_bytes([bytes[22], bytes[23]]);
        let sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
        let bits = u16::from_le_bytes([bytes[34], bytes[35]]);
        assert_eq!(channels, 2);
        assert_eq!(sample_rate, 22050);
        assert_eq!(bits, 16);
        assert_eq!(bytes.len(), 44 + pcm.len());

        std::fs::remove_dir_all(&dir).ok();
    }
}
