use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use crossbeam_channel::{unbounded, Receiver, Sender};
use lru::LruCache;

use super::decode::{self, Decoded};
use super::devices;
use super::pitch;

const DECODE_CACHE_CAPACITY: usize = 64;
const WORKER_COUNT: usize = 2;

/// A request to play one sound. Volume and pitch are already resolved to
/// their final per-play values (button volume * tab volume, etc.) by the
/// caller -- the engine itself doesn't know about tabs/buttons.
pub struct PlayJob {
    pub path: PathBuf,
    pub volume: f32,
    pub pitch_semitones: f32,
}

struct Voice {
    left: Vec<f32>,
    right: Vec<f32>,
    pos: usize,
    /// The button's file path, as given to `PlayJob` -- used only for the
    /// now-playing UI indicator (`playing_paths`), never for playback.
    source: String,
}

type CacheKey = (PathBuf, u64, u64);

fn fingerprint(path: &Path) -> CacheKey {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (path.to_path_buf(), mtime, meta.len())
        }
        Err(_) => (path.to_path_buf(), 0, 0),
    }
}

/// Owns the output stream and the decode/mix pipeline. `stop_all()` just
/// clears the voice list, so it's instant regardless of how much is
/// queued up decoding -- this is what makes the global Space-bar stop
/// reliable no matter how many buttons were just mashed.
pub struct AudioEngine {
    job_tx: Sender<PlayJob>,
    voices: Arc<Mutex<Vec<Voice>>>,
    master_gain: Arc<Mutex<f32>>,
    stream: cpal::Stream,
    pub device_name: Option<String>,
}

impl AudioEngine {
    /// Builds the output stream for `device_name` (or the system default
    /// if `None`/not found) and starts the decode worker pool. This does
    /// real I/O (device enumeration, stream setup) so callers on a UI
    /// thread should run it off the main thread and poll for completion.
    pub fn new(device_name: Option<&str>) -> Result<Self> {
        let device = devices::resolve_device(device_name)
            .ok_or_else(|| anyhow!("no audio output device available"))?;
        let resolved_name = Some(device.to_string());

        let voices: Arc<Mutex<Vec<Voice>>> = Arc::new(Mutex::new(Vec::new()));
        let master_gain = Arc::new(Mutex::new(1.0f32));

        let stream = build_stream(&device, voices.clone(), master_gain.clone())
            .context("building output stream")?;
        stream.play().context("starting output stream")?;

        let (job_tx, job_rx): (Sender<PlayJob>, Receiver<PlayJob>) = unbounded();

        for _ in 0..WORKER_COUNT {
            let job_rx = job_rx.clone();
            let voices = voices.clone();
            thread::spawn(move || decode_worker(job_rx, voices));
        }

        Ok(Self { job_tx, voices, master_gain, stream, device_name: resolved_name })
    }

    /// Enqueues a play job. Never blocks the caller (UI thread) on decode
    /// work -- the actual decoding happens on the worker pool.
    pub fn play(&self, job: PlayJob) {
        if self.job_tx.send(job).is_err() {
            log::error!("audio worker pool is gone; dropping play request");
        }
    }

    /// Stops every currently-playing sound immediately. Bound to the
    /// global Space hotkey. Does not touch the decode queue, so sounds
    /// already in flight when Space is pressed just won't produce a voice.
    pub fn stop_all(&self) {
        self.voices.lock().unwrap().clear();
    }

    pub fn set_master_gain(&self, gain: f32) {
        *self.master_gain.lock().unwrap() = gain.clamp(0.0, 2.0);
    }

    /// File paths (as given to `PlayJob`) with at least one voice still
    /// sounding right now. Polled once per UI frame to drive the
    /// now-playing glow on sound buttons -- cheap, just a mutex lock over
    /// a handful of small voices.
    pub fn playing_paths(&self) -> HashSet<String> {
        self.voices.lock().unwrap().iter().map(|v| v.source.clone()).collect()
    }

    pub fn pause(&self) {
        let _ = self.stream.pause();
    }

    pub fn resume(&self) {
        let _ = self.stream.play();
    }
}

