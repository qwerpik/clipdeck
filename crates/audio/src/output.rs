//! cpal device enumeration and streams (monitor output, microphone capture, and the
//! output-device flavour of the virtual microphone used with VB-CABLE on Windows).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, SampleFormat, StreamConfig, SupportedBufferSize};
use rtrb::{Consumer, Producer, RingBuffer};

use crate::error::{AudioError, Result};
use crate::mixer::{Mixer, MixerHandle, mixer};
use crate::resample::LinearResampler;

/// A render callback: fill interleaved `out` with `channels` channels.
pub type RenderFn = Box<dyn FnMut(&mut [f32], usize) + Send + 'static>;

/// A device as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Human-readable name (also the identifier we store in the config).
    pub name: String,
    /// Whether this is the system default.
    pub is_default: bool,
}

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|desc| desc.name().to_string())
        .unwrap_or_else(|_| d.to_string())
}

/// Output devices.
pub fn output_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default = host.default_output_device().map(|d| device_name(&d));
    host.output_devices()
        .map(|it| {
            it.map(|d| {
                let name = device_name(&d);
                DeviceInfo {
                    is_default: Some(&name) == default.as_ref(),
                    name,
                }
            })
            .collect()
        })
        .unwrap_or_default()
}

/// Input devices.
pub fn input_devices() -> Vec<DeviceInfo> {
    let host = cpal::default_host();
    let default = host.default_input_device().map(|d| device_name(&d));
    host.input_devices()
        .map(|it| {
            it.map(|d| {
                let name = device_name(&d);
                DeviceInfo {
                    is_default: Some(&name) == default.as_ref(),
                    name,
                }
            })
            .collect()
        })
        .unwrap_or_default()
}

/// Name of the audio host in use (e.g. `PipeWire`, `WASAPI`).
pub fn host_name() -> String {
    format!("{:?}", cpal::default_host().id())
}

fn find_output(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(name) = name
        && let Ok(mut devs) = host.output_devices()
        && let Some(d) = devs.find(|d| device_name(d) == name)
    {
        return Ok(d);
    }
    host.default_output_device()
        .ok_or_else(|| AudioError::Device("no output device".into()))
}

fn find_input(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(name) = name
        && let Ok(mut devs) = host.input_devices()
        && let Some(d) = devs.find(|d| device_name(d) == name)
    {
        return Ok(d);
    }
    host.default_input_device()
        .ok_or_else(|| AudioError::Device("no input device".into()))
}

/// Find an output device whose name contains `needle` (case-insensitive).
pub fn find_output_containing(needle: &str) -> Option<String> {
    let n = needle.to_ascii_lowercase();
    output_devices()
        .into_iter()
        .map(|d| d.name)
        .find(|name| name.to_ascii_lowercase().contains(&n))
}

fn buffer_size(supported: &SupportedBufferSize, wanted: u32) -> BufferSize {
    match supported {
        SupportedBufferSize::Range { min, max } if wanted >= *min && wanted <= *max => {
            BufferSize::Fixed(wanted)
        }
        _ => BufferSize::Default,
    }
}

/// A running output stream (keep it alive; dropping stops the sound).
pub struct OutputStream {
    _stream: cpal::Stream,
    /// Device name.
    pub device: String,
    /// Sample rate.
    pub rate: u32,
    /// Channel count.
    pub channels: u16,
    /// Set when the device reported an error (disconnect, etc.).
    pub failed: Arc<AtomicBool>,
}

impl std::fmt::Debug for OutputStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputStream")
            .field("device", &self.device)
            .field("rate", &self.rate)
            .finish()
    }
}

/// Open an output device and drive `render` from its callback.
pub fn open_output(
    device: Option<&str>,
    buffer_frames: u32,
    render: impl FnOnce(u32) -> RenderFn,
) -> Result<OutputStream> {
    let dev = find_output(device)?;
    let name = device_name(&dev);
    let supported = dev
        .default_output_config()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    let rate = supported.sample_rate();
    let channels = supported.channels();
    let mut render = render(rate);
    let failed = Arc::new(AtomicBool::new(false));
    let f2 = failed.clone();
    let err_cb = move |e: cpal::Error| {
        tracing::warn!(%e, "output stream error");
        f2.store(true, Ordering::Release);
    };
    let config = StreamConfig {
        channels,
        sample_rate: rate,
        buffer_size: buffer_size(supported.buffer_size(), buffer_frames),
    };
    let ch = channels as usize;
    let stream = match supported.sample_format() {
        SampleFormat::F32 => dev.build_output_stream::<f32, _, _>(
            config,
            move |data: &mut [f32], _| render(data, ch),
            err_cb,
            None,
        ),
        fmt => {
            // Integer devices: render into a preallocated f32 scratch block, then convert.
            let mut scratch = vec![0f32; 1 << 16];
            match fmt {
                SampleFormat::I16 => dev.build_output_stream::<i16, _, _>(
                    config,
                    move |data: &mut [i16], _| {
                        for chunk in data.chunks_mut(scratch.len() / ch * ch) {
                            let s = &mut scratch[..chunk.len()];
                            render(s, ch);
                            for (o, v) in chunk.iter_mut().zip(s.iter()) {
                                *o = (v.clamp(-1.0, 1.0) * 32767.0) as i16;
                            }
                        }
                    },
                    err_cb,
                    None,
                ),
                SampleFormat::I32 => dev.build_output_stream::<i32, _, _>(
                    config,
                    move |data: &mut [i32], _| {
                        for chunk in data.chunks_mut(scratch.len() / ch * ch) {
                            let s = &mut scratch[..chunk.len()];
                            render(s, ch);
                            for (o, v) in chunk.iter_mut().zip(s.iter()) {
                                *o = (f64::from(v.clamp(-1.0, 1.0)) * 2_147_483_647.0) as i32;
                            }
                        }
                    },
                    err_cb,
                    None,
                ),
                other => {
                    return Err(AudioError::Device(format!(
                        "unsupported sample format {other:?} on {name}"
                    )));
                }
            }
        }
    }
    .map_err(|e| AudioError::Device(format!("{name}: {e}")))?;
    stream
        .play()
        .map_err(|e| AudioError::Device(format!("{name}: {e}")))?;
    Ok(OutputStream {
        _stream: stream,
        device: name,
        rate,
        channels,
        failed,
    })
}

