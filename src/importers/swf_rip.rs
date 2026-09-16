use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use swf::extensions::ReadSwfExt;
use swf::{AudioCompression, SoundFormat};

use super::ImportedButton;
use super::swf_adpcm;

pub struct SwfRipResult {
    pub tab_name: String,
    pub buttons: Vec<ImportedButton>,
    /// Sounds found but left behind because we can't decode the codec.
    pub skipped: usize,
    /// Human-readable codec names behind `skipped`, for the toast.
    pub skipped_codecs: Vec<String>,
    /// Sounds we could decode but failed to write out (disk full, bad path).
    /// Counted separately so a write failure can't masquerade as an
    /// unsupported codec.
    pub failed: usize,
}

// Tag codes we care about. Everything else is skipped without being parsed
// at all -- see `walk_tags`.
const TAG_END: u16 = 0;
const TAG_DEFINE_SOUND: u16 = 14;
const TAG_SOUND_STREAM_HEAD: u16 = 18;
const TAG_SOUND_STREAM_BLOCK: u16 = 19;
const TAG_DEFINE_SPRITE: u16 = 39;
const TAG_SOUND_STREAM_HEAD_2: u16 = 45;
const TAG_EXPORT_ASSETS: u16 = 56;
const TAG_SYMBOL_CLASS: u16 = 76;

/// How deep to follow nested `DefineSprite` tag lists. Real files nest a
/// handful at most; the cap exists so a malformed or cyclic file can't
/// recurse us into a stack overflow.
const MAX_SPRITE_DEPTH: usize = 16;

/// Pulls every embedded sound out of a Flash `.swf` file.
///
/// This ONLY walks the SWF's static tag structure to copy out audio payloads
/// -- it never runs an ActionScript interpreter, and in fact never even
/// parses the ActionScript tag bodies: the walker below reads each tag's
/// code and length, and only looks inside the handful of sound-related tags
/// it knows. So there is no code-execution surface for whatever a malicious
/// old soundboard's script might otherwise try to do.
///
/// Decodes the three codecs that cover essentially every real Flash
/// soundboard: ADPCM (the most common by far), MP3, and raw/uncompressed
/// PCM. Nellymoser, Speex and AAC are rare here -- they're microphone and
/// streaming codecs, not authored-content ones -- and are counted and named
/// rather than attempted. All decoding is pure Rust; nothing here shells out
/// to ffmpeg or any other external tool.
pub fn extract_sounds(path: &Path) -> Result<SwfRipResult> {
    let started = std::time::Instant::now();
    let data = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let swf_buf =
        swf::decompress_swf(&data[..]).context("not a valid .swf (bad header/compression)")?;
    let version = swf_buf.header.version();

    // First pass: character id -> friendly name, where the author assigned
    // one. Cheap enough to do as its own walk, and it means a sound tag that
    // appears before its ExportAssets still gets its real name.
    let mut names: HashMap<u16, String> = HashMap::new();
    collect_names(&swf_buf.data, version, 0, &mut names);

    let stem =
        path.file_stem().and_then(|s| s.to_str()).unwrap_or("swf-soundboard").to_string();
    let out_dir = crate::persistence::data_dir().join("swf-sounds").join(unique_dir_name(&stem));

    let result = rip_tags(&swf_buf.data, version, names, &stem, out_dir);

    log::info!(
        "ripped {} sound(s) from {} in {:?} ({} skipped, {} failed)",
        result.buttons.len(),
        path.display(),
        started.elapsed(),
        result.skipped,
        result.failed
    );

    Ok(result)
}

/// The extraction proper, split from file handling so tests can point it at
/// a temporary directory instead of the real app data dir.
fn rip_tags(
    data: &[u8],
    version: u8,
    names: HashMap<u16, String>,
    stem: &str,
    out_dir: PathBuf,
) -> SwfRipResult {
    let mut ctx = RipContext {
        out_dir,
        out_dir_created: false,
        names,
        buttons: Vec::new(),
        skipped: 0,
        skipped_codecs: Vec::new(),
        failed: 0,
        counter: 0,
        version,
    };

    let mut stream = StreamState::default();
    walk_tags(data, 0, &mut ctx, &mut stream);
    flush_stream(&mut ctx, &mut stream);

    // Don't leave an empty timestamped directory behind on a rip that found
    // nothing -- these accumulate in the data dir otherwise.
    if ctx.buttons.is_empty() && ctx.out_dir_created {
        let _ = fs::remove_dir_all(&ctx.out_dir);
    }

    SwfRipResult {
        tab_name: stem.to_string(),
        buttons: ctx.buttons,
        skipped: ctx.skipped,
        skipped_codecs: ctx.skipped_codecs,
        failed: ctx.failed,
    }
}

