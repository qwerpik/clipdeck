//! Clipdeck audio: decoding, clip cache, loudness, the lock-free mixer and device streams.

pub mod clip;
pub mod decode;
pub mod engine;
pub mod error;
pub mod loudness;
pub mod mixer;
pub mod output;
pub mod record;
pub mod resample;
pub mod waveform;

pub use engine::{Engine, EngineConfig, EngineEvent, PlayRequest};
pub use error::{AudioError, Result};
