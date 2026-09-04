use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use crossbeam_channel::{select, unbounded, Receiver, Sender, TryRecvError};
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

// Play and prewarm ride on *separate* channels rather than one job enum,
// because they have opposite urgency and prewarm massively outnumbers
// play. Switching to a tab queues one prewarm per button -- 157 of them
// for a scraped board -- and with a single FIFO queue the click that
// follows lands behind every one of those decodes. That was the "after
// switching tabs it takes a really long time to play a sound" bug: not
// slow decoding, just a click waiting its turn behind a whole tab's
// worth of background work. Two channels let the workers always serve a
// click first (see `decode_worker`).

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

/// Peak level of the mixed output, written by the audio callback and
/// drained by the UI's level meters. Lock-free on purpose: the callback
/// runs on a realtime-priority thread where blocking on a mutex risks
/// audible dropouts. Only one thread writes, only the UI reads-and-resets,
/// so a plain load/compare/store (rather than a CAS loop) is fine -- the
/// worst a lost race can do is drop one frame's worth of meter movement.
#[derive(Default)]
pub struct PeakMeter {
    left: AtomicU32,
    right: AtomicU32,
}

impl PeakMeter {
    fn report(&self, l: f32, r: f32) {
        if l > f32::from_bits(self.left.load(Ordering::Relaxed)) {
            self.left.store(l.to_bits(), Ordering::Relaxed);
        }
        if r > f32::from_bits(self.right.load(Ordering::Relaxed)) {
            self.right.store(r.to_bits(), Ordering::Relaxed);
        }
    }

    /// Peak since the previous call, resetting the accumulator.
    pub fn take(&self) -> (f32, f32) {
        (
            f32::from_bits(self.left.swap(0, Ordering::Relaxed)),
            f32::from_bits(self.right.swap(0, Ordering::Relaxed)),
        )
    }
}

/// Owns the output stream and the decode/mix pipeline. `stop_all()` just
/// clears the voice list, so it's instant regardless of how much is
/// queued up decoding -- this is what makes the global Space-bar stop
/// reliable no matter how many buttons were just mashed.
pub struct AudioEngine {
    play_tx: Sender<PlayJob>,
    prewarm_tx: Sender<PathBuf>,
    voices: Arc<Mutex<Vec<Voice>>>,
    master_gain: Arc<Mutex<f32>>,
    peak: Arc<PeakMeter>,
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
        let peak: Arc<PeakMeter> = Arc::default();

        let stream = build_stream(&device, voices.clone(), master_gain.clone(), peak.clone())
            .context("building output stream")?;
        stream.play().context("starting output stream")?;

        let (play_tx, play_rx): (Sender<PlayJob>, Receiver<PlayJob>) = unbounded();
        let (prewarm_tx, prewarm_rx): (Sender<PathBuf>, Receiver<PathBuf>) = unbounded();

        // One cache behind a mutex, shared by every worker. Previously each
        // worker built its own `DecodeCache`, so with two workers a sound
        // prewarmed by one was invisible to the other and a click had
        // roughly a coin-flip chance of paying full decode cost despite
        // the prewarm having already done that exact work.
        let cache = Arc::new(Mutex::new(DecodeCache::new()));

        for _ in 0..WORKER_COUNT {
            let play_rx = play_rx.clone();
            let prewarm_rx = prewarm_rx.clone();
            let voices = voices.clone();
            let cache = cache.clone();
            thread::spawn(move || decode_worker(play_rx, prewarm_rx, voices, cache));
        }