struct RipContext {
    out_dir: PathBuf,
    out_dir_created: bool,
    names: HashMap<u16, String>,
    buttons: Vec<ImportedButton>,
    skipped: usize,
    skipped_codecs: Vec<String>,
    failed: usize,
    counter: u32,
    version: u8,
}

impl RipContext {
    /// Creates the output directory on first use, so a rip that extracts
    /// nothing leaves nothing behind.
    fn out_dir(&mut self) -> Result<&Path> {
        if !self.out_dir_created {
            fs::create_dir_all(&self.out_dir).context("creating output directory")?;
            self.out_dir_created = true;
        }
        Ok(&self.out_dir)
    }

    fn note_skipped(&mut self, compression: AudioCompression) {
        self.skipped += 1;
        self.skipped_codecs.push(codec_name(compression).to_string());
    }
}

/// State for timeline (streaming) audio, which arrives as a `SoundStreamHead`
/// followed by many `SoundStreamBlock`s rather than as one tag.
#[derive(Default)]
struct StreamState {
    format: Option<SoundFormat>,
    /// Already-decoded PCM for ADPCM streams, or raw MP3 frames for MP3 ones.
    buffer: Vec<u8>,
    /// Set once we've seen a head we can't decode, so it's still reported
    /// even though no bytes get buffered.
    saw_undecodable: bool,
}

/// Walks a tag list without `swf::parse_swf`.
///
/// `parse_swf` bails on the first malformed tag, throwing away every sound
/// found before it, and fully parses shapes, fonts and ActionScript we have
/// no use for. This reads only each tag's code and length, hands the body to
/// a parser for the few sound-related tags, and always advances by the
/// declared length -- so one corrupt tag costs one sound, not the whole rip.
fn walk_tags(data: &[u8], depth: usize, ctx: &mut RipContext, stream: &mut StreamState) {
    let version = ctx.version;
    let mut reader = swf::read::Reader::new(data, version);

    loop {
        let Ok((code, length)) = reader.read_tag_code_and_length() else { break };
        if code == TAG_END {
            break;
        }

        let remaining = reader.get_ref();
        if length > remaining.len() {
            // Truncated final tag: nothing trustworthy left to read.
            break;
        }
        let body = &remaining[..length];
        *reader.get_mut() = &remaining[length..];

        match code {
            TAG_DEFINE_SOUND => {
                let mut body_reader = swf::read::Reader::new(body, version);
                match body_reader.read_define_sound() {
                    Ok(sound) => handle_define_sound(ctx, &sound),
                    Err(e) => log::warn!("skipping malformed DefineSound tag: {e}"),
                }
            }
            TAG_SOUND_STREAM_HEAD | TAG_SOUND_STREAM_HEAD_2 => {
                // A new stream starts here, so finish the previous one.
                flush_stream(ctx, stream);
                let mut body_reader = swf::read::Reader::new(body, version);
                match body_reader.read_sound_stream_head() {
                    Ok(head) => stream.format = Some(head.stream_format),
                    Err(e) => log::warn!("skipping malformed SoundStreamHead tag: {e}"),
                }
            }
            TAG_SOUND_STREAM_BLOCK => handle_stream_block(stream, body),
            TAG_DEFINE_SPRITE => {
                // Body is id(u16) + frame_count(u16) + a nested tag list.
                // Sounds live inside movieclips constantly, and not
                // recursing here is why they used to go missing.
                if depth < MAX_SPRITE_DEPTH && body.len() > 4 {
                    // A sprite's timeline owns its own stream, so don't let
                    // one leak across the boundary in either direction.
                    flush_stream(ctx, stream);
                    let mut nested = StreamState::default();
                    walk_tags(&body[4..], depth + 1, ctx, &mut nested);
                    flush_stream(ctx, &mut nested);
                }
            }
            _ => {}
        }
    }
}

