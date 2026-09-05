use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, StreamTrait};
use crossbeam_channel::{select, unbounded, Receiver, Sender, TryRecvError};
use lru::LruCache;
use uuid::Uuid;

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
    /// Which button triggered this, so its sliders can keep acting on the
    /// sound after it has started (see `set_button_gain`/`retune_button`).
    pub button_id: Uuid,
}

/// Re-render a sounding voice at a new pitch and crossfade into it.
struct RetuneJob {
    button_id: Uuid,
    path: PathBuf,
    semitones: f32,
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

/// Samples spent crossfading from the old rendering to a retuned one.
/// Long enough to hide the phase discontinuity a phase vocoder leaves
/// when you splice two renderings together, short enough that the pitch
/// change still feels immediate -- about 5ms at 48kHz.
const RETUNE_FADE: usize = 256;

/// Per-sample gain smoothing coefficient. Jumping the gain in one step
/// while audio is running produces zipper noise on every slider move, so
/// the voice glides to its target instead.
const GAIN_GLIDE: f32 = 0.002;

/// Faster glide used when a voice is being stopped. Cutting samples dead
/// mid-waveform is a step discontinuity, which speakers reproduce as an
/// audible click -- mashing Space on a busy board made the whole board
/// pop. ~15ms is short enough to still feel instant.
const RELEASE_GLIDE: f32 = 0.01;

struct Voice {
    left: Vec<f32>,
    right: Vec<f32>,
    pos: usize,
    /// The button's file path, as given to `PlayJob` -- used only for the
    /// now-playing UI indicator (`playing_paths`), never for playback.
    source: String,
    /// The button that started this voice. Volume and pitch are applied
    /// live by looking voices up with it, rather than being baked into
    /// the samples at trigger time.
    button_id: Uuid,
    gain: f32,
    target_gain: f32,
    semitones: f32,
    /// The rendering being faded out of, during a retune.
    fade_from: Option<(Vec<f32>, Vec<f32>)>,
    fade_pos: usize,
    /// Voice is releasing towards silence and should be dropped once it
    /// gets there.
    stopping: bool,
}

impl Voice {
    /// Sample pair at the current position, blended with the outgoing
    /// rendering while a retune crossfade is in flight.
    fn sample(&self, offset: usize) -> (f32, f32) {
        let i = self.pos + offset;
        let (mut l, mut r) = (self.left[i], self.right[i]);

        if let Some((old_l, old_r)) = &self.fade_from {
            let t = ((self.fade_pos + offset) as f32 / RETUNE_FADE as f32).clamp(0.0, 1.0);
            if i < old_l.len() {
                l = old_l[i] * (1.0 - t) + l * t;
                r = old_r[i] * (1.0 - t) + r * t;
            }
        }
        (l, r)
    }
}

/// Path + mtime + size + pitch. Pitch is part of the key because a
/// shifted rendering is cached alongside the raw one: the phase vocoder
/// costs ~50ms per 3s clip in release (and over a second in a debug
/// build), and it used to run on *every* play of a pitched button.
type CacheKey = (PathBuf, u64, u64, i32);

/// Quantized so slider values that round to the same hundredth of a
/// semitone share a cache entry instead of each rendering afresh.
fn pitch_key(semitones: f32) -> i32 {
    (semitones * 100.0).round() as i32
}

fn fingerprint(path: &Path, semitones: f32) -> CacheKey {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            (path.to_path_buf(), mtime, meta.len(), pitch_key(semitones))
        }
        Err(_) => (path.to_path_buf(), 0, 0, pitch_key(semitones)),
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

/// Owns the output stream and the decode/mix pipeline. `stop_all()` only
/// touches voices that already exist, so it's instant regardless of how
/// much is queued up decoding -- this is what makes the Space-bar stop
/// reliable no matter how many buttons were just mashed.
pub struct AudioEngine {
    play_tx: Sender<PlayJob>,
    prewarm_tx: Sender<(PathBuf, f32)>,
    retune_tx: Sender<RetuneJob>,
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
        let (prewarm_tx, prewarm_rx): (Sender<(PathBuf, f32)>, Receiver<(PathBuf, f32)>) = unbounded();
        let (retune_tx, retune_rx): (Sender<RetuneJob>, Receiver<RetuneJob>) = unbounded();

