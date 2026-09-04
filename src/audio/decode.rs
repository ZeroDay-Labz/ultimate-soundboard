use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{anyhow, Context, Result};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::resample::resample_stereo;

pub const TARGET_SR: u32 = 48000;

/// Planar stereo PCM at 48kHz -- the canonical decoded form used
/// everywhere downstream (pitch-shift and the mixer both want per-channel
/// access, so we deinterleave once here and never look back).
#[derive(Clone)]
pub struct Decoded {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Decoded {
    pub fn frames(&self) -> usize {
        self.left.len()
    }

    /// Rough in-memory size, used by the LRU decode cache to budget itself.
    pub fn byte_size(&self) -> usize {
        (self.left.len() + self.right.len()) * std::mem::size_of::<f32>()
    }
}

/// Decodes any supported audio file to 48kHz stereo float PCM. Tries the
/// pure-Rust Symphonia decoder first (covers wav/mp3/flac/aac/ogg-vorbis
/// with no system dependency); if that fails -- most notably for
/// Opus/Discord-style ogg, which Symphonia doesn't decode -- falls back to
/// shelling out to `ffmpeg`, exactly like the original Python app did for
/// the same class of file.
pub fn decode_file(path: &Path) -> Result<Decoded> {
    match decode_with_symphonia(path) {
        Ok(d) => Ok(d),
        Err(symphonia_err) => {
            log::warn!("symphonia could not decode {path:?} ({symphonia_err}); trying ffmpeg");
            decode_with_ffmpeg(path)
                .with_context(|| format!("ffmpeg fallback also failed for {}", path.display()))
        }
    }
}

fn decode_with_symphonia(path: &Path) -> Result<Decoded> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe().format(
        &hint,
        mss,
        &FormatOptions::default(),
        &MetadataOptions::default(),
    )?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .cloned()
        .ok_or_else(|| anyhow!("no playable audio track"))?;
    let track_id = track.id;

    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())?;

    let native_sr = track
        .codec_params
        .sample_rate
        .ok_or_else(|| anyhow!("unknown sample rate"))?;

    let mut interleaved: Vec<f32> = Vec::new();
    let mut native_channels: usize = track.codec_params.channels.map(|c| c.count()).unwrap_or(2).max(1);

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(e.into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(audio_buf) => {
                let spec = *audio_buf.spec();
                native_channels = spec.channels.count().max(1);
                let frames = audio_buf.frames() as u64;
                if frames == 0 {
                    continue;
                }
                let mut sample_buf = SampleBuffer::<f32>::new(frames, spec);
                sample_buf.copy_interleaved_ref(audio_buf);
                interleaved.extend_from_slice(sample_buf.samples());
            }
            Err(SymphoniaError::DecodeError(msg)) => {
                log::debug!("skipping bad packet in {}: {msg}", path.display());
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }

    if interleaved.is_empty() {
        return Err(anyhow!("no audio decoded"));
    }

    let (left, right) = deinterleave_to_stereo(&interleaved, native_channels);
    to_target_rate(left, right, native_sr)
}

fn deinterleave_to_stereo(interleaved: &[f32], channels: usize) -> (Vec<f32>, Vec<f32>) {
    let channels = channels.max(1);
    let frames = interleaved.len() / channels;
    let mut left = Vec::with_capacity(frames);
    let mut right = Vec::with_capacity(frames);
    for frame in interleaved.chunks_exact(channels) {
        if channels == 1 {
            left.push(frame[0]);
            right.push(frame[0]);
        } else {
            left.push(frame[0]);
            right.push(frame[1]);
        }
    }
    (left, right)
}

fn to_target_rate(left: Vec<f32>, right: Vec<f32>, native_sr: u32) -> Result<Decoded> {
    if native_sr == TARGET_SR || left.is_empty() {
        return Ok(Decoded { left, right });
    }
    match resample_stereo(&left, &right, native_sr, TARGET_SR) {
        Ok((l, r)) => Ok(Decoded { left: l, right: r }),
        Err(e) => {
            log::warn!("resample {native_sr}->{TARGET_SR} failed ({e}); using un-resampled audio");
            Ok(Decoded { left, right })
        }
    }
}

// ---------------- ffmpeg fallback ----------------

fn ffmpeg_path() -> String {
    if let Ok(p) = std::env::var("ULTIMATE_SOUNDBOARD_FFMPEG") {
        if !p.trim().is_empty() {
            return p;
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate.to_string_lossy().to_string();
            }
        }
    }
    "ffmpeg".to_string()
}

fn decode_with_ffmpeg(path: &Path) -> Result<Decoded> {
    let ffmpeg = ffmpeg_path();
    let output = Command::new(&ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error"])
        .args(["-probesize", "5M", "-analyzeduration", "0"])
        .arg("-i")
        .arg(path)
        .args(["-vn", "-ac", "2", "-ar", &TARGET_SR.to_string(), "-f", "f32le", "pipe:1"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| {
            format!("failed to spawn `{ffmpeg}` -- install ffmpeg or set ULTIMATE_SOUNDBOARD_FFMPEG")
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!("ffmpeg decode failed: {}", stderr.trim()));
    }

    let raw = output.stdout;
    let frame_count = raw.len() / 8; // f32 stereo = 8 bytes/frame
    if frame_count == 0 {
        return Err(anyhow!("ffmpeg produced no audio data"));
    }

    let mut left = Vec::with_capacity(frame_count);
    let mut right = Vec::with_capacity(frame_count);
    for chunk in raw.chunks_exact(8) {
        left.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        right.push(f32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
    }

    Ok(Decoded { left, right })
}