/// Open an output and attach a fresh [`Mixer`] to it.
pub fn open_mixer_output(
    device: Option<&str>,
    buffer_frames: u32,
) -> Result<(OutputStream, MixerHandle)> {
    let mut handle_slot: Option<MixerHandle> = None;
    let stream = open_output(device, buffer_frames, |rate| {
        let (mut m, h): (Mixer, MixerHandle) = mixer(rate);
        handle_slot = Some(h);
        Box::new(move |out, ch| m.render(out, ch))
    })?;
    let handle = handle_slot.ok_or_else(|| AudioError::Device("mixer not created".into()))?;
    Ok((stream, handle))
}

/// A running capture stream feeding stereo frames at `target_rate` into a ring.
pub struct InputStream {
    _stream: cpal::Stream,
    /// Device name.
    pub device: String,
    /// Device rate.
    pub rate: u32,
    /// Set on device error.
    pub failed: Arc<AtomicBool>,
}

impl std::fmt::Debug for InputStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputStream")
            .field("device", &self.device)
            .finish()
    }
}

/// Open the microphone; returns the stream and the consumer end of its stereo ring.
pub fn open_input(device: Option<&str>, target_rate: u32) -> Result<(InputStream, Consumer<f32>)> {
    let dev = find_input(device)?;
    let supported = dev
        .default_input_config()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    open_capture(&dev, &supported, target_rate)
}

/// Record what plays on the default output device (WASAPI loopback). Windows only.
#[cfg(windows)]
pub fn open_loopback(target_rate: u32) -> Result<(InputStream, Consumer<f32>)> {
    let dev = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| AudioError::Device("no output device".into()))?;
    let supported = dev
        .default_output_config()
        .map_err(|e| AudioError::Device(e.to_string()))?;
    open_capture(&dev, &supported, target_rate)
}

fn open_capture(
    dev: &cpal::Device,
    supported: &cpal::SupportedStreamConfig,
    target_rate: u32,
) -> Result<(InputStream, Consumer<f32>)> {
    let name = device_name(dev);
    let rate = supported.sample_rate();
    let channels = supported.channels() as usize;
    // ~0.5 s of stereo audio; the mixer trims latency on its side.
    let (tx, rx) = RingBuffer::<f32>::new(target_rate as usize);
    let failed = Arc::new(AtomicBool::new(false));
    let f2 = failed.clone();
    let err_cb = move |e: cpal::Error| {
        tracing::warn!(%e, "input stream error");
        f2.store(true, Ordering::Release);
    };
    let config = StreamConfig {
        channels: channels as u16,
        sample_rate: rate,
        buffer_size: BufferSize::Default,
    };
    let mut feeder = CaptureFeeder::new(tx, channels, rate, target_rate);
    let stream = match supported.sample_format() {
        SampleFormat::F32 => dev.build_input_stream::<f32, _, _>(
            config,
            move |d: &[f32], _| feeder.push(d.iter().copied()),
            err_cb,
            None,
        ),
        SampleFormat::I16 => dev.build_input_stream::<i16, _, _>(
            config,
            move |d: &[i16], _| feeder.push(d.iter().map(|s| f32::from(*s) / 32768.0)),
            err_cb,
            None,
        ),
        SampleFormat::I32 => dev.build_input_stream::<i32, _, _>(
            config,
            move |d: &[i32], _| {
                feeder.push(d.iter().map(|s| (f64::from(*s) / 2_147_483_648.0) as f32))
            },
            err_cb,
            None,
        ),
        other => {
            return Err(AudioError::Device(format!(
                "unsupported input format {other:?}"
            )));
        }
    }
    .map_err(|e| AudioError::Device(format!("{name}: {e}")))?;
    stream
        .play()
        .map_err(|e| AudioError::Device(format!("{name}: {e}")))?;
    Ok((
        InputStream {
            _stream: stream,
            device: name,
            rate,
            failed,
        },
        rx,
    ))
}