/// The name-collecting pass. Separate from `walk_tags` so names are complete
/// before any sound is written, whatever order the tags appear in.
fn collect_names(data: &[u8], version: u8, depth: usize, names: &mut HashMap<u16, String>) {
    let encoding = swf::SwfStr::encoding_for_version(version);
    let mut reader = swf::read::Reader::new(data, version);

    loop {
        let Ok((code, length)) = reader.read_tag_code_and_length() else { break };
        if code == TAG_END {
            break;
        }
        let remaining = reader.get_ref();
        if length > remaining.len() {
            break;
        }
        let body = &remaining[..length];
        *reader.get_mut() = &remaining[length..];

        match code {
            TAG_EXPORT_ASSETS => {
                let mut body_reader = swf::read::Reader::new(body, version);
                if let Ok(assets) = body_reader.read_export_assets() {
                    for asset in assets {
                        names.insert(asset.id, asset.name.to_str_lossy(encoding).into_owned());
                    }
                }
            }
            TAG_SYMBOL_CLASS => {
                let mut body_reader = swf::read::Reader::new(body, version);
                if let Ok(count) = body_reader.read_u16() {
                    for _ in 0..count {
                        let (Ok(id), Ok(name)) =
                            (body_reader.read_u16(), body_reader.read_str())
                        else {
                            break;
                        };
                        names
                            .entry(id)
                            .or_insert_with(|| name.to_str_lossy(encoding).into_owned());
                    }
                }
            }
            TAG_DEFINE_SPRITE => {
                if depth < MAX_SPRITE_DEPTH && body.len() > 4 {
                    collect_names(&body[4..], version, depth + 1, names);
                }
            }
            _ => {}
        }
    }
}

fn handle_define_sound(ctx: &mut RipContext, sound: &swf::Sound) {
    ctx.counter += 1;
    let label = ctx
        .names
        .get(&sound.id)
        .cloned()
        .unwrap_or_else(|| format!("Sound {}", ctx.counter));

    match extract_define_sound(ctx, &label, sound) {
        Ok(Some(file)) => ctx.buttons.push(ImportedButton { label, file }),
        Ok(None) => ctx.note_skipped(sound.format.compression),
        Err(e) => {
            // A decode/write failure is not an unsupported codec -- count it
            // separately so the toast doesn't lie about why.
            ctx.failed += 1;
            log::warn!("failed to extract sound {}: {e:#}", sound.id);
        }
    }
}

/// Buffers one `SoundStreamBlock`. MP3 blocks carry a 4-byte
/// SampleCount+SeekSamples prefix; ADPCM blocks carry none, and each one is
/// an independent ADPCM stream with its own code-size header, so they have
/// to be decoded per block and the PCM concatenated -- concatenating the raw
/// bytes and decoding once produces noise after the first block.
fn handle_stream_block(stream: &mut StreamState, block: &[u8]) {
    let Some(format) = &stream.format else { return };
    match format.compression {
        AudioCompression::Mp3 => {
            if block.len() > 4 {
                stream.buffer.extend_from_slice(&block[4..]);
            }
        }
        AudioCompression::Adpcm => {
            match swf_adpcm::decode(block, format.is_stereo, 0) {
                Ok(pcm) => stream.buffer.extend_from_slice(&pcm),
                Err(e) => log::warn!("skipping malformed ADPCM stream block: {e}"),
            }
        }
        AudioCompression::Uncompressed | AudioCompression::UncompressedUnknownEndian => {
            stream.buffer.extend_from_slice(block);
        }
        _ => stream.saw_undecodable = true,
    }
}

