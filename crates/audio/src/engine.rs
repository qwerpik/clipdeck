//! The control side of playback: routes triggers to the monitor and virtual-mic mixers,
//! decodes on worker threads, caches clips, measures loudness, and recovers from device loss.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use clipdeck_core::model::{PlaybackMode, Route, Trim};
use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use parking_lot::Mutex;
use rtrb::RingBuffer;

use crate::clip::{Clip, ClipCache};
use crate::decode;
use crate::error::Result;
use crate::loudness::{self, LoudnessCache};
use crate::mixer::{Command, MixerEvent, MixerHandle, Source, VoiceSpec, memory_source, mixer};
use crate::output::{self, InputStream, OutputStream, RenderFn, VirtualMic};
use crate::resample::LinearResampler;

/// Clips longer than this are streamed from disk instead of decoded into RAM.
pub const STREAM_THRESHOLD_MS: u32 = 60_000;

/// Engine settings (a subset of the user config).
#[derive(Debug, Clone, PartialEq)]
pub struct EngineConfig {
    /// Monitor output device (`None` = default).
    pub monitor_device: Option<String>,
    /// Microphone to pass through (`None` = default).
    pub input_device: Option<String>,
    /// Play to your own output.
    pub monitor_enabled: bool,
    /// Mix the real mic into the virtual mic.
    pub mic_passthrough: bool,
    /// Master sound volume 0..1.
    pub master_volume: f32,
    /// Monitor volume 0..1.
    pub monitor_volume: f32,
    /// Mic passthrough gain 0..2.
    pub mic_volume: f32,
    /// Requested buffer size.
    pub buffer_frames: u32,
    /// Normalize loudness.
    pub normalize: bool,
    /// Target LUFS.
    pub target_lufs: f32,
    /// Mute the real mic while a sound plays on the mic.
    pub block_voice: bool,
    /// Clip cache budget in bytes.
    pub cache_bytes: usize,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            monitor_device: None,
            input_device: None,
            monitor_enabled: true,
            mic_passthrough: true,
            master_volume: 0.8,
            monitor_volume: 0.6,
            mic_volume: 1.0,
            buffer_frames: 256,
            normalize: true,
            target_lufs: -16.0,
            block_voice: false,
            cache_bytes: 512 << 20,
        }
    }
}

impl EngineConfig {
    /// Build from the user config.
    pub fn from_config(a: &clipdeck_core::config::AudioConfig) -> Self {
        EngineConfig {
            monitor_device: a.monitor_device.clone(),
            input_device: a.input_device.clone(),
            monitor_enabled: a.monitor_enabled,
            mic_passthrough: a.mic_passthrough,
            master_volume: a.master_volume,
            monitor_volume: a.monitor_volume,
            mic_volume: a.mic_volume,
            buffer_frames: a.buffer_frames,
            normalize: a.normalize,
            target_lufs: a.target_lufs,
            block_voice: a.block_voice,
            ..Default::default()
        }
    }
}

/// What to play.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayRequest {
    /// Sound id (for restart semantics and events).
    pub sound: u64,
    /// File.
    pub path: PathBuf,
    /// Per-sound volume.
    pub volume: f32,
    /// Trim window.
    pub trim: Trim,
    /// Loop.
    pub looped: bool,
    /// Mode.
    pub mode: PlaybackMode,
    /// Route.
    pub route: Route,
    /// Loudness if already known (skips measuring).
    pub lufs: Option<f32>,
}

/// Events for the UI.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineEvent {
    /// A sound started (first callback rendered it).
    Started {
        /// Sound id.
        sound: u64,
        /// Voice id.
        voice: u64,
        /// Trigger→render latency in µs.
        latency_us: Option<u32>,
    },
    /// A sound finished on all outputs.
    Finished {
        /// Sound id.
        sound: u64,
        /// Voice id.
        voice: u64,
    },
    /// Loudness / duration measured for a file.
    Analyzed {
        /// Sound id.
        sound: u64,
        /// LUFS.
        lufs: Option<f32>,
        /// Duration.
        duration_ms: u32,
    },
    /// Something the user should see.
    Error {
        /// Sound id, if related to one.
        sound: Option<u64>,
        /// Message.
        message: String,
    },
    /// Outputs (re)opened; text describes them.
    DevicesChanged(String),
}

