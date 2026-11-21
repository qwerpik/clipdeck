//! Platform integration for Clipdeck: virtual microphone backends, global hotkeys,
//! the tray icon and single-instance IPC.

pub mod hotkeys;
pub mod ipc;

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(windows)]
pub mod windows;

use clipdeck_audio::output::VirtualMic;

/// Reverse-DNS application id (Wayland app id, .desktop file name, portal registration).
pub const APP_ID: &str = "io.github.qwerpik.clipdeck";

/// The platform's virtual microphone, if one is available.
///
/// * Linux: a native PipeWire source called "Clipdeck Mic".
/// * Windows: VB-CABLE's `CABLE Input` (pick `CABLE Output` as the mic in other apps), or the
///   output device named in `configured`.
pub fn virtual_mic(configured: Option<&str>, buffer_frames: u32) -> Option<Box<dyn VirtualMic>> {
    #[cfg(target_os = "linux")]
    {
        if let Some(name) = configured {
            return Some(Box::new(clipdeck_audio::output::DeviceMic::new(
                name.to_string(),
                buffer_frames,
            )));
        }
        Some(Box::new(linux::pwmic::PipeWireMic::new(buffer_frames)))
    }
    #[cfg(windows)]
    {
        windows::virtual_mic(configured, buffer_frames)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (configured, buffer_frames);
        None
    }
}

/// What to select as the microphone in Discord on this platform.
pub fn mic_hint() -> &'static str {
    if cfg!(windows) {
        "CABLE Output (VB-Audio Virtual Cable)"
    } else {
        "Clipdeck Mic"
    }
}
