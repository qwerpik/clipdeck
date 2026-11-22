//! The COM class and its APO interfaces.
#![allow(non_snake_case)] // COM method names

use std::cell::UnsafeCell;
use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use clipdeck_micfx_proto as proto;
use proto::{EffectState, StreamInfo, View};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, CloseHandle, E_INVALIDARG, E_POINTER, HANDLE,
    HLOCAL, INVALID_HANDLE_VALUE, LocalFree, PROPERTYKEY, S_FALSE,
};
use windows::Win32::Media::Audio::Apo::{
    APO_CONNECTION_DESCRIPTOR, APO_CONNECTION_PROPERTY, APO_FLAG_DEFAULT, APO_REG_PROPERTIES,
    APOERR_FORMAT_NOT_SUPPORTED, APOERR_NUM_CONNECTIONS_INVALID, APOInitBaseStruct,
    APOInitSystemEffects2, APOInitSystemEffects3, BUFFER_SILENT, BUFFER_VALID, IAudioMediaType,
    IAudioProcessingObject, IAudioProcessingObject_Impl, IAudioProcessingObjectConfiguration,
    IAudioProcessingObjectConfiguration_Impl, IAudioProcessingObjectRT,
    IAudioProcessingObjectRT_Impl, IAudioSystemEffects, IAudioSystemEffects_Impl,
    UNCOMPRESSEDAUDIOFORMAT,
};
use windows::Win32::Media::Audio::PKEY_AudioEndpoint_GUID;
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CLSIDFromString, CoCreateInstance, CoTaskMemAlloc, CoTaskMemFree,
    IClassFactory, IClassFactory_Impl,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    PAGE_READWRITE, UnmapViewOfFile,
};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::core::{BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR, Ref, Result, w};
use windows_core::implement;

/// Our class id.
pub const CLSID_MICFX: GUID = GUID::from_u128(proto::CLSID_U128);

/// `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT`.
const SUBTYPE_FLOAT: GUID = GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71);

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn tick() -> u64 {
    // SAFETY: no arguments.
    unsafe { GetTickCount64() }
}

fn debug(msg: &str) {
    let w = HSTRING::from(format!("clipdeck_micfx: {msg}\n"));
    // SAFETY: valid NUL-terminated string.
    unsafe { windows::Win32::System::Diagnostics::Debug::OutputDebugStringW(&w) };
}

/// The endpoint's original effect, run before ours.
struct Original {
    apo: IAudioProcessingObject,
    rt: IAudioProcessingObjectRT,
    cfg: IAudioProcessingObjectConfiguration,
}

/// One mapped shared-memory slot.
struct Slot {
    handle: HANDLE,
    base: MEMORY_MAPPED_VIEW_ADDRESS,
    token: u64,
}

impl Slot {
    fn view(&self) -> View<'_> {
        // SAFETY: `base` maps MAPPING_BYTES (page aligned) for as long as `self` lives.
        unsafe { View::from_raw(self.base.Value.cast()) }
    }

    /// Create or open slot `n` and try to claim it.
    fn claim(n: u32, info: &StreamInfo) -> Option<Slot> {
        // Everyone (the user's Clipdeck runs at medium integrity, we run as LocalService) may
        // read and write; the low mandatory label keeps integrity checks out of the way.
        let mut psd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: valid SDDL string and out pointer.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                w!("D:(A;;GA;;;WD)(A;;GA;;;SY)(A;;GA;;;LS)S:(ML;;NW;;;LW)"),
                SDDL_REVISION_1,
                &mut psd,
                None,
            )
            .ok()?;
        }
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: psd.0,
            bInheritHandle: false.into(),
        };
        let name = HSTRING::from(proto::mapping_name(n));
        // SAFETY: page-file backed mapping with a valid name and security attributes.
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                Some(&sa),
                PAGE_READWRITE,
                0,
                proto::MAPPING_BYTES as u32,
                &name,
            )
        };
        // SAFETY: allocated by ConvertStringSecurityDescriptorToSecurityDescriptorW.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(psd.0)));
        }
        let handle = match handle {
            Ok(h) => h,
            Err(e) => {
                debug(&format!("CreateFileMapping {n}: {e}"));
                return None;
            }
        };
        // SAFETY: valid mapping handle.
        let base =
            unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, proto::MAPPING_BYTES) };
        if base.Value.is_null() {
            // SAFETY: we own the handle.
            unsafe {
                let _ = CloseHandle(handle);
            }
            return None;
        }
        let token = (u64::from(std::process::id()) << 32)
            | (NEXT_TOKEN.fetch_add(1, Ordering::Relaxed) & 0xffff_ffff);
        let slot = Slot {
            handle,
            base,
            token,
        };
        // SAFETY: no arguments.
        let pid = unsafe { GetCurrentProcessId() };
        if slot.view().effect_claim(token, info, pid, tick()) {
            Some(slot)
        } else {
            None
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.view().effect_release(self.token);
        // SAFETY: mapped and opened by us.
        unsafe {
            let _ = UnmapViewOfFile(self.base);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Configuration-time state (Initialize, format negotiation, lock/unlock).
#[derive(Default)]
struct Config {
    original: Option<Original>,
    name: String,
    endpoint: String,
    status: u32,
    original_hr: u32,
}

/// Processing state. Touched by `APOProcess` and by Lock/Unlock/Reset, which the audio engine
/// never runs concurrently with `APOProcess`.
#[derive(Default)]
struct Rt {
    locked: bool,
    channels: usize,
    rate: u32,
    state: Option<EffectState>,
    slot: Option<Slot>,
    original: Option<IAudioProcessingObjectRT>,
}

#[implement(
    IAudioProcessingObject,
    IAudioProcessingObjectRT,
    IAudioProcessingObjectConfiguration,
    IAudioSystemEffects
)]
struct MicFx {
    cfg: Mutex<Config>,
    rt: UnsafeCell<Rt>,
}

