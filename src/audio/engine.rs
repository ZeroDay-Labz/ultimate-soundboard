use std::collections::HashSet;
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

// Byte-budgeted, not item-count-limited: a big single tab's worth of
// sounds (the Realm of Darkness scraper alone can pull down 150+ per
// board) should fit comfortably without forcing an explicit "drop the
// old tab's cache" step -- plain LRU eviction naturally ages out
// whichever tab hasn't been visited recently once the budget is hit.
const DECODE_CACHE_MAX_BYTES: usize = 512 * 1024 * 1024;
const WORKER_COUNT: usize = 2;

/// A request to play one sound. Volume and pitch are already resolved to
/// their final per-play values (button volume * tab volume, etc.) by the
/// caller -- the engine itself doesn't know about tabs/buttons.
pub struct PlayJob {
    pub path: PathBuf,
    pub volume: f32,
    pub pitch_semitones: f32,
}

enum EngineJob {
    Play(PlayJob),
    /// Decode-and-cache-only, no voice produced. Used to warm the decode
    /// cache for a tab's sounds in the background (on startup and on tab
    /// switch) so the *first* click on a button is as fast as every
    /// click after it, instead of paying full decode+resample+ffmpeg-
    /// fallback cost on that first click.
    Prewarm(PathBuf),
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
    job_tx: Sender<EngineJob>,
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

        let (job_tx, job_rx): (Sender<EngineJob>, Receiver<EngineJob>) = unbounded();

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
        if self.job_tx.send(EngineJob::Play(job)).is_err() {
            log::error!("audio worker pool is gone; dropping play request");
        }
    }

    /// Queues background decode-and-cache for every path (skips ones
    /// already fresh in cache almost instantly). Call this whenever the
    /// visible tab changes so its buttons are warm by the time the user
    /// actually clicks one.
    pub fn prewarm(&self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if self.job_tx.send(EngineJob::Prewarm(path)).is_err() {
                break;
            }
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

struct DecodeCache {
    items: LruCache<CacheKey, Arc<Decoded>>,
    bytes: usize,
}

impl DecodeCache {
    fn new() -> Self {
        Self { items: LruCache::unbounded(), bytes: 0 }
    }

    fn get(&mut self, key: &CacheKey) -> Option<Arc<Decoded>> {
        self.items.get(key).cloned()
    }

    fn put(&mut self, key: CacheKey, decoded: Decoded) -> Arc<Decoded> {
        let size = decoded.byte_size();
        let arc = Arc::new(decoded);
        self.bytes += size;
        if let Some(evicted) = self.items.put(key, arc.clone()) {
            self.bytes = self.bytes.saturating_sub(evicted.byte_size());
        }
        while self.bytes > DECODE_CACHE_MAX_BYTES {
            match self.items.pop_lru() {
                Some((_, v)) => self.bytes = self.bytes.saturating_sub(v.byte_size()),
                None => break,
            }
        }
        arc
    }
}

/// Decodes (using the shared cache) or returns the cached result. Shared
/// by both play jobs and prewarm jobs so a prewarm followed by an actual
/// click never decodes twice.
fn decode_cached(cache: &mut DecodeCache, path: &Path) -> Option<Arc<Decoded>> {
    let key = fingerprint(path);
    if let Some(hit) = cache.get(&key) {
        return Some(hit);
    }
    match decode::decode_file(path) {
        Ok(d) => Some(cache.put(key, d)),
        Err(e) => {
            log::error!("failed to decode {}: {e}", path.display());
            None
        }
    }
}

fn decode_worker(job_rx: Receiver<EngineJob>, voices: Arc<Mutex<Vec<Voice>>>) {
    let mut cache = DecodeCache::new();

    while let Ok(job) = job_rx.recv() {
        match job {
            EngineJob::Prewarm(path) => {
                if path.exists() {
                    decode_cached(&mut cache, &path);
                }
            }
            EngineJob::Play(job) => {
                if !job.path.exists() {
                    log::warn!("play requested for missing file: {}", job.path.display());
                    continue;
                }

                let Some(decoded) = decode_cached(&mut cache, &job.path) else { continue };

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
