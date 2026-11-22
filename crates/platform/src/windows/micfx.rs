//! In-microphone mode: Clipdeck's sounds are added to the real microphone by
//! `clipdeck_micfx.dll`, an audio effect Windows runs inside the microphone's capture pipeline.
//! No virtual cable and no device switch in Discord (this is how Soundpad works).
//!
//! * [`MicFxMic`] is the [`VirtualMic`] that feeds the effect through shared memory.
//! * [`install`] / [`uninstall`] register the effect on chosen microphones (administrator).
//! * [`endpoints`] and [`live_instances`] report what is installed and running.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use clipdeck_audio::mixer::MixerShared;
use clipdeck_audio::output::{RenderFn, VirtualMic};
use clipdeck_audio::resample::LinearResampler;
use clipdeck_micfx_proto as proto;
use proto::{StreamInfo, View};
use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, HLOCAL, LUID, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GRANT_ACCESS, GetNamedSecurityInfoW, NO_MULTIPLE_TRUSTEE, SE_REGISTRY_KEY,
    SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_GROUP, TRUSTEE_IS_SID, TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACL, AdjustTokenPrivileges, CONTAINER_INHERIT_ACE, CreateWellKnownSid,
    DACL_SECURITY_INFORMATION, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, SE_PRIVILEGE_ENABLED,
    TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY, WinBuiltinAdministratorsSid,
};
use windows_sys::Win32::Storage::FileSystem::{
    CopyFileW, MOVEFILE_DELAY_UNTIL_REBOOT, MoveFileExW,
};
use windows_sys::Win32::System::Memory::{
    FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile, OpenFileMappingW,
    UnmapViewOfFile,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_ALL_ACCESS, KEY_READ, KEY_WOW64_64KEY, REG_DWORD, REG_MULTI_SZ,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW,
    RegDeleteValueW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::System::Services::{
    CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
    SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START,
    SERVICE_STATUS, SERVICE_STOP, SERVICE_STOPPED, StartServiceW,
};
use windows_sys::Win32::System::SystemInformation::{GetSystemDirectoryW, GetTickCount64};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetExitCodeProcess, INFINITE, OpenProcessToken,
    SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL, WaitForSingleObject,
};
use windows_sys::Win32::UI::Shell::{
    IsUserAnAdmin, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
};

/// Rate the mixer runs at; each effect instance gets its own resampler.
const MIX_RATE: u32 = 48_000;
/// Stereo history of rendered audio shared by all live instances.
const HISTORY: u64 = 1 << 14;

const MMDEVICES_CAPTURE: &str =
    r"SOFTWARE\Microsoft\Windows\CurrentVersion\MMDevices\Audio\Capture";
const AUDIO_KEY: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\Audio";
const DISABLE_PROTECTED_AUDIO_DG: &str = "DisableProtectedAudioDG";
/// `PKEY_FX_EndpointEffectClsid`.
const FX_EFX: &str = "{d04e05a6-594b-4fb6-a80d-01af5eed7d1d},7";
/// `PKEY_EFX_ProcessingModes_Supported_For_Streaming`.
const FX_EFX_MODES: &str = "{d3993a3f-99c2-4402-b5ec-a92a0367664b},7";
/// `PKEY_AudioEndpoint_Disable_SysFx` ("Disable all enhancements").
const FX_DISABLE_SYSFX: &str = "{1da5d803-d492-4edd-8c23-e0c0ffee7f0e},5";
/// `AUDIO_SIGNALPROCESSINGMODE_DEFAULT`.
const MODE_DEFAULT: &str = "{C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}";
/// `PKEY_Device_DeviceDesc` ("Mikrofon") and `PKEY_DeviceInterface_FriendlyName` ("SYN Max Air").
const PROP_DESC: &str = "{a45c254e-df1c-4efd-8020-67d146a850e0},2";
const PROP_ADAPTER: &str = "{b3f8fa53-0004-438e-9003-51a46e139bfc},6";
/// File name of the effect next to `clipdeck.exe`.
pub const DLL_NAME: &str = "clipdeck_micfx.dll";

fn tick() -> u64 {
    // SAFETY: no arguments.
    unsafe { GetTickCount64() }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---------------------------------------------------------------------------------------------
// Registry helpers

struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: opened by us.
        unsafe { RegCloseKey(self.0) };
    }
}

