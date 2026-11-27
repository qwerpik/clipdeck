//! Record "what you hear": a PipeWire capture stream on the default sink's monitor
//! (`stream.capture.sink = true`), converted by PipeWire to 48 kHz stereo f32.

use std::thread::JoinHandle;

use pipewire as pw;
use pw::context::ContextRc;
use pw::main_loop::MainLoopRc;
use pw::properties::PropertiesBox;
use pw::spa::param::ParamType;
use pw::spa::param::audio::{AudioFormat, AudioInfoRaw};
use pw::spa::pod::serialize::PodSerializer;
use pw::spa::pod::{Object, Pod, Value};
use pw::spa::utils::{Direction, SpaTypes};
use pw::stream::{StreamFlags, StreamRc};
use rtrb::{Producer, RingBuffer};

const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;

/// Keeps the capture running; dropping it stops the stream.
pub struct MonitorCapture {
    quit: Option<pw::channel::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for MonitorCapture {
    fn drop(&mut self) {
        if let Some(q) = self.quit.take() {
            let _ = q.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Start capturing the default output; returns the guard, the stereo ring and its rate.
///
/// `CLIPDECK_RECORD_TARGET` names a node to record instead of the default sink (tests).
pub fn open_monitor() -> Result<(MonitorCapture, rtrb::Consumer<f32>, u32), String> {
    if !super::pwmic::PipeWireMic::available() {
        return Err("PipeWire is not running".into());
    }
    let (tx, rx) = RingBuffer::<f32>::new(RATE as usize * CHANNELS as usize * 2);
    let (quit_tx, quit_rx) = pw::channel::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let thread = std::thread::Builder::new()
        .name("clipdeck-pw-capture".into())
        .spawn(move || {
            if let Err(e) = run(tx, quit_rx, &ready_tx) {
                let _ = ready_tx.send(Err(e));
            }
        })
        .map_err(|e| e.to_string())?;
    match ready_rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(Ok(())) => Ok((
            MonitorCapture {
                quit: Some(quit_tx),
                thread: Some(thread),
            },
            rx,
            RATE,
        )),
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

fn run(
    tx: Producer<f32>,
    quit_rx: pw::channel::Receiver<()>,
    ready: &std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();
    let mainloop = MainLoopRc::new(None).map_err(|e| format!("PipeWire main loop: {e}"))?;
    let context = ContextRc::new(&mainloop, None).map_err(|e| format!("PipeWire context: {e}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| format!("Cannot connect to PipeWire: {e}"))?;

    let mut props = PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Music");
    props.insert(*pw::keys::NODE_NAME, "clipdeck_recorder");
    props.insert(*pw::keys::NODE_DESCRIPTION, "Clipdeck Recorder");
    props.insert(*pw::keys::MEDIA_NAME, "Clipdeck Recorder");
    props.insert(*pw::keys::APP_NAME, "Clipdeck");
    props.insert("stream.capture.sink", "true");
    props.insert("node.dont-reconnect", "false");
    if let Ok(t) = std::env::var("CLIPDECK_RECORD_TARGET") {
        props.insert("target.object", t);
    }

    let stream = StreamRc::new(core.clone(), "Clipdeck Recorder", props)
        .map_err(|e| format!("PipeWire stream: {e}"))?;
    let _listener = stream
        .add_local_listener_with_user_data(tx)
        .process(|stream, tx| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let (offset, size) = {
                let c = d.chunk();
                (c.offset() as usize, c.size() as usize)
            };
            let Some(bytes) = d.data() else { return };
            let end = (offset + size).min(bytes.len());
            let bytes = &bytes[offset.min(end)..end];
            for b in bytes.as_chunks::<4>().0 {
                if tx.is_full() {
                    break;
                }
                let _ = tx.push(f32::from_le_bytes(*b));
            }
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
            Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS | StreamFlags::RT_PROCESS,
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
