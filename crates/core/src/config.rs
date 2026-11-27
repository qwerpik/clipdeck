//! User settings, stored as TOML in the platform config directory.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{PlaybackMode, Route};

/// Theme brightness.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    /// Dark (default).
    #[default]
    Dark,
    /// Light.
    Light,
}

/// Library view style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewMode {
    /// Large cards.
    #[default]
    Grid,
    /// Compact rows.
    List,
}

/// What happens when a sound ends by itself (Soundpad's "continue playback" / "repeat").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AfterEnd {
    /// Nothing.
    #[default]
    Stop,
    /// Play the next sound in the list.
    Next,
    /// Play the same sound again.
    Repeat,
}

/// What the recorder captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordSource {
    /// Everything you hear (the default output device, loopback).
    #[default]
    Speakers,
    /// The microphone.
    Microphone,
}

/// Recorder settings (Soundpad's "Recorder" preferences).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecorderConfig {
    /// Source used by the record hotkey and the record button's main action.
    pub source: RecordSource,
    /// Folder for recordings; `None` = `<data dir>/recordings`.
    pub folder: Option<String>,
    /// Add finished recordings to the library.
    pub add_to_library: bool,
    /// Category (by name) for new recordings; created on demand. Empty = none.
    pub category: String,
    /// Cut silence at the start and end.
    pub trim_silence: bool,
    /// Raise or lower the recording so its peak sits at -1 dBFS.
    pub normalize: bool,
    /// Stop automatically after this many seconds (0 = no limit).
    pub max_seconds: u32,
}

impl Default for RecorderConfig {
    fn default() -> Self {
        RecorderConfig {
            source: RecordSource::Speakers,
            folder: None,
            add_to_library: true,
            category: "Recordings".into(),
            trim_silence: true,
            normalize: true,
            max_seconds: 300,
        }
    }
}

/// Audio settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Master volume 0.0..=1.0 for sounds.
    pub master_volume: f32,
    /// Monitor (your headphones) volume 0.0..=1.0.
    pub monitor_volume: f32,
    /// Play sounds to your own output device.
    pub monitor_enabled: bool,
    /// Mix your real microphone into the virtual mic.
    pub mic_passthrough: bool,
    /// Real microphone gain 0.0..=2.0.
    pub mic_volume: f32,
    /// Output device for monitoring; `None` = system default.
    pub monitor_device: Option<String>,
    /// Real microphone to pass through; `None` = system default.
    pub input_device: Option<String>,
    /// Windows: output device that feeds the virtual cable; `None` = auto-detect VB-CABLE.
    pub cable_device: Option<String>,
    /// Requested buffer size in frames (128–2048).
    pub buffer_frames: u32,
    /// Default playback mode.
    pub playback_mode: PlaybackMode,
    /// Default route for triggers.
    pub route: Route,
    /// Loudness normalization on/off.
    pub normalize: bool,
    /// Normalization target, LUFS.
    pub target_lufs: f32,
    /// Cut the real microphone while a sound plays on the mic (Soundpad's "block voice").
    pub block_voice: bool,
    /// Delay before every sound starts, ms, so there is time to press push-to-talk.
    pub ptt_delay_ms: u32,
    /// Extra delay on the microphone only, ms ("karaoke": you hear the sound first).
    pub karaoke_delay_ms: u32,
    /// What to do when a sound ends by itself.
    pub after_end: AfterEnd,
    /// Play [`AudioConfig::activation_sound`] on the microphone before every sound, so apps
    /// with voice activation open the mic in time (Soundpad's "voice activation sound").
    pub activation_enabled: bool,
    /// File used for that, e.g. a short burst of noise.
    pub activation_sound: Option<String>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        AudioConfig {
            master_volume: 0.8,
            monitor_volume: 0.6,
            monitor_enabled: true,
            mic_passthrough: true,
            mic_volume: 1.0,
            monitor_device: None,
            input_device: None,
            cable_device: None,
            buffer_frames: 256,
            playback_mode: PlaybackMode::Restart,
            route: Route::Both,
            normalize: true,
            target_lufs: -16.0,
            block_voice: false,
            ptt_delay_ms: 0,
            karaoke_delay_ms: 0,
            after_end: AfterEnd::Stop,
            activation_enabled: false,
            activation_sound: None,
        }
    }
}