impl Key {
    fn open(path: &str, write: bool) -> Option<Key> {
        let mut h: HKEY = std::ptr::null_mut();
        let sam = if write { KEY_ALL_ACCESS } else { KEY_READ } | KEY_WOW64_64KEY;
        // SAFETY: valid strings and out pointer.
        let r = unsafe { RegOpenKeyExW(HKEY_LOCAL_MACHINE, wide(path).as_ptr(), 0, sam, &mut h) };
        (r == ERROR_SUCCESS).then_some(Key(h))
    }

    fn create(path: &str) -> Result<Key, String> {
        let mut h: HKEY = std::ptr::null_mut();
        // SAFETY: valid strings and out pointer.
        let r = unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                wide(path).as_ptr(),
                0,
                std::ptr::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_ALL_ACCESS | KEY_WOW64_64KEY,
                std::ptr::null(),
                &mut h,
                std::ptr::null_mut(),
            )
        };
        if r == ERROR_SUCCESS {
            Ok(Key(h))
        } else {
            Err(format!("cannot open HKLM\\{path} for writing (error {r})"))
        }
    }

    fn get(&self, name: &str) -> Option<(REG_VALUE_TYPE, Vec<u8>)> {
        let n = wide(name);
        let mut ty: REG_VALUE_TYPE = 0;
        let mut len = 0u32;
        // SAFETY: size query.
        let r = unsafe {
            RegQueryValueExW(
                self.0,
                n.as_ptr(),
                std::ptr::null(),
                &mut ty,
                std::ptr::null_mut(),
                &mut len,
            )
        };
        if r != ERROR_SUCCESS {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: buffer of `len` bytes.
        let r = unsafe {
            RegQueryValueExW(
                self.0,
                n.as_ptr(),
                std::ptr::null(),
                &mut ty,
                buf.as_mut_ptr(),
                &mut len,
            )
        };
        buf.truncate(len as usize);
        (r == ERROR_SUCCESS).then_some((ty, buf))
    }

    fn get_string(&self, name: &str) -> Option<String> {
        let (ty, b) = self.get(name)?;
        if ty != REG_SZ && ty != REG_MULTI_SZ {
            return None;
        }
        let units: Vec<u16> = b
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        Some(String::from_utf16_lossy(&units))
    }

    fn get_dword(&self, name: &str) -> Option<u32> {
        match self.get(name)? {
            (REG_DWORD, b) if b.len() >= 4 => Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]])),
            _ => None,
        }
    }

    fn set(&self, name: &str, ty: REG_VALUE_TYPE, data: &[u8]) -> Result<(), String> {
        // SAFETY: valid name and data.
        let r = unsafe {
            RegSetValueExW(
                self.0,
                wide(name).as_ptr(),
                0,
                ty,
                data.as_ptr(),
                data.len() as u32,
            )
        };
        if r == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(format!("cannot write registry value {name} (error {r})"))
        }
    }

    fn set_string(&self, name: &str, value: &str) -> Result<(), String> {
        let w = wide(value);
        let bytes: Vec<u8> = w.iter().flat_map(|u| u.to_le_bytes()).collect();
        self.set(name, REG_SZ, &bytes)
    }

    fn set_multi(&self, name: &str, values: &[&str]) -> Result<(), String> {
        let mut w: Vec<u16> = Vec::new();
        for v in values {
            w.extend(v.encode_utf16());
            w.push(0);
        }
        w.push(0);
        let bytes: Vec<u8> = w.iter().flat_map(|u| u.to_le_bytes()).collect();
        self.set(name, REG_MULTI_SZ, &bytes)
    }

    fn set_dword(&self, name: &str, v: u32) -> Result<(), String> {
        self.set(name, REG_DWORD, &v.to_le_bytes())
    }

    fn delete(&self, name: &str) {
        // SAFETY: valid name.
        unsafe { RegDeleteValueW(self.0, wide(name).as_ptr()) };
    }

    fn subkeys(&self) -> Vec<String> {
        let mut out = Vec::new();
        for i in 0.. {
            let mut buf = [0u16; 256];
            let mut len = buf.len() as u32;
            // SAFETY: buffer and length describe `buf`.
            let r = unsafe {
                RegEnumKeyExW(
                    self.0,
                    i,
                    buf.as_mut_ptr(),
                    &mut len,
                    std::ptr::null(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&buf[..len as usize]));
        }
        out
    }
}