/// Converts captured frames to stereo at the mixer rate without allocating.
struct CaptureFeeder {
    tx: Producer<f32>,
    channels: usize,
    resampler: LinearResampler,
    frame: [f32; 2],
    pending: [f32; 2],
    idx: usize,
}

impl CaptureFeeder {
    fn new(tx: Producer<f32>, channels: usize, from: u32, to: u32) -> Self {
        CaptureFeeder {
            tx,
            channels: channels.max(1),
            resampler: LinearResampler::new(from, to),
            frame: [0.0; 2],
            pending: [0.0; 2],
            idx: 0,
        }
    }

    fn push(&mut self, samples: impl Iterator<Item = f32>) {
        for s in samples {
            match (self.channels, self.idx) {
                (1, _) => self.frame = [s, s],
                (_, 0) => self.frame[0] = s,
                (_, 1) => self.frame[1] = s,
                _ => {}
            }
            self.idx += 1;
            if self.idx >= self.channels {
                self.idx = 0;
                self.pending = self.frame;
                let tx = &mut self.tx;
                self.resampler.process(&self.pending, |l, r| {
                    if tx.slots() >= 2 {
                        let _ = tx.push(l);
                        let _ = tx.push(r);
                    }
                });
            }
        }
    }
}

/// The virtual microphone abstraction implemented per platform.
pub trait VirtualMic: Send {
    /// What the user should select in Discord etc.
    fn display_name(&self) -> String;
    /// Sample rate and channels the sink will run at.
    fn prepare(&mut self) -> std::result::Result<(u32, u16), String>;
    /// Start pulling audio from `render`.
    fn start(&mut self, render: RenderFn) -> std::result::Result<(), String>;
    /// Stop and release the device.
    fn stop(&mut self);
    /// Whether the sink failed since it was started.
    fn failed(&self) -> bool;
    /// Whether the sink already carries the user's real microphone (the Windows in-mic
    /// effect), so the engine must not capture and mix it in again. Such a sink applies the
    /// mic gain and "block voice" itself, from the state handed to [`VirtualMic::attach`].
    fn carries_real_mic(&self) -> bool {
        false
    }
    /// Receives the mixer's shared state before [`VirtualMic::start`].
    fn attach(&mut self, _shared: std::sync::Arc<crate::mixer::MixerShared>) {}
}

/// A virtual microphone that is a plain output device (VB-CABLE `CABLE Input` on Windows,
/// or any loopback sink).
pub struct DeviceMic {
    device: String,
    buffer_frames: u32,
    stream: Option<OutputStream>,
    rate: u32,
}

impl DeviceMic {
    /// Use the output device called `device`.
    pub fn new(device: String, buffer_frames: u32) -> Self {
        DeviceMic {
            device,
            buffer_frames,
            stream: None,
            rate: 48_000,
        }
    }
}

impl VirtualMic for DeviceMic {
    fn display_name(&self) -> String {
        self.device.clone()
    }
    fn prepare(&mut self) -> std::result::Result<(u32, u16), String> {
        let dev = find_output(Some(&self.device)).map_err(|e| e.to_string())?;
        if device_name(&dev) != self.device {
            return Err(format!("device `{}` not found", self.device));
        }
        let cfg = dev.default_output_config().map_err(|e| e.to_string())?;
        self.rate = cfg.sample_rate();
        Ok((self.rate, cfg.channels()))
    }
    fn start(&mut self, render: RenderFn) -> std::result::Result<(), String> {
        let s = open_output(Some(&self.device), self.buffer_frames, move |_| render)
            .map_err(|e| e.to_string())?;
        self.stream = Some(s);
        Ok(())
    }
    fn stop(&mut self) {
        self.stream = None;
    }
    fn failed(&self) -> bool {
        self.stream
            .as_ref()
            .is_some_and(|s| s.failed.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_feeder_mono_to_stereo_and_resample() {
        let (tx, mut rx) = RingBuffer::new(10_000);
        let mut f = CaptureFeeder::new(tx, 1, 48_000, 48_000);
        f.push([0.1f32, 0.2, 0.3].into_iter());
        let v: Vec<f32> = std::iter::from_fn(|| rx.pop().ok()).collect();
        assert_eq!(v, vec![0.1, 0.1, 0.2, 0.2, 0.3, 0.3]);

        let (tx, mut rx) = RingBuffer::new(100_000);
        let mut f = CaptureFeeder::new(tx, 4, 44_100, 48_000);
        let input: Vec<f32> = (0..44_100 * 4)
            .map(|i| if i % 4 == 1 { 0.5 } else { 0.25 })
            .collect();
        f.push(input.into_iter());
        let n = rx.slots() / 2;
        assert!((n as i64 - 48_000).abs() < 4, "{n}");
        assert_eq!(rx.pop().unwrap(), 0.25);
        assert_eq!(rx.pop().unwrap(), 0.5);
    }
}