/// Status of the outputs, for the settings page.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceStatus {
    /// Monitor device name and rate, or the error.
    pub monitor: std::result::Result<(String, u32), String>,
    /// Virtual mic name and rate, or the error.
    pub virtual_mic: std::result::Result<(String, u32), String>,
    /// Captured microphone name, or the error.
    pub input: std::result::Result<String, String>,
}

impl Default for DeviceStatus {
    fn default() -> Self {
        DeviceStatus {
            monitor: Err("not started".into()),
            virtual_mic: Err("not started".into()),
            input: Err("not started".into()),
        }
    }
}

/// One playing voice as shown in the UI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NowPlaying {
    /// Voice id.
    pub voice: u64,
    /// Sound id.
    pub sound: u64,
    /// Position (ms).
    pub pos_ms: u32,
    /// Length (ms, 0 = unknown).
    pub len_ms: u32,
}

struct Output {
    handle: MixerHandle,
    rate: u32,
}

struct Inner {
    cfg: EngineConfig,
    monitor: Option<Output>,
    mic: Option<Output>,
    cache: ClipCache,
    durations: HashMap<PathBuf, u32>,
    /// voice id → outputs that still have it
    live: HashMap<u64, (u64, u8)>,
    graveyard: Vec<Arc<Clip>>,
}

/// Shared atomics.
#[derive(Debug, Default)]
pub struct EngineStats {
    /// Last measured trigger→render latency (µs).
    pub last_latency_us: AtomicU32,
    /// Best latency (µs).
    pub best_latency_us: AtomicU32,
    /// Plays.
    pub plays: AtomicU64,
}

/// Handle to the running engine. Cheap to clone; all methods are thread-safe.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Mutex<Inner>>,
    loudness: Arc<Mutex<LoudnessCache>>,
    events_tx: Sender<EngineEvent>,
    events_rx: Receiver<EngineEvent>,
    jobs: Sender<Job>,
    device_tx: Sender<DeviceCmd>,
    status: Arc<Mutex<DeviceStatus>>,
    stats: Arc<EngineStats>,
    next_voice: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
    shutdown: Arc<AtomicBool>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Engine")
    }
}

enum Job {
    Decode {
        req: PlayRequest,
        voice: u64,
        rate: u32,
        targets: u8,
        triggered: Instant,
    },
    Analyze {
        sound: u64,
        path: PathBuf,
    },
}

enum DeviceCmd {
    Reopen,
    Shutdown,
}

/// Target bits.
const MON: u8 = 1;
const MIC: u8 = 2;

/// Factory for the platform's virtual microphone (called on the device thread).
pub type MicFactory = Box<dyn Fn() -> Option<Box<dyn VirtualMic>> + Send + Sync>;