        // One cache behind a mutex, shared by every worker. Previously each
        // worker built its own `DecodeCache`, so with two workers a sound
        // prewarmed by one was invisible to the other and a click had
        // roughly a coin-flip chance of paying full decode cost despite
        // the prewarm having already done that exact work.
        let cache = Arc::new(Mutex::new(DecodeCache::new()));

        for _ in 0..WORKER_COUNT {
            let play_rx = play_rx.clone();
            let prewarm_rx = prewarm_rx.clone();
            let retune_rx = retune_rx.clone();
            let voices = voices.clone();
            let cache = cache.clone();
            thread::spawn(move || decode_worker(play_rx, prewarm_rx, retune_rx, voices, cache));
        }

        Ok(Self {
            play_tx,
            prewarm_tx,
            retune_tx,
            voices,
            master_gain,
            peak,
            stream,
            device_name: resolved_name,
        })
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
    pub fn prewarm(&self, jobs: impl IntoIterator<Item = (PathBuf, f32)>) {
        for job in jobs {
            if self.prewarm_tx.send(job).is_err() {
                break;
            }
        }
    }

    /// Applies a button's volume to anything it already has sounding.
    ///
    /// Cheap enough to call on every slider frame: it only touches the
    /// voice's target gain, which the callback glides towards, so there
    /// is no re-render and no zipper noise.
    pub fn set_button_gain(&self, button_id: Uuid, gain: f32) {
        if let Ok(mut voices) = self.voices.lock() {
            for v in voices.iter_mut().filter(|v| v.button_id == button_id) {
                v.target_gain = gain.clamp(0.0, 2.0);
            }
        }
    }

    /// Re-pitches anything this button already has sounding.
    ///
    /// Unlike gain this needs the clip re-rendered, so it goes to the
    /// worker pool rather than being applied inline.
    pub fn retune_button(&self, button_id: Uuid, path: PathBuf, semitones: f32) {
        let needed = self
            .voices
            .lock()
            .map(|v| v.iter().any(|v| v.button_id == button_id))
            .unwrap_or(false);
        if !needed {
            return;
        }
        let _ = self.retune_tx.send(RetuneJob { button_id, path, semitones });
    }

