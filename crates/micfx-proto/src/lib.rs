//! Protocol between Clipdeck and `clipdeck_micfx.dll`, the audio effect (an APO) that Windows
//! runs inside the user's real microphone. This is how sounds reach Discord and games without a
//! virtual cable: the effect adds them to the microphone signal itself.
//!
//! Every running effect instance owns one named shared-memory slot
//! ([`mapping_name`]). The effect creates the mapping (it runs in `audiodg.exe`, which may create
//! `Global\` objects), publishes its stream format, and on every `APOProcess` pulls the frames
//! Clipdeck put into the ring and adds them to the microphone signal.
//!
//! The ring is single-producer (Clipdeck) / single-consumer (the effect) with monotonically
//! increasing frame counters, so neither side ever locks. Samples are stored as `f32` bits in
//! `AtomicU32`, which keeps cross-process access free of data races. All atomics used here are
//! lock-free on x86-64 and ARM64, so they work across processes.
//!
//! The crate has no dependencies and no platform code, so the protocol is unit-tested on every
//! OS.

use std::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};

/// `b"CDFX"`.
pub const MAGIC: u32 = u32::from_le_bytes(*b"CDFX");
/// Protocol version; part of the mapping name so mismatched builds never meet.
pub const VERSION: u32 = 1;
/// Number of slots (concurrent effect instances).
pub const SLOTS: u32 = 8;
/// Ring capacity in frames (about 340 ms at 48 kHz).
pub const RING_FRAMES: u64 = 1 << 14;
/// Highest channel count the ring can carry.
pub const MAX_CHANNELS: u32 = 8;
/// UTF-16 units stored for the device name.
pub const NAME_LEN: usize = 128;
/// UTF-16 units stored for the endpoint GUID (`{xxxxxxxx-...}`).
pub const ENDPOINT_LEN: usize = 40;
/// Bytes reserved for the header.
pub const HEADER_BYTES: usize = 4096;
/// Total mapping size.
pub const MAPPING_BYTES: usize = HEADER_BYTES + RING_FRAMES as usize * MAX_CHANNELS as usize * 4;
/// When Clipdeck has not written for this long, the effect passes the microphone through
/// untouched (Clipdeck quit or hung).
pub const CLIENT_TIMEOUT_MS: u64 = 400;
/// When an effect has not processed for this long, its slot may be taken over, and Clipdeck
/// stops feeding it.
pub const APO_TIMEOUT_MS: u64 = 1000;

/// `flags` bit: mute the real microphone right now ("block my voice while a sound plays").
pub const FLAG_DUCK_MIC: u32 = 1;
/// `status` bit: the endpoint's original effect was loaded and runs before ours.
pub const STATUS_ORIGINAL_LOADED: u32 = 1;
/// `status` bit: an original effect is configured but failed to load (see `original_hr`).
pub const STATUS_ORIGINAL_FAILED: u32 = 2;

/// COM class id of the effect, as registered under `HKLM\SOFTWARE\Classes\CLSID`.
pub const CLSID: &str = "{7B3F9A52-4C1E-4D8A-9F60-2A5CC11DDEC0}";
/// Same, as a number (for `GUID::from_u128`).
pub const CLSID_U128: u128 = 0x7B3F9A52_4C1E_4D8A_9F60_2A5CC11DDEC0;
/// `HKLM` key with Clipdeck's install state; one subkey per microphone endpoint GUID, holding
/// [`REG_ORIGINAL_EFX`] when the endpoint already had an effect that ours must chain to.
pub const REG_KEY: &str = r"SOFTWARE\Clipdeck\MicFx";
/// Value (REG_SZ) with the CLSID of the endpoint's original effect.
pub const REG_ORIGINAL_EFX: &str = "OriginalEfx";

/// Name of the shared-memory mapping for `slot`.
pub fn mapping_name(slot: u32) -> String {
    format!("Global\\ClipdeckMicFx{VERSION}.{slot}")
}

