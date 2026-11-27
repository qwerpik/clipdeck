//! `AppCore`: library + config + engine + request handling, shared by the GUI, the headless
//! daemon (`clipdeck serve`) and the IPC server. No Slint types here.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clipdeck_audio::engine::{Engine, EngineConfig, EngineEvent, PlayRequest};
use clipdeck_audio::record::{RecordOptions, Recorder};
use clipdeck_core::config::{Config, RecordSource};
use clipdeck_core::hotkey::Hotkey;
use clipdeck_core::library::{AddReport, Library};
use clipdeck_core::model::{Route, Sound, SoundId};
use clipdeck_core::paths::Paths;
use clipdeck_core::protocol::{self, Dialect, Request};
use clipdeck_core::spl::{self, SplDocument};
use clipdeck_platform::hotkeys::Binding;
use crossbeam_channel::{Receiver, Sender, unbounded};
use parking_lot::Mutex;

/// Things the window should react to.
#[derive(Debug, Clone, PartialEq)]
pub enum UiSignal {
    /// Library changed (reload the model).
    LibraryChanged,
    /// Config changed (re-apply settings).
    ConfigChanged,
    /// Show the window.
    Show,
    /// Show the window on a page (0 library … 4 settings).
    ShowPage(i32),
    /// Hide the window.
    Hide,
    /// Toggle the window.
    Toggle,
    /// Quit.
    Quit,
    /// Message for the snackbar.
    Notice(String),
    /// Playback started/finished; refresh the now-playing bar.
    Playback,
    /// Device status changed.
    Devices(String),
    /// Hotkeys need re-registering.
    HotkeysChanged,
    /// Recording started or stopped.
    Recording,
}

/// The application core.
pub struct AppCore {
    /// Paths.
    pub paths: Paths,
    /// Settings.
    pub config: Mutex<Config>,
    /// Library.
    pub library: Mutex<Library>,
    /// Audio engine.
    pub engine: Engine,
    signals_tx: Sender<UiSignal>,
    signals_rx: Receiver<UiSignal>,
    library_dirty: AtomicBool,
    config_dirty: AtomicBool,
    /// Set when shutting down.
    pub quitting: AtomicBool,
    /// Last sound played from the library and the engine's stop epoch at that moment.
    last: Mutex<Option<(SoundId, u64)>>,
    /// Voice-activation sound and its length, cached.
    activation: Mutex<Option<(PathBuf, u32)>>,
    /// The recording in progress.
    recorder: Mutex<Option<(Recorder, RecordSource)>>,
}

/// Sound id used for the voice-activation sound (never a library id).
const ACTIVATION_SOUND_ID: u64 = u64::MAX - 1;

impl std::fmt::Debug for AppCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AppCore")
    }
}

impl AppCore {
    /// Load config and library, start the engine and the background saver.
    pub fn start(paths: Paths) -> Arc<AppCore> {
        let _ = paths.ensure();
        let mut notices = Vec::new();
        let config = Config::load(&paths.config_file()).unwrap_or_else(|e| {
            notices.push(e.user_message());
            backup_broken(&paths.config_file());
            Config::default()
        });
        let library = Library::load(&paths.library_file()).unwrap_or_else(|e| {
            notices.push(e.user_message());
            backup_broken(&paths.library_file());
            Library::new()
        });
        let ecfg = EngineConfig::from_config(&config.audio);
        let cable = config.audio.cable_device.clone();
        let buf = config.audio.buffer_frames;
        let engine = Engine::start(
            ecfg,
            Box::new(move || clipdeck_platform::virtual_mic(cable.as_deref(), buf)),
            Some(paths.data_dir.join("loudness.tsv")),
        );
        let (tx, rx) = unbounded();
        let core = Arc::new(AppCore {
            paths,
            config: Mutex::new(config),
            library: Mutex::new(library),
            engine,
            signals_tx: tx,
            signals_rx: rx,
            library_dirty: AtomicBool::new(false),
            config_dirty: AtomicBool::new(false),
            quitting: AtomicBool::new(false),
            last: Mutex::new(None),
            activation: Mutex::new(None),
            recorder: Mutex::new(None),
        });
        for n in notices {
            core.notify(n);
        }
        core.spawn_background();
        core.analyze_missing();
        core
    }