fn decode_worker(job_rx: Receiver<PlayJob>, voices: Arc<Mutex<Vec<Voice>>>) {
    let mut cache: LruCache<CacheKey, Arc<Decoded>> =
        LruCache::new(NonZeroUsize::new(DECODE_CACHE_CAPACITY).unwrap());

    while let Ok(job) = job_rx.recv() {
        if !job.path.exists() {
            log::warn!("play requested for missing file: {}", job.path.display());
            continue;
        }

        let key = fingerprint(&job.path);
        let decoded = if let Some(hit) = cache.get(&key) {
            hit.clone()
        } else {
            match decode::decode_file(&job.path) {
                Ok(d) => {
                    let arc = Arc::new(d);
                    cache.put(key, arc.clone());
                    arc
                }
                Err(e) => {
                    log::error!("failed to decode {}: {e}", job.path.display());
                    continue;
                }
            }
        };

        let (left, right) = if job.pitch_semitones.abs() > 0.01 {
            pitch::shift_stereo(&decoded.left, &decoded.right, job.pitch_semitones, decode::TARGET_SR as f32)
        } else {
            (decoded.left.clone(), decoded.right.clone())
        };

        let volume = job.volume.clamp(0.0, 2.0);
        let left: Vec<f32> = left.iter().map(|s| s * volume).collect();
        let right: Vec<f32> = right.iter().map(|s| s * volume).collect();

        if left.is_empty() {
            continue;
        }

        let source = job.path.to_string_lossy().to_string();
        voices.lock().unwrap().push(Voice { left, right, pos: 0, source });
    }
}

fn build_stream(
    device: &cpal::Device,
    voices: Arc<Mutex<Vec<Voice>>>,
    gain: Arc<Mutex<f32>>,
) -> Result<cpal::Stream> {
    let forced = cpal::StreamConfig {
        channels: 2,
        sample_rate: decode::TARGET_SR,
        buffer_size: cpal::BufferSize::Default,
    };

    // Prefer float32 output at our fixed 48kHz/stereo target -- matches
    // what the Python app forced via PortAudio and works transparently
    // with PipeWire/WASAPI shared-mode resampling on real hardware.
    if let Ok(stream) = build_typed_stream::<f32>(device, forced.clone(), voices.clone(), gain.clone()) {
        return Ok(stream);
    }
    log::warn!("48kHz/stereo/f32 output stream not available, trying device's native config");

    let supported = device.default_output_config().context("no default output config")?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    match sample_format {
        cpal::SampleFormat::F32 => build_typed_stream::<f32>(device, config, voices, gain),
        cpal::SampleFormat::I16 => build_typed_stream::<i16>(device, config, voices, gain),
        cpal::SampleFormat::U16 => build_typed_stream::<u16>(device, config, voices, gain),
        other => Err(anyhow!("unsupported device sample format: {other:?}")),
    }
}

fn build_typed_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    voices: Arc<Mutex<Vec<Voice>>>,
    gain: Arc<Mutex<f32>>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let channels = config.channels.max(1) as usize;
    let scratch: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let warned = Arc::new(AtomicBool::new(false));

    let err_fn = move |e| log::error!("audio stream error: {e}");

    let stream = device
        .build_output_stream(
            config,
            move |data: &mut [T], _info: &cpal::OutputCallbackInfo| {
                let frames = data.len() / channels;
                let g = match gain.lock() {
                    Ok(g) => *g,
                    Err(_) => 1.0,
                };

                let mut mix = match scratch.lock() {
                    Ok(m) => m,
                    Err(_) => {
                        if !warned.swap(true, Ordering::Relaxed) {
                            log::error!("audio scratch buffer mutex poisoned");
                        }
                        return;
                    }
                };
                mix.clear();
                mix.resize(frames * 2, 0.0);

                if let Ok(mut voices) = voices.lock() {
                    voices.retain_mut(|v| {
                        let remaining = v.left.len().saturating_sub(v.pos);
                        if remaining == 0 {
                            return false;
                        }
                        let n = remaining.min(frames);
                        for f in 0..n {
                            mix[f * 2] += v.left[v.pos + f];
                            mix[f * 2 + 1] += v.right[v.pos + f];
                        }
                        v.pos += n;
                        v.pos < v.left.len()
                    });
                }

                for f in 0..frames {
                    let l = (mix[f * 2] * g).clamp(-1.0, 1.0);
                    let r = (mix[f * 2 + 1] * g).clamp(-1.0, 1.0);
                    let base = f * channels;
                    if channels == 1 {
                        data[base] = T::from_sample((l + r) * 0.5);
                    } else {
                        data[base] = T::from_sample(l);
                        data[base + 1] = T::from_sample(r);
                        for c in data[base + 2..base + channels].iter_mut() {
                            *c = T::from_sample(0.0f32);
                        }
                    }
                }
            },
            err_fn,
            None,
        )
        .context("device.build_output_stream")?;

    Ok(stream)
}
