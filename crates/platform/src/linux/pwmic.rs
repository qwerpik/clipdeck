//! "Clipdeck Mic": a native PipeWire source node that other apps record from.
//!
//! The node is a PipeWire output stream with `media.class = Audio/Source`, so it shows
//! up as a selectable microphone in Discord, OBS, browsers and `pavucontrol`. Its process
//! callback pulls audio from the engine's mic mixer.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use clipdeck_audio::output::{RenderFn, VirtualMic};
use pipewire as pw;
use pw::context::ContextRc;
use pw::main_loop::MainLoopRc;
use pw::properties::PropertiesBox;
use pw::spa::param::ParamType;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{Object, Pod, Value};
use pw::spa::utils::{Direction, SpaTypes};
use pw::stream::{StreamFlags, StreamRc, StreamState};

/// Node name other tools can target (`pw-record --target clipdeck_mic`).
pub const NODE_NAME: &str = "clipdeck_mic";
/// What users see in Discord.
pub const NODE_DESCRIPTION: &str = "Clipdeck Mic";

const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;

/// PipeWire virtual microphone.
pub struct PipeWireMic {
    latency_frames: u32,
    thread: Option<JoinHandle<()>>,
    quit: Option<pw::channel::Sender<()>>,
    failed: Arc<AtomicBool>,
}