/// Shared header at the start of the mapping.
#[repr(C)]
#[derive(Debug)]
pub struct Header {
    /// [`MAGIC`] once initialized.
    pub magic: AtomicU32,
    /// [`VERSION`].
    pub version: AtomicU32,
    /// Stream sample rate (effect).
    pub rate: AtomicU32,
    /// Interleaved channels of the stream and of the ring (effect).
    pub channels: AtomicU32,
    /// Frames per `APOProcess` call, last seen (effect).
    pub period: AtomicU32,
    /// `STATUS_*` bits (effect).
    pub status: AtomicU32,
    /// HRESULT of loading the original effect, if it failed (effect).
    pub original_hr: AtomicU32,
    /// `FLAG_*` bits (Clipdeck).
    pub flags: AtomicU32,
    /// Gain applied to the real microphone, `f32` bits (Clipdeck).
    pub mic_gain: AtomicU32,
    /// Process id of the owning effect (diagnostics).
    pub pid: AtomicU32,
    /// Owner token of the effect instance holding this slot; 0 = free.
    pub owner: AtomicU64,
    /// Milliseconds tick of the last `APOProcess` (effect).
    pub apo_tick: AtomicU64,
    /// Frames processed since the slot was claimed (effect).
    pub processed: AtomicU64,
    /// Total frames written into the ring (Clipdeck).
    pub write: AtomicU64,
    /// Total frames consumed from the ring (effect).
    pub read: AtomicU64,
    /// Milliseconds tick of Clipdeck's last write (Clipdeck).
    pub client_tick: AtomicU64,
    /// Process calls that wanted more frames than the ring held while Clipdeck was alive.
    pub underruns: AtomicU64,
    /// Device friendly name, UTF-16, NUL padded (effect).
    pub name: [AtomicU16; NAME_LEN],
    /// Endpoint GUID string, UTF-16, NUL padded (effect).
    pub endpoint: [AtomicU16; ENDPOINT_LEN],
}

const _: () = assert!(std::mem::size_of::<Header>() <= HEADER_BYTES);

fn f(v: &AtomicU32) -> f32 {
    f32::from_bits(v.load(Ordering::Relaxed))
}

fn store_str(dst: &[AtomicU16], s: &str) {
    let mut units = s.encode_utf16();
    for d in dst.iter().take(dst.len() - 1) {
        d.store(units.next().unwrap_or(0), Ordering::Relaxed);
    }
    if let Some(last) = dst.last() {
        last.store(0, Ordering::Relaxed);
    }
}

fn load_str(src: &[AtomicU16]) -> String {
    let units: Vec<u16> = src
        .iter()
        .map(|u| u.load(Ordering::Relaxed))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Format and identity an effect publishes when it claims a slot.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    /// Sample rate.
    pub rate: u32,
    /// Channels (clamped to [`MAX_CHANNELS`]).
    pub channels: u32,
    /// Device friendly name, e.g. "Microphone (SM950)".
    pub name: String,
    /// Endpoint GUID string, e.g. `{A6825A6D-...}`.
    pub endpoint: String,
}

/// Per-instance real-time state of the effect side.
#[derive(Debug, Clone)]
pub struct EffectState {
    gain: f32,
    smooth: f32,
}

impl EffectState {
    /// New state for a stream at `rate`.
    pub fn new(rate: u32) -> Self {
        EffectState {
            gain: 1.0,
            // ~5 ms one-pole ramp so gain changes never click.
            smooth: 1.0 - (-1.0 / (0.005 * rate.max(1) as f32)).exp(),
        }
    }
}

/// A view of one mapped slot.
#[derive(Debug, Clone, Copy)]
pub struct View<'a> {
    /// The header.
    pub header: &'a Header,
    ring: &'a [AtomicU32],
}