impl MicFx {
    fn new() -> Self {
        MicFx {
            cfg: Mutex::new(Config::default()),
            rt: UnsafeCell::new(Rt::default()),
        }
    }

    fn cfg(&self) -> std::sync::MutexGuard<'_, Config> {
        self.cfg
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// # Safety
    /// Only from APO calls the engine serializes with `APOProcess`.
    #[allow(clippy::mut_from_ref)]
    unsafe fn rt(&self) -> &mut Rt {
        // SAFETY: see the function contract.
        unsafe { &mut *self.rt.get() }
    }
}

fn format_of(t: &IAudioMediaType) -> Result<UNCOMPRESSEDAUDIOFORMAT> {
    let mut f = UNCOMPRESSEDAUDIOFORMAT::default();
    // SAFETY: valid out pointer.
    unsafe { t.GetUncompressedAudioFormat(&mut f)? };
    Ok(f)
}

fn check_format(requested: &IAudioMediaType, opposite: Option<&IAudioMediaType>) -> Result<()> {
    let f = format_of(requested)?;
    let ok = f.guidFormatType == SUBTYPE_FLOAT
        && f.dwBytesPerSampleContainer == 4
        && (1..=proto::MAX_CHANNELS).contains(&f.dwSamplesPerFrame)
        && f.fFramesPerSecond >= 8000.0;
    if !ok {
        return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
    }
    if let Some(o) = opposite {
        let g = format_of(o)?;
        if g.dwSamplesPerFrame != f.dwSamplesPerFrame
            || (g.fFramesPerSecond - f.fFramesPerSecond).abs() > 0.5
            || g.guidFormatType != f.guidFormatType
        {
            return Err(APOERR_FORMAT_NOT_SUPPORTED.into());
        }
    }
    Ok(())
}

fn prop_string(store: &IPropertyStore, key: &PROPERTYKEY) -> Option<String> {
    // SAFETY: valid key; the returned string is freed with CoTaskMemFree.
    unsafe {
        let pv = store.GetValue(key).ok()?;
        let s = PropVariantToStringAlloc(&pv).ok()?;
        let out = s.to_string().ok();
        CoTaskMemFree(Some(s.0 as *const c_void));
        out
    }
}

fn original_clsid(endpoint: &str) -> Option<GUID> {
    if endpoint.is_empty() {
        return None;
    }
    let key = HSTRING::from(format!("{}\\{endpoint}", proto::REG_KEY));
    let value = HSTRING::from(proto::REG_ORIGINAL_EFX);
    let mut buf = [0u16; 64];
    let mut len = std::mem::size_of_val(&buf) as u32;
    // SAFETY: buffer and length describe `buf`.
    let err = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            &key,
            &value,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if err.is_err() {
        return None;
    }
    // SAFETY: RegGetValueW NUL-terminates REG_SZ data.
    let clsid = unsafe { CLSIDFromString(PCWSTR(buf.as_ptr())) }.ok()?;
    (clsid != CLSID_MICFX).then_some(clsid)
}

