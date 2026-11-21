//! Error type for the core crate.

use std::path::{Path, PathBuf};

/// Core result alias.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong in the model layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// File system failure.
    #[error("{path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A file could not be parsed.
    #[error("{path}: {message}")]
    Parse {
        /// Path involved.
        path: PathBuf,
        /// What was wrong.
        message: String,
    },
    /// A sound list is not valid Soundpad XML.
    #[error("not a Soundpad sound list: {0}")]
    Spl(String),
    /// No platform directories (no home directory).
    #[error("could not determine the config/data directories")]
    NoDirs,
}

impl Error {
    /// Wrap an I/O error with its path.
    pub fn io(path: &Path, source: std::io::Error) -> Self {
        Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }

    /// One-line text for a snackbar.
    pub fn user_message(&self) -> String {
        match self {
            Error::Io { path, source } => format!("Could not access {}: {source}", path.display()),
            Error::Parse { path, .. } => format!("{} is damaged; using defaults", path.display()),
            Error::Spl(m) => format!("Not a valid Soundpad list ({m})"),
            Error::NoDirs => "No home directory found".into(),
        }
    }
}