impl Engine {
    /// Start the engine: opens the monitor output, the virtual mic (from `mic_factory`) and
    /// the microphone capture on a dedicated device thread.
    pub fn start(
        cfg: EngineConfig,
        mic_factory: MicFactory,
        loudness_cache: Option<PathBuf>,
    ) -> Engine {
        let (events_tx, events_rx) = unbounded();
        let (jobs_tx, jobs_rx) = unbounded::<Job>();
        let (device_tx, device_rx) = unbounded();
        let loud = loudness_cache
            .as_deref()
            .map(LoudnessCache::load)
            .unwrap_or_default();
        let engine = Engine {
            inner: Arc::new(Mutex::new(Inner {
                cache: ClipCache::new(cfg.cache_bytes),
                cfg,
                monitor: None,
                mic: None,
                durations: HashMap::new(),
                live: HashMap::new(),
                graveyard: Vec::new(),
            })),
            loudness: Arc::new(Mutex::new(loud)),
            events_tx,
            events_rx,
            jobs: jobs_tx,
            device_tx,
            status: Arc::new(Mutex::new(DeviceStatus::default())),
            stats: Arc::new(EngineStats::default()),
            next_voice: Arc::new(AtomicU64::new(1)),
            paused: Arc::new(AtomicBool::new(false)),
            threads: Arc::new(Mutex::new(Vec::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
        };

        // Device thread: owns the cpal streams (not Send on every platform).
        let (ready_tx, ready_rx) = bounded::<()>(1);
        {
            let e = engine.clone();
            let t = std::thread::Builder::new()
                .name("clipdeck-devices".into())
                .spawn(move || device_thread(e, mic_factory, device_rx, ready_tx))
                .ok();
            engine.threads.lock().extend(t);
        }
        let _ = ready_rx.recv_timeout(Duration::from_secs(5));

        // Decode workers.
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 4))
            .unwrap_or(2);
        for i in 0..workers {
            let e = engine.clone();
            let rx = jobs_rx.clone();
            let t = std::thread::Builder::new()
                .name(format!("clipdeck-decode-{i}"))
                .spawn(move || {
                    while let Ok(job) = rx.recv() {
                        if e.shutdown.load(Ordering::Acquire) {
                            break;
                        }
                        e.run_job(job);
                    }
                })
                .ok();
            engine.threads.lock().extend(t);
        }

        // Housekeeping: drain mixer events, drop retired clips, persist loudness.
        {
            let e = engine.clone();
            let t = std::thread::Builder::new()
                .name("clipdeck-housekeeping".into())
                .spawn(move || {
                    let mut buf = Vec::new();
                    let mut last_save = Instant::now();
                    while !e.shutdown.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(15));
                        e.housekeeping(&mut buf);
                        if last_save.elapsed() > Duration::from_secs(30) {
                            last_save = Instant::now();
                            if let Some(p) = &loudness_cache {
                                let _ = e.loudness.lock().save(p);
                            }
                        }
                    }
                    if let Some(p) = &loudness_cache {
                        let _ = e.loudness.lock().save(p);
                    }
                })
                .ok();
            engine.threads.lock().extend(t);
        }
        engine
    }

    /// Event stream for the UI.
    pub fn events(&self) -> Receiver<EngineEvent> {
        self.events_rx.clone()
    }

    /// Latency stats.
    pub fn stats(&self) -> Arc<EngineStats> {
        self.stats.clone()
    }

    /// Current device status.
    pub fn device_status(&self) -> DeviceStatus {
        self.status.lock().clone()
    }

    /// Apply new settings; reopens devices only when a device setting changed.
    pub fn configure(&self, cfg: EngineConfig) {
        let reopen = {
            let mut inner = self.inner.lock();
            let old = std::mem::replace(&mut inner.cfg, cfg);
            let c = &inner.cfg;
            let reopen = old.monitor_device != c.monitor_device
                || old.input_device != c.input_device
                || old.buffer_frames != c.buffer_frames
                || old.mic_passthrough != c.mic_passthrough;
            Self::apply_gains(&inner);
            reopen
        };
        if reopen {
            let _ = self.device_tx.send(DeviceCmd::Reopen);
        }
    }

    /// Reopen all devices (after the user plugged something in).
    pub fn reopen_devices(&self) {
        let _ = self.device_tx.send(DeviceCmd::Reopen);
    }

    fn apply_gains(inner: &Inner) {
        let c = &inner.cfg;
        if let Some(m) = &inner.monitor {
            m.handle.shared.set_master(c.master_volume);
            m.handle.shared.set_output_gain(c.monitor_volume);
            m.handle
                .shared
                .enabled
                .store(c.monitor_enabled, Ordering::Relaxed);
        }
        if let Some(m) = &inner.mic {
            m.handle.shared.set_master(c.master_volume);
            m.handle.shared.set_output_gain(1.0);
            m.handle
                .shared
                .set_mic_gain(if c.mic_passthrough { c.mic_volume } else { 0.0 });
            m.handle
                .shared
                .block_voice
                .store(c.block_voice, Ordering::Relaxed);
        }
    }

    /// Trigger a sound. Returns the voice id.
    pub fn play(&self, req: PlayRequest) -> u64 {
        let triggered = Instant::now();
        let voice = self.next_voice.fetch_add(1, Ordering::Relaxed);
        self.stats.plays.fetch_add(1, Ordering::Relaxed);
        if self.paused.swap(false, Ordering::AcqRel) {
            self.set_paused(false);
        }
        let mut inner = self.inner.lock();
        let mut targets = 0u8;
        if req.route.to_monitor() && inner.monitor.is_some() {
            targets |= MON;
        }
        if req.route.to_mic() && inner.mic.is_some() {
            targets |= MIC;
        }
        if targets == 0 {
            drop(inner);
            let _ = self.events_tx.send(EngineEvent::Error {
                sound: Some(req.sound),
                message: "No audio output is available".into(),
            });
            return voice;
        }

        let duration = match inner.durations.get(&req.path) {
            Some(d) => Some(*d),
            None => {
                let d = decode::probe_duration_ms(&req.path).ok();
                if let Some(d) = d {
                    inner.durations.insert(req.path.clone(), d);
                }
                d
            }
        };
        if !req.path.is_file() {
            drop(inner);
            let _ = self.events_tx.send(EngineEvent::Error {
                sound: Some(req.sound),
                message: format!("File not found: {}", req.path.display()),
            });
            return voice;
        }
        if duration.is_some_and(|d| d > STREAM_THRESHOLD_MS) {
            let lufs = req.lufs.or_else(|| self.loudness.lock().get(&req.path));
            let norm = if inner.cfg.normalize {
                loudness::normalization_gain(lufs, 0.0, inner.cfg.target_lufs)
            } else {
                1.0
            };
            let gain = req.volume * norm;
            let inner = &mut *inner;
            for (bit, out) in [(MON, inner.monitor.as_mut()), (MIC, inner.mic.as_mut())] {
                if targets & bit == 0 {
                    continue;
                }
                let Some(out) = out else { continue };
                let source = spawn_stream(&req.path, out.rate, req.trim, duration.unwrap_or(0));
                let _ = out.handle.send(Command::Play(VoiceSpec {
                    voice,
                    sound: req.sound,
                    source,
                    gain,
                    looped: false,
                    mode: req.mode,
                    triggered: Some(triggered),
                }));
            }
            inner.live.insert(voice, (req.sound, targets));
            return voice;
        }

        // Group targets by rate so each distinct rate is decoded once.
        let mut pending: Vec<(u32, u8)> = Vec::new();
        for (bit, rate) in [
            (MON, inner.monitor.as_ref().map(|o| o.rate)),
            (MIC, inner.mic.as_ref().map(|o| o.rate)),
        ] {
            let Some(rate) = rate else { continue };
            if targets & bit == 0 {
                continue;
            }
            match pending.iter_mut().find(|(r, _)| *r == rate) {
                Some(p) => p.1 |= bit,
                None => pending.push((rate, bit)),
            }
        }
        inner.live.insert(voice, (req.sound, targets));
        for (rate, bits) in pending {
            match inner.cache.get(&req.path, rate) {
                Some(clip) => {
                    let gain = self.gain_for(&inner.cfg, &req, &clip);
                    Self::dispatch(&mut inner, &req, voice, &clip, gain, bits, triggered);
                }
                None => {
                    let _ = self.jobs.send(Job::Decode {
                        req: req.clone(),
                        voice,
                        rate,
                        targets: bits,
                        triggered,
                    });
                }
            }
        }
        voice
    }

    fn gain_for(&self, cfg: &EngineConfig, req: &PlayRequest, clip: &Clip) -> f32 {
        let norm = if cfg.normalize {
            loudness::normalization_gain(req.lufs.or(clip.lufs), clip.peak, cfg.target_lufs)
        } else {
            1.0
        };
        req.volume * norm
    }

    fn dispatch(
        inner: &mut Inner,
        req: &PlayRequest,
        voice: u64,
        clip: &Arc<Clip>,
        gain: f32,
        bits: u8,
        triggered: Instant,
    ) {
        for (bit, out) in [(MON, inner.monitor.as_mut()), (MIC, inner.mic.as_mut())] {
            if bits & bit == 0 {
                continue;
            }
            let Some(out) = out else { continue };
            let spec = VoiceSpec {
                voice,
                sound: req.sound,
                source: memory_source(clip.clone(), req.trim.start_ms, req.trim.end_ms),
                gain,
                looped: req.looped,
                mode: req.mode,
                triggered: Some(triggered),
            };
            if out.handle.send(Command::Play(spec)).is_err() {
                tracing::warn!("mixer command queue full; dropped a play");
            }
        }
    }

    fn run_job(&self, job: Job) {
        match job {
            Job::Decode {
                req,
                voice,
                rate,
                targets,
                triggered,
            } => match Clip::load(&req.path, rate) {
                Ok(clip) => {
                    let clip = Arc::new(clip);
                    if let Some(l) = clip.lufs {
                        self.loudness.lock().put(&req.path, l);
                    }
                    let _ = self.events_tx.send(EngineEvent::Analyzed {
                        sound: req.sound,
                        lufs: clip.lufs,
                        duration_ms: clip.duration_ms(),
                    });
                    let mut inner = self.inner.lock();
                    let evicted = inner.cache.insert(&req.path, rate, clip.clone());
                    inner.graveyard.extend(evicted);
                    // Only start if the voice was not stopped while decoding.
                    if inner.live.contains_key(&voice) {
                        let gain = self.gain_for(&inner.cfg, &req, &clip);
                        Self::dispatch(&mut inner, &req, voice, &clip, gain, targets, triggered);
                    }
                }
                Err(e) => {
                    self.inner.lock().live.remove(&voice);
                    tracing::warn!(%e, "decode failed");
                    let _ = self.events_tx.send(EngineEvent::Error {
                        sound: Some(req.sound),
                        message: e.user_message(),
                    });
                }
            },
            Job::Analyze { sound, path } => {
                if let Some(l) = self.loudness.lock().get(&path) {
                    let d = self.inner.lock().durations.get(&path).copied();
                    let duration_ms = d
                        .or_else(|| decode::probe_duration_ms(&path).ok())
                        .unwrap_or(0);
                    let _ = self.events_tx.send(EngineEvent::Analyzed {
                        sound,
                        lufs: Some(l),
                        duration_ms,
                    });
                    return;
                }
                // Measure at most 10 minutes; long files are representative well before that.
                match decode::decode_native_limited(&path, Some(48_000 * 600)) {
                    Ok(d) => {
                        let lufs = loudness::integrated_lufs(&d.samples, d.rate);
                        if let Some(l) = lufs {
                            self.loudness.lock().put(&path, l);
                        }
                        let duration_ms =
                            decode::probe_duration_ms(&path).unwrap_or(d.duration_ms());
                        self.inner.lock().durations.insert(path, duration_ms);
                        let _ = self.events_tx.send(EngineEvent::Analyzed {
                            sound,
                            lufs,
                            duration_ms,
                        });
                    }
                    Err(e) => tracing::debug!(%e, "analysis failed"),
                }
            }
        }
    }

    /// Queue background loudness/duration analysis.
    pub fn analyze(&self, sound: u64, path: PathBuf) {
        let _ = self.jobs.send(Job::Analyze { sound, path });
    }

    /// Decode into the cache ahead of time so the first trigger is instant.
    pub fn preload(&self, path: &Path) {
        let rates: Vec<u32> = {
            let inner = self.inner.lock();
            inner
                .monitor
                .iter()
                .chain(inner.mic.iter())
                .map(|o| o.rate)
                .collect()
        };
        for rate in rates {
            if self.inner.lock().cache.get(path, rate).is_some() {
                continue;
            }
            if decode::probe_duration_ms(path).is_ok_and(|d| d > STREAM_THRESHOLD_MS) {
                return;
            }
            if let Ok(clip) = Clip::load(path, rate) {
                if let Some(l) = clip.lufs {
                    self.loudness.lock().put(path, l);
                }
                let mut inner = self.inner.lock();
                let ev = inner.cache.insert(path, rate, Arc::new(clip));
                inner.graveyard.extend(ev);
            }
        }
    }

    /// Stop every voice of a sound.
    pub fn stop_sound(&self, sound: u64) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.live.retain(|_, (s, _)| *s != sound);
        for out in [inner.monitor.as_mut(), inner.mic.as_mut()]
            .into_iter()
            .flatten()
        {
            let _ = out.handle.send(Command::StopSound(sound));
        }
    }

    /// Stop everything.
    pub fn stop_all(&self) {
        {
            let mut guard = self.inner.lock();
            let inner = &mut *guard;
            inner.live.clear();
            for out in [inner.monitor.as_mut(), inner.mic.as_mut()]
                .into_iter()
                .flatten()
            {
                let _ = out.handle.send(Command::StopAll);
            }
        }
        self.set_paused(false);
    }

    /// Change the gain of a playing sound.
    pub fn set_sound_volume(&self, sound: u64, volume: f32) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        for out in [inner.monitor.as_mut(), inner.mic.as_mut()]
            .into_iter()
            .flatten()
        {
            let _ = out.handle.send(Command::SetSoundGain(sound, volume));
        }
    }

    /// Pause or resume everything.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Release);
        let inner = self.inner.lock();
        for out in [inner.monitor.as_ref(), inner.mic.as_ref()]
            .into_iter()
            .flatten()
        {
            out.handle.shared.paused.store(paused, Ordering::Relaxed);
        }
    }

    /// Toggle pause; returns the new state.
    pub fn toggle_pause(&self) -> bool {
        let p = !self.paused.load(Ordering::Acquire);
        self.set_paused(p);
        p
    }

    /// Whether paused.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Master volume.
    pub fn set_master_volume(&self, v: f32) {
        let mut inner = self.inner.lock();
        inner.cfg.master_volume = v.clamp(0.0, 1.0);
        Self::apply_gains(&inner);
    }

    /// Monitor on/off.
    pub fn set_monitor_enabled(&self, on: bool) {
        let mut inner = self.inner.lock();
        inner.cfg.monitor_enabled = on;
        Self::apply_gains(&inner);
    }

    /// Mic passthrough on/off.
    pub fn set_mic_passthrough(&self, on: bool) {
        let changed = {
            let mut inner = self.inner.lock();
            let changed = inner.cfg.mic_passthrough != on;
            inner.cfg.mic_passthrough = on;
            Self::apply_gains(&inner);
            changed
        };
        if changed {
            let _ = self.device_tx.send(DeviceCmd::Reopen);
        }
    }

    /// What is playing (from whichever output is running; monitor preferred).
    pub fn now_playing(&self) -> Vec<NowPlaying> {
        let inner = self.inner.lock();
        let mut seen: HashMap<u64, NowPlaying> = HashMap::new();
        for out in [inner.monitor.as_ref(), inner.mic.as_ref()]
            .into_iter()
            .flatten()
        {
            let rate = u64::from(out.rate.max(1));
            for (voice, sound, pos, len) in out.handle.shared.active() {
                seen.entry(voice).or_insert(NowPlaying {
                    voice,
                    sound,
                    pos_ms: (pos * 1000 / rate) as u32,
                    len_ms: (len * 1000 / rate) as u32,
                });
            }
        }
        let mut v: Vec<NowPlaying> = seen.into_values().collect();
        v.sort_by_key(|n| n.voice);
        v
    }

    /// Output peak levels `(monitor, mic)`.
    pub fn peaks(&self) -> (f32, f32) {
        let inner = self.inner.lock();
        (
            inner
                .monitor
                .as_ref()
                .map(|o| o.handle.shared.peak())
                .unwrap_or(0.0),
            inner
                .mic
                .as_ref()
                .map(|o| o.handle.shared.peak())
                .unwrap_or(0.0),
        )
    }

    fn housekeeping(&self, buf: &mut Vec<MixerEvent>) {
        let mut guard = self.inner.lock();
        let inner = &mut *guard;
        inner.graveyard.clear();
        let mut events: Vec<(u8, MixerEvent)> = Vec::new();
        for (bit, out) in [(MON, inner.monitor.as_mut()), (MIC, inner.mic.as_mut())] {
            if let Some(o) = out {
                buf.clear();
                o.handle.housekeeping(buf);
                events.extend(buf.iter().map(|e| (bit, *e)));
            }
        }
        for (bit, ev) in events {
            match ev {
                MixerEvent::Started {
                    voice,
                    sound,
                    latency_us,
                } => {
                    // Report once per voice (first output to start it).
                    if let Some((_, t)) = inner.live.get_mut(&voice)
                        && *t & 0x80 == 0
                    {
                        *t |= 0x80;
                        if let Some(l) = latency_us {
                            self.stats.last_latency_us.store(l, Ordering::Relaxed);
                            let best = self.stats.best_latency_us.load(Ordering::Relaxed);
                            if best == 0 || l < best {
                                self.stats.best_latency_us.store(l, Ordering::Relaxed);
                            }
                        }
                        let _ = self.events_tx.send(EngineEvent::Started {
                            sound,
                            voice,
                            latency_us,
                        });
                    }
                }
                MixerEvent::Finished { voice, .. } | MixerEvent::Dropped { voice } => {
                    let sound = sound_of(&ev).unwrap_or(0);
                    let done = match inner.live.get_mut(&voice) {
                        Some((_, t)) => {
                            *t &= !bit;
                            *t & 0x7F == 0
                        }
                        None => false,
                    };
                    if done {
                        let s = inner.live.remove(&voice).map(|(s, _)| s).unwrap_or(sound);
                        let _ = self
                            .events_tx
                            .send(EngineEvent::Finished { sound: s, voice });
                    }
                }
            }
        }
    }

    /// Stop threads and close devices.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Release);
        let _ = self.device_tx.send(DeviceCmd::Shutdown);
        let threads: Vec<JoinHandle<()>> = std::mem::take(&mut *self.threads.lock());
        drop(self.jobs.clone());
        for t in threads {
            if t.thread().name() == Some("clipdeck-devices")
                || t.thread().name() == Some("clipdeck-housekeeping")
            {
                let _ = t.join();
            }
        }
    }
}

