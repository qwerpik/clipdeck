//! Audio errors.

use std::path::PathBuf;

/// Audio result alias.
pub type Result<T, E = AudioError> = std::result::Result<T, E>;

/// Audio failures, with user-facing wording in [`AudioError::user_message`].
#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    /// The file could not be opened.
    #[error("cannot open {path}: {source}")]
    Open {
        /// File.
        path: PathBuf,
        /// Cause.
        source: std::io::Error,
    },
    /// The container/codec is not supported or the data is corrupt.
    #[error("cannot decode {path}: {message}")]
    Decode {
        /// File.
        path: PathBuf,
        /// Cause.
        message: String,
    },
    /// Resampler failure.
    #[error("resampling failed: {0}")]
    Resample(String),
    /// No audio device / stream failure.
    #[error("audio device error: {0}")]
    Device(String),
    /// The engine thread is gone.
    #[error("audio engine is not running")]
    EngineGone,
}

impl AudioError {
    /// Short text for a snackbar.
    pub fn user_message(&self) -> String {
        match self {
            AudioError::Open { path, .. } => format!("File not found: {}", path.display()),
            AudioError::Decode { path, .. } => {
                format!("Unsupported or damaged audio file: {}", path.display())
            }
            AudioError::Resample(_) => "Could not convert the sample rate".into(),
            AudioError::Device(m) => format!("Audio device problem: {m}"),
            AudioError::EngineGone => "Audio engine stopped; restart Clipdeck".into(),
        }
    }
}
