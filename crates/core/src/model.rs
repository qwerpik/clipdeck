//! The sound library model.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Stable identifier of a sound; survives renames and reordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SoundId(pub u64);

impl std::fmt::Display for SoundId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Stable identifier of a category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CategoryId(pub u64);

/// What happens to other voices when a sound starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMode {
    /// Let everything play; each trigger adds a voice.
    Overlap,
    /// Cut this sound's previous voice, then start again (Soundpad's default feel).
    #[default]
    Restart,
    /// Silence every other voice first.
    StopOthers,
}

impl PlaybackMode {
    /// All modes in UI order.
    pub const ALL: [PlaybackMode; 3] = [
        PlaybackMode::Overlap,
        PlaybackMode::Restart,
        PlaybackMode::StopOthers,
    ];

    /// Short human label.
    pub fn label(self) -> &'static str {
        match self {
            PlaybackMode::Overlap => "Overlap",
            PlaybackMode::Restart => "Restart",
            PlaybackMode::StopOthers => "Stop others",
        }
    }
}

/// Where a trigger is routed (Soundpad's "play mode").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    /// Microphone and, if monitoring is on, your headphones.
    #[default]
    Both,
    /// Only your headphones ("play only to me").
    MonitorOnly,
    /// Only the virtual microphone.
    MicOnly,
}

impl Route {
    /// Whether this route feeds the virtual microphone.
    pub fn to_mic(self) -> bool {
        matches!(self, Route::Both | Route::MicOnly)
    }
    /// Whether this route feeds the monitor output.
    pub fn to_monitor(self) -> bool {
        matches!(self, Route::Both | Route::MonitorOnly)
    }
}

/// Inclusive-exclusive playback window inside the file, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Trim {
    /// Start offset.
    pub start_ms: u32,
    /// End offset; `None` plays to the end.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<u32>,
}

impl Trim {
    /// True when the trim does nothing.
    pub fn is_identity(&self) -> bool {
        self.start_ms == 0 && self.end_ms.is_none()
    }
}

/// A triggerable clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sound {
    /// Stable id.
    pub id: SoundId,
    /// Display name (Soundpad's "tag").
    pub name: String,
    /// Absolute path to the audio file.
    pub path: PathBuf,
    /// Categories this sound belongs to, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub categories: Vec<CategoryId>,
    /// Linear gain 0.0..=2.0 on top of the master volume.
    #[serde(default = "unit_gain")]
    pub volume: f32,
    /// Global hotkey in `Ctrl+Alt+K` style.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    /// Favorite flag.
    #[serde(default, skip_serializing_if = "is_false")]
    pub favorite: bool,
    /// Loop until stopped.
    #[serde(default, skip_serializing_if = "is_false")]
    pub looped: bool,
    /// Per-sound playback mode; `None` uses the global default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<PlaybackMode>,
    /// Playback window.
    #[serde(default, skip_serializing_if = "Trim::is_identity")]
    pub trim: Trim,
    /// Card color as `RRGGBB` (imported from Soundpad's `color`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Artist metadata (kept for `.spl` round-trips).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub artist: String,
    /// Title metadata (kept for `.spl` round-trips).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// Duration in milliseconds, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
    /// Measured integrated loudness in LUFS, once analyzed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loudness_lufs: Option<f32>,
    /// Date added, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub added_on: String,
    /// Date last played, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_played_on: String,
    /// Times played.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub play_count: u32,
    /// Soundpad file identity (`serial-high-low` or the path), kept for round-trips.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_hash: String,
    /// Soundpad Alt+Numcode hotkey, kept for round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_code: Option<u32>,
}

fn unit_gain() -> f32 {
    1.0
}
fn is_false(b: &bool) -> bool {
    !*b
}
fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Sound {
    /// Build a sound from a path, deriving the name from the file stem.
    pub fn from_path(id: SoundId, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let name = display_name(&path);
        Sound {
            id,
            name,
            path,
            categories: Vec::new(),
            volume: 1.0,
            hotkey: None,
            favorite: false,
            looped: false,
            mode: None,
            trim: Trim::default(),
            color: None,
            artist: String::new(),
            title: String::new(),
            duration_ms: None,
            loudness_lufs: None,
            added_on: crate::date::today(),
            last_played_on: String::new(),
            play_count: 0,
            file_hash: String::new(),
            num_code: None,
        }
    }

    /// Whether the file exists right now.
    pub fn file_exists(&self) -> bool {
        self.path.is_file()
    }
}

/// File stem as a display name (`air horn.mp3` → `air horn`).
pub fn display_name(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// A user category; categories nest through `parent`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Category {
    /// Stable id.
    pub id: CategoryId,
    /// Display name.
    pub name: String,
    /// Parent category for nesting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CategoryId>,
    /// Color `RRGGBB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Hidden from the navigation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub hidden: bool,
    /// Hotkey that plays a random sound from this category.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    /// Soundpad icon reference, kept for round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Soundpad category index (`index` attribute), kept for round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<u32>,
}

/// Supported audio file extensions (lower case).
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "flac", "ogg", "oga", "m4a", "aac", "mp4", "mka", "webm", "alac", "aif", "aiff",
];

/// Whether a path has an audio extension we can decode.
pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_uses_stem() {
        assert_eq!(display_name(Path::new("/a/air horn.mp3")), "air horn");
    }

    #[test]
    fn audio_extension_check_is_case_insensitive() {
        assert!(is_audio_file(Path::new("x.MP3")));
        assert!(!is_audio_file(Path::new("x.txt")));
    }

    #[test]
    fn route_flags() {
        assert!(Route::Both.to_mic() && Route::Both.to_monitor());
        assert!(!Route::MonitorOnly.to_mic());
        assert!(!Route::MicOnly.to_monitor());
    }
}