impl PipeWireMic {
    /// New (not yet started) virtual mic with the requested quantum.
    pub fn new(latency_frames: u32) -> Self {
        PipeWireMic {
            latency_frames: latency_frames.clamp(64, 4096),
            thread: None,
            quit: None,
            failed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether a PipeWire server is reachable.
    pub fn available() -> bool {
        let runtime = std::env::var_os("PIPEWIRE_RUNTIME_DIR")
            .or_else(|| std::env::var_os("XDG_RUNTIME_DIR"))
            .map(std::path::PathBuf::from);
        let remote = std::env::var("PIPEWIRE_REMOTE").unwrap_or_else(|_| "pipewire-0".into());
        runtime.is_some_and(|d| d.join(&remote).exists())
    }
}

impl VirtualMic for PipeWireMic {
    fn display_name(&self) -> String {
        NODE_DESCRIPTION.into()
    }

    fn prepare(&mut self) -> Result<(u32, u16), String> {
        if !Self::available() {
            return Err("PipeWire is not running".into());
        }
        Ok((RATE, CHANNELS as u16))
    }

    fn start(&mut self, render: RenderFn) -> Result<(), String> {
        self.stop();
        self.failed.store(false, Ordering::Release);
        let (quit_tx, quit_rx) = pw::channel::channel::<()>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let failed = self.failed.clone();
        let latency = self.latency_frames;
        let thread = std::thread::Builder::new()
            .name("clipdeck-pipewire".into())
            .spawn(move || {
                if let Err(e) = run(render, quit_rx, failed.clone(), latency, &ready_tx) {
                    failed.store(true, Ordering::Release);
                    let _ = ready_tx.send(Err(e));
                }
            })
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(Ok(())) => {
                self.thread = Some(thread);
                self.quit = Some(quit_tx);
                Ok(())
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = quit_tx.send(());
                Err("PipeWire did not respond".into())
            }
        }
    }

    fn stop(&mut self) {
        if let Some(q) = self.quit.take() {
            let _ = q.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
}

impl Drop for PipeWireMic {
    fn drop(&mut self) {
        self.stop();
    }
}

struct UserData {
    render: RenderFn,
    scratch: Vec<f32>,
    failed: Arc<AtomicBool>,
}

fn run(
    render: RenderFn,
    quit_rx: pw::channel::Receiver<()>,
    failed: Arc<AtomicBool>,
    latency: u32,
    ready: &std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();
    let mainloop = MainLoopRc::new(None).map_err(|e| format!("PipeWire main loop: {e}"))?;
    let context = ContextRc::new(&mainloop, None).map_err(|e| format!("PipeWire context: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("Cannot connect to PipeWire: {e}"))?;

    let f_core = failed.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _seq, _res, message| {
            if id == pw::core::PW_ID_CORE {
                tracing::warn!(message, "PipeWire core error");
                f_core.store(true, Ordering::Release);
            }
        })
        .register();

    let mut props = PropertiesBox::new();
    // `Audio/Source/Virtual` on a client stream crashes audioconvert (PipeWire 1.6); a plain
    // source class works and is listed by Discord, browsers and pavucontrol alike.
    props.insert(*pw::keys::MEDIA_CLASS, "Audio/Source");
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Playback");
    props.insert(*pw::keys::MEDIA_ROLE, "Communication");
    props.insert(*pw::keys::NODE_NAME, NODE_NAME);
    props.insert(*pw::keys::NODE_DESCRIPTION, NODE_DESCRIPTION);
    props.insert(*pw::keys::MEDIA_NAME, NODE_DESCRIPTION);
    props.insert(*pw::keys::APP_NAME, "Clipdeck");
    props.insert(*pw::keys::MEDIA_ICON_NAME, "audio-input-microphone");
    props.insert(*pw::keys::NODE_LATENCY, format!("{latency}/{RATE}"));
    props.insert(*pw::keys::NODE_ALWAYS_PROCESS, "true");
    props.insert("audio.position", "FL,FR");
    props.insert("audio.channels", CHANNELS.to_string());
    props.insert("audio.rate", RATE.to_string());

    let stream = StreamRc::new(core.clone(), NODE_DESCRIPTION, props)
        .map_err(|e| format!("PipeWire stream: {e}"))?;
    let data = UserData {
        render,
        scratch: vec![0.0; 8192 * CHANNELS as usize],
        failed: failed.clone(),
    };
    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(|_s, ud, _old, new| {
            if let StreamState::Error(e) = new {
                tracing::warn!(e, "Clipdeck Mic stream error");
                ud.failed.store(true, Ordering::Release);
            }
        })
        .process(|stream, ud| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let requested = buffer.requested() as usize;
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let stride = std::mem::size_of::<f32>() * CHANNELS as usize;
            let max_frames = d.as_raw().maxsize as usize / stride;
            let frames = if requested == 0 {
                max_frames.min(1024)
            } else {
                requested.min(max_frames)
            };
            let Some(bytes) = d.data() else { return };
            let n = frames * CHANNELS as usize;
            let bytes = &mut bytes[..n * 4];
            if (bytes.as_ptr() as usize).is_multiple_of(std::mem::align_of::<f32>()) {
                // SAFETY: the mapped buffer is at least `n * 4` bytes, f32-aligned (checked above),
                // and exclusively ours until it is queued back when `buffer` drops.
                let out =
                    unsafe { std::slice::from_raw_parts_mut(bytes.as_mut_ptr().cast::<f32>(), n) };
                (ud.render)(out, CHANNELS as usize);
            } else {
                let n = n.min(ud.scratch.len());
                let s = &mut ud.scratch[..n];
                (ud.render)(s, CHANNELS as usize);
                for (chunk, v) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(s.iter()) {
                    chunk.copy_from_slice(&v.to_le_bytes());
                }
            }
            let chunk = d.chunk_mut();
            *chunk.offset_mut() = 0;
            *chunk.stride_mut() = stride as i32;
            *chunk.size_mut() = (frames * stride) as u32;
        })
        .register()
        .map_err(|e| format!("PipeWire listener: {e}"))?;

    let mut info = AudioInfoRaw::new();
    info.set_format(AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(CHANNELS);
    let mut position = [0u32; pw::spa::param::audio::MAX_CHANNELS];
    position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let obj = Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let bytes: Vec<u8> =
        PodSerializer::serialize(std::io::Cursor::new(Vec::new()), &Value::Object(obj))
            .map_err(|e| format!("PipeWire format: {e:?}"))?
            .0
            .into_inner();
    let pod = Pod::from_bytes(&bytes).ok_or("PipeWire format pod")?;
    let mut params = [pod];
    stream
        .connect(
            Direction::Output,
            None,
            StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|e| format!("PipeWire connect: {e}"))?;

    let ml = mainloop.clone();
    let _quit = quit_rx.attach(mainloop.loop_(), move |()| ml.quit());
    let _ = ready.send(Ok(()));
    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}
