//! Clipdeck user interface (Slint, Material 3 Expressive) and the shared application core.

pub mod app;
mod keys;
mod theme;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use clipdeck_audio::engine::PlayRequest;
use clipdeck_core::model::{AUDIO_EXTENSIONS, CategoryId, PlaybackMode, Route, SoundId, Trim};
use clipdeck_core::paths::Paths;
use clipdeck_core::search::Searcher;
use clipdeck_platform::hotkeys::Hotkeys;
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::app::{AppCore, UiSignal};

#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::restriction,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::todo,
    unused_qualifications
)]
mod generated {
    slint::include_modules!();
}
pub use generated::*;

const PAGE_LIBRARY: i32 = 0;
const PAGE_FAVORITES: i32 = 1;
const PAGE_RECENT: i32 = 2;
const PAGE_CATEGORY: i32 = 3;
const PAGE_SETTINGS: i32 = 4;

/// Card colors offered in the editor (index 0 = none).
const CARD_COLORS: [u32; 11] = [
    0, 0xE53935, 0xFB8C00, 0xFDD835, 0x43A047, 0x00ACC1, 0x1E88E5, 0x5E35B1, 0xD81B60, 0x6D4C41,
    0x546E7A,
];
const BUFFERS: [u32; 4] = [128, 256, 512, 1024];

struct Ui {
    core: Arc<AppCore>,
    app: slint::Weak<AppWindow>,
    model: Rc<VecModel<SoundItem>>,
    cats: Rc<VecModel<CategoryItem>>,
    page: RefCell<(i32, Option<CategoryId>)>,
    search: RefCell<String>,
    searcher: RefCell<Searcher>,
    row_of: RefCell<HashMap<u64, Vec<usize>>>,
    playing_rows: RefCell<HashSet<usize>>,
    np_timer: Timer,
    snack_timer: Timer,
    hotkeys: RefCell<Option<Hotkeys>>,
    tray: RefCell<Option<ClipdeckTray>>,
    watcher: RefCell<Option<notify::RecommendedWatcher>>,
    outputs: RefCell<Vec<String>>,
    inputs: RefCell<Vec<String>>,
    editor_extra_color: RefCell<Option<u32>>,
}