fn delete_tree(path: &str) {
    // SAFETY: valid string.
    unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, wide(path).as_ptr()) };
    if let Some((parent, leaf)) = path.rsplit_once('\\')
        && let Some(k) = Key::open(parent, true)
    {
        // RegDeleteTreeW leaves the key itself; remove it too.
        // SAFETY: valid strings.
        unsafe { windows_sys::Win32::System::Registry::RegDeleteKeyW(k.0, wide(leaf).as_ptr()) };
    }
}

// ---------------------------------------------------------------------------------------------
// Endpoints and status

/// A capture endpoint (microphone) as Windows lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct MicEndpoint {
    /// Endpoint GUID, e.g. `{A6825A6D-55EF-46BF-958D-8C922E5AE44E}`.
    pub guid: String,
    /// "Mikrofon (SYN Max Air)".
    pub name: String,
    /// Plugged in and enabled.
    pub active: bool,
    /// Clipdeck's effect is registered on it.
    pub installed: bool,
}

/// All microphones Windows knows, active ones first.
pub fn endpoints() -> Vec<MicEndpoint> {
    let Some(root) = Key::open(MMDEVICES_CAPTURE, false) else {
        return Vec::new();
    };
    let mut out: Vec<MicEndpoint> = root
        .subkeys()
        .into_iter()
        .filter_map(|guid| {
            let base = format!("{MMDEVICES_CAPTURE}\\{guid}");
            let state = Key::open(&base, false)?
                .get_dword("DeviceState")
                .unwrap_or(0);
            let props = Key::open(&format!("{base}\\Properties"), false)?;
            let desc = props
                .get_string(PROP_DESC)
                .unwrap_or_else(|| "Microphone".into());
            let name = match props.get_string(PROP_ADAPTER) {
                Some(a) if !a.is_empty() => format!("{desc} ({a})"),
                _ => desc,
            };
            let installed = Key::open(&format!("{base}\\FxProperties"), false)
                .and_then(|k| k.get_string(FX_EFX))
                .is_some_and(|c| c.eq_ignore_ascii_case(proto::CLSID));
            Some(MicEndpoint {
                guid,
                name,
                active: state == 1,
                installed,
            })
        })
        .filter(|e| e.active || e.installed)
        .collect();
    out.sort_by(|a, b| b.active.cmp(&a.active).then(a.name.cmp(&b.name)));
    out
}

/// Whether the effect is installed on at least one microphone.
pub fn is_installed() -> bool {
    Key::open(proto::REG_KEY, false).is_some_and(|k| !k.subkeys().is_empty())
}

/// A running effect instance (an app is recording from that microphone right now).
#[derive(Debug, Clone)]
pub struct LiveInstance {
    /// Format and device.
    pub info: StreamInfo,
    /// `STATUS_*` bits.
    pub status: u32,
    /// HRESULT of loading the device's original effect, when that failed.
    pub original_hr: u32,
    /// Blocks where Clipdeck delivered too little audio.
    pub underruns: u64,
}

struct Mapped {
    handle: HANDLE,
    base: MEMORY_MAPPED_VIEW_ADDRESS,
}

impl Mapped {
    fn open(slot: u32) -> Option<Mapped> {
        let name = wide(&proto::mapping_name(slot));
        // SAFETY: valid name.
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS, 0, name.as_ptr()) };
        if handle.is_null() {
            return None;
        }
        // SAFETY: valid mapping handle; the effect created it with MAPPING_BYTES.
        let base =
            unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, proto::MAPPING_BYTES) };
        if base.Value.is_null() {
            // SAFETY: we own the handle.
            unsafe { CloseHandle(handle) };
            return None;
        }
        Some(Mapped { handle, base })
    }

    fn view(&self) -> View<'_> {
        // SAFETY: MAPPING_BYTES are mapped (page aligned) while `self` lives.
        unsafe { View::from_raw(self.base.Value.cast()) }
    }
}

impl Drop for Mapped {
    fn drop(&mut self) {
        // SAFETY: mapped and opened by us.
        unsafe {
            UnmapViewOfFile(self.base);
            CloseHandle(self.handle);
        }
    }
}