    /// UI signal stream.
    pub fn signals(&self) -> Receiver<UiSignal> {
        self.signals_rx.clone()
    }

    /// Send a UI signal.
    pub fn signal(&self, s: UiSignal) {
        let _ = self.signals_tx.send(s);
    }

    /// Show a snackbar message.
    pub fn notify(&self, msg: impl Into<String>) {
        self.signal(UiSignal::Notice(msg.into()));
    }

    fn spawn_background(self: &Arc<Self>) {
        // Engine events → library updates and UI signals.
        let me = self.clone();
        let events = self.engine.events();
        let _ = std::thread::Builder::new()
            .name("clipdeck-events".into())
            .spawn(move || {
                while let Ok(ev) = events.recv() {
                    me.on_engine_event(ev);
                }
            });
        // Debounced saver.
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("clipdeck-saver".into())
            .spawn(move || {
                while !me.quitting.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(500));
                    let limit_hit = me.recorder.lock().as_ref().is_some_and(|r| r.0.is_done());
                    if limit_hit {
                        me.finish_recording();
                    }
                    me.flush();
                }
            });
    }

    fn on_engine_event(&self, ev: EngineEvent) {
        match ev {
            EngineEvent::Started { sound, .. } => {
                self.library.lock().record_play(SoundId(sound));
                self.mark_library_dirty();
                self.signal(UiSignal::Playback);
            }
            EngineEvent::Finished { sound, .. } => {
                self.signal(UiSignal::Playback);
                self.after_end(SoundId(sound));
            }
            EngineEvent::Analyzed {
                sound,
                lufs,
                duration_ms,
            } => {
                let mut lib = self.library.lock();
                if let Some(s) = lib.sound_mut(SoundId(sound)) {
                    let changed = s.loudness_lufs != lufs || s.duration_ms != Some(duration_ms);
                    if lufs.is_some() {
                        s.loudness_lufs = lufs;
                    }
                    if duration_ms > 0 {
                        s.duration_ms = Some(duration_ms);
                    }
                    if changed {
                        drop(lib);
                        self.mark_library_dirty();
                        self.signal(UiSignal::LibraryChanged);
                    }
                }
            }
            EngineEvent::Error { message, .. } => self.notify(message),
            EngineEvent::DevicesChanged(s) => self.signal(UiSignal::Devices(s)),
        }
    }

    /// Mark the library for saving.
    pub fn mark_library_dirty(&self) {
        self.library_dirty.store(true, Ordering::Release);
    }

    /// Mark the config for saving.
    pub fn mark_config_dirty(&self) {
        self.config_dirty.store(true, Ordering::Release);
    }

    /// Write pending changes now.
    pub fn flush(&self) {
        if self.library_dirty.swap(false, Ordering::AcqRel) {
            let lib = self.library.lock().clone();
            if let Err(e) = lib.save(&self.paths.library_file(), Some(&self.paths.backup_dir())) {
                tracing::error!(%e, "saving library failed");
                self.notify(e.user_message());
            }
        }
        if self.config_dirty.swap(false, Ordering::AcqRel) {
            let cfg = self.config.lock().clone();
            if let Err(e) = cfg.save(&self.paths.config_file()) {
                tracing::error!(%e, "saving config failed");
                self.notify(e.user_message());
            }
        }
    }

    /// Queue loudness/duration analysis for sounds that lack it.
    pub fn analyze_missing(&self) {
        let todo: Vec<(SoundId, PathBuf)> = self
            .library
            .lock()
            .sounds
            .iter()
            .filter(|s| (s.loudness_lufs.is_none() || s.duration_ms.is_none()) && s.file_exists())
            .map(|s| (s.id, s.path.clone()))
            .collect();
        for (id, p) in todo {
            self.engine.analyze(id.0, p);
        }
    }

    /// Preload hotkeyed and favorite sounds so their first trigger is instant.
    pub fn warm_cache(self: &Arc<Self>) {
        let me = self.clone();
        let _ = std::thread::Builder::new()
            .name("clipdeck-warm".into())
            .spawn(move || {
                let paths: Vec<PathBuf> = me
                    .library
                    .lock()
                    .sounds
                    .iter()
                    .filter(|s| s.hotkey.is_some() || s.favorite)
                    .take(200)
                    .map(|s| s.path.clone())
                    .collect();
                for p in paths {
                    me.engine.preload(&p);
                }
            });
    }

    /// Apply the current config to the engine.
    pub fn apply_audio_config(&self) {
        let cfg = EngineConfig::from_config(&self.config.lock().audio);
        self.engine.configure(cfg);
    }

    /// Update config through a closure; saves and re-applies.
    pub fn update_config(&self, f: impl FnOnce(&mut Config)) {
        {
            let mut c = self.config.lock();
            f(&mut c);
            c.sanitize();
        }
        self.mark_config_dirty();
        self.apply_audio_config();
        self.signal(UiSignal::ConfigChanged);
    }

    /// Continue after `sound` ended by itself, per the "after a sound ends" setting.
    fn after_end(&self, sound: SoundId) {
        let mode = self.config.lock().audio.after_end;
        if mode == clipdeck_core::config::AfterEnd::Stop {
            return;
        }
        let Some((last, epoch)) = *self.last.lock() else {
            return;
        };
        // Only when the last sound ended on its own and nothing else is playing.
        if last != sound
            || epoch != self.engine.stop_epoch()
            || !self.engine.now_playing().is_empty()
        {
            return;
        }
        let _ = match mode {
            clipdeck_core::config::AfterEnd::Next => self.play_step(1),
            _ => self.play(last, None),
        };
    }

    /// Play the sound `dir` places after the last one (wrapping), skipping missing files.
    pub fn play_step(&self, dir: i32) -> Result<(), String> {
        let ids: Vec<SoundId> = {
            let lib = self.library.lock();
            lib.sounds
                .iter()
                .filter(|s| s.file_exists())
                .map(|s| s.id)
                .collect()
        };
        if ids.is_empty() {
            return Err("The library is empty".into());
        }
        let last = self.last.lock().map(|(id, _)| id);
        let n = ids.len() as i64;
        let next = match last.and_then(|l| ids.iter().position(|i| *i == l)) {
            Some(i) => (i as i64 + i64::from(dir)).rem_euclid(n) as usize,
            None if dir < 0 => ids.len() - 1,
            None => 0,
        };
        self.play(ids[next], None)
    }

    /// Play a library sound.
    pub fn play(&self, id: SoundId, route: Option<Route>) -> Result<(), String> {
        let (req, missing) = {
            let lib = self.library.lock();
            let s = lib.sound(id).ok_or("No such sound")?;
            let cfg = self.config.lock();
            (
                PlayRequest {
                    sound: s.id.0,
                    path: s.path.clone(),
                    volume: s.volume,
                    trim: s.trim,
                    looped: s.looped,
                    mode: s.mode.unwrap_or(cfg.audio.playback_mode),
                    route: route.unwrap_or(cfg.audio.route),
                    lufs: s.loudness_lufs,
                    mic_delay_ms: 0,
                },
                !s.file_exists(),
            )
        };
        if missing {
            return Err(format!("File not found: {}", req.path.display()));
        }
        *self.last.lock() = Some((id, self.engine.stop_epoch()));
        let mut req = req;
        req.mic_delay_ms = self.play_activation(req.route);
        self.engine.play(req);
        Ok(())
    }

    /// Start the voice-activation sound on the microphone when enabled and the sound goes
    /// there. Returns how long the real sound must wait on the microphone, ms.
    fn play_activation(&self, route: Route) -> u32 {
        if route == Route::MonitorOnly {
            return 0;
        }
        let path = {
            let cfg = self.config.lock();
            match (&cfg.audio.activation_sound, cfg.audio.activation_enabled) {
                (Some(p), true) => PathBuf::from(p),
                _ => return 0,
            }
        };
        if !path.exists() {
            return 0;
        }
        let duration = {
            let mut cache = self.activation.lock();
            match cache.as_ref() {
                Some((p, d)) if *p == path => *d,
                _ => {
                    let d = clipdeck_audio::decode::probe_duration_ms(&path).unwrap_or(0);
                    *cache = Some((path.clone(), d));
                    d
                }
            }
        };
        if duration == 0 {
            return 0;
        }
        self.engine.play(PlayRequest {
            sound: ACTIVATION_SOUND_ID,
            path,
            volume: 1.0,
            trim: Default::default(),
            looped: false,
            mode: clipdeck_core::model::PlaybackMode::Overlap,
            route: Route::MicOnly,
            lufs: None,
            mic_delay_ms: 0,
        });
        duration.min(5_000)
    }

    /// Play a file that is not in the library.
    pub fn play_file(&self, path: &Path, route: Option<Route>) {
        let cfg = self.config.lock().audio.clone();
        self.engine.play(PlayRequest {
            sound: u64::MAX - (path.to_string_lossy().len() as u64),
            path: path.to_path_buf(),
            volume: 1.0,
            trim: Default::default(),
            looped: false,
            mode: cfg.playback_mode,
            route: route.unwrap_or(cfg.route),
            lufs: None,
            mic_delay_ms: 0,
        });
    }

    /// Play a random sound (optionally among `ids`).
    pub fn play_random(&self, ids: Option<&[SoundId]>) -> Result<(), String> {
        let pick = {
            let lib = self.library.lock();
            let pool: Vec<SoundId> = match ids {
                Some(v) => v.to_vec(),
                None => lib
                    .sounds
                    .iter()
                    .filter(|s| s.file_exists())
                    .map(|s| s.id)
                    .collect(),
            };
            if pool.is_empty() {
                return Err("No sounds to pick from".into());
            }
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as usize)
                .unwrap_or(0);
            pool[nanos % pool.len()]
        };
        self.play(pick, None)
    }

    /// Start recording from `source` (`None` = the configured one).
    pub fn start_recording(&self, source: Option<RecordSource>) -> Result<RecordSource, String> {
        let mut slot = self.recorder.lock();
        if let Some((_, s)) = slot.as_ref() {
            return Err(format!("Already recording {}", source_name(*s)));
        }
        let (cfg, mic) = {
            let c = self.config.lock();
            (c.recorder.clone(), c.audio.input_device.clone())
        };
        let source = source.unwrap_or(cfg.source);
        let dir = cfg
            .folder
            .as_deref()
            .filter(|f| !f.trim().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| self.paths.recordings_dir());
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        let opts = RecordOptions {
            path: clipdeck_audio::record::unique_path(&dir, &clipdeck_core::date::now_stamp()),
            trim_silence: cfg.trim_silence,
            normalize: cfg.normalize,
            max_seconds: cfg.max_seconds,
        };
        let rec = Recorder::start(clipdeck_platform::record_capture(source, mic), opts)?;
        *slot = Some((rec, source));
        drop(slot);
        self.signal(UiSignal::Recording);
        Ok(source)
    }

    /// Stop recording, save and add the file to the library. Returns the sound's name.
    pub fn stop_recording(&self) -> Result<String, String> {
        let Some((rec, _)) = self.recorder.lock().take() else {
            return Err("Not recording".into());
        };
        self.signal(UiSignal::Recording);
        let done = rec.stop()?;
        let cfg = self.config.lock().recorder.clone();
        let name = done
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if cfg.add_to_library {
            let cat = {
                let wanted = cfg.category.trim();
                if wanted.is_empty() {
                    None
                } else {
                    let mut lib = self.library.lock();
                    let found = lib
                        .categories
                        .iter()
                        .find(|c| c.name.eq_ignore_ascii_case(wanted))
                        .map(|c| c.id);
                    Some(found.unwrap_or_else(|| lib.add_category(wanted, None)))
                }
            };
            self.add_paths(std::slice::from_ref(&done.path), cat);
        }
        Ok(name)
    }

    /// Stop and report the result in the snackbar.
    pub fn finish_recording(&self) {
        match self.stop_recording() {
            Ok(name) => self.notify(format!("Saved {name}")),
            Err(e) => self.notify(e),
        }
    }

    /// Start or stop recording.
    pub fn toggle_recording(&self, source: Option<RecordSource>) {
        if self.recorder.lock().is_some() {
            self.finish_recording();
        } else if let Err(e) = self.start_recording(source) {
            self.notify(e);
        }
    }

    /// The recording in progress: source, elapsed ms and peak since the last call.
    pub fn recording(&self) -> Option<(RecordSource, u64, f32)> {
        self.recorder
            .lock()
            .as_ref()
            .map(|(r, s)| (*s, r.elapsed_ms(), r.take_peak()))
    }

    /// Add files or folders.
    pub fn add_paths(
        &self,
        paths: &[PathBuf],
        category: Option<clipdeck_core::CategoryId>,
    ) -> AddReport {
        let report = self.library.lock().add_paths(paths, category);
        for id in &report.added {
            if let Some(p) = self.library.lock().sound(*id).map(|s| s.path.clone()) {
                self.engine.analyze(id.0, p);
            }
        }
        if !report.added.is_empty() {
            self.mark_library_dirty();
            self.signal(UiSignal::LibraryChanged);
        }
        report
    }

    /// Import a Soundpad `.spl` (and its `stats.spl` if one sits next to it).
    pub fn import_spl(&self, path: &Path) -> Result<spl::ImportReport, String> {
        let doc = SplDocument::load(path).map_err(|e| e.user_message())?;
        let stats_path = path.with_file_name("stats.spl");
        let stats = std::fs::read_to_string(&stats_path)
            .ok()
            .and_then(|t| spl::parse_stats(&t).ok())
            .unwrap_or_default();
        let report = doc.import_into(&mut self.library.lock(), Some(path), &stats);
        self.mark_library_dirty();
        self.signal(UiSignal::LibraryChanged);
        self.signal(UiSignal::HotkeysChanged);
        self.analyze_missing();
        Ok(report)
    }

    /// Export to a Soundpad `.spl`.
    pub fn export_spl(&self, path: &Path) -> Result<(), String> {
        let doc = SplDocument::from_library(&self.library.lock(), Some(path));
        doc.save(path).map_err(|e| e.user_message())
    }

    /// Hotkey registrations for the current library and config.
    pub fn bindings(&self) -> Vec<Binding> {
        let cfg = self.config.lock();
        if !cfg.hotkeys.enabled {
            return Vec::new();
        }
        let mut v = Vec::new();
        let mut push = |id: String, h: &Option<String>, desc: String| {
            if let Some(h) = h
                && let Ok(hk) = h.parse::<Hotkey>()
            {
                v.push(Binding {
                    id,
                    hotkey: hk,
                    description: desc,
                });
            }
        };
        push(
            "stop-all".into(),
            &cfg.hotkeys.stop_all,
            "Stop all sounds".into(),
        );
        push("pause".into(), &cfg.hotkeys.pause, "Pause / resume".into());
        push(
            "record".into(),
            &cfg.hotkeys.record,
            "Start / stop recording".into(),
        );
        let lib = self.library.lock();
        for s in &lib.sounds {
            push(
                format!("sound-{}", s.id.0),
                &s.hotkey,
                format!("Play {}", s.name),
            );
        }
        for c in &lib.categories {
            push(
                format!("category-{}", c.id.0),
                &c.hotkey,
                format!("Random from {}", c.name),
            );
        }
        v
    }

    /// A hotkey fired.
    pub fn on_hotkey(&self, id: &str) {
        if id == "stop-all" {
            self.engine.stop_all();
            return;
        }
        if id == "pause" {
            self.engine.toggle_pause();
            return;
        }
        if id == "record" {
            self.toggle_recording(None);
            return;
        }
        if let Some(n) = id
            .strip_prefix("sound-")
            .and_then(|n| n.parse::<u64>().ok())
        {
            if let Err(e) = self.play(SoundId(n), None) {
                self.notify(e);
            }
            return;
        }
        if let Some(n) = id
            .strip_prefix("category-")
            .and_then(|n| n.parse::<u64>().ok())
        {
            let ids = self
                .library
                .lock()
                .sounds_in_category(clipdeck_core::CategoryId(n));
            if let Err(e) = self.play_random(Some(&ids)) {
                self.notify(e);
            }
        }
    }

    /// Handle one IPC / CLI request line.
    pub fn handle_line(&self, line: &str) -> String {
        match protocol::parse(line) {
            Ok((req, dialect)) => self.handle(req, dialect),
            Err(e) => protocol::error_response(
                if line.contains('(') {
                    Dialect::Soundpad
                } else {
                    Dialect::Clipdeck
                },
                &e.to_string(),
            ),
        }
    }

    fn ok(d: Dialect, msg: &str) -> String {
        match d {
            Dialect::Clipdeck => {
                if msg.is_empty() {
                    "OK".into()
                } else {
                    format!("OK {msg}")
                }
            }
            Dialect::Soundpad => "R-200".into(),
        }
    }

    /// Execute a parsed request.
    pub fn handle(&self, req: Request, d: Dialect) -> String {
        let err = |m: String| protocol::error_response(d, &m);
        match req {
            Request::Play { target, route } => {
                let id = self.library.lock().resolve(&target);
                match id {
                    Some(id) => match self.play(id, route) {
                        Ok(()) => {
                            let name = self
                                .library
                                .lock()
                                .sound(id)
                                .map(|s| s.name.clone())
                                .unwrap_or_default();
                            Self::ok(d, &format!("playing {name}"))
                        }
                        Err(e) => err(e),
                    },
                    None => {
                        let p = PathBuf::from(&target);
                        if p.is_file() {
                            self.play_file(&p, route);
                            Self::ok(d, &format!("playing {}", p.display()))
                        } else {
                            err(format!("no sound matches `{target}`"))
                        }
                    }
                }
            }
            Request::PlayIndex { index, route } => {
                let id = self.library.lock().sounds.get(index - 1).map(|s| s.id);
                match id {
                    Some(id) => match self.play(id, route) {
                        Ok(()) => Self::ok(d, ""),
                        Err(e) => err(e),
                    },
                    None => match d {
                        Dialect::Soundpad => "R-404: Sound not found.".into(),
                        Dialect::Clipdeck => err(format!("no sound at position {index}")),
                    },
                }
            }
            Request::Step(dir) => match self.play_step(dir) {
                Ok(()) => Self::ok(d, ""),
                Err(e) => err(e),
            },
            Request::PlayRandom => match self.play_random(None) {
                Ok(()) => Self::ok(d, ""),
                Err(e) => err(e),
            },
            Request::Stop(None) => {
                self.engine.stop_all();
                Self::ok(d, "stopped")
            }
            Request::Stop(Some(t)) => match self.library.lock().resolve(&t) {
                Some(id) => {
                    self.engine.stop_sound(id.0);
                    Self::ok(d, "stopped")
                }
                None => err(format!("no sound matches `{t}`")),
            },
            Request::TogglePause => {
                let p = self.engine.toggle_pause();
                Self::ok(d, if p { "paused" } else { "resumed" })
            }
            Request::SetVolume(v) => {
                self.update_config(|c| c.audio.master_volume = f32::from(v) / 100.0);
                Self::ok(d, &format!("volume {v}"))
            }
            Request::GetVolume => {
                let v = (self.config.lock().audio.master_volume * 100.0).round() as u32;
                match d {
                    Dialect::Soundpad => v.to_string(),
                    Dialect::Clipdeck => format!("OK {v}"),
                }
            }
            Request::List => {
                let lib = self.library.lock();
                let mut s = String::from("OK");
                for (i, x) in lib.sounds.iter().enumerate() {
                    s.push_str(&format!(
                        "\n{:>4}  {:<8}  {}{}",
                        i + 1,
                        x.hotkey.clone().unwrap_or_default(),
                        x.name,
                        if x.file_exists() { "" } else { "  (missing)" }
                    ));
                }
                s
            }
            Request::Count => {
                let n = self.library.lock().sounds.len();
                match d {
                    Dialect::Soundpad => n.to_string(),
                    Dialect::Clipdeck => format!("OK {n}"),
                }
            }
            Request::Status => {
                let playing = self.engine.now_playing();
                match d {
                    Dialect::Soundpad => {
                        if self.engine.is_paused() {
                            "PAUSED".into()
                        } else if playing.is_empty() {
                            "STOPPED".into()
                        } else {
                            "PLAYING".into()
                        }
                    }
                    Dialect::Clipdeck => {
                        let lib = self.library.lock();
                        let names: Vec<String> = playing
                            .iter()
                            .map(|n| {
                                let name = lib
                                    .sound(SoundId(n.sound))
                                    .map(|s| s.name.clone())
                                    .unwrap_or_else(|| "file".into());
                                format!("{name} {}/{}s", n.pos_ms / 1000, n.len_ms / 1000)
                            })
                            .collect();
                        let st = self.engine.device_status();
                        format!(
                            "OK {}\nmonitor: {}\nvirtual mic: {}\nmic input: {}",
                            if names.is_empty() {
                                "idle".into()
                            } else {
                                names.join(", ")
                            },
                            st.monitor
                                .map(|(n, r)| format!("{n} @ {r} Hz"))
                                .unwrap_or_else(|e| e),
                            st.virtual_mic
                                .map(|(n, r)| format!("{n} @ {r} Hz"))
                                .unwrap_or_else(|e| e),
                            st.input.unwrap_or_else(|e| e),
                        )
                    }
                }
            }
            Request::Show(page) => {
                let idx = page
                    .as_deref()
                    .map(|p| match p.to_ascii_lowercase().as_str() {
                        "favorites" | "favourites" => 1,
                        "recent" => 2,
                        "categories" => 3,
                        "settings" | "preferences" => 4,
                        _ => 0,
                    });
                self.signal(UiSignal::Show);
                if let Some(i) = idx {
                    self.signal(UiSignal::ShowPage(i));
                }
                Self::ok(d, "")
            }
            Request::Hide => {
                self.signal(UiSignal::Hide);
                Self::ok(d, "")
            }
            Request::Toggle => {
                self.signal(UiSignal::Toggle);
                Self::ok(d, "")
            }
            Request::Reload => {
                if let Ok(lib) = Library::load(&self.paths.library_file()) {
                    *self.library.lock() = lib;
                }
                if let Ok(cfg) = Config::load(&self.paths.config_file()) {
                    *self.config.lock() = cfg;
                }
                self.apply_audio_config();
                self.signal(UiSignal::LibraryChanged);
                self.signal(UiSignal::ConfigChanged);
                self.signal(UiSignal::HotkeysChanged);
                Self::ok(d, "reloaded")
            }
            Request::Quit => {
                self.signal(UiSignal::Quit);
                Self::ok(d, "bye")
            }
            Request::Ping => match d {
                Dialect::Soundpad => "R-200".into(),
                Dialect::Clipdeck => "OK pong".into(),
            },
            Request::Version => match d {
                Dialect::Soundpad => env!("CARGO_PKG_VERSION").into(),
                Dialect::Clipdeck => format!("OK {}", env!("CARGO_PKG_VERSION")),
            },
            Request::SoundlistXml => {
                let doc = SplDocument::from_library(&self.library.lock(), None);
                doc.to_xml()
            }
            Request::ToggleMic => {
                let on = !self.config.lock().audio.mic_passthrough;
                self.update_config(|c| c.audio.mic_passthrough = on);
                Self::ok(
                    d,
                    if on {
                        "mic passthrough on"
                    } else {
                        "mic passthrough off"
                    },
                )
            }
            Request::StartRecording(src) => match self.start_recording(src) {
                Ok(s) => Self::ok(d, &format!("recording {}", source_name(s))),
                Err(e) => err(e),
            },
            Request::StopRecording => match self.stop_recording() {
                Ok(name) => Self::ok(d, &format!("saved {name}")),
                Err(e) => err(e),
            },
            Request::RecordingPosition => {
                let ms = self.recording().map(|r| r.1).unwrap_or(0);
                match d {
                    Dialect::Soundpad => ms.to_string(),
                    Dialect::Clipdeck => format!("OK {ms}"),
                }
            }
            Request::Seek { ms, relative } => {
                if self.engine.now_playing().is_empty() {
                    return err("nothing is playing".into());
                }
                self.engine.seek(None, ms, relative);
                Self::ok(d, "")
            }
            Request::PlaybackPosition | Request::PlaybackDuration => {
                let np = self.engine.now_playing();
                let v = np
                    .last()
                    .map(|n| {
                        if matches!(req, Request::PlaybackPosition) {
                            n.pos_ms
                        } else {
                            n.len_ms
                        }
                    })
                    .unwrap_or(0);
                match d {
                    Dialect::Soundpad => v.to_string(),
                    Dialect::Clipdeck => format!("OK {v}"),
                }
            }
            Request::ToggleMonitor => {
                let on = !self.config.lock().audio.monitor_enabled;
                self.update_config(|c| c.audio.monitor_enabled = on);
                Self::ok(d, if on { "monitor on" } else { "monitor off" })
            }
        }
    }

    /// Edit one sound.
    pub fn edit_sound(&self, id: SoundId, f: impl FnOnce(&mut Sound)) {
        let hotkey_changed = {
            let mut lib = self.library.lock();
            let Some(s) = lib.sound_mut(id) else { return };
            let before = s.hotkey.clone();
            f(s);
            before != s.hotkey
        };
        self.mark_library_dirty();
        self.signal(UiSignal::LibraryChanged);
        if hotkey_changed {
            self.signal(UiSignal::HotkeysChanged);
        }
    }

    /// Stop engine and save.
    pub fn shutdown(&self) {
        self.quitting.store(true, Ordering::Release);
        if self.recorder.lock().is_some() {
            let _ = self.stop_recording();
        }
        self.engine.stop_all();
        self.flush();
        self.engine.shutdown();
    }
}

/// "what you hear" / "the microphone".
pub fn source_name(s: RecordSource) -> &'static str {
    match s {
        RecordSource::Speakers => "what you hear",
        RecordSource::Microphone => "the microphone",
    }
}

fn backup_broken(path: &Path) {
    if path.exists() {
        let b = path.with_extension(format!("broken-{}", clipdeck_core::date::today()));
        let _ = std::fs::copy(path, b);
    }
}