impl IAudioProcessingObject_Impl for MicFx_Impl {
    fn Reset(&self) -> Result<()> {
        if let Some(o) = &self.cfg().original {
            // SAFETY: plain COM call.
            unsafe { o.apo.Reset()? };
        }
        // SAFETY: the engine does not call Reset during APOProcess.
        let rt = unsafe { self.rt() };
        if rt.rate > 0 {
            rt.state = Some(EffectState::new(rt.rate));
        }
        Ok(())
    }

    fn GetLatency(&self) -> Result<i64> {
        match &self.cfg().original {
            // SAFETY: plain COM call.
            Some(o) => unsafe { o.apo.GetLatency() },
            None => Ok(0),
        }
    }

    fn GetRegistrationProperties(&self) -> Result<*mut APO_REG_PROPERTIES> {
        let mut p = APO_REG_PROPERTIES {
            clsid: CLSID_MICFX,
            Flags: APO_FLAG_DEFAULT,
            u32MajorVersion: 1,
            u32MinorVersion: 0,
            u32MinInputConnections: 1,
            u32MaxInputConnections: 1,
            u32MinOutputConnections: 1,
            u32MaxOutputConnections: 1,
            u32MaxInstances: u32::MAX,
            u32NumAPOInterfaces: 1,
            iidAPOInterfaceList: [IAudioProcessingObject::IID],
            ..Default::default()
        };
        for (d, s) in p
            .szFriendlyName
            .iter_mut()
            .zip("Clipdeck Mic FX".encode_utf16())
        {
            *d = s;
        }
        for (d, s) in p
            .szCopyrightInfo
            .iter_mut()
            .zip("GPL-3.0-or-later, Clipdeck contributors".encode_utf16())
        {
            *d = s;
        }
        // SAFETY: the caller frees the block with CoTaskMemFree.
        unsafe {
            let mem = CoTaskMemAlloc(std::mem::size_of::<APO_REG_PROPERTIES>())
                .cast::<APO_REG_PROPERTIES>();
            if mem.is_null() {
                return Err(windows::Win32::Foundation::E_OUTOFMEMORY.into());
            }
            mem.write(p);
            Ok(mem)
        }
    }

    fn Initialize(&self, cbdatasize: u32, pbydata: *const u8) -> Result<()> {
        let base_size = std::mem::size_of::<APOInitBaseStruct>();
        if pbydata.is_null() || (cbdatasize as usize) < base_size {
            return Ok(()); // plain (non-system-effect) initialization: nothing to read
        }
        let size = cbdatasize as usize;
        let props_off = std::mem::offset_of!(APOInitSystemEffects2, pAPOEndpointProperties);
        let discovery = if size == std::mem::size_of::<APOInitSystemEffects2>() {
            // SAFETY: the engine passed an APOInitSystemEffects2 of exactly this size.
            unsafe {
                (*pbydata.cast::<APOInitSystemEffects2>())
                    .InitializeForDiscoveryOnly
                    .as_bool()
            }
        } else if size == std::mem::size_of::<APOInitSystemEffects3>() {
            // SAFETY: as above, APOInitSystemEffects3.
            unsafe {
                (*pbydata.cast::<APOInitSystemEffects3>())
                    .InitializeForDiscoveryOnly
                    .as_bool()
            }
        } else {
            false
        };
        let mut cfg = self.cfg();
        if size >= props_off + std::mem::size_of::<*mut c_void>() {
            // SAFETY: every APOInitSystemEffects* variant has the endpoint property store right
            // after the base struct, and `size` covers it.
            let raw = unsafe { *pbydata.add(props_off).cast::<*mut c_void>() };
            // SAFETY: borrowed COM pointer owned by the caller for the duration of the call.
            if let Some(store) = unsafe { IPropertyStore::from_raw_borrowed(&raw) } {
                cfg.name = prop_string(store, &PKEY_Device_FriendlyName).unwrap_or_default();
                cfg.endpoint = prop_string(store, &PKEY_AudioEndpoint_GUID).unwrap_or_default();
            }
        }
        if discovery {
            return Ok(());
        }
        if let Some(clsid) = original_clsid(&cfg.endpoint) {
            let created = (|| -> Result<Original> {
                // SAFETY: plain COM calls; the init block is passed through unchanged, as the
                // engine gave it to us.
                unsafe {
                    let apo: IAudioProcessingObject =
                        CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?;
                    let rt: IAudioProcessingObjectRT = apo.cast()?;
                    let c: IAudioProcessingObjectConfiguration = apo.cast()?;
                    apo.Initialize(std::slice::from_raw_parts(pbydata, cbdatasize as usize))?;
                    Ok(Original { apo, rt, cfg: c })
                }
            })();
            match created {
                Ok(o) => {
                    cfg.original = Some(o);
                    cfg.status = proto::STATUS_ORIGINAL_LOADED;
                }
                Err(e) => {
                    debug(&format!("original effect {clsid:?} failed: {e}"));
                    cfg.status = proto::STATUS_ORIGINAL_FAILED;
                    cfg.original_hr = e.code().0 as u32;
                }
            }
        }
        Ok(())
    }