/// Effect instances that are processing right now.
pub fn live_instances() -> Vec<LiveInstance> {
    let now = tick();
    (0..proto::SLOTS)
        .filter_map(|s| {
            let m = Mapped::open(s)?;
            let v = m.view();
            let (_, info) = v.stream(now)?;
            Some(LiveInstance {
                info,
                status: v.header.status.load(Ordering::Relaxed),
                original_hr: v.header.original_hr.load(Ordering::Relaxed),
                underruns: v.header.underruns.load(Ordering::Relaxed),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// The virtual mic

/// Feeds Clipdeck's mic mix into every running effect instance.
pub struct MicFxMic {
    shared: Option<Arc<MixerShared>>,
    quit: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MicFxMic {
    /// New, not started.
    pub fn new() -> Self {
        MicFxMic {
            shared: None,
            quit: Arc::new(AtomicBool::new(false)),
            failed: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

impl Default for MicFxMic {
    fn default() -> Self {
        Self::new()
    }
}

impl VirtualMic for MicFxMic {
    fn display_name(&self) -> String {
        "your microphone".into()
    }

    fn prepare(&mut self) -> Result<(u32, u16), String> {
        Ok((MIX_RATE, 2))
    }

    fn start(&mut self, render: RenderFn) -> Result<(), String> {
        self.stop();
        self.quit.store(false, Ordering::Release);
        let shared = self.shared.clone();
        let quit = self.quit.clone();
        let failed = self.failed.clone();
        let t = std::thread::Builder::new()
            .name("clipdeck-micfx".into())
            .spawn(move || {
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    feed(render, shared, &quit);
                }))
                .is_err()
                {
                    failed.store(true, Ordering::Release);
                }
            })
            .map_err(|e| e.to_string())?;
        self.thread = Some(t);
        Ok(())
    }

    fn stop(&mut self) {
        self.quit.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }

    fn carries_real_mic(&self) -> bool {
        true
    }

    fn attach(&mut self, shared: Arc<MixerShared>) {
        self.shared = Some(shared);
    }
}

impl Drop for MicFxMic {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Per-instance consumer of the shared render history.
struct Tap {
    owner: u64,
    rate: u32,
    channels: usize,
    pos: u64,
    resampler: LinearResampler,
    out: Vec<f32>,
}

fn feed(mut render: RenderFn, shared: Option<Arc<MixerShared>>, quit: &AtomicBool) {
    // SAFETY: plain thread-priority call on the current thread.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL) };
    let mut maps: Vec<Option<Mapped>> = (0..proto::SLOTS).map(|_| None).collect();
    let mut taps: Vec<Option<Tap>> = (0..proto::SLOTS).map(|_| None).collect();
    let mut history = vec![0f32; HISTORY as usize * 2];
    let mut head: u64 = 0;
    let mut scratch = vec![0f32; 4096 * 2];
    let mut last_scan = 0u64;
    let mut idle_clock = tick();

    while !quit.load(Ordering::Acquire) {
        let now = tick();
        if now.saturating_sub(last_scan) >= 250 {
            last_scan = now;
            for (s, m) in maps.iter_mut().enumerate() {
                if m.is_none() {
                    *m = Mapped::open(s as u32);
                }
            }
        }

        let (mic_gain, duck) = match &shared {
            Some(sh) => {
                let gain = f32::from_bits(sh.mic_gain.load(Ordering::Relaxed));
                let playing = sh
                    .slots
                    .iter()
                    .any(|s| s.voice.load(Ordering::Relaxed) != 0);
                (gain, sh.block_voice.load(Ordering::Relaxed) && playing)
            }
            None => (1.0, false),
        };

        // Discover instances and compute how much audio each one needs.
        let mut deficit: u64 = 0;
        let mut any_live = false;
        for (m, tap) in maps.iter().zip(taps.iter_mut()) {
            let Some(m) = m else {
                *tap = None;
                continue;
            };
            let v = m.view();
            let Some((owner, info)) = v.stream(now) else {
                *tap = None;
                continue;
            };
            any_live = true;
            if tap.as_ref().is_none_or(|t| t.owner != owner) {
                *tap = Some(Tap {
                    owner,
                    rate: info.rate.max(8000),
                    channels: info.channels.clamp(1, proto::MAX_CHANNELS) as usize,
                    pos: head,
                    resampler: LinearResampler::new(MIX_RATE, info.rate.max(8000)),
                    out: Vec::with_capacity(8192),
                });
            }
            let Some(t) = tap.as_mut() else { continue };
            v.client_controls(mic_gain, duck);
            v.client_heartbeat(now);
            let need = need_frames(&v, t.rate);
            let need48 = need * u64::from(MIX_RATE) / u64::from(t.rate) + 2;
            deficit = deficit.max(need48.saturating_sub(head - t.pos));
        }

        // Render: on demand while someone listens, in real time (discarded) otherwise, so
        // sounds started while no app records the microphone still end on time.
        let mut to_render = if any_live {
            idle_clock = now;
            deficit
        } else {
            let frames = now.saturating_sub(idle_clock) * u64::from(MIX_RATE) / 1000;
            idle_clock += frames * 1000 / u64::from(MIX_RATE);
            frames
        };
        while to_render > 0 {
            let n = to_render.min(4096) as usize;
            render(&mut scratch[..n * 2], 2);
            for f in scratch[..n * 2].as_chunks::<2>().0 {
                let i = (head % HISTORY) as usize * 2;
                history[i] = f[0];
                history[i + 1] = f[1];
                head += 1;
            }
            to_render -= n as u64;
        }

        // Deliver.
        for (m, tap) in maps.iter().zip(taps.iter_mut()) {
            let (Some(m), Some(t)) = (m, tap.as_mut()) else {
                continue;
            };
            let v = m.view();
            if head - t.pos > HISTORY - 4096 {
                t.pos = head.saturating_sub(1024); // fell far behind (clock drift, stall)
            }
            let need = need_frames(&v, t.rate) as usize;
            t.out.clear();
            let ch = t.channels;
            while t.out.len() / ch < need && t.pos < head {
                let i = (t.pos % HISTORY) as usize * 2;
                let frame = [history[i], history[i + 1]];
                let out = &mut t.out;
                t.resampler.process(&frame, |l, r| {
                    if ch == 1 {
                        out.push((l + r) * 0.5);
                    } else {
                        for c in 0..ch {
                            out.push(if c % 2 == 0 { l } else { r });
                        }
                    }
                });
                t.pos += 1;
            }
            if !t.out.is_empty() {
                v.client_write(&t.out, ch, now);
            }
        }

        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Frames to add so the instance's ring holds about two of its periods plus scheduling slack.
fn need_frames(v: &View<'_>, rate: u32) -> u64 {
    let period = match v.header.period.load(Ordering::Relaxed) {
        0 => rate / 100,
        p => p,
    };
    let target = u64::from((2 * period).max(rate / 50) + rate / 200);
    target.saturating_sub(v.client_fill())
}

// ---------------------------------------------------------------------------------------------
// Install / uninstall (administrator)

/// Whether this process runs elevated.
pub fn is_admin() -> bool {
    // SAFETY: no arguments.
    unsafe { IsUserAnAdmin() != 0 }
}

/// Run `clipdeck.exe <args>` elevated (UAC prompt) and wait for it. Returns its exit code.
pub fn run_elevated(args: &str) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let file = wide(&exe.to_string_lossy());
    let params = wide(args);
    let verb = wide("runas");
    // SAFETY: zero is a valid SHELLEXECUTEINFOW; fields set below.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 0; // SW_HIDE
    // SAFETY: `info` and the strings it points to live across the call.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err("The administrator prompt was cancelled.".into());
    }
    let mut code = 1u32;
    // SAFETY: process handle returned because of SEE_MASK_NOCLOSEPROCESS.
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code)
}

fn enable_privilege(name: &str) {
    // SAFETY: standard token adjustment on our own process.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return;
        }
        let mut luid = LUID::default();
        if LookupPrivilegeValueW(std::ptr::null(), wide(name).as_ptr(), &mut luid) != 0 {
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut());
        }
        CloseHandle(token);
    }
}

/// Make HKLM\`path` writable by Administrators. The audio endpoint keys belong to the audio
/// service; like Soundpad and Equalizer APO we take ownership and grant Administrators full
/// control.
fn take_ownership(path: &str) -> Result<(), String> {
    enable_privilege("SeTakeOwnershipPrivilege");
    enable_privilege("SeRestorePrivilege");
    let object = wide(&format!("MACHINE\\{path}"));
    let mut sid = [0u8; 68];
    let mut sid_len = sid.len() as u32;
    // SAFETY: buffer of SECURITY_MAX_SID_SIZE bytes.
    if unsafe {
        CreateWellKnownSid(
            WinBuiltinAdministratorsSid,
            std::ptr::null_mut(),
            sid.as_mut_ptr().cast(),
            &mut sid_len,
        )
    } == 0
    {
        return Err("cannot build the Administrators SID".into());
    }
    let psid = sid.as_mut_ptr().cast();
    // SAFETY: valid object name and SID; the ACL pointers come from the API and are freed with
    // LocalFree.
    unsafe {
        let r = SetNamedSecurityInfoW(
            object.as_ptr(),
            SE_REGISTRY_KEY,
            OWNER_SECURITY_INFORMATION,
            psid,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        );
        if r != ERROR_SUCCESS {
            return Err(format!("cannot take ownership of HKLM\\{path} (error {r})"));
        }
        let mut dacl: *mut ACL = std::ptr::null_mut();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let r = GetNamedSecurityInfoW(
            object.as_ptr(),
            SE_REGISTRY_KEY,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut dacl,
            std::ptr::null_mut(),
            &mut sd,
        );
        if r != ERROR_SUCCESS {
            return Err(format!(
                "cannot read permissions of HKLM\\{path} (error {r})"
            ));
        }
        let ea = EXPLICIT_ACCESS_W {
            grfAccessPermissions: KEY_ALL_ACCESS,
            grfAccessMode: GRANT_ACCESS,
            grfInheritance: CONTAINER_INHERIT_ACE,
            Trustee: TRUSTEE_W {
                pMultipleTrustee: std::ptr::null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_GROUP,
                ptstrName: psid.cast(),
            },
        };
        let mut new_dacl: *mut ACL = std::ptr::null_mut();
        let r = SetEntriesInAclW(1, &ea, dacl, &mut new_dacl);
        LocalFree(sd as HLOCAL);
        if r != ERROR_SUCCESS {
            return Err(format!(
                "cannot extend permissions of HKLM\\{path} (error {r})"
            ));
        }
        let r = SetNamedSecurityInfoW(
            object.as_ptr(),
            SE_REGISTRY_KEY,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            new_dacl,
            std::ptr::null(),
        );
        LocalFree(new_dacl as HLOCAL);
        if r != ERROR_SUCCESS {
            return Err(format!(
                "cannot change permissions of HKLM\\{path} (error {r})"
            ));
        }
    }
    Ok(())
}

/// Open HKLM\`path` for writing, creating it and taking ownership as needed.
fn writable(path: &str) -> Result<Key, String> {
    if let Some(k) = Key::open(path, true) {
        return Ok(k);
    }
    if Key::open(path, false).is_some() {
        take_ownership(path)?;
        return Key::open(path, true).ok_or_else(|| format!("HKLM\\{path} stays read-only"));
    }
    // Missing: make the parent writable and create it.
    if let Some((parent, _)) = path.rsplit_once('\\')
        && Key::open(parent, true).is_none()
    {
        take_ownership(parent)?;
    }
    Key::create(path)
}

/// Save `name` of `src` into `backup` once (value or "absent" marker), so uninstall restores it.
fn backup_value(src: &Key, backup: &Key, name: &str) -> Result<(), String> {
    let saved = format!("Saved:{name}");
    let absent = format!("Absent:{name}");
    if backup.get(&saved).is_some() || backup.get(&absent).is_some() {
        return Ok(());
    }
    match src.get(name) {
        Some((ty, data)) => backup.set(&saved, ty, &data),
        None => backup.set_dword(&absent, 1),
    }
}

fn restore_value(dst: &Key, backup: &Key, name: &str) -> Result<(), String> {
    if let Some((ty, data)) = backup.get(&format!("Saved:{name}")) {
        dst.set(name, ty, &data)
    } else {
        dst.delete(name);
        Ok(())
    }
}

fn fnv(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0x811c_9dc5u32, |h, b| {
        (h ^ u32::from(*b)).wrapping_mul(0x0100_0193)
    })
}

fn system_dir() -> PathBuf {
    let mut buf = [0u16; 260];
    // SAFETY: buffer and length describe `buf`.
    let n = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..n.min(buf.len())]))
}

