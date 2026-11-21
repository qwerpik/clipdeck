//! Windows backends.

use clipdeck_audio::output::{DeviceMic, VirtualMic, find_output_containing};

/// Name fragment of VB-CABLE's playback endpoint.
pub const CABLE_INPUT: &str = "CABLE Input";
/// What users pick as their microphone in Discord.
pub const CABLE_OUTPUT: &str = "CABLE Output";
/// Download page.
pub const VB_CABLE_URL: &str = "https://vb-audio.com/Cable/";

/// The configured cable device, or the auto-detected VB-CABLE.
pub fn cable_device(configured: Option<&str>) -> Option<String> {
    configured
        .map(str::to_string)
        .or_else(|| find_output_containing(CABLE_INPUT))
}

/// Virtual mic writing into VB-CABLE (or the configured device).
pub fn virtual_mic(configured: Option<&str>, buffer_frames: u32) -> Option<Box<dyn VirtualMic>> {
    cable_device(configured)
        .map(|d| Box::new(DeviceMic::new(d, buffer_frames)) as Box<dyn VirtualMic>)
}

/// Attach to the parent console so CLI output shows when the GUI-subsystem exe is run from a
/// terminal. Returns true when attached.
pub fn attach_parent_console() -> bool {
    // SAFETY: plain Win32 call without pointers.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        ) != 0
    }
}