fn flush_stream(ctx: &mut RipContext, stream: &mut StreamState) {
    let Some(format) = stream.format.take() else {
        stream.buffer.clear();
        stream.saw_undecodable = false;
        return;
    };

    let buffer = std::mem::take(&mut stream.buffer);
    let saw_undecodable = std::mem::take(&mut stream.saw_undecodable);

    if buffer.is_empty() {
        // An undecodable stream used to vanish entirely here, because
        // nothing had been buffered for it to flush.
        if saw_undecodable {
            ctx.note_skipped(format.compression);
        }
        return;
    }

    ctx.counter += 1;
    let label = format!("Stream {}", ctx.counter);

    let written = match format.compression {
        AudioCompression::Mp3 => write_mp3(ctx, &label, &buffer),
        // ADPCM was decoded to PCM block by block on the way in; raw
        // streams are already PCM. Either way it's 16-bit unless the header
        // says otherwise.
        AudioCompression::Adpcm => {
            let pcm_format = SoundFormat { is_16_bit: true, ..format.clone() };
            write_wav(ctx, &label, &buffer, &pcm_format)
        }
        _ => write_wav(ctx, &label, &buffer, &format),
    };

    match written {
        Ok(file) => ctx.buttons.push(ImportedButton { label, file }),
        Err(e) => {
            ctx.failed += 1;
            log::warn!("failed to write extracted stream audio: {e:#}");
        }
    }
}

/// Decodes one `DefineSound` payload to a file. `Ok(None)` means "codec we
/// don't support"; `Err` means we understood it but couldn't produce a file.
fn extract_define_sound(
    ctx: &mut RipContext,
    label: &str,
    sound: &swf::Sound,
) -> Result<Option<String>> {
    match sound.format.compression {
        AudioCompression::Adpcm => {
            // Unlike MP3 there is no SeekSamples prefix here: ADPCM data
            // starts at byte 0 with the 2-bit AdpcmCodeSize header.
            let pcm = swf_adpcm::decode(sound.data, sound.format.is_stereo, sound.num_samples)?;
            if pcm.is_empty() {
                return Ok(None);
            }
            // ADPCM always decodes to 16-bit, whatever the header's size bit
            // claims about the original source material.
            let format = SoundFormat { is_16_bit: true, ..sound.format.clone() };
            Ok(Some(write_wav(ctx, label, &pcm, &format)?))
        }
        AudioCompression::Mp3 => {
            if sound.data.len() < 2 {
                return Ok(None);
            }
            // First 2 bytes are the SWF-specific "SeekSamples" field, not
            // part of the MP3 stream itself.
            Ok(Some(write_mp3(ctx, label, &sound.data[2..])?))
        }
        AudioCompression::Uncompressed | AudioCompression::UncompressedUnknownEndian => {
            if sound.data.is_empty() {
                return Ok(None);
            }
            Ok(Some(write_wav(ctx, label, sound.data, &sound.format)?))
        }
        // Nellymoser (microphone codec), Speex (voice chat) and AAC. See the
        // module docs -- these are named in the UI rather than attempted.
        _ => Ok(None),
    }
}

/// Display names for the codec list in the "skipped" toast. `{:?}` on the
/// enum leaks Rust-ish spellings like `Nellymoser16Khz` at the user.
fn codec_name(compression: AudioCompression) -> &'static str {
    match compression {
        AudioCompression::UncompressedUnknownEndian | AudioCompression::Uncompressed => "PCM",
        AudioCompression::Adpcm => "ADPCM",
        AudioCompression::Mp3 => "MP3",
        AudioCompression::Nellymoser16Khz
        | AudioCompression::Nellymoser8Khz
        | AudioCompression::Nellymoser => "Nellymoser",
        AudioCompression::Aac => "AAC",
        AudioCompression::Speex => "Speex",
    }
}

fn write_mp3(ctx: &mut RipContext, label: &str, bytes: &[u8]) -> Result<String> {
    let out_dir = ctx.out_dir()?.to_path_buf();
    let name = unique_filename(&out_dir, label, "mp3");
    let path = out_dir.join(&name);
    let mut f = fs::File::create(&path)?;
    f.write_all(bytes)?;
    Ok(path.to_string_lossy().to_string())
}

/// Wraps raw PCM in a minimal WAV header. Flash's raw sample layout already
/// matches WAV's convention (8-bit unsigned, 16-bit signed little-endian),
/// so no sample conversion is needed -- just framing.
fn write_wav(
    ctx: &mut RipContext,
    label: &str,
    pcm: &[u8],
    format: &SoundFormat,
) -> Result<String> {
    let out_dir = ctx.out_dir()?.to_path_buf();
    write_wav_to(&out_dir, label, pcm, format)
}

