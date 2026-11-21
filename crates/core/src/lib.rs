//! Clipdeck core: library model, configuration, search, Soundpad `.spl` interop and the
//! remote-control protocol shared by the app and the CLI.

pub mod config;
pub mod date;
pub mod error;
pub mod hotkey;
pub mod library;
pub mod model;
pub mod paths;
pub mod protocol;
pub mod search;
pub mod spl;

pub use config::Config;
pub use error::{Error, Result};
pub use library::Library;
pub use model::{Category, CategoryId, PlaybackMode, Route, Sound, SoundId, Trim};
pub use paths::Paths;