impl<'a> View<'a> {
    /// View the mapping at `base`.
    ///
    /// # Safety
    /// `base` must point to at least [`MAPPING_BYTES`] bytes that are 8-byte aligned, either
    /// zeroed or previously written only through this protocol, and that stay mapped for `'a`.
    pub unsafe fn from_raw(base: *mut u8) -> View<'a> {
        // SAFETY: guaranteed by the caller; every field is an atomic, so shared mutation from
        // other threads or processes is not a data race.
        unsafe {
            let header = &*base.cast::<Header>();
            let ring = std::slice::from_raw_parts(
                base.add(HEADER_BYTES).cast::<AtomicU32>(),
                RING_FRAMES as usize * MAX_CHANNELS as usize,
            );
            View { header, ring }
        }
    }

    /// Published stream info, if the slot is held by a live effect.
    pub fn stream(&self, now_ms: u64) -> Option<(u64, StreamInfo)> {
        let h = self.header;
        if h.magic.load(Ordering::Acquire) != MAGIC || h.version.load(Ordering::Relaxed) != VERSION
        {
            return None;
        }
        let owner = h.owner.load(Ordering::Acquire);
        if owner == 0 || now_ms.saturating_sub(h.apo_tick.load(Ordering::Relaxed)) > APO_TIMEOUT_MS
        {
            return None;
        }
        Some((
            owner,
            StreamInfo {
                rate: h.rate.load(Ordering::Relaxed),
                channels: h.channels.load(Ordering::Relaxed),
                name: load_str(&h.name),
                endpoint: load_str(&h.endpoint),
            },
        ))
    }

    // ----- effect side -------------------------------------------------------------------

    /// Try to take this slot for the effect instance `token` (non-zero, unique per instance).
    /// Succeeds when the slot is free or its previous owner stopped processing.
    pub fn effect_claim(&self, token: u64, info: &StreamInfo, pid: u32, now_ms: u64) -> bool {
        let h = self.header;
        let old = h.owner.load(Ordering::Acquire);
        if old != 0 && now_ms.saturating_sub(h.apo_tick.load(Ordering::Relaxed)) <= APO_TIMEOUT_MS {
            return false;
        }
        // Refresh the tick before claiming so a third instance cannot judge us stale between
        // the exchange and the first process call.
        h.apo_tick.store(now_ms, Ordering::Relaxed);
        if h.owner
            .compare_exchange(old, token, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        h.rate.store(info.rate, Ordering::Relaxed);
        h.channels
            .store(info.channels.clamp(1, MAX_CHANNELS), Ordering::Relaxed);
        h.period.store(0, Ordering::Relaxed);
        h.processed.store(0, Ordering::Relaxed);
        h.underruns.store(0, Ordering::Relaxed);
        h.pid.store(pid, Ordering::Relaxed);
        store_str(&h.name, &info.name);
        store_str(&h.endpoint, &info.endpoint);
        // Drop whatever a previous owner left unread.
        h.read
            .store(h.write.load(Ordering::Acquire), Ordering::Release);
        h.version.store(VERSION, Ordering::Relaxed);
        h.magic.store(MAGIC, Ordering::Release);
        true
    }

    /// Release the slot if `token` still owns it.
    pub fn effect_release(&self, token: u64) {
        let _ = self
            .header
            .owner
            .compare_exchange(token, 0, Ordering::AcqRel, Ordering::Relaxed);
    }

    /// Record status bits and the HRESULT of loading the original effect.
    pub fn effect_status(&self, status: u32, original_hr: u32) {
        self.header.status.store(status, Ordering::Relaxed);
        self.header
            .original_hr
            .store(original_hr, Ordering::Relaxed);
    }

    /// Process one block in place: scale the microphone, add Clipdeck's frames.
    ///
    /// `buf` is interleaved with `channels` (must equal the published channel count); it holds
    /// the microphone signal on entry (zeros when the engine flagged it silent). Real-time
    /// safe: no allocation, no locks, no system calls. Returns whether any of Clipdeck's
    /// frames were added.
    pub fn effect_process(
        &self,
        buf: &mut [f32],
        channels: usize,
        now_ms: u64,
        st: &mut EffectState,
    ) -> bool {
        let h = self.header;
        let channels = channels.clamp(1, MAX_CHANNELS as usize);
        let frames = buf.len() / channels;
        h.apo_tick.store(now_ms, Ordering::Relaxed);
        h.period.store(frames as u32, Ordering::Relaxed);
        h.processed.fetch_add(frames as u64, Ordering::Relaxed);

        let alive =
            now_ms.saturating_sub(h.client_tick.load(Ordering::Acquire)) <= CLIENT_TIMEOUT_MS;
        let write = h.write.load(Ordering::Acquire);
        let mut read = h.read.load(Ordering::Relaxed);
        let target = if !alive {
            1.0
        } else if h.flags.load(Ordering::Relaxed) & FLAG_DUCK_MIC != 0 {
            0.0
        } else {
            f(&h.mic_gain).clamp(0.0, 4.0)
        };
        let mut avail = write.wrapping_sub(read);
        if !alive || avail > RING_FRAMES {
            // Clipdeck is gone (or the counters are inconsistent): forget queued audio.
            read = write;
            avail = 0;
        }
        let n = (avail as usize).min(frames);
        if alive && n < frames {
            h.underruns.fetch_add(1, Ordering::Relaxed);
        }

        let unity = (st.gain - 1.0).abs() < 1e-6 && (target - 1.0).abs() < 1e-6;
        for (i, frame) in buf.chunks_exact_mut(channels).enumerate() {
            if !unity {
                st.gain += (target - st.gain) * st.smooth;
                for s in frame.iter_mut() {
                    *s *= st.gain;
                }
            }
            if i < n {
                let base = ((read + i as u64) % RING_FRAMES) as usize * channels;
                for (c, s) in frame.iter_mut().enumerate() {
                    *s += f32::from_bits(self.ring[base + c].load(Ordering::Relaxed));
                }
            }
        }
        if unity {
            st.gain = 1.0;
        }
        h.read.store(read.wrapping_add(n as u64), Ordering::Release);
        n > 0
    }

    // ----- Clipdeck side -----------------------------------------------------------------

    /// Frames queued in the ring and not yet consumed.
    pub fn client_fill(&self) -> u64 {
        let h = self.header;
        h.write
            .load(Ordering::Relaxed)
            .wrapping_sub(h.read.load(Ordering::Acquire))
            .min(RING_FRAMES)
    }

    /// Append interleaved frames (`channels` must equal the published channel count). Returns
    /// the number of frames written; fewer when the ring is full.
    pub fn client_write(&self, samples: &[f32], channels: usize, now_ms: u64) -> usize {
        let h = self.header;
        let channels = channels.clamp(1, MAX_CHANNELS as usize);
        let write = h.write.load(Ordering::Relaxed);
        let space = (RING_FRAMES - self.client_fill()) as usize;
        let n = (samples.len() / channels).min(space);
        for (i, frame) in samples.chunks_exact(channels).take(n).enumerate() {
            let base = ((write + i as u64) % RING_FRAMES) as usize * channels;
            for (c, s) in frame.iter().enumerate() {
                self.ring[base + c].store(s.to_bits(), Ordering::Relaxed);
            }
        }
        h.write
            .store(write.wrapping_add(n as u64), Ordering::Release);
        h.client_tick.store(now_ms, Ordering::Release);
        n
    }

    /// Mark Clipdeck alive without writing (keeps the mic gain applied while idle).
    pub fn client_heartbeat(&self, now_ms: u64) {
        self.header.client_tick.store(now_ms, Ordering::Release);
    }

    /// Set the real-microphone gain and whether to mute it right now.
    pub fn client_controls(&self, mic_gain: f32, duck: bool) {
        let h = self.header;
        h.mic_gain.store(mic_gain.to_bits(), Ordering::Relaxed);
        h.flags
            .store(if duck { FLAG_DUCK_MIC } else { 0 }, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    struct Mem(Vec<u64>);
    impl Mem {
        fn new() -> Self {
            Mem(vec![0u64; MAPPING_BYTES / 8])
        }
        fn view(&mut self) -> View<'_> {
            // SAFETY: zeroed, 8-aligned, MAPPING_BYTES long, lives as long as the borrow.
            unsafe { View::from_raw(self.0.as_mut_ptr().cast()) }
        }
    }

    fn info(ch: u32) -> StreamInfo {
        StreamInfo {
            rate: 48_000,
            channels: ch,
            name: "Microphone (Test)".into(),
            endpoint: "{A6825A6D-55EF-46BF-958D-8C922E5AE44E}".into(),
        }
    }

    #[test]
    fn claim_publish_release() {
        let mut m = Mem::new();
        let v = m.view();
        assert!(v.stream(10).is_none());
        assert!(v.effect_claim(7, &info(2), 42, 1000));
        let (owner, s) = v.stream(1000).unwrap();
        assert_eq!(owner, 7);
        assert_eq!(s, info(2));
        // Held by a live instance: a second one cannot take it.
        assert!(!v.effect_claim(8, &info(1), 43, 1500));
        // Stale owner: takeover allowed.
        assert!(v.effect_claim(8, &info(1), 43, 1000 + APO_TIMEOUT_MS + 1));
        assert_eq!(v.stream(2001).unwrap().1.channels, 1);
        v.effect_release(7); // not the owner any more: no effect
        assert!(v.stream(2001).is_some());
        v.effect_release(8);
        assert!(v.stream(2001).is_none());
    }

    #[test]
    fn mixes_clip_frames_into_mic() {
        let mut m = Mem::new();
        let v = m.view();
        v.effect_claim(1, &info(2), 1, 0);
        v.client_controls(1.0, false);
        let clip: Vec<f32> = (0..960).map(|i| i as f32 / 1000.0).collect(); // 480 frames
        assert_eq!(v.client_write(&clip, 2, 5), 480);
        assert_eq!(v.client_fill(), 480);
        let mut st = EffectState::new(48_000);
        let mut buf = vec![0.25f32; 2 * 256];
        assert!(v.effect_process(&mut buf, 2, 10, &mut st));
        assert!((buf[0] - 0.25).abs() < 1e-6);
        assert!((buf[11] - (0.25 + 0.011)).abs() < 1e-6);
        assert_eq!(v.client_fill(), 224);
        // Next block: 224 frames of clip, the rest is plain mic; one underrun counted.
        let mut buf = vec![0.25f32; 2 * 256];
        assert!(v.effect_process(&mut buf, 2, 15, &mut st));
        assert!((buf[0] - (0.25 + 0.512)).abs() < 1e-5);
        assert!((buf[2 * 250] - 0.25).abs() < 1e-6);
        assert_eq!(v.header.underruns.load(Ordering::Relaxed), 1);
        assert_eq!(v.client_fill(), 0);
    }

    #[test]
    fn passes_mic_through_when_clipdeck_is_gone() {
        let mut m = Mem::new();
        let v = m.view();
        v.effect_claim(1, &info(1), 1, 0);
        v.client_controls(0.0, true); // muted + ducked...
        v.client_write(&[0.5; 100], 1, 0);
        let mut st = EffectState::new(48_000);
        let mut buf = vec![0.3f32; 64];
        // ...but Clipdeck stopped writing long ago: unity gain, queued audio dropped.
        assert!(!v.effect_process(&mut buf, 1, 10_000, &mut st));
        assert!(buf.iter().all(|s| (s - 0.3).abs() < 1e-6));
        assert_eq!(v.client_fill(), 0);
        assert_eq!(v.header.underruns.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn duck_and_gain_ramp_without_clicks() {
        let mut m = Mem::new();
        let v = m.view();
        v.effect_claim(1, &info(1), 1, 0);
        v.client_controls(1.0, true);
        v.client_heartbeat(0);
        let mut st = EffectState::new(48_000);
        let mut buf = vec![1.0f32; 4800]; // 100 ms
        v.effect_process(&mut buf, 1, 1, &mut st);
        // Smooth: first sample barely moved, end of block fully muted, monotonic in between.
        assert!(buf[0] > 0.99);
        assert!(buf[4799] < 1e-3);
        assert!(buf.windows(2).all(|w| w[1] <= w[0] + 1e-7));
    }

    #[test]
    fn ring_wraps_and_never_overfills() {
        let mut m = Mem::new();
        let v = m.view();
        v.effect_claim(1, &info(2), 1, 0);
        let mut st = EffectState::new(48_000);
        let mut produced = 0u64;
        let mut consumed = 0u64;
        let mut t = 0;
        for round in 0..200u64 {
            t += 5;
            let chunk: Vec<f32> = (0..700u64)
                .flat_map(|i| {
                    let x = ((produced + i) % 1000) as f32;
                    [x, -x]
                })
                .collect();
            produced += v.client_write(&chunk, 2, t) as u64;
            assert!(v.client_fill() <= RING_FRAMES);
            let mut buf = vec![0.0f32; 2 * 480];
            v.effect_process(&mut buf, 2, t, &mut st);
            for (i, fr) in buf.as_chunks::<2>().0.iter().enumerate() {
                if (consumed + i as u64) < produced {
                    let x = ((consumed + i as u64) % 1000) as f32;
                    assert_eq!(*fr, [x, -x], "round {round} frame {i}");
                }
            }
            consumed = v.header.read.load(Ordering::Relaxed);
        }
        assert!(consumed > RING_FRAMES * 4, "wrapped several times");
    }
}