/// Install the in-mic effect on exactly `guids` (none = uninstall) through an elevated
/// `clipdeck micfx …` process.
#[cfg(windows)]
fn apply_micfx(guids: &[String]) -> Result<(), String> {
    use clipdeck_platform::windows::micfx;
    let file = std::env::temp_dir().join(format!("clipdeck-micfx-ui-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&file);
    let action = if guids.is_empty() {
        "micfx uninstall".to_string()
    } else {
        let list: Vec<String> = guids.iter().map(|g| format!("\"{g}\"")).collect();
        format!("micfx install {}", list.join(" "))
    };
    let code = micfx::run_elevated(&format!("{action} --result-file \"{}\"", file.display()))?;
    let msg = std::fs::read_to_string(&file).unwrap_or_default();
    let _ = std::fs::remove_file(&file);
    if code == 0 && msg.starts_with("OK") {
        Ok(())
    } else {
        Err(msg
            .strip_prefix("ERR ")
            .unwrap_or("Changing the microphones failed.")
            .to_string())
    }
}

#[cfg(not(windows))]
fn apply_micfx(_guids: &[String]) -> Result<(), String> {
    Ok(())
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

fn with_ui(f: impl FnOnce(&Rc<Ui>)) {
    UI.with(|u| {
        if let Some(ui) = u.borrow().as_ref() {
            f(ui);
        }
    });
}

fn color_of(hex: &Option<String>) -> Option<slint::Color> {
    hex.as_deref()
        .and_then(theme::parse_hex)
        .map(|v| slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn fmt_ms(ms: u32) -> String {
    if ms < 10_000 {
        return format!("{:.1} s", f64::from(ms) / 1000.0);
    }
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

/// Open a file, folder or URL with the system handler.
pub fn open_external(target: &str) {
    #[cfg(windows)]
    let r = {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new("cmd")
            .args(["/C", "start", "", target])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
    };
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(target).spawn();
    if let Err(e) = r {
        tracing::warn!(%e, target, "could not open");
    }
}

impl Ui {
    fn app(&self) -> Option<AppWindow> {
        self.app.upgrade()
    }

    fn snack(&self, msg: impl Into<String>) {
        let Some(app) = self.app() else { return };
        app.set_snack_text(SharedString::from(msg.into()));
        app.set_snack_shown(true);
        let weak = self.app.clone();
        self.snack_timer.start(
            TimerMode::SingleShot,
            Duration::from_millis(4500),
            move || {
                if let Some(a) = weak.upgrade() {
                    a.set_snack_shown(false);
                }
            },
        );
    }

    /// Rebuild the visible sound list.
    fn refresh(&self) {
        let Some(app) = self.app() else { return };
        let (page, cat) = *self.page.borrow();
        let lib = self.core.library.lock();
        let base: Vec<SoundId> = match page {
            PAGE_FAVORITES => lib.favorites(),
            PAGE_RECENT => lib.recent(200),
            PAGE_CATEGORY => match cat {
                Some(c) => lib.sounds_in_category(c),
                None => Vec::new(),
            },
            _ => lib.sounds.iter().map(|s| s.id).collect(),
        };
        let query = self.search.borrow().clone();
        let ids = self.searcher.borrow_mut().search(&lib, &query, Some(&base));
        let playing: HashMap<u64, (u32, u32)> = self
            .core
            .engine
            .now_playing()
            .iter()
            .map(|n| (n.sound, (n.pos_ms, n.len_ms)))
            .collect();
        let mut row_of: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut playing_rows = HashSet::new();
        let positions: HashMap<SoundId, usize> = lib
            .sounds
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect();
        let items: Vec<SoundItem> = ids
            .iter()
            .enumerate()
            .filter_map(|(row, id)| {
                let s = lib.sound(*id)?;
                row_of.entry(id.0).or_default().push(row);
                let p = playing.get(&id.0);
                if p.is_some() {
                    playing_rows.insert(row);
                }
                let color = color_of(&s.color);
                Some(SoundItem {
                    id: id.0 as i32,
                    index: positions.get(id).copied().unwrap_or(0) as i32 + 1,
                    name: s.name.clone().into(),
                    hotkey: s.hotkey.clone().unwrap_or_default().into(),
                    duration: s.duration_ms.map(fmt_ms).unwrap_or_default().into(),
                    category: s
                        .categories
                        .first()
                        .and_then(|c| lib.category(*c))
                        .map(|c| c.name.clone())
                        .unwrap_or_default()
                        .into(),
                    favorite: s.favorite,
                    missing: !s.file_exists(),
                    looped: s.looped,
                    color: color.unwrap_or_default(),
                    has_color: color.is_some(),
                    playing: p.is_some(),
                    progress: p
                        .map(|(pos, len)| {
                            if *len > 0 {
                                *pos as f32 / *len as f32
                            } else {
                                0.0
                            }
                        })
                        .unwrap_or(0.0),
                })
            })
            .collect();
        let count = items.len();
        self.model.set_vec(items);
        *self.row_of.borrow_mut() = row_of;
        *self.playing_rows.borrow_mut() = playing_rows;

        let (title, subtitle) = match page {
            PAGE_FAVORITES => ("Favorites".to_string(), format!("{count} sound(s)")),
            PAGE_RECENT => ("Recent".to_string(), "Most recently played".to_string()),
            PAGE_CATEGORY => (
                cat.and_then(|c| lib.category(c))
                    .map(|c| c.name.clone())
                    .unwrap_or_else(|| "Categories".into()),
                format!("{count} sound(s)"),
            ),
            PAGE_SETTINGS => ("Settings".to_string(), "Saved automatically".to_string()),
            _ => (
                "Library".to_string(),
                if query.is_empty() {
                    format!("{} sounds", lib.sounds.len())
                } else {
                    format!("{count} of {} sounds", lib.sounds.len())
                },
            ),
        };
        app.set_page_title(title.into());
        app.set_page_subtitle(subtitle.into());
        app.set_library_count(lib.sounds.len() as i32);
        app.set_favorites_count(lib.favorites().len() as i32);

        // Category list (depth-first).
        let mut cats = Vec::new();
        fn walk(
            lib: &clipdeck_core::Library,
            parent: Option<CategoryId>,
            depth: i32,
            out: &mut Vec<CategoryItem>,
        ) {
            for c in lib.categories.iter().filter(|c| c.parent == parent) {
                let color = color_of(&c.color);
                out.push(CategoryItem {
                    id: c.id.0 as i32,
                    name: c.name.clone().into(),
                    count: lib.sounds_in_category(c.id).len() as i32,
                    depth,
                    color: color.unwrap_or_default(),
                    has_color: color.is_some(),
                });
                walk(lib, Some(c.id), depth + 1, out);
            }
        }
        walk(&lib, None, 0, &mut cats);
        self.cats.set_vec(cats);
        drop(lib);
        if app.get_selected() >= count as i32 {
            app.set_selected(count as i32 - 1);
        }
    }

    /// Update playing flags/progress and the now-playing bar. Returns whether anything plays.
    fn tick(&self) -> bool {
        let Some(app) = self.app() else { return false };
        let now = self.core.engine.now_playing();
        let row_of = self.row_of.borrow();
        let mut new_rows: HashMap<usize, f32> = HashMap::new();
        for n in &now {
            if let Some(rows) = row_of.get(&n.sound) {
                let p = if n.len_ms > 0 {
                    n.pos_ms as f32 / n.len_ms as f32
                } else {
                    0.0
                };
                for r in rows {
                    new_rows.insert(*r, p);
                }
            }
        }
        let mut playing_rows = self.playing_rows.borrow_mut();
        for r in playing_rows.iter() {
            if !new_rows.contains_key(r)
                && let Some(mut item) = self.model.row_data(*r)
            {
                item.playing = false;
                item.progress = 0.0;
                self.model.set_row_data(*r, item);
            }
        }
        for (r, p) in &new_rows {
            if let Some(mut item) = self.model.row_data(*r)
                && (!item.playing || (item.progress - p).abs() > 0.002)
            {
                item.playing = true;
                item.progress = *p;
                self.model.set_row_data(*r, item);
            }
        }
        *playing_rows = new_rows.keys().copied().collect();
        drop(playing_rows);
        drop(row_of);

        let active = !now.is_empty();
        app.set_np_active(active);
        app.set_np_count(now.len() as i32);
        if let Some(last) = now.last() {
            let lib = self.core.library.lock();
            let name = lib
                .sound(SoundId(last.sound))
                .map(|s| s.name.clone())
                .unwrap_or_else(|| "Preview".into());
            app.set_np_title(name.into());
            app.set_np_progress(if last.len_ms > 0 {
                last.pos_ms as f32 / last.len_ms as f32
            } else {
                0.0
            });
            app.set_np_time(format!("{} / {}", fmt_ms(last.pos_ms), fmt_ms(last.len_ms)).into());
        } else {
            app.set_np_progress(0.0);
            app.set_np_time(SharedString::default());
        }
        let (mon, mic) = self.core.engine.peaks();
        app.set_level(mon.max(mic));
        active
    }

    fn start_ticking(self: &Rc<Self>) {
        if self.np_timer.running() {
            return;
        }
        let mut idle_ticks = 0;
        self.np_timer
            .start(TimerMode::Repeated, Duration::from_millis(33), move || {
                with_ui(|ui| {
                    if ui.tick() {
                        idle_ticks = 0;
                    } else {
                        idle_ticks += 1;
                        // Keep going briefly for decode-in-flight plays, then sleep (idle CPU ≈ 0).
                        if idle_ticks > 45 {
                            ui.np_timer.stop();
                        }
                    }
                });
            });
    }

    fn play(self: &Rc<Self>, id: i32) {
        if let Err(e) = self.core.play(SoundId(id as u64), None) {
            self.snack(e);
        }
        self.start_ticking();
    }

    fn register_hotkeys(&self) {
        let errors = match self.hotkeys.borrow_mut().as_mut() {
            Some(h) => h.set_bindings(&self.core.bindings()),
            None => return,
        };
        if let Some((_, e)) = errors.first() {
            self.snack(if errors.len() > 1 {
                format!("{e} (+{} more)", errors.len() - 1)
            } else {
                e.clone()
            });
        }
    }

    // ---------------------------------------------------------------- settings

    fn load_settings(&self) {
        let Some(app) = self.app() else { return };
        let s = app.global::<Settings>();
        let cfg = self.core.config.lock().clone();
        let outs: Vec<String> = clipdeck_audio::output::output_devices()
            .into_iter()
            .map(|d| d.name)
            .filter(|n| n != "unknown" && !n.contains("Clipdeck"))
            .collect();
        let ins: Vec<String> = clipdeck_audio::output::input_devices()
            .into_iter()
            .map(|d| d.name)
            .filter(|n| n != "unknown" && !n.contains("Clipdeck") && !n.contains("CABLE Output"))
            .collect();
        let mut out_model = vec![SharedString::from("System default")];
        out_model.extend(outs.iter().map(SharedString::from));
        let mut in_model = vec![SharedString::from("System default")];
        in_model.extend(ins.iter().map(SharedString::from));
        let mut cable_model = vec![SharedString::from("Auto-detect VB-CABLE")];
        cable_model.extend(outs.iter().map(SharedString::from));
        let idx = |list: &[String], v: &Option<String>| {
            v.as_ref()
                .and_then(|v| list.iter().position(|x| x == v))
                .map(|i| i as i32 + 1)
                .unwrap_or(0)
        };
        s.set_output_index(idx(&outs, &cfg.audio.monitor_device));
        s.set_input_index(idx(&ins, &cfg.audio.input_device));
        s.set_cable_index(idx(&outs, &cfg.audio.cable_device));
        s.set_output_devices(ModelRc::new(VecModel::from(out_model)));
        s.set_input_devices(ModelRc::new(VecModel::from(in_model)));
        s.set_cable_devices(ModelRc::new(VecModel::from(cable_model)));
        *self.outputs.borrow_mut() = outs;
        *self.inputs.borrow_mut() = ins;

        s.set_is_windows(cfg!(windows));
        self.refresh_mics();
        s.set_buffer_index(
            BUFFERS
                .iter()
                .position(|b| *b >= cfg.audio.buffer_frames)
                .unwrap_or(1) as i32,
        );
        s.set_mode_index(match cfg.audio.playback_mode {
            PlaybackMode::Overlap => 0,
            PlaybackMode::Restart => 1,
            PlaybackMode::StopOthers => 2,
        });
        s.set_route_index(match cfg.audio.route {
            Route::Both => 0,
            Route::MonitorOnly => 1,
            Route::MicOnly => 2,
        });
        s.set_normalize(cfg.audio.normalize);
        s.set_target_lufs(cfg.audio.target_lufs);
        s.set_block_voice(cfg.audio.block_voice);
        s.set_monitor_volume(cfg.audio.monitor_volume);
        s.set_mic_volume(cfg.audio.mic_volume);
        s.set_hotkeys_enabled(cfg.hotkeys.enabled);
        s.set_stop_hotkey(cfg.hotkeys.stop_all.clone().unwrap_or_default().into());
        s.set_pause_hotkey(cfg.hotkeys.pause.clone().unwrap_or_default().into());
        s.set_dark(cfg.ui.theme == clipdeck_core::config::ThemeMode::Dark);
        let seed = theme::parse_hex(&cfg.ui.seed_color).unwrap_or(theme::SEEDS[0]);
        s.set_seeds(ModelRc::new(VecModel::from(
            theme::SEEDS
                .iter()
                .map(|v| slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, *v as u8))
                .collect::<Vec<_>>(),
        )));
        s.set_seed_index(theme::SEEDS.iter().position(|v| *v == seed).unwrap_or(0) as i32);
        s.set_reduce_motion(cfg.ui.reduce_motion);
        s.set_start_minimized(cfg.ui.start_minimized);
        s.set_tray(cfg.ui.tray);
        s.set_close_to_tray(cfg.ui.close_to_tray);
        s.set_gpu_rendering(cfg.ui.gpu_rendering);
        s.set_watch_folders(ModelRc::new(VecModel::from(
            cfg.ui
                .watch_folders
                .iter()
                .map(SharedString::from)
                .collect::<Vec<_>>(),
        )));
        s.set_config_path(self.core.paths.config_file().display().to_string().into());
        s.set_library_path(self.core.paths.library_file().display().to_string().into());
        s.set_version(env!("CARGO_PKG_VERSION").into());
        s.set_mic_hint(clipdeck_platform::mic_hint().into());
        if let Some(h) = self.hotkeys.borrow().as_ref() {
            s.set_hotkey_note(h.note().into());
        }
        app.set_master_volume(cfg.audio.master_volume);
        app.set_monitor_on(cfg.audio.monitor_enabled);
        app.set_mic_on(cfg.audio.mic_passthrough);
        app.set_grid_view(cfg.ui.view == clipdeck_core::config::ViewMode::Grid);
        app.global::<Motion>().set_reduce(cfg.ui.reduce_motion);
        theme::apply(
            &app,
            seed,
            cfg.ui.theme == clipdeck_core::config::ThemeMode::Dark,
        );
        self.update_device_status();
    }

    /// Fill the Windows "Microphones" list (in-mic effect per microphone).
    #[cfg(windows)]
    fn refresh_mics(&self) {
        use clipdeck_platform::windows::micfx;
        let Some(app) = self.app() else { return };
        let s = app.global::<Settings>();
        let live = micfx::live_instances();
        let items: Vec<MicItem> = micfx::endpoints()
            .into_iter()
            .map(|e| {
                let running = live
                    .iter()
                    .any(|l| l.info.endpoint.eq_ignore_ascii_case(&e.guid));
                let status = if !e.active {
                    "Unplugged"
                } else if running {
                    "On · sounds go in here right now"
                } else if e.installed {
                    "On · active as soon as an app uses this microphone"
                } else {
                    ""
                };
                MicItem {
                    name: e.name.into(),
                    guid: e.guid.into(),
                    checked: e.installed,
                    installed: e.installed,
                    status: status.into(),
                }
            })
            .collect();
        s.set_micfx_installed(items.iter().any(|m| m.installed));
        s.set_mics(ModelRc::new(VecModel::from(items)));
        s.set_micfx_dirty(false);
    }

    #[cfg(not(windows))]
    fn refresh_mics(&self) {}

    fn update_device_status(&self) {
        let Some(app) = self.app() else { return };
        let s = app.global::<Settings>();
        let st = self.core.engine.device_status();
        match &st.virtual_mic {
            Ok((name, rate)) => {
                s.set_virtual_mic_status(format!("“{name}” ready · {} kHz", rate / 1000).into());
                s.set_virtual_mic_ok(true);
                app.set_mic_ok(true);
            }
            Err(e) => {
                s.set_virtual_mic_status(e.clone().into());
                s.set_virtual_mic_ok(false);
                app.set_mic_ok(false);
                app.set_mic_label(
                    if cfg!(windows) {
                        "VB-CABLE not found — Settings"
                    } else {
                        "Virtual mic unavailable — Settings"
                    }
                    .into(),
                );
            }
        }
        s.set_monitor_status(
            match &st.monitor {
                Ok((n, r)) => format!("Now: {n} · {} kHz", r / 1000),
                Err(e) => e.clone(),
            }
            .into(),
        );
        s.set_input_status(
            match &st.input {
                Ok(n) => format!("Passing through: {n}"),
                Err(e) => e.clone(),
            }
            .into(),
        );
        let stats = self.core.engine.stats();
        let best = stats
            .best_latency_us
            .load(std::sync::atomic::Ordering::Relaxed);
        if best > 0 {
            s.set_latency_text(
                format!(
                    "trigger→audio {:.1} ms + device buffer",
                    f64::from(best) / 1000.0
                )
                .into(),
            );
        }
    }

    fn save_settings(&self) {
        let Some(app) = self.app() else { return };
        let s = app.global::<Settings>();
        let outs = self.outputs.borrow().clone();
        let ins = self.inputs.borrow().clone();
        let pick = |list: &[String], i: i32| {
            if i <= 0 {
                None
            } else {
                list.get(i as usize - 1).cloned()
            }
        };
        let tray_before = self.core.config.lock().ui.tray;
        let hk_before = self.core.config.lock().hotkeys.enabled;
        self.core.update_config(|c| {
            c.audio.monitor_device = pick(&outs, s.get_output_index());
            c.audio.input_device = pick(&ins, s.get_input_index());
            c.audio.cable_device = pick(&outs, s.get_cable_index());
            c.audio.buffer_frames = BUFFERS[s.get_buffer_index().clamp(0, 3) as usize];
            c.audio.playback_mode = [
                PlaybackMode::Overlap,
                PlaybackMode::Restart,
                PlaybackMode::StopOthers,
            ][s.get_mode_index().clamp(0, 2) as usize];
            c.audio.route = [Route::Both, Route::MonitorOnly, Route::MicOnly]
                [s.get_route_index().clamp(0, 2) as usize];
            c.audio.normalize = s.get_normalize();
            c.audio.target_lufs = s.get_target_lufs();
            c.audio.block_voice = s.get_block_voice();
            c.audio.monitor_volume = s.get_monitor_volume();
            c.audio.mic_volume = s.get_mic_volume();
            c.hotkeys.enabled = s.get_hotkeys_enabled();
            c.ui.theme = if s.get_dark() {
                clipdeck_core::config::ThemeMode::Dark
            } else {
                clipdeck_core::config::ThemeMode::Light
            };
            c.ui.seed_color = format!(
                "#{:06X}",
                theme::SEEDS[s.get_seed_index().clamp(0, 7) as usize]
            );
            c.ui.reduce_motion = s.get_reduce_motion();
            c.ui.start_minimized = s.get_start_minimized();
            c.ui.tray = s.get_tray();
            c.ui.close_to_tray = s.get_close_to_tray();
            c.ui.gpu_rendering = s.get_gpu_rendering();
        });
        let cfg = self.core.config.lock().clone();
        app.global::<Motion>().set_reduce(cfg.ui.reduce_motion);
        theme::apply(
            &app,
            theme::parse_hex(&cfg.ui.seed_color).unwrap_or(theme::SEEDS[0]),
            cfg.ui.theme == clipdeck_core::config::ThemeMode::Dark,
        );
        if tray_before != cfg.ui.tray {
            self.set_tray(cfg.ui.tray);
        }
        if hk_before != cfg.hotkeys.enabled {
            self.register_hotkeys();
        }
    }

    fn set_tray(&self, on: bool) {
        if !on {
            *self.tray.borrow_mut() = None;
            return;
        }
        if self.tray.borrow().is_some() {
            return;
        }
        match ClipdeckTray::new() {
            Ok(t) => {
                t.on_show_hide(|| with_ui(|ui| ui.toggle_window()));
                let core = self.core.clone();
                t.on_stop_all(move || core.engine.stop_all());
                let core = self.core.clone();
                t.on_toggle_mic(move || {
                    let r = core.handle_line("mic");
                    core.notify(r.trim_start_matches("OK ").to_string());
                });
                let core = self.core.clone();
                t.on_toggle_monitor(move || {
                    let r = core.handle_line("monitor");
                    core.notify(r.trim_start_matches("OK ").to_string());
                });
                t.on_quit(|| {
                    let _ = slint::quit_event_loop();
                });
                *self.tray.borrow_mut() = Some(t);
            }
            Err(e) => tracing::warn!(%e, "tray unavailable"),
        }
    }

    fn toggle_window(&self) {
        let Some(app) = self.app() else { return };
        if app.window().is_visible() {
            let _ = app.hide();
        } else {
            let _ = app.show();
        }
    }

    // ---------------------------------------------------------------- editor

    fn open_editor(self: &Rc<Self>, id: i32) {
        let Some(app) = self.app() else { return };
        let ed = app.global::<Editor>();
        let lib = self.core.library.lock();
        let Some(s) = lib.sound(SoundId(id as u64)) else {
            return;
        };
        ed.set_id(id);
        ed.set_name(s.name.clone().into());
        ed.set_volume(s.volume);
        ed.set_hotkey(s.hotkey.clone().unwrap_or_default().into());
        ed.set_hotkey_error(SharedString::default());
        ed.set_mode_index(match s.mode {
            None => 0,
            Some(PlaybackMode::Overlap) => 1,
            Some(PlaybackMode::Restart) => 2,
            Some(PlaybackMode::StopOthers) => 3,
        });
        ed.set_looped(s.looped);
        ed.set_favorite(s.favorite);
        let mut names = vec![SharedString::from("No category")];
        names.extend(
            lib.categories
                .iter()
                .map(|c| SharedString::from(c.name.as_str())),
        );
        let ci = s
            .categories
            .first()
            .and_then(|c| lib.categories.iter().position(|x| x.id == *c))
            .map(|i| i as i32 + 1)
            .unwrap_or(0);
        ed.set_category_names(ModelRc::new(VecModel::from(names)));
        ed.set_category_index(ci);
        let mut palette: Vec<slint::Color> = CARD_COLORS
            .iter()
            .map(|v| slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, *v as u8))
            .collect();
        let current = s.color.as_deref().and_then(theme::parse_hex);
        let color_index = match current {
            None => 0,
            Some(v) => match CARD_COLORS.iter().position(|c| *c == v) {
                Some(i) => i as i32,
                None => {
                    palette.push(slint::Color::from_rgb_u8(
                        (v >> 16) as u8,
                        (v >> 8) as u8,
                        v as u8,
                    ));
                    palette.len() as i32 - 1
                }
            },
        };
        *self.editor_extra_color.borrow_mut() = current.filter(|v| !CARD_COLORS.contains(v));
        ed.set_palette(ModelRc::new(VecModel::from(palette)));
        ed.set_color_index(color_index);
        let dur = s.duration_ms.unwrap_or(0);
        ed.set_duration_ms(dur as i32);
        ed.set_trim_start(if dur > 0 {
            s.trim.start_ms as f32 / dur as f32
        } else {
            0.0
        });
        ed.set_trim_end(match (s.trim.end_ms, dur) {
            (Some(e), d) if d > 0 => (e as f32 / d as f32).min(1.0),
            _ => 1.0,
        });
        ed.set_path(s.path.display().to_string().into());
        ed.set_loudness(
            s.loudness_lufs
                .map(|l| format!("{l:.1} LUFS"))
                .unwrap_or_default()
                .into(),
        );
        ed.set_waveform(slint::Image::default());
        let path = s.path.clone();
        drop(lib);
        ed.set_open(true);

        // Waveform off the UI thread.
        let weak = self.app.clone();
        std::thread::spawn(move || {
            let Ok(wf) = clipdeck_audio::waveform::compute(&path, 600) else {
                return;
            };
            let (w, h) = (1200u32, 240u32);
            let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(w, h);
            let px = buf.make_mut_slice();
            let n = wf.buckets.len().max(1);
            for x in 0..w as usize {
                let (lo, hi) = wf.buckets[(x * n / w as usize).min(n - 1)];
                let mid = h as f32 / 2.0;
                let top = (mid - hi.clamp(0.0, 1.0) * mid * 0.95) as usize;
                let bot = ((mid - lo.clamp(-1.0, 0.0) * mid * 0.95) as usize).min(h as usize - 1);
                for y in top..=bot.max(top) {
                    px[y * w as usize + x] = slint::Rgba8Pixel {
                        r: 255,
                        g: 255,
                        b: 255,
                        a: 255,
                    };
                }
            }
            let dur = wf.duration_ms;
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(app) = weak.upgrade() {
                    let ed = app.global::<Editor>();
                    if ed.get_open() {
                        ed.set_waveform(slint::Image::from_rgba8(buf));
                        if ed.get_duration_ms() == 0 {
                            ed.set_duration_ms(dur as i32);
                        }
                    }
                }
            });
        });
    }

    fn editor_trim(&self, ed: &Editor) -> Trim {
        let dur = ed.get_duration_ms().max(0) as f32;
        if dur <= 0.0 {
            return Trim::default();
        }
        let start = (ed.get_trim_start().clamp(0.0, 1.0) * dur) as u32;
        let end = ed.get_trim_end().clamp(0.0, 1.0);
        Trim {
            start_ms: start,
            end_ms: (end < 0.999).then_some((end * dur) as u32),
        }
    }

    fn save_editor(&self) {
        let Some(app) = self.app() else { return };
        let ed = app.global::<Editor>();
        let id = SoundId(ed.get_id() as u64);
        let hotkey = ed.get_hotkey().to_string();
        let hotkey = (!hotkey.is_empty()).then_some(hotkey);
        // A hotkey belongs to one sound: take it away from any other.
        if let Some(h) = &hotkey {
            let owner = self.core.library.lock().hotkey_owner(h, Some(id));
            if let Some(other) = owner {
                self.core.edit_sound(other, |s| s.hotkey = None);
            }
        }
        let cat = {
            let lib = self.core.library.lock();
            let i = ed.get_category_index();
            if i <= 0 {
                None
            } else {
                lib.categories.get(i as usize - 1).map(|c| c.id)
            }
        };
        let color = match ed.get_color_index() {
            0 => None,
            i if (i as usize) < CARD_COLORS.len() => {
                Some(format!("{:06X}", CARD_COLORS[i as usize]))
            }
            _ => self.editor_extra_color.borrow().map(|v| format!("{v:06X}")),
        };
        let trim = self.editor_trim(&ed);
        let name = ed.get_name().trim().to_string();
        self.core.edit_sound(id, |s| {
            if !name.is_empty() {
                s.name = name;
            }
            s.volume = ed.get_volume().clamp(0.0, 2.0);
            s.hotkey = hotkey;
            s.mode = [
                None,
                Some(PlaybackMode::Overlap),
                Some(PlaybackMode::Restart),
                Some(PlaybackMode::StopOthers),
            ][ed.get_mode_index().clamp(0, 3) as usize];
            s.looped = ed.get_looped();
            s.favorite = ed.get_favorite();
            match cat {
                Some(c) => {
                    s.categories.retain(|x| *x != c);
                    s.categories.insert(0, c);
                    s.categories.truncate(1);
                }
                None => s.categories.clear(),
            }
            s.color = color;
            s.trim = trim;
        });
        self.core.engine.set_sound_volume(id.0, ed.get_volume());
        ed.set_open(false);
    }

    fn preview_editor(self: &Rc<Self>) {
        let Some(app) = self.app() else { return };
        let ed = app.global::<Editor>();
        let id = SoundId(ed.get_id() as u64);
        let Some(path) = self.core.library.lock().sound(id).map(|s| s.path.clone()) else {
            return;
        };
        let lufs = self
            .core
            .library
            .lock()
            .sound(id)
            .and_then(|s| s.loudness_lufs);
        self.core.engine.play(PlayRequest {
            sound: id.0,
            path,
            volume: ed.get_volume(),
            trim: self.editor_trim(&ed),
            looped: false,
            mode: PlaybackMode::Restart,
            route: Route::MonitorOnly,
            lufs,
        });
        self.start_ticking();
    }

    fn open_category_editor(&self, id: i32) {
        let Some(app) = self.app() else { return };
        let lib = self.core.library.lock();
        let Some(c) = lib.category(CategoryId(id as u64)) else {
            return;
        };
        let ce = app.global::<CategoryEditor>();
        ce.set_id(id);
        ce.set_name(c.name.clone().into());
        ce.set_hotkey(c.hotkey.clone().unwrap_or_default().into());
        let v = c.color.as_deref().and_then(theme::parse_hex);
        ce.set_color_index(
            v.and_then(|v| CARD_COLORS.iter().position(|x| *x == v))
                .unwrap_or(0) as i32,
        );
        ce.set_palette(ModelRc::new(VecModel::from(
            CARD_COLORS
                .iter()
                .map(|v| slint::Color::from_rgb_u8((v >> 16) as u8, (v >> 8) as u8, *v as u8))
                .collect::<Vec<_>>(),
        )));
        ce.set_open(true);
    }

    // ---------------------------------------------------------------- files

    fn add_paths(&self, paths: Vec<PathBuf>) {
        let cat = match *self.page.borrow() {
            (PAGE_CATEGORY, Some(c)) => Some(c),
            _ => None,
        };
        let r = self.core.add_paths(&paths, cat);
        let mut msg = format!("Added {} sound(s)", r.added.len());
        if r.duplicates > 0 {
            msg.push_str(&format!(", {} already in the library", r.duplicates));
        }
        if r.unsupported > 0 {
            msg.push_str(&format!(", {} not audio", r.unsupported));
        }
        self.snack(msg);
    }

    fn start_watcher(&self) {
        use notify::{EventKind, RecursiveMode, Watcher};
        let folders = self.core.config.lock().ui.watch_folders.clone();
        if folders.is_empty() {
            *self.watcher.borrow_mut() = None;
            return;
        }
        // Pick up files added while we were closed.
        let paths: Vec<PathBuf> = folders
            .iter()
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect();
        let r = self.core.add_paths(&paths, None);
        if !r.added.is_empty() {
            self.snack(format!(
                "Found {} new sound(s) in watched folders",
                r.added.len()
            ));
        }
        let core = self.core.clone();
        let watcher = notify::recommended_watcher(move |ev: notify::Result<notify::Event>| {
            if let Ok(ev) = ev
                && matches!(
                    ev.kind,
                    EventKind::Create(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))
                )
            {
                let files: Vec<PathBuf> = ev
                    .paths
                    .into_iter()
                    .filter(|p| clipdeck_core::model::is_audio_file(p) && p.is_file())
                    .collect();
                if !files.is_empty() {
                    let r = core.add_paths(&files, None);
                    if !r.added.is_empty() {
                        core.notify(format!(
                            "Added {} new sound(s) from a watched folder",
                            r.added.len()
                        ));
                    }
                }
            }
        });
        match watcher {
            Ok(mut w) => {
                for f in &paths {
                    if let Err(e) = w.watch(f, RecursiveMode::Recursive) {
                        tracing::warn!(%e, folder = %f.display(), "cannot watch");
                    }
                }
                *self.watcher.borrow_mut() = Some(w);
            }
            Err(e) => tracing::warn!(%e, "folder watching unavailable"),
        }
    }
}

fn pick_files_async(folder: bool) {
    std::thread::spawn(move || {
        let dlg = rfd::FileDialog::new().set_title(if folder {
            "Add a folder of sounds"
        } else {
            "Add sounds"
        });
        let picked: Vec<PathBuf> = if folder {
            dlg.pick_folder().into_iter().collect()
        } else {
            dlg.add_filter("Audio", AUDIO_EXTENSIONS)
                .pick_files()
                .unwrap_or_default()
        };
        if picked.is_empty() {
            return;
        }
        let _ = slint::invoke_from_event_loop(move || with_ui(|ui| ui.add_paths(picked)));
    });
}

fn handle_signal(ui: &Rc<Ui>, s: UiSignal) {
    let Some(app) = ui.app() else { return };
    match s {
        UiSignal::LibraryChanged => ui.refresh(),
        UiSignal::ConfigChanged => {
            let cfg = ui.core.config.lock().audio.clone();
            app.set_master_volume(cfg.master_volume);
            app.set_monitor_on(cfg.monitor_enabled);
            app.set_mic_on(cfg.mic_passthrough);
        }
        UiSignal::Show => {
            let _ = app.show();
        }
        UiSignal::Hide => {
            let _ = app.hide();
        }
        UiSignal::Toggle => ui.toggle_window(),
        UiSignal::ShowPage(p) => app.invoke_navigate(p, -1),
        UiSignal::Quit => {
            let _ = slint::quit_event_loop();
        }
        UiSignal::Notice(n) => ui.snack(n),
        UiSignal::Playback => ui.start_ticking(),
        UiSignal::Devices(_) => {
            ui.update_device_status();
        }
        UiSignal::HotkeysChanged => ui.register_hotkeys(),
    }
}

fn wire(ui: &Rc<Ui>, app: &AppWindow) {
    app.set_sounds(ModelRc::from(ui.model.clone()));
    app.set_categories(ModelRc::from(ui.cats.clone()));

    app.on_play(|id| with_ui(|ui| ui.play(id)));
    app.on_stop_all(|| {
        with_ui(|ui| {
            ui.core.engine.stop_all();
            ui.start_ticking();
        })
    });
    app.on_search_edited(|t| {
        with_ui(|ui| {
            *ui.search.borrow_mut() = t.to_string();
            if let Some(app) = ui.app() {
                app.set_selected(if t.is_empty() { -1 } else { 0 });
            }
            ui.refresh();
        })
    });
    app.on_navigate(|page, cat| {
        with_ui(|ui| {
            let cat = if page == PAGE_CATEGORY {
                let lib = ui.core.library.lock();
                if cat >= 0 {
                    Some(CategoryId(cat as u64))
                } else {
                    lib.categories.first().map(|c| c.id)
                }
            } else {
                None
            };
            *ui.page.borrow_mut() = (page, cat);
            if let Some(app) = ui.app() {
                app.set_page(page);
                app.set_category_id(cat.map(|c| c.0 as i32).unwrap_or(-1));
                app.set_selected(-1);
            }
            if page == PAGE_SETTINGS {
                ui.load_settings();
            }
            ui.refresh();
        })
    });
    app.on_edit(|id| with_ui(|ui| ui.open_editor(id)));
    app.on_toggle_favorite(|id| {
        with_ui(|ui| {
            ui.core
                .edit_sound(SoundId(id as u64), |s| s.favorite = !s.favorite)
        })
    });
    app.on_add_files(|| pick_files_async(false));
    app.on_add_folder(|| pick_files_async(true));
    app.on_files_dropped(|e| {
        let paths: Vec<PathBuf> = e
            .data
            .file_paths()
            .map(|v| v.into_iter().map(Path::to_path_buf).collect())
            .unwrap_or_default();
        if !paths.is_empty() {
            with_ui(|ui| ui.add_paths(paths));
        }
    });
    app.on_master_volume_changed(|v| {
        with_ui(|ui| {
            ui.core.engine.set_master_volume(v);
            ui.core.config.lock().audio.master_volume = v;
            ui.core.mark_config_dirty();
        })
    });
    app.on_monitor_toggled(|v| {
        with_ui(|ui| ui.core.update_config(|c| c.audio.monitor_enabled = v))
    });
    app.on_mic_toggled(|v| with_ui(|ui| ui.core.update_config(|c| c.audio.mic_passthrough = v)));
    app.on_dismiss_snack(|| {
        with_ui(|ui| {
            if let Some(a) = ui.app() {
                a.set_snack_shown(false);
            }
        })
    });
    app.on_view_changed(|grid| {
        with_ui(|ui| {
            ui.core.config.lock().ui.view = if grid {
                clipdeck_core::config::ViewMode::Grid
            } else {
                clipdeck_core::config::ViewMode::List
            };
            ui.core.mark_config_dirty();
        })
    });
    app.on_random(|| {
        with_ui(|ui| {
            let ids: Vec<SoundId> = ui.model.iter().map(|s| SoundId(s.id as u64)).collect();
            if let Err(e) = ui
                .core
                .play_random(if ids.is_empty() { None } else { Some(&ids) })
            {
                ui.snack(e);
            }
            ui.start_ticking();
        })
    });
    app.on_new_category(|| {
        with_ui(|ui| {
            let n = ui.core.library.lock().categories.len() + 1;
            let id = ui
                .core
                .library
                .lock()
                .add_category(&format!("Category {n}"), None);
            ui.core.mark_library_dirty();
            *ui.page.borrow_mut() = (PAGE_CATEGORY, Some(id));
            if let Some(app) = ui.app() {
                app.set_category_id(id.0 as i32);
            }
            ui.refresh();
            ui.open_category_editor(id.0 as i32);
        })
    });
    app.on_edit_category(|id| with_ui(|ui| ui.open_category_editor(id)));
    let ce = app.global::<CategoryEditor>();
    ce.on_cancel(|| {
        with_ui(|ui| {
            if let Some(app) = ui.app() {
                app.global::<CategoryEditor>().set_open(false);
            }
        })
    });
    ce.on_save(|| {
        with_ui(|ui| {
            let Some(app) = ui.app() else { return };
            let ce = app.global::<CategoryEditor>();
            let id = CategoryId(ce.get_id() as u64);
            let name = ce.get_name().trim().to_string();
            let color = match ce.get_color_index() {
                i if i > 0 && (i as usize) < CARD_COLORS.len() => {
                    Some(format!("{:06X}", CARD_COLORS[i as usize]))
                }
                _ => None,
            };
            let hotkey = Some(ce.get_hotkey().to_string()).filter(|h| !h.is_empty());
            if let Some(c) = ui.core.library.lock().category_mut(id) {
                if !name.is_empty() {
                    c.name = name;
                }
                c.color = color;
                c.hotkey = hotkey;
            }
            ui.core.mark_library_dirty();
            ce.set_open(false);
            ui.refresh();
            ui.register_hotkeys();
        })
    });
    ce.on_remove(|| {
        with_ui(|ui| {
            let Some(app) = ui.app() else { return };
            let ce = app.global::<CategoryEditor>();
            let id = CategoryId(ce.get_id() as u64);
            ui.core.library.lock().remove_category(id);
            ui.core.mark_library_dirty();
            ce.set_open(false);
            let first = ui.core.library.lock().categories.first().map(|c| c.id);
            *ui.page.borrow_mut() = (PAGE_CATEGORY, first);
            app.set_category_id(first.map(|c| c.0 as i32).unwrap_or(-1));
            ui.refresh();
            ui.register_hotkeys();
            ui.snack("Category deleted (its sounds stay in the library)");
        })
    });
    ce.on_capture_hotkey(|t, c, a, s, m| {
        with_ui(|ui| {
            if let (Some(app), Some(h)) = (ui.app(), keys::from_event(&t, c, a, s, m)) {
                app.global::<CategoryEditor>().set_hotkey(h.into());
            }
        })
    });
    ce.on_clear_hotkey(|| {
        with_ui(|ui| {
            if let Some(app) = ui.app() {
                app.global::<CategoryEditor>()
                    .set_hotkey(SharedString::default());
            }
        })
    });

    // Settings.
    let s = app.global::<Settings>();
    s.on_changed(|| with_ui(|ui| ui.save_settings()));
    s.on_capture_hotkey(|which, t, c, a, sh, m| {
        with_ui(|ui| {
            let Some(h) = keys::from_event(&t, c, a, sh, m) else {
                return;
            };
            ui.core.update_config(|cfg| match which.as_str() {
                "stop" => cfg.hotkeys.stop_all = Some(h.clone()),
                _ => cfg.hotkeys.pause = Some(h.clone()),
            });
            ui.load_settings();
            ui.register_hotkeys();
        })
    });
    s.on_clear_hotkey(|which| {
        with_ui(|ui| {
            ui.core.update_config(|cfg| match which.as_str() {
                "stop" => cfg.hotkeys.stop_all = None,
                _ => cfg.hotkeys.pause = None,
            });
            ui.load_settings();
            ui.register_hotkeys();
        })
    });
    s.on_import_spl(|| {
        std::thread::spawn(|| {
            let Some(f) = rfd::FileDialog::new()
                .set_title("Import a Soundpad sound list")
                .add_filter("Soundpad list", &["spl"])
                .pick_file()
            else {
                return;
            };
            let _ = slint::invoke_from_event_loop(move || {
                with_ui(|ui| match ui.core.import_spl(&f) {
                    Ok(r) => ui.snack(format!(
                        "Imported {} sound(s), {} categories{}",
                        r.added,
                        r.categories,
                        if r.missing > 0 {
                            format!(" — {} file(s) not found on this computer", r.missing)
                        } else {
                            String::new()
                        }
                    )),
                    Err(e) => ui.snack(e),
                })
            });
        });
    });
    s.on_export_spl(|| {
        std::thread::spawn(|| {
            let Some(f) = rfd::FileDialog::new()
                .set_title("Export as Soundpad list")
                .set_file_name("clipdeck.spl")
                .add_filter("Soundpad list", &["spl"])
                .save_file()
            else {
                return;
            };
            let _ = slint::invoke_from_event_loop(move || {
                with_ui(|ui| match ui.core.export_spl(&f) {
                    Ok(()) => ui.snack(format!("Exported to {}", f.display())),
                    Err(e) => ui.snack(e),
                })
            });
        });
    });
    s.on_add_watch_folder(|| {
        std::thread::spawn(|| {
            let Some(f) = rfd::FileDialog::new()
                .set_title("Watch a folder")
                .pick_folder()
            else {
                return;
            };
            let _ = slint::invoke_from_event_loop(move || {
                with_ui(|ui| {
                    let f = f.display().to_string();
                    ui.core.update_config(|c| {
                        if !c.ui.watch_folders.contains(&f) {
                            c.ui.watch_folders.push(f.clone());
                        }
                    });
                    ui.load_settings();
                    ui.start_watcher();
                })
            });
        });
    });
    s.on_remove_watch_folder(|i| {
        with_ui(|ui| {
            ui.core.update_config(|c| {
                if (i as usize) < c.ui.watch_folders.len() {
                    c.ui.watch_folders.remove(i as usize);
                }
            });
            ui.load_settings();
            ui.start_watcher();
        })
    });
    s.on_relink(|| {
        std::thread::spawn(|| {
            let Some(f) = rfd::FileDialog::new()
                .set_title("Where are the missing files?")
                .pick_folder()
            else {
                return;
            };
            let _ = slint::invoke_from_event_loop(move || {
                with_ui(|ui| {
                    let n = ui.core.library.lock().relink_missing(&f);
                    ui.core.mark_library_dirty();
                    ui.refresh();
                    ui.snack(format!("Relinked {n} file(s)"));
                })
            });
        });
    });
    s.on_open_data_folder(|| {
        with_ui(|ui| open_external(&ui.core.paths.data_dir.display().to_string()))
    });
    s.on_open_url(|u| open_external(&u));
    s.on_refresh_mics(|| with_ui(|ui| ui.refresh_mics()));
    s.on_mic_toggled(|i, on| {
        with_ui(|ui| {
            let Some(app) = ui.app() else { return };
            let s = app.global::<Settings>();
            let model = s.get_mics();
            if let Some(mut row) = model.row_data(i as usize) {
                row.checked = on;
                model.set_row_data(i as usize, row);
            }
            s.set_micfx_dirty(model.iter().any(|m| m.checked != m.installed));
        })
    });
    s.on_apply_mics(|| {
        with_ui(|ui| {
            let Some(app) = ui.app() else { return };
            let s = app.global::<Settings>();
            let guids: Vec<String> = s
                .get_mics()
                .iter()
                .filter(|m| m.checked)
                .map(|m| m.guid.to_string())
                .collect();
            s.set_micfx_busy(true);
            s.set_micfx_note("Waiting for the administrator prompt…".into());
            std::thread::spawn(move || {
                let result = apply_micfx(&guids);
                let _ = slint::invoke_from_event_loop(move || {
                    with_ui(|ui| {
                        let Some(app) = ui.app() else { return };
                        let s = app.global::<Settings>();
                        s.set_micfx_busy(false);
                        s.set_micfx_note(
                            match &result {
                                Ok(()) if guids.is_empty() => {
                                    "Removed. Clipdeck uses VB-CABLE again.".to_string()
                                }
                                Ok(()) => "Done. Sounds now go into the microphones switched on."
                                    .to_string(),
                                Err(e) => e.clone(),
                            }
                            .into(),
                        );
                        ui.refresh_mics();
                        ui.core.engine.reopen_devices();
                    })
                });
            });
        })
    });
    s.on_reopen_devices(|| {
        with_ui(|ui| {
            ui.core.engine.reopen_devices();
            ui.snack("Reconnecting audio devices…");
        })
    });

    // Editor.
    let ed = app.global::<Editor>();
    ed.on_save(|| with_ui(|ui| ui.save_editor()));
    ed.on_cancel(|| {
        with_ui(|ui| {
            if let Some(app) = ui.app() {
                app.global::<Editor>().set_open(false);
            }
        })
    });
    ed.on_preview(|| with_ui(|ui| ui.preview_editor()));
    ed.on_remove(|| {
        with_ui(|ui| {
            let Some(app) = ui.app() else { return };
            let ed = app.global::<Editor>();
            let id = SoundId(ed.get_id() as u64);
            ui.core.engine.stop_sound(id.0);
            let name = ui
                .core
                .library
                .lock()
                .sound(id)
                .map(|s| s.name.clone())
                .unwrap_or_default();
            ui.core.library.lock().remove_sounds(&[id]);
            ui.core.mark_library_dirty();
            ed.set_open(false);
            ui.refresh();
            ui.register_hotkeys();
            ui.snack(format!("Removed “{name}” (the file stays on disk)"));
        })
    });
    ed.on_reveal(|| {
        with_ui(|ui| {
            if let Some(app) = ui.app() {
                let p = PathBuf::from(app.global::<Editor>().get_path().as_str());
                if let Some(dir) = p.parent() {
                    open_external(&dir.display().to_string());
                }
            }
        })
    });
    ed.on_capture_hotkey(|t, c, a, s, m| {
        with_ui(|ui| {
            let Some(app) = ui.app() else { return };
            let ed = app.global::<Editor>();
            let Some(h) = keys::from_event(&t, c, a, s, m) else {
                return;
            };
            let id = SoundId(ed.get_id() as u64);
            let owner = ui
                .core
                .library
                .lock()
                .hotkey_owner(&h, Some(id))
                .and_then(|o| ui.core.library.lock().sound(o).map(|s| s.name.clone()));
            let risky = h
                .parse::<clipdeck_core::hotkey::Hotkey>()
                .map(|k| k.is_risky())
                .unwrap_or(false);
            ed.set_hotkey_error(match (owner, risky) {
                (Some(n), _) => format!("Also used by “{n}”; saving moves it here").into(),
                (None, true) => {
                    "A bare key blocks normal typing everywhere; consider adding Ctrl/Alt".into()
                }
                _ => SharedString::default(),
            });
            ed.set_hotkey(h.into());
        })
    });
    ed.on_clear_hotkey(|| {
        with_ui(|ui| {
            if let Some(app) = ui.app() {
                app.global::<Editor>().set_hotkey(SharedString::default());
                app.global::<Editor>()
                    .set_hotkey_error(SharedString::default());
            }
        })
    });

    // Window close: hide to tray or quit.
    app.window().on_close_requested(|| {
        let mut hide = false;
        with_ui(|ui| {
            let cfg = ui.core.config.lock().ui.clone();
            hide = cfg.close_to_tray && cfg.tray && ui.tray.borrow().is_some();
        });
        if hide {
            slint::CloseRequestResponse::HideWindow
        } else {
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        }
    });
}

fn proc_stats() -> Option<(u64, u64)> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let rss_kb = status
            .lines()
            .find(|l| l.starts_with("VmRSS:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let after = stat.rsplit_once(')')?.1;
        let f: Vec<&str> = after.split_whitespace().collect();
        let ticks: u64 = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
        Some((rss_kb, ticks * 10))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Run the GUI until the user quits.
pub fn run(paths: Paths, minimized: bool) -> anyhow::Result<()> {
    let bench = std::env::var_os("CLIPDECK_BENCH").is_some();
    let core = AppCore::start(paths);

    // Single-instance IPC.
    let c2 = core.clone();
    let _server = match clipdeck_platform::ipc::serve(Box::new(move |l| c2.handle_line(l))) {
        Ok(s) => Some(s),
        Err(clipdeck_platform::ipc::IpcError::AlreadyRunning) => {
            let _ = clipdeck_platform::ipc::request("show");
            core.shutdown();
            return Ok(());
        }
        Err(e) => {
            tracing::warn!(%e, "IPC unavailable; the CLI will not reach this instance");
            None
        }
    };

    // Software rendering keeps idle RAM ~40 MB (the GL driver alone maps ~70 MB); users can opt
    // into the GPU renderer in Settings. An explicit SLINT_BACKEND always wins.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        let renderer = if core.config.lock().ui.gpu_rendering {
            "femtovg"
        } else {
            "software"
        };
        if let Err(e) = slint::BackendSelector::new()
            .renderer_name(renderer.into())
            .select()
        {
            tracing::warn!(%e, renderer, "renderer selection failed; using default");
        }
    }
    let app = AppWindow::new()?;
    // Must be set before the window is shown: compositors match windows and .desktop files by it.
    let _ = slint::set_xdg_app_id(clipdeck_platform::APP_ID);
    let cfg = core.config.lock().clone();
    app.window().set_size(slint::LogicalSize::new(
        cfg.ui.window_width as f32,
        cfg.ui.window_height as f32,
    ));
    let ui = Rc::new(Ui {
        core: core.clone(),
        app: app.as_weak(),
        model: Rc::new(VecModel::default()),
        cats: Rc::new(VecModel::default()),
        page: RefCell::new((PAGE_LIBRARY, None)),
        search: RefCell::new(String::new()),
        searcher: RefCell::new(Searcher::new()),
        row_of: RefCell::new(HashMap::new()),
        playing_rows: RefCell::new(HashSet::new()),
        np_timer: Timer::default(),
        snack_timer: Timer::default(),
        hotkeys: RefCell::new(None),
        tray: RefCell::new(None),
        watcher: RefCell::new(None),
        outputs: RefCell::new(Vec::new()),
        inputs: RefCell::new(Vec::new()),
        editor_extra_color: RefCell::new(None),
    });
    UI.with(|u| *u.borrow_mut() = Some(ui.clone()));
    wire(&ui, &app);
    ui.load_settings();
    ui.refresh();

    // Core → UI signals.
    let signals = core.signals();
    std::thread::Builder::new()
        .name("clipdeck-ui-signals".into())
        .spawn(move || {
            while let Ok(s) = signals.recv() {
                let quit = s == UiSignal::Quit;
                let _ = slint::invoke_from_event_loop(move || with_ui(|ui| handle_signal(ui, s)));
                if quit {
                    break;
                }
            }
        })?;

    // Start-minimized needs the tray up front; everything else is deferred until the window is up.
    let want_hidden = (minimized || cfg.ui.start_minimized) && cfg.ui.tray && !bench;
    if want_hidden {
        ui.set_tray(true);
    }
    let hidden = want_hidden && ui.tray.borrow().is_some();
    if !hidden {
        app.show()?;
    }

    // Hotkeys (created on the UI thread; Windows delivers them through its message loop), tray and
    // folder watching start right after the first frame so a big library never delays the window.
    let init_timer = Timer::default();
    init_timer.start(TimerMode::SingleShot, Duration::from_millis(0), move || {
        with_ui(|ui| {
            let core = ui.core.clone();
            let c3 = core.clone();
            *ui.hotkeys.borrow_mut() = Some(Hotkeys::new(Arc::new(move |id: &str| {
                c3.on_hotkey(id);
                c3.signal(UiSignal::Playback);
            })));
            if !bench {
                ui.register_hotkeys();
                ui.set_tray(core.config.lock().ui.tray);
                ui.start_watcher();
            }
            core.warm_cache();
            if let Some(app) = ui.app()
                && let Some(h) = ui.hotkeys.borrow().as_ref()
            {
                app.global::<Settings>().set_hotkey_note(h.note().into());
            }
        })
    });

    // Benchmark mode: report startup and idle numbers, then quit.
    let bench_timer = Timer::default();
    if bench {
        println!("shown");
        // Let startup work (analysis, cache warm-up) settle for 3 s, then sample 5 s of idle.
        let mut ticks = 0u32;
        let mut before = None;
        bench_timer.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            ticks += 1;
            if ticks == 3 {
                before = proc_stats();
            }
            if ticks == 8 {
                if let (Some((_, c0)), Some((rss, c1))) = (before, proc_stats()) {
                    println!(
                        "idle cpu={:.2}% rss={:.1} MB",
                        (c1 - c0) as f64 / 5000.0 * 100.0,
                        rss as f64 / 1024.0
                    );
                } else {
                    println!("idle (no /proc on this platform)");
                }
                let _ = slint::quit_event_loop();
            }
        });
    }

    slint::run_event_loop_until_quit()?;

    // Remember window size.
    let size = app.window().size();
    let scale = app.window().scale_factor();
    if size.width > 0 && scale > 0.0 {
        core.config.lock().ui.window_width = (size.width as f32 / scale) as u32;
        core.config.lock().ui.window_height = (size.height as f32 / scale) as u32;
        core.mark_config_dirty();
    }
    UI.with(|u| *u.borrow_mut() = None);
    core.shutdown();
    Ok(())
}