    fn IsInputFormatSupported(
        &self,
        poppositeformat: Ref<IAudioMediaType>,
        prequestedinputformat: Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        let req = prequestedinputformat.ok()?;
        check_format(req, poppositeformat.as_ref())?;
        if let Some(o) = &self.cfg().original {
            // SAFETY: plain COM call with the caller's arguments.
            return unsafe { o.apo.IsInputFormatSupported(poppositeformat.as_ref(), req) };
        }
        Ok(req.clone())
    }

    fn IsOutputFormatSupported(
        &self,
        poppositeformat: Ref<IAudioMediaType>,
        prequestedoutputformat: Ref<IAudioMediaType>,
    ) -> Result<IAudioMediaType> {
        let req = prequestedoutputformat.ok()?;
        check_format(req, poppositeformat.as_ref())?;
        if let Some(o) = &self.cfg().original {
            // SAFETY: plain COM call with the caller's arguments.
            return unsafe { o.apo.IsOutputFormatSupported(poppositeformat.as_ref(), req) };
        }
        Ok(req.clone())
    }

    fn GetInputChannelCount(&self) -> Result<u32> {
        // SAFETY: read-only access to a value set during LockForProcess.
        Ok(unsafe { self.rt() }.channels as u32)
    }
}

impl IAudioProcessingObjectConfiguration_Impl for MicFx_Impl {
    fn LockForProcess(
        &self,
        u32numinputconnections: u32,
        ppinputconnections: *const *const APO_CONNECTION_DESCRIPTOR,
        u32numoutputconnections: u32,
        ppoutputconnections: *const *const APO_CONNECTION_DESCRIPTOR,
    ) -> Result<()> {
        if u32numinputconnections != 1 || u32numoutputconnections != 1 {
            return Err(APOERR_NUM_CONNECTIONS_INVALID.into());
        }
        if ppinputconnections.is_null() || ppoutputconnections.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: the engine passes one valid descriptor per connection.
        let input = unsafe { &**ppinputconnections };
        let format = input.pFormat.as_ref().ok_or(E_INVALIDARG)?;
        let f = format_of(format)?;
        let channels = (f.dwSamplesPerFrame as usize).clamp(1, proto::MAX_CHANNELS as usize);
        let rate = f.fFramesPerSecond.round() as u32;

        let mut cfg = self.cfg();
        let mut original_rt = None;
        if let Some(o) = &cfg.original {
            // SAFETY: plain COM call with the caller's arguments.
            match unsafe {
                o.cfg.LockForProcess(
                    std::slice::from_raw_parts(ppinputconnections, u32numinputconnections as usize),
                    std::slice::from_raw_parts(
                        ppoutputconnections,
                        u32numoutputconnections as usize,
                    ),
                )
            } {
                Ok(()) => original_rt = Some(o.rt.clone()),
                Err(e) => {
                    // Keep the microphone working with our effect alone.
                    debug(&format!("original LockForProcess failed: {e}"));
                    cfg.status = proto::STATUS_ORIGINAL_FAILED;
                    cfg.original_hr = e.code().0 as u32;
                    cfg.original = None;
                }
            }
        }
        let info = StreamInfo {
            rate,
            channels: channels as u32,
            name: cfg.name.clone(),
            endpoint: cfg.endpoint.clone(),
        };
        let slot = (0..proto::SLOTS).find_map(|n| Slot::claim(n, &info));
        if let Some(s) = &slot {
            s.view().effect_status(cfg.status, cfg.original_hr);
        } else {
            debug("no free shared-memory slot; passing the microphone through");
        }
        drop(cfg);

        // SAFETY: not concurrent with APOProcess (the APO is unlocked here).
        let rt = unsafe { self.rt() };
        rt.channels = channels;
        rt.rate = rate;
        rt.state = Some(EffectState::new(rate));
        rt.slot = slot;
        rt.original = original_rt;
        rt.locked = true;
        Ok(())
    }