fn sound_of(ev: &MixerEvent) -> Option<u64> {
    match ev {
        MixerEvent::Started { sound, .. } | MixerEvent::Finished { sound, .. } => Some(*sound),
        MixerEvent::Dropped { .. } => None,
    }
}

/// Spawn a decoder thread streaming `path` at `rate` into a ring; returns the mixer source.
fn spawn_stream(path: &Path, rate: u32, trim: Trim, duration_ms: u32) -> Source {
    let (mut tx, rx) = RingBuffer::<f32>::new(rate as usize * 2); // ~1 s
    let finished = Arc::new(AtomicBool::new(false));
    let f2 = finished.clone();
    let path = path.to_path_buf();
    let total = (u64::from(
        trim.end_ms
            .unwrap_or(duration_ms)
            .saturating_sub(trim.start_ms),
    ) * u64::from(rate)
        / 1000) as usize;
    let _ = std::thread::Builder::new()
        .name("clipdeck-stream".into())
        .spawn(move || {
            let result = stream_file(&path, rate, trim, &mut tx);
            if let Err(e) = result {
                tracing::warn!(%e, "streaming failed");
            }
            f2.store(true, Ordering::Release);
        });
    Source::Stream {
        rx,
        finished,
        played: 0,
        total,
    }
}

fn stream_file(path: &Path, rate: u32, trim: Trim, tx: &mut rtrb::Producer<f32>) -> Result<()> {
    // Decode progressively in ~2 s chunks to keep memory flat.
    let d = decode::decode_native(path)?;
    let mut rs = LinearResampler::new(d.rate, rate);
    let start = (u64::from(trim.start_ms) * u64::from(d.rate) / 1000) as usize * 2;
    let end = trim
        .end_ms
        .map(|e| (u64::from(e) * u64::from(d.rate) / 1000) as usize * 2)
        .unwrap_or(d.samples.len())
        .min(d.samples.len());
    let mut out: Vec<f32> = Vec::with_capacity(8192);
    for chunk in d.samples[start.min(end)..end].chunks(4096) {
        out.clear();
        rs.process(chunk, |l, r| {
            out.push(l);
            out.push(r);
        });
        let mut i = 0;
        while i < out.len() {
            if tx.is_abandoned() {
                return Ok(());
            }
            let n = tx.slots().min(out.len() - i);
            if n == 0 {
                std::thread::sleep(Duration::from_millis(5));
                continue;
            }
            for &s in &out[i..i + n] {
                let _ = tx.push(s);
            }
            i += n;
        }
    }
    Ok(())
}