/// The effect DLL shipped next to `clipdeck.exe`.
pub fn bundled_dll() -> Option<PathBuf> {
    let p = std::env::current_exe().ok()?.parent()?.join(DLL_NAME);
    p.exists().then_some(p)
}

fn delete_file_later(p: &Path) {
    if std::fs::remove_file(p).is_err() {
        let w = wide(&p.to_string_lossy());
        // SAFETY: valid path; schedules deletion at the next boot.
        unsafe { MoveFileExW(w.as_ptr(), std::ptr::null(), MOVEFILE_DELAY_UNTIL_REBOOT) };
    }
}

fn clsid_key() -> String {
    format!(r"SOFTWARE\Classes\CLSID\{}", proto::CLSID)
}

fn apo_key() -> String {
    format!(
        r"SOFTWARE\Classes\AudioEngine\AudioProcessingObjects\{}",
        proto::CLSID
    )
}

/// Register the effect DLL (copied into System32 under a content-unique name, because the audio
/// engine keeps the old one loaded).
fn register_dll(src: &Path) -> Result<(), String> {
    let bytes = std::fs::read(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let dest = system_dir().join(format!(
        "clipdeck_micfx-{}-{:08x}.dll",
        env!("CARGO_PKG_VERSION"),
        fnv(&bytes)
    ));
    if !dest.exists() {
        let (s, d) = (wide(&src.to_string_lossy()), wide(&dest.to_string_lossy()));
        // SAFETY: valid paths.
        if unsafe { CopyFileW(s.as_ptr(), d.as_ptr(), 0) } == 0 {
            return Err(format!(
                "cannot copy the effect to {}: {}",
                dest.display(),
                std::io::Error::last_os_error()
            ));
        }
    }
    let inproc = Key::create(&format!(r"{}\InprocServer32", clsid_key()))?;
    let previous = inproc.get_string("");
    inproc.set_string("", &dest.to_string_lossy())?;
    inproc.set_string("ThreadingModel", "Both")?;
    Key::create(&clsid_key())?.set_string("", "Clipdeck Mic FX")?;
    if let Some(prev) = previous
        && !prev.eq_ignore_ascii_case(&dest.to_string_lossy())
    {
        delete_file_later(Path::new(&prev));
    }

    // Same registration Soundpad's UniteFx uses (re/samples/registry).
    let apo = Key::create(&apo_key())?;
    apo.set_string("FriendlyName", "Clipdeck Mic FX")?;
    apo.set_string("Copyright", "GPL-3.0-or-later, Clipdeck contributors")?;
    apo.set_dword("MajorVersion", 1)?;
    apo.set_dword("MinorVersion", 0)?;
    apo.set_dword("Flags", 0x0e)?; // APO_FLAG_DEFAULT
    apo.set_dword("MinInputConnections", 1)?;
    apo.set_dword("MaxInputConnections", 1)?;
    apo.set_dword("MinOutputConnections", 1)?;
    apo.set_dword("MaxOutputConnections", 1)?;
    apo.set_dword("MaxInstances", u32::MAX)?;
    apo.set_dword("NumAPOInterfaces", 1)?;
    apo.set_string("APOInterface0", "{FD7F2B29-24D0-4B5C-B177-592C39F9CA10}")?;
    Ok(())
}

fn install_endpoint(guid: &str) -> Result<(), String> {
    let fx = writable(&format!(r"{MMDEVICES_CAPTURE}\{guid}\FxProperties"))?;
    let backup = Key::create(&format!(r"{}\{guid}", proto::REG_KEY))?;
    for v in [FX_EFX, FX_EFX_MODES, FX_DISABLE_SYSFX] {
        backup_value(&fx, &backup, v)?;
    }
    // Chain to the device's own effect, if it had one.
    match fx.get_string(FX_EFX) {
        Some(c) if !c.eq_ignore_ascii_case(proto::CLSID) && !c.is_empty() => {
            backup.set_string(proto::REG_ORIGINAL_EFX, &c)?;
        }
        Some(_) => {}
        None => backup.delete(proto::REG_ORIGINAL_EFX),
    }
    fx.set_string(FX_EFX, proto::CLSID)?;
    if fx.get(FX_EFX_MODES).is_none() {
        fx.set_multi(FX_EFX_MODES, &[MODE_DEFAULT])?;
    }
    // "Disable all enhancements" would skip every effect, ours included.
    if fx.get_dword(FX_DISABLE_SYSFX).unwrap_or(0) != 0 {
        fx.set_dword(FX_DISABLE_SYSFX, 0)?;
    }
    Ok(())
}

fn uninstall_endpoint(guid: &str) -> Result<(), String> {
    let backup_path = format!(r"{}\{guid}", proto::REG_KEY);
    if let Some(backup) = Key::open(&backup_path, false)
        && let Ok(fx) = writable(&format!(r"{MMDEVICES_CAPTURE}\{guid}\FxProperties"))
    {
        for v in [FX_EFX, FX_EFX_MODES, FX_DISABLE_SYSFX] {
            restore_value(&fx, &backup, v)?;
        }
    }
    delete_tree(&backup_path);
    Ok(())
}

/// Restart the Windows Audio service so audiodg reloads endpoint effects.
pub fn restart_audio_service() -> Result<(), String> {
    // SAFETY: service-control calls with handles we open and close here.
    unsafe {
        let scm = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
        if scm.is_null() {
            return Err("cannot open the service manager".into());
        }
        let svc = OpenServiceW(
            scm,
            wide("Audiosrv").as_ptr(),
            SERVICE_STOP | SERVICE_START | SERVICE_QUERY_STATUS,
        );
        if svc.is_null() {
            CloseServiceHandle(scm);
            return Err("cannot open the Windows Audio service".into());
        }
        let mut st: SERVICE_STATUS = std::mem::zeroed();
        let wait_for = |want: u32, st: &mut SERVICE_STATUS| {
            for _ in 0..100 {
                if QueryServiceStatus(svc, st) != 0 && st.dwCurrentState == want {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            false
        };
        ControlService(svc, SERVICE_CONTROL_STOP, &mut st);
        wait_for(SERVICE_STOPPED, &mut st);
        let started = StartServiceW(svc, 0, std::ptr::null()) != 0;
        let running = wait_for(SERVICE_RUNNING, &mut st);
        CloseServiceHandle(svc);
        CloseServiceHandle(scm);
        if started || running {
            Ok(())
        } else {
            Err("the Windows Audio service did not restart; restart Windows".into())
        }
    }
}

/// Install the effect on exactly these microphones (endpoint GUIDs) and remove it from all
/// others. An empty list uninstalls. Needs administrator rights; restarts Windows Audio.
pub fn install(guids: &[String], dll: &Path) -> Result<(), String> {
    if !is_admin() {
        return Err("administrator rights are needed".into());
    }
    let previous = Key::open(proto::REG_KEY, false)
        .map(|k| k.subkeys())
        .unwrap_or_default();
    if guids.is_empty() {
        return uninstall();
    }
    register_dll(dll)?;
    let root = Key::create(proto::REG_KEY)?;
    // Windows only loads unsigned endpoint effects with the audio engine's DRM check off; the
    // same switch Soundpad flips ("Disable Windows Audio DRM Validation").
    let audio = writable(AUDIO_KEY)?;
    backup_value(&audio, &root, DISABLE_PROTECTED_AUDIO_DG)?;
    audio.set_dword(DISABLE_PROTECTED_AUDIO_DG, 1)?;
    for g in guids {
        install_endpoint(g)?;
    }
    for g in previous
        .iter()
        .filter(|p| !guids.iter().any(|g| g.eq_ignore_ascii_case(p)))
    {
        uninstall_endpoint(g)?;
    }
    restart_audio_service()
}

/// Remove the effect from every microphone and undo all registry changes.
pub fn uninstall() -> Result<(), String> {
    if !is_admin() {
        return Err("administrator rights are needed".into());
    }
    if let Some(root) = Key::open(proto::REG_KEY, false) {
        for g in root.subkeys() {
            uninstall_endpoint(&g)?;
        }
        if let Ok(audio) = writable(AUDIO_KEY) {
            restore_value(&audio, &root, DISABLE_PROTECTED_AUDIO_DG)?;
        }
    }
    let dll = Key::open(&format!(r"{}\InprocServer32", clsid_key()), false)
        .and_then(|k| k.get_string(""));
    delete_tree(&apo_key());
    delete_tree(&clsid_key());
    delete_tree(proto::REG_KEY);
    let restarted = restart_audio_service();
    if let Some(p) = dll {
        delete_file_later(Path::new(&p));
    }
    restarted
}