    fn UnlockForProcess(&self) -> Result<()> {
        {
            // SAFETY: not concurrent with APOProcess.
            let rt = unsafe { self.rt() };
            rt.locked = false;
            rt.slot = None;
            rt.original = None;
        }
        if let Some(o) = &self.cfg().original {
            // SAFETY: plain COM call.
            let _ = unsafe { o.cfg.UnlockForProcess() };
        }
        Ok(())
    }
}

impl IAudioProcessingObjectRT_Impl for MicFx_Impl {
    fn APOProcess(
        &self,
        u32numinputconnections: u32,
        ppinputconnections: *const *const APO_CONNECTION_PROPERTY,
        u32numoutputconnections: u32,
        ppoutputconnections: *mut *mut APO_CONNECTION_PROPERTY,
    ) {
        // Real-time: no allocation, no locks, no logging below this line.
        // SAFETY: APOProcess is only called while locked, on one thread at a time.
        let rt = unsafe { self.rt() };
        if !rt.locked
            || u32numinputconnections != 1
            || u32numoutputconnections != 1
            || ppinputconnections.is_null()
            || ppoutputconnections.is_null()
        {
            return;
        }
        // SAFETY: one valid connection property each, owned by the engine.
        let (input, output) = unsafe { (&**ppinputconnections, &mut **ppoutputconnections) };
        if let Some(o) = &rt.original {
            // SAFETY: same arguments the engine gave us.
            unsafe {
                o.APOProcess(
                    u32numinputconnections,
                    ppinputconnections,
                    u32numoutputconnections,
                    ppoutputconnections,
                );
            }
        }
        let frames = input.u32ValidFrameCount as usize;
        let channels = rt.channels.max(1);
        let n = frames * channels;
        let in_ptr = input.pBuffer as *mut f32;
        let out_ptr = output.pBuffer as *mut f32;
        if out_ptr.is_null() || (in_ptr.is_null() && rt.original.is_none()) {
            return;
        }
        let silent = if rt.original.is_some() {
            output.u32BufferFlags == BUFFER_SILENT
        } else {
            input.u32BufferFlags == BUFFER_SILENT
        };
        // SAFETY: the engine sized both buffers for `frames` frames of `channels` f32 samples
        // (checked formats in LockForProcess); input and output may alias (in-place).
        let buf = unsafe {
            if rt.original.is_none() && in_ptr != out_ptr && !silent {
                std::ptr::copy_nonoverlapping(in_ptr, out_ptr, n);
            }
            std::slice::from_raw_parts_mut(out_ptr, n)
        };
        if silent {
            buf.fill(0.0);
        }
        let mut mixed = false;
        if let (Some(slot), Some(state)) = (&rt.slot, rt.state.as_mut()) {
            mixed = slot.view().effect_process(buf, channels, tick(), state);
        }
        output.u32ValidFrameCount = frames as u32;
        output.u32BufferFlags = if silent && !mixed {
            BUFFER_SILENT
        } else {
            BUFFER_VALID
        };
    }

    fn CalcInputFrames(&self, u32outputframecount: u32) -> u32 {
        u32outputframecount
    }

    fn CalcOutputFrames(&self, u32inputframecount: u32) -> u32 {
        u32inputframecount
    }
}

impl IAudioSystemEffects_Impl for MicFx_Impl {}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> Result<()> {
        if ppvobject.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: checked non-null.
        unsafe { *ppvobject = std::ptr::null_mut() };
        if !punkouter.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let unk: IUnknown = MicFx::new().into();
        // SAFETY: valid IID pointer and out pointer from the caller.
        unsafe { unk.query(riid, ppvobject).ok() }
    }

    fn LockServer(&self, _flock: BOOL) -> Result<()> {
        Ok(())
    }
}

/// COM entry point.
///
/// # Safety
/// Called by COM with valid pointers.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() || riid.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null; COM passes valid pointers.
    unsafe {
        *ppv = std::ptr::null_mut();
        if *rclsid != CLSID_MICFX {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        factory.query(riid, ppv)
    }
}

/// COM entry point. The audio engine keeps us loaded; never ask to be unloaded.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}
