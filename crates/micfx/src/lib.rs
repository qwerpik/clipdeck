//! `clipdeck_micfx.dll`: an audio processing object (APO) that Windows loads into the real
//! microphone's capture pipeline (`audiodg.exe`). It adds the sounds Clipdeck plays to the
//! microphone signal, so Discord, games and every other app hear them without a virtual cable.
//! This is the technique Soundpad uses (`UniteFx.dll`, see `re/findings.md` Q5), written from
//! scratch.
//!
//! * Registered as an endpoint effect (EFX) on the chosen capture endpoints by
//!   `clipdeck micfx install`.
//! * If the endpoint already had an effect (vendor noise suppression etc.), that effect is
//!   created and run first, so nothing the user had is lost.
//! * Audio arrives through the lock-free shared-memory ring in `clipdeck-micfx-proto`.
//!
//! On non-Windows targets the crate is empty.

#[cfg(windows)]
mod apo;