fn device_thread(
    engine: Engine,
    mic_factory: MicFactory,
    rx: Receiver<DeviceCmd>,
    ready: Sender<()>,
) {
    let mut monitor_stream: Option<OutputStream> = None;
    let mut mic_sink: Option<Box<dyn VirtualMic>> = None;
    let mut input_stream: Option<InputStream> = None;
    let mut first = true;

    let open_all = |monitor_stream: &mut Option<OutputStream>,
                    mic_sink: &mut Option<Box<dyn VirtualMic>>,
                    input_stream: &mut Option<InputStream>| {
        *monitor_stream = None;
        if let Some(mut m) = mic_sink.take() {
            m.stop();
        }
        *input_stream = None;
        let cfg = engine.inner.lock().cfg.clone();
        let mut status = DeviceStatus::default();

        // Monitor.
        let monitor =
            match output::open_mixer_output(cfg.monitor_device.as_deref(), cfg.buffer_frames) {
                Ok((stream, handle)) => {
                    status.monitor = Ok((stream.device.clone(), stream.rate));
                    let rate = stream.rate;
                    *monitor_stream = Some(stream);
                    Some(Output { handle, rate })
                }
                Err(e) => {
                    status.monitor = Err(e.user_message());
                    None
                }
            };

        // Virtual mic.
        let mut mic_out = None;
        match mic_factory() {
            Some(mut sink) => match sink.prepare() {
                Ok((rate, _ch)) => {
                    let (mut m, handle) = mixer(rate);
                    let render: RenderFn = Box::new(move |out, ch| m.render(out, ch));
                    match sink.start(render) {
                        Ok(()) => {
                            status.virtual_mic = Ok((sink.display_name(), rate));
                            mic_out = Some(Output { handle, rate });
                            *mic_sink = Some(sink);
                        }
                        Err(e) => status.virtual_mic = Err(e),
                    }
                }
                Err(e) => status.virtual_mic = Err(e),
            },
            None => status.virtual_mic = Err("No virtual microphone backend".into()),
        }

        // Real microphone passthrough into the virtual mic.
        if let Some(mic) = mic_out.as_mut() {
            if cfg.mic_passthrough {
                match output::open_input(cfg.input_device.as_deref(), mic.rate) {
                    Ok((stream, ring)) => {
                        status.input = Ok(stream.device.clone());
                        let _ = mic.handle.send(Command::SetMicInput(Some(ring)));
                        *input_stream = Some(stream);
                    }
                    Err(e) => status.input = Err(e.user_message()),
                }
            } else {
                status.input = Err("Passthrough is off".into());
            }
        } else {
            status.input = Err("No virtual microphone".into());
        }

        let summary = format!(
            "monitor: {} · mic: {}",
            status
                .monitor
                .as_ref()
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|e| e.clone()),
            status
                .virtual_mic
                .as_ref()
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|e| e.clone())
        );
        {
            let mut inner = engine.inner.lock();
            inner.monitor = monitor;
            inner.mic = mic_out;
            inner.live.clear();
            Engine::apply_gains(&inner);
        }
        *engine.status.lock() = status;
        let _ = engine.events_tx.send(EngineEvent::DevicesChanged(summary));
    };

    loop {
        if first {
            open_all(&mut monitor_stream, &mut mic_sink, &mut input_stream);
            let _ = ready.send(());
            first = false;
        }
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(DeviceCmd::Reopen) => {
                open_all(&mut monitor_stream, &mut mic_sink, &mut input_stream)
            }
            Ok(DeviceCmd::Shutdown) | Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                break;
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                let failed = monitor_stream
                    .as_ref()
                    .is_some_and(|s| s.failed.load(Ordering::Acquire))
                    || mic_sink.as_ref().is_some_and(|m| m.failed())
                    || input_stream
                        .as_ref()
                        .is_some_and(|s| s.failed.load(Ordering::Acquire));
                if failed {
                    tracing::info!("audio device failed; reopening");
                    std::thread::sleep(Duration::from_millis(500));
                    open_all(&mut monitor_stream, &mut mic_sink, &mut input_stream);
                }
            }
        }
    }
    if let Some(mut m) = mic_sink.take() {
        m.stop();
    }
    drop(monitor_stream);
    drop(input_stream);
}

/// Run a mixer offline (no device) — used by tests and the benchmark.
pub fn offline_render(clip: Arc<Clip>, frames: usize) -> Vec<f32> {
    let (mut m, mut h) = mixer(clip.rate);
    let _ = h.send(Command::Play(VoiceSpec {
        voice: 1,
        sound: 1,
        source: memory_source(clip, 0, None),
        gain: 1.0,
        looped: false,
        mode: PlaybackMode::Overlap,
        triggered: None,
    }));
    let mut out = vec![0.0; frames * 2];
    m.render(&mut out, 2);
    out
}