fn write_wav_to(
    out_dir: &Path,
    label: &str,
    pcm: &[u8],
    format: &SoundFormat,
) -> Result<String> {
    let channels: u16 = if format.is_stereo { 2 } else { 1 };
    let bits_per_sample: u16 = if format.is_16_bit { 16 } else { 8 };
    let sample_rate: u32 = format.sample_rate as u32;
    let block_align: u16 = channels * (bits_per_sample / 8);
    let byte_rate: u32 = sample_rate * block_align as u32;

    // A trailing partial frame makes some decoders unhappy, so keep the data
    // chunk a whole number of frames.
    let usable = pcm.len() - (pcm.len() % block_align.max(1) as usize);
    let pcm = &pcm[..usable];
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
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{stem}-{ts}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wraps a tag body in its code+length header, using the extended form
    /// when the body doesn't fit the short 6-bit length field.
    fn tag(code: u16, body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        if body.len() < 0x3F {
            out.extend_from_slice(&((code << 6) | body.len() as u16).to_le_bytes());
        } else {
            out.extend_from_slice(&((code << 6) | 0x3F).to_le_bytes());
            out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        }
        out.extend_from_slice(body);
        out
    }

    /// DefineSound body: id(u16) + format(u8) + num_samples(u32) + payload.
    fn define_sound_body(id: u16, compression: u8, payload: &[u8], num_samples: u32) -> Vec<u8> {
        let mut body = Vec::new();
        body.extend_from_slice(&id.to_le_bytes());
        // compression<<4 | rate(3 = 44100)<<2 | 16-bit<<1 | mono
        // Mono because the hand-built ADPCM payloads below are mono; the
        // stereo layout has its own test in `swf_adpcm`.
        body.push((compression << 4) | (3 << 2) | (1 << 1));
        body.extend_from_slice(&num_samples.to_le_bytes());
        body.extend_from_slice(payload);
        body
    }

    /// Wraps a tag stream in the smallest valid uncompressed (`FWS`) SWF,
    /// hand-assembled per the SWF19 spec so tests stay fully offline.
    fn build_swf(tags: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        body.push(0x00); // RECT: nbits = 0 -> a single zero byte
        body.extend_from_slice(&0u16.to_le_bytes()); // frame rate
        body.extend_from_slice(&1u16.to_le_bytes()); // frame count
        body.extend_from_slice(tags);
        body.extend_from_slice(&0u16.to_le_bytes()); // End tag

        let mut file = Vec::new();
        file.extend_from_slice(b"FWS");
        file.push(6); // version
        file.extend_from_slice(&(8 + body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);
        file
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let dir = std::env::temp_dir()
                .join(format!("ultimate-soundboard-swftest-{}", uuid::Uuid::new_v4()));
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// Runs the real pipeline over hand-built SWF bytes, into a temp dir.
    fn rip(swf_bytes: &[u8], out: &TempDir) -> SwfRipResult {
        let swf_buf = swf::decompress_swf(std::io::Cursor::new(swf_bytes)).expect("decompress");
        let version = swf_buf.header.version();
        let mut names = HashMap::new();
        collect_names(&swf_buf.data, version, 0, &mut names);
        rip_tags(&swf_buf.data, version, names, "test", out.0.clone())
    }

    /// A 4-bit mono ADPCM stream of zero codes, which decodes to `frames`
    /// copies of `seed` (see swf_adpcm's tests).
    fn adpcm_payload(seed: i16, frames: usize) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        let mut push = |value: u32, n: u32| {
            for i in (0..n).rev() {
                bits.push((value >> i) & 1 == 1);
            }
        };
        push(2, 2); // AdpcmCodeSize 2 -> 4 bits per code
        push(seed as u16 as u32, 16);
        push(0, 6);
        for _ in 0..frames.saturating_sub(1) {
            push(0, 4);
        }

        let mut bytes = vec![0u8; bits.len().div_ceil(8)];
        for (i, bit) in bits.iter().enumerate() {
            if *bit {
                bytes[i / 8] |= 1 << (7 - (i % 8));
            }
        }
        bytes
    }

    /// The headline fix: ADPCM used to land in the catch-all `_ => Ok(None)`
    /// arm and be reported as an unsupported codec.
    #[test]
    fn extracts_an_adpcm_sound() {
        let out = TempDir::new();
        let payload = adpcm_payload(4321, 64);
        let swf = build_swf(&tag(TAG_DEFINE_SOUND, &define_sound_body(1, 1, &payload, 64)));

        let result = rip(&swf, &out);
        assert_eq!(result.skipped, 0, "ADPCM must not be reported as unsupported");
        assert_eq!(result.failed, 0);
        assert_eq!(result.buttons.len(), 1);

        // Mono 16-bit: 64 frames * 2 bytes, after the 44-byte header.
        let written = fs::read(&result.buttons[0].file).unwrap();
        assert_eq!(&written[0..4], b"RIFF");
        assert_eq!(written.len(), 44 + 64 * 2);
        let first = i16::from_le_bytes([written[44], written[45]]);
        assert_eq!(first, 4321);
    }

    #[test]
    fn extracts_mp3_sound_and_strips_seek_samples_header() {
        let out = TempDir::new();
        let mut payload = vec![0u8, 0]; // SeekSamples
        payload.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let swf = build_swf(&tag(TAG_DEFINE_SOUND, &define_sound_body(1, 2, &payload, 4)));

        let result = rip(&swf, &out);
        assert_eq!(result.buttons.len(), 1);
        assert!(result.buttons[0].file.ends_with(".mp3"));
        assert_eq!(fs::read(&result.buttons[0].file).unwrap(), vec![0xAA, 0xBB, 0xCC, 0xDD]);
    }

    /// Sounds authored inside a movieclip were invisible before: the old
    /// walk only looked at top-level tags.
    #[test]
    fn finds_sounds_nested_inside_a_sprite() {
        let out = TempDir::new();
        let inner = tag(TAG_DEFINE_SOUND, &define_sound_body(7, 1, &adpcm_payload(11, 8), 8));

        let mut sprite_body = Vec::new();
        sprite_body.extend_from_slice(&99u16.to_le_bytes()); // sprite id
        sprite_body.extend_from_slice(&1u16.to_le_bytes()); // frame count
        sprite_body.extend_from_slice(&inner);
        sprite_body.extend_from_slice(&0u16.to_le_bytes()); // End of sprite

        let result = rip(&build_swf(&tag(TAG_DEFINE_SPRITE, &sprite_body)), &out);
        assert_eq!(result.buttons.len(), 1, "sound inside DefineSprite was not found");
    }

    /// The resilience guarantee: one corrupt tag costs one sound, not the
    /// whole rip. `swf::parse_swf` used to bail here and lose everything.
    #[test]
    fn a_corrupt_tag_between_two_sounds_loses_only_that_tag() {
        let out = TempDir::new();
        let mut tags = Vec::new();
        tags.extend_from_slice(&tag(TAG_DEFINE_SOUND, &define_sound_body(1, 1, &adpcm_payload(1, 8), 8)));
        // A DefineSound tag whose body is far too short to parse.
        tags.extend_from_slice(&tag(TAG_DEFINE_SOUND, &[0x01]));
        tags.extend_from_slice(&tag(TAG_DEFINE_SOUND, &define_sound_body(2, 1, &adpcm_payload(2, 8), 8)));

        let result = rip(&build_swf(&tags), &out);
        assert_eq!(result.buttons.len(), 2, "a malformed tag aborted the whole walk");
    }

    /// Unsupported codecs are still found, counted, and named in plain
    /// language rather than as Rust enum spellings.
    #[test]
    fn unsupported_codecs_are_counted_and_named() {
        let out = TempDir::new();
        let mut tags = Vec::new();
        tags.extend_from_slice(&tag(TAG_DEFINE_SOUND, &define_sound_body(1, 6, &[0u8; 16], 16)));
        tags.extend_from_slice(&tag(TAG_DEFINE_SOUND, &define_sound_body(2, 11, &[0u8; 16], 16)));

        let result = rip(&build_swf(&tags), &out);
        assert!(result.buttons.is_empty());
        assert_eq!(result.skipped, 2);
        assert_eq!(result.skipped_codecs, vec!["Nellymoser", "Speex"]);
        // Nothing was extracted, so no stray directory should be left behind.
        assert!(!out.0.exists(), "an empty rip left its output directory behind");
    }

    /// ExportAssets names should win over the generated "Sound N" fallback.
    #[test]
    fn uses_exported_asset_names_for_labels() {
        let out = TempDir::new();
        let mut export_body = Vec::new();
        export_body.extend_from_slice(&1u16.to_le_bytes()); // one export
        export_body.extend_from_slice(&5u16.to_le_bytes()); // character id
        export_body.extend_from_slice(b"laugh\0");

        let mut tags = Vec::new();
        tags.extend_from_slice(&tag(TAG_EXPORT_ASSETS, &export_body));
        tags.extend_from_slice(&tag(TAG_DEFINE_SOUND, &define_sound_body(5, 1, &adpcm_payload(3, 8), 8)));

        let result = rip(&build_swf(&tags), &out);
        assert_eq!(result.buttons.len(), 1);
        assert_eq!(result.buttons[0].label, "laugh");
    }

    /// Manual verification against a real .swf, the way
    /// `realm_of_darkness`'s live test works -- `#[ignore]`d so CI stays
    /// offline and fixture-free. Point it at a file and inspect the results:
    ///
    /// ```sh
    /// SWF_RIP_FIXTURE=~/Downloads/angel_v1.swf \
    ///   cargo test --bin ultimate-soundboard rips_a_real_swf -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore = "needs a real .swf; set SWF_RIP_FIXTURE"]
    fn rips_a_real_swf() {
        let Ok(fixture) = std::env::var("SWF_RIP_FIXTURE") else {
            panic!("set SWF_RIP_FIXTURE to a .swf path");
        };
        let path = PathBuf::from(fixture);
        let data = fs::read(&path).expect("reading fixture");
        let swf_buf = swf::decompress_swf(&data[..]).expect("decompress");
        let version = swf_buf.header.version();
        let mut names = HashMap::new();
        collect_names(&swf_buf.data, version, 0, &mut names);

        let out = PathBuf::from(
            std::env::var("SWF_RIP_OUT").unwrap_or_else(|_| "/tmp/swf-rip-check".into()),
        );
        let _ = fs::remove_dir_all(&out);

        let started = std::time::Instant::now();
        let result = rip_tags(&swf_buf.data, version, names, "fixture", out.clone());
        let elapsed = started.elapsed();

        println!("--- {} (SWF v{version}) ---", path.display());
        println!("extracted: {}", result.buttons.len());
        println!("skipped:   {} {:?}", result.skipped, result.skipped_codecs);
        println!("failed:    {}", result.failed);
        println!("elapsed:   {elapsed:?}");
        println!("output:    {}", out.display());
        for b in result.buttons.iter().take(10) {
            let size = fs::metadata(&b.file).map(|m| m.len()).unwrap_or(0);
            println!("  {:>8} B  {}", size, b.label);
        }

        assert!(!result.buttons.is_empty(), "extracted nothing from a real .swf");
    }

    #[test]
    fn wav_header_round_trips_format_fields() {
        let out = TempDir::new();
        fs::create_dir_all(&out.0).unwrap();

        let format = SoundFormat {
            compression: AudioCompression::Uncompressed,
            sample_rate: 22050,
            is_stereo: true,
            is_16_bit: true,
        };
        let pcm = vec![0u8; 400]; // 100 stereo 16-bit frames
        let path = write_wav_to(&out.0, "Test Sound", &pcm, &format).unwrap();

        let bytes = fs::read(&path).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes([bytes[22], bytes[23]]), 2); // channels
        assert_eq!(u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]), 22050);
        assert_eq!(u16::from_le_bytes([bytes[34], bytes[35]]), 16); // bits
        assert_eq!(bytes.len(), 44 + pcm.len());
    }

    /// A trailing partial frame is trimmed so the data chunk is always a
    /// whole number of frames.
    #[test]
    fn wav_data_chunk_is_a_whole_number_of_frames() {
        let out = TempDir::new();
        fs::create_dir_all(&out.0).unwrap();

        let format = SoundFormat {
            compression: AudioCompression::Uncompressed,
            sample_rate: 11025,
            is_stereo: true,
            is_16_bit: true,
        };
        let path = write_wav_to(&out.0, "Ragged", &vec![0u8; 401], &format).unwrap();
        let bytes = fs::read(&path).unwrap();
        assert_eq!(bytes.len(), 44 + 400, "the odd trailing byte should be dropped");
    }
}