    /// Releases every currently-playing sound. Bound to the
    /// global Space hotkey. Does not touch the decode queue, so sounds
    /// already in flight when Space is pressed just won't produce a voice.
    pub fn stop_all(&self) {
        if let Ok(mut voices) = self.voices.lock() {
            for v in voices.iter_mut() {
                v.stopping = true;
                v.target_gain = 0.0;
            }
        }
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
    let key = fingerprint(path, 0.0);
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

/// The clip rendered at `semitones`, cached like the raw decode.
///
/// Without this, every play of a pitched button re-ran the phase vocoder
/// over the whole clip before a single sample reached the speakers. That
/// is what made pitched buttons feel broken: the cost is paid once per
/// (file, pitch) now, and prewarming pays it before the first click.
fn shifted_cached(cache: &Mutex<DecodeCache>, path: &Path, semitones: f32) -> Option<Arc<Decoded>> {
    if semitones.abs() < 0.01 {
        return decode_cached(cache, path);
    }

    let key = fingerprint(path, semitones);
    if let Some(hit) = cache.lock().unwrap().get(&key) {
        return Some(hit);
    }

    let raw = decode_cached(cache, path)?;
    let (left, right) =
        pitch::shift_stereo(&raw.left, &raw.right, semitones, decode::TARGET_SR as f32);
    Some(cache.lock().unwrap().put(key, Decoded { left, right }))
}

fn decode_worker(
    play_rx: Receiver<PlayJob>,
    prewarm_rx: Receiver<(PathBuf, f32)>,
    retune_rx: Receiver<RetuneJob>,
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

        // Retunes are ahead of prewarm too: they're a response to a live
        // slider, so they need to land while the user is still listening.
        match retune_rx.try_recv() {
            Ok(job) => {
                handle_retune(job, &voices, &cache);
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
            recv(retune_rx) -> msg => match msg {
                Ok(job) => handle_retune(job, &voices, &cache),
                Err(_) => return,
            },
            recv(prewarm_rx) -> msg => match msg {
                Ok((path, semitones)) => {
                    if path.exists() {
                        // Warms the *pitched* rendering, not just the raw
                        // decode: a pitched button whose vocoder pass
                        // hasn't been done yet still stalls on first click.
                        shifted_cached(&cache, &path, semitones);
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

    let Some(decoded) = shifted_cached(cache, &job.path, job.pitch_semitones) else { return };

    if decoded.left.is_empty() {
        return;
    }

    // Volume is *not* baked into the samples any more -- it lives on the
    // voice so the button's slider keeps working on a sound that is
    // already playing.
    let gain = job.volume.clamp(0.0, 2.0);
    let source = job.path.to_string_lossy().to_string();

    voices.lock().unwrap().push(Voice {
        left: decoded.left.clone(),
        right: decoded.right.clone(),
        pos: 0,
        source,
        button_id: job.button_id,
        gain,
        target_gain: gain,
        semitones: job.pitch_semitones,
        fade_from: None,
        fade_pos: 0,
        stopping: false,
    });
}

/// Swaps a sounding voice over to a rendering at a new pitch, keeping its
/// playback position.
///
/// Position carries over directly because the shift is duration
/// preserving: sample N in the retuned rendering is the same instant of
/// the clip as sample N in the old one. The crossfade covers the phase
/// discontinuity between two independent vocoder runs, which would
/// otherwise be an audible click.
fn handle_retune(job: RetuneJob, voices: &Mutex<Vec<Voice>>, cache: &Mutex<DecodeCache>) {
    let matches_any = voices
        .lock()
        .unwrap()
        .iter()
        .any(|v| v.button_id == job.button_id && pitch_key(v.semitones) != pitch_key(job.semitones));
    if !matches_any {
        return;
    }

    let Some(decoded) = shifted_cached(cache, &job.path, job.semitones) else { return };

    let mut voices = voices.lock().unwrap();
    for v in voices.iter_mut() {
        if v.button_id != job.button_id || pitch_key(v.semitones) == pitch_key(job.semitones) {
            continue;
        }
        if v.pos >= decoded.left.len() {
            continue;
        }
        let old = (std::mem::take(&mut v.left), std::mem::take(&mut v.right));
        v.left = decoded.left.clone();
        v.right = decoded.right.clone();
        v.semitones = job.semitones;
        v.fade_from = Some(old);
        v.fade_pos = 0;
    }
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
                        let glide = if v.stopping { RELEASE_GLIDE } else { GAIN_GLIDE };
                        for f in 0..n {
                            // Glide rather than jump, so moving a volume
                            // slider under a playing sound doesn't step
                            // the gain and buzz.
                            v.gain += (v.target_gain - v.gain) * glide;
                            let (l, r) = v.sample(f);
                            mix[f * 2] += l * v.gain;
                            mix[f * 2 + 1] += r * v.gain;
                        }
                        v.pos += n;

                        // Retire a released voice once it's inaudible.
                        if v.stopping && v.gain < 0.0005 {
                            return false;
                        }

                        if v.fade_from.is_some() {
                            v.fade_pos += n;
                            if v.fade_pos >= RETUNE_FADE {
                                v.fade_from = None;
                                v.fade_pos = 0;
                            }
                        }

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