/// Hotkey settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HotkeyConfig {
    /// Global hotkeys enabled.
    pub enabled: bool,
    /// Stop-all hotkey.
    pub stop_all: Option<String>,
    /// Pause/resume hotkey.
    pub pause: Option<String>,
    /// Start/stop recording hotkey.
    pub record: Option<String>,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        HotkeyConfig {
            enabled: true,
            stop_all: Some("Ctrl+Alt+S".into()),
            pause: None,
            record: None,
        }
    }
}

/// Look and window behavior.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Dark or light.
    pub theme: ThemeMode,
    /// Seed color for Material dynamic color, `#RRGGBB`.
    pub seed_color: String,
    /// Grid or list.
    pub view: ViewMode,
    /// Disable spring animations.
    pub reduce_motion: bool,
    /// Start hidden in the tray.
    pub start_minimized: bool,
    /// Show a tray icon.
    pub tray: bool,
    /// Closing the window hides to tray instead of quitting.
    pub close_to_tray: bool,
    /// Remembered window width.
    pub window_width: u32,
    /// Remembered window height.
    pub window_height: u32,
    /// Folders watched for new sounds.
    pub watch_folders: Vec<String>,
    /// Render with the GPU (OpenGL). Off = software renderer, ~70 MB less RAM.
    pub gpu_rendering: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        UiConfig {
            theme: ThemeMode::Dark,
            seed_color: "#7C5CFF".into(),
            view: ViewMode::Grid,
            reduce_motion: false,
            start_minimized: false,
            tray: true,
            close_to_tray: false,
            window_width: 1180,
            window_height: 760,
            watch_folders: Vec::new(),
            gpu_rendering: false,
        }
    }
}

/// Whole configuration file.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Audio.
    pub audio: AudioConfig,
    /// Hotkeys.
    pub hotkeys: HotkeyConfig,
    /// UI.
    pub ui: UiConfig,
    /// Recorder.
    pub recorder: RecorderConfig,
}

impl Config {
    /// Load from `path`; a missing file yields defaults, a broken file is an error.
    pub fn load(path: &Path) -> Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut cfg: Config = toml::from_str(&text).map_err(|e| Error::Parse {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;
                cfg.sanitize();
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Save atomically (write temp file, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        crate::paths::write_atomic(path, text.as_bytes())
    }

    /// Clamp out-of-range values a hand-edited file might contain.
    pub fn sanitize(&mut self) {
        let a = &mut self.audio;
        a.master_volume = a.master_volume.clamp(0.0, 1.0);
        a.monitor_volume = a.monitor_volume.clamp(0.0, 1.0);
        a.mic_volume = a.mic_volume.clamp(0.0, 2.0);
        a.buffer_frames = a.buffer_frames.clamp(64, 4096);
        a.target_lufs = a.target_lufs.clamp(-40.0, -5.0);
        self.recorder.max_seconds = self.recorder.max_seconds.min(3600);
        self.ui.window_width = self.ui.window_width.clamp(480, 7680);
        self.ui.window_height = self.ui.window_height.clamp(360, 4320);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_partial_files() {
        let dir = std::env::temp_dir().join(format!("clipdeck-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.audio.master_volume = 0.5;
        cfg.ui.theme = ThemeMode::Light;
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);

        std::fs::write(&path, "[audio]\nmaster_volume = 7.0\n").unwrap();
        let loaded = Config::load(&path).unwrap();
        assert_eq!(loaded.audio.master_volume, 1.0);
        assert_eq!(loaded.ui, UiConfig::default());

        std::fs::write(&path, "this is = = not toml").unwrap();
        assert!(Config::load(&path).is_err());
        assert_eq!(
            Config::load(&dir.join("missing.toml")).unwrap(),
            Config::default()
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