        Ok(Self { play_tx, prewarm_tx, voices, master_gain, peak, stream, device_name: resolved_name })
    }

    /// Enqueues a play job. Never blocks the caller (UI thread) on decode
    /// work -- the actual decoding happens on the worker pool.
    pub fn play(&self, job: PlayJob) {
        if self.play_tx.send(job).is_err() {
            log::error!("audio worker pool is gone; dropping play request");
        }
    }

    /// Queues background decode-and-cache for every path (skips ones
    /// already fresh in cache almost instantly). Call this whenever the
    /// visible tab changes so its buttons are warm by the time the user
    /// actually clicks one.
    pub fn prewarm(&self, paths: impl IntoIterator<Item = PathBuf>) {
        for path in paths {
            if self.prewarm_tx.send(path).is_err() {
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

    /// File paths currently sounding, mapped to how far through the clip
    /// the furthest-along voice of that file is (0.0-1.0). Polled once per
    /// UI frame to drive the now-playing glow and the progress sweep on
    /// sound buttons -- cheap, just a mutex lock over a handful of voices.
    /// When the same sound is retriggered while still playing, the newest
    /// (least advanced) voice is what the user perceives, but reporting
    /// the furthest-along one keeps the bar monotonic instead of jumping
    /// backwards mid-sweep.
    pub fn playing_progress(&self) -> HashMap<String, f32> {
        let mut out: HashMap<String, f32> = HashMap::new();
        for v in self.voices.lock().unwrap().iter() {
            let total = v.left.len().max(1) as f32;
            let progress = (v.pos as f32 / total).clamp(0.0, 1.0);
            out.entry(v.source.clone())
                .and_modify(|p| *p = p.max(progress))
                .or_insert(progress);
        }
        out
    }

    /// Output peak level since the last call, for the UI's level meters.
    pub fn peak_levels(&self) -> (f32, f32) {
        self.peak.take()
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
/// The decode itself deliberately happens *outside* the cache lock --
/// holding it across `decode_file` would serialize the whole worker pool
/// behind one slow file and undo the point of having more than one
/// worker. Two workers racing on the same uncached path can each decode
/// it once; that's rare, harmless, and much cheaper than the contention
/// a decode-under-lock would cause.
fn decode_cached(cache: &Mutex<DecodeCache>, path: &Path) -> Option<Arc<Decoded>> {
    let key = fingerprint(path);
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return Some(hit);
    }
    match decode::decode_file(path) {
        Ok(d) => Some(cache.lock().unwrap().put(key, d)),
        Err(e) => {
            log::error!("failed to decode {}: {e}", path.display());
            None
        }
    }
}

fn decode_worker(
    play_rx: Receiver<PlayJob>,
    prewarm_rx: Receiver<PathBuf>,
    voices: Arc<Mutex<Vec<Voice>>>,
    cache: Arc<Mutex<DecodeCache>>,
) {
    loop {
        // Drain every pending play before even looking at prewarm work.
        // This is the whole reason the two are on separate channels: a
        // click must never wait behind a tab's worth of background
        // decodes. A click can still wait on one already-in-flight
        // prewarm decode per worker, which is a single file's worth of
        // delay rather than a whole tab's.
        match play_rx.try_recv() {
            Ok(job) => {
                handle_play(job, &voices, &cache);
                continue;
            }
            Err(TryRecvError::Disconnected) => return,
            Err(TryRecvError::Empty) => {}
        }

        // Nothing to play right now, so block until either channel has
        // work rather than spinning.
        select! {
            recv(play_rx) -> msg => match msg {
                Ok(job) => handle_play(job, &voices, &cache),
                Err(_) => return,
            },
            recv(prewarm_rx) -> msg => match msg {
                Ok(path) => {
                    if path.exists() {
                        decode_cached(&cache, &path);
                    }
                }
                Err(_) => return,
            },
        }
    }
}

fn handle_play(job: PlayJob, voices: &Mutex<Vec<Voice>>, cache: &Mutex<DecodeCache>) {
    if !job.path.exists() {
        log::warn!("play requested for missing file: {}", job.path.display());
        return;
    }

    let Some(decoded) = decode_cached(cache, &job.path) else { return };

    let (left, right) = if job.pitch_semitones.abs() > 0.01 {
        pitch::shift_stereo(&decoded.left, &decoded.right, job.pitch_semitones, decode::TARGET_SR as f32)
    } else {
        (decoded.left.clone(), decoded.right.clone())
    };

    let volume = job.volume.clamp(0.0, 2.0);
    let left: Vec<f32> = left.iter().map(|s| s * volume).collect();
    let right: Vec<f32> = right.iter().map(|s| s * volume).collect();

    if left.is_empty() {
        return;
    }

    let source = job.path.to_string_lossy().to_string();
    voices.lock().unwrap().push(Voice { left, right, pos: 0, source });
}

fn build_stream(
    device: &cpal::Device,
    voices: Arc<Mutex<Vec<Voice>>>,
    gain: Arc<Mutex<f32>>,
    peak: Arc<PeakMeter>,
) -> Result<cpal::Stream> {
    let forced = cpal::StreamConfig {
        channels: 2,
        sample_rate: decode::TARGET_SR,
        buffer_size: cpal::BufferSize::Default,
    };

    // Prefer float32 output at our fixed 48kHz/stereo target -- matches
    // what the Python app forced via PortAudio and works transparently
    // with PipeWire/WASAPI shared-mode resampling on real hardware.
    if let Ok(stream) =
        build_typed_stream::<f32>(device, forced.clone(), voices.clone(), gain.clone(), peak.clone())
    {
        return Ok(stream);
    }
    log::warn!("48kHz/stereo/f32 output stream not available, trying device's native config");

    let supported = device.default_output_config().context("no default output config")?;
    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();

    match sample_format {
        cpal::SampleFormat::F32 => build_typed_stream::<f32>(device, config, voices, gain, peak),
        cpal::SampleFormat::I16 => build_typed_stream::<i16>(device, config, voices, gain, peak),
        cpal::SampleFormat::U16 => build_typed_stream::<u16>(device, config, voices, gain, peak),
        other => Err(anyhow!("unsupported device sample format: {other:?}")),
    }
}

fn build_typed_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    voices: Arc<Mutex<Vec<Voice>>>,
    gain: Arc<Mutex<f32>>,
    peak: Arc<PeakMeter>,
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

                let mut peak_l = 0.0f32;
                let mut peak_r = 0.0f32;

                for f in 0..frames {
                    let l = (mix[f * 2] * g).clamp(-1.0, 1.0);
                    let r = (mix[f * 2 + 1] * g).clamp(-1.0, 1.0);
                    peak_l = peak_l.max(l.abs());
                    peak_r = peak_r.max(r.abs());
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

                peak.report(peak_l, peak_r);
            },
            err_fn,
            None,
        )
        .context("device.build_output_stream")?;

    Ok(stream)
}
