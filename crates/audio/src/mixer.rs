//! The real-time mixer.
//!
//! One [`Mixer`] runs inside each output callback (monitor, virtual mic). The control side
//! talks to it only through lock-free SPSC queues ([`rtrb`]) and atomics:
//!
//! * commands in ([`Command`]),
//! * finished voices out (`retire`), so the last `Arc<Clip>` is never dropped on the audio thread,
//! * events out ([`MixerEvent`]),
//! * per-slot playback positions and the output peak via [`MixerShared`].
//!
//! The render path does no allocation, locking, I/O or logging.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use clipdeck_core::model::PlaybackMode;
use rtrb::{Consumer, Producer, RingBuffer};

use crate::clip::Clip;

/// Most voices that can sound at once per output.
pub const MAX_VOICES: usize = 48;
/// Fade length when a voice is stopped, in milliseconds.
const STOP_FADE_MS: f32 = 12.0;

/// Where samples come from.
pub enum Source {
    /// A decoded clip in memory, playing `start..end` frames.
    Memory {
        /// Clip.
        clip: Arc<Clip>,
        /// Next frame to play.
        pos: usize,
        /// First frame (loop start).
        start: usize,
        /// End frame (exclusive).
        end: usize,
    },
    /// A streamed long clip fed by a decoder thread (interleaved stereo).
    Stream {
        /// Samples from the decoder.
        rx: Consumer<f32>,
        /// Set by the decoder when it has pushed everything.
        finished: Arc<AtomicBool>,
        /// Frames played so far.
        played: usize,
        /// Total frames if known.
        total: usize,
    },
}

/// A voice to start.
pub struct VoiceSpec {
    /// Unique voice id (shared by the same trigger across outputs).
    pub voice: u64,
    /// Sound id (for restart semantics and UI).
    pub sound: u64,
    /// Sample source.
    pub source: Source,
    /// Linear gain (per-sound volume × normalization).
    pub gain: f32,
    /// Loop until stopped.
    pub looped: bool,
    /// What to do with other voices.
    pub mode: PlaybackMode,
    /// When the trigger happened, for latency measurement.
    pub triggered: Option<Instant>,
}

/// Control commands.
pub enum Command {
    /// Start a voice.
    Play(VoiceSpec),
    /// Fade out all voices of a sound.
    StopSound(u64),
    /// Fade out one voice.
    StopVoice(u64),
    /// Fade out everything.
    StopAll,
    /// Change a sound's gain on the fly.
    SetSoundGain(u64, f32),
    /// Microphone input ring (or `None` to disconnect).
    SetMicInput(Option<Consumer<f32>>),
}

/// Events sent back to the control thread.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MixerEvent {
    /// A voice produced its first samples; latency from trigger in microseconds.
    Started {
        /// Voice id.
        voice: u64,
        /// Sound id.
        sound: u64,
        /// Trigger-to-render latency (µs) if measured.
        latency_us: Option<u32>,
    },
    /// A voice ended (naturally or stopped).
    Finished {
        /// Voice id.
        voice: u64,
        /// Sound id.
        sound: u64,
    },
    /// A play was dropped because all voice slots were busy.
    Dropped {
        /// Voice id.
        voice: u64,
    },
}

/// Lock-free state readable from any thread.
#[derive(Debug)]
pub struct MixerShared {
    /// Master gain (f32 bits).
    pub master: AtomicU32,
    /// Output gain applied after the master (monitor volume), f32 bits.
    pub output_gain: AtomicU32,
    /// Mic passthrough gain, f32 bits (0 disables).
    pub mic_gain: AtomicU32,
    /// Mute the mic passthrough while a voice plays.
    pub block_voice: AtomicBool,
    /// Global pause.
    pub paused: AtomicBool,
    /// Output enabled (when false the mixer renders silence but keeps voices advancing).
    pub enabled: AtomicBool,
    /// Peak of the last block (f32 bits), for meters.
    pub peak: AtomicU32,
    /// Per-slot `(voice id, sound id, position frames, length frames)`; voice 0 = empty.
    pub slots: [SlotInfo; MAX_VOICES],
    /// Sample rate of this mixer.
    pub rate: AtomicU32,
    /// Callbacks rendered (health check).
    pub callbacks: AtomicU64,
}

/// Public view of one voice slot.
#[derive(Debug, Default)]
pub struct SlotInfo {
    /// Voice id (0 = empty).
    pub voice: AtomicU64,
    /// Sound id.
    pub sound: AtomicU64,
    /// Position in frames.
    pub pos: AtomicU64,
    /// Length in frames (0 = unknown).
    pub len: AtomicU64,
}

fn f(v: &AtomicU32) -> f32 {
    f32::from_bits(v.load(Ordering::Relaxed))
}

impl MixerShared {
    fn new(rate: u32) -> Self {
        MixerShared {
            master: AtomicU32::new(1.0f32.to_bits()),
            output_gain: AtomicU32::new(1.0f32.to_bits()),
            mic_gain: AtomicU32::new(0.0f32.to_bits()),
            block_voice: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            enabled: AtomicBool::new(true),
            peak: AtomicU32::new(0),
            slots: std::array::from_fn(|_| SlotInfo::default()),
            rate: AtomicU32::new(rate),
            callbacks: AtomicU64::new(0),
        }
    }
    /// Set the master gain.
    pub fn set_master(&self, g: f32) {
        self.master.store(g.max(0.0).to_bits(), Ordering::Relaxed);
    }
    /// Set the output gain.
    pub fn set_output_gain(&self, g: f32) {
        self.output_gain
            .store(g.max(0.0).to_bits(), Ordering::Relaxed);
    }
    /// Set the mic passthrough gain.
    pub fn set_mic_gain(&self, g: f32) {
        self.mic_gain.store(g.max(0.0).to_bits(), Ordering::Relaxed);
    }
    /// Last block peak.
    pub fn peak(&self) -> f32 {
        f(&self.peak)
    }
    /// Snapshot of active voices: `(voice, sound, pos_frames, len_frames)`.
    pub fn active(&self) -> Vec<(u64, u64, u64, u64)> {
        self.slots
            .iter()
            .filter_map(|s| {
                let v = s.voice.load(Ordering::Acquire);
                (v != 0).then(|| {
                    (
                        v,
                        s.sound.load(Ordering::Relaxed),
                        s.pos.load(Ordering::Relaxed),
                        s.len.load(Ordering::Relaxed),
                    )
                })
            })
            .collect()
    }
}

struct Voice {
    voice: u64,
    sound: u64,
    source: Source,
    gain: f32,
    current: f32,
    looped: bool,
    /// Remaining fade-out samples; `None` while playing normally.
    fade: Option<u32>,
    fade_len: u32,
    triggered: Option<Instant>,
    started: bool,
}

/// The real-time half; lives inside the output callback.
pub struct Mixer {
    voices: Vec<Voice>,
    commands: Consumer<Command>,
    retire: Producer<Voice>,
    retire_src: Producer<Source>,
    events: Producer<MixerEvent>,
    shared: Arc<MixerShared>,
    mic: Option<Consumer<f32>>,
    mic_retire: Producer<Consumer<f32>>,
    mic_level: f32,
    rate: u32,
    smooth: f32,
}

/// The control half for one mixer.
pub struct MixerHandle {
    /// Command queue into the mixer.
    pub commands: Producer<Command>,
    /// Shared atomics.
    pub shared: Arc<MixerShared>,
    retire: Consumer<Voice>,
    retire_src: Consumer<Source>,
    mic_retire: Consumer<Consumer<f32>>,
    /// Events from the mixer.
    pub events: Consumer<MixerEvent>,
}

impl MixerHandle {
    /// Send a command; returns it back if the queue is full.
    pub fn send(&mut self, cmd: Command) -> Result<(), Command> {
        self.commands
            .push(cmd)
            .map_err(|rtrb::PushError::Full(c)| c)
    }

    /// Drop retired voices/sources here (control thread) and drain events into `out`.
    pub fn housekeeping(&mut self, out: &mut Vec<MixerEvent>) {
        while let Ok(v) = self.retire.pop() {
            drop(v);
        }
        while let Ok(s) = self.retire_src.pop() {
            drop(s);
        }
        while let Ok(m) = self.mic_retire.pop() {
            drop(m);
        }
        while let Ok(e) = self.events.pop() {
            out.push(e);
        }
    }
}

/// Create a mixer pair for an output running at `rate`.
pub fn mixer(rate: u32) -> (Mixer, MixerHandle) {
    let (cmd_tx, cmd_rx) = RingBuffer::new(256);
    let (ret_tx, ret_rx) = RingBuffer::new(MAX_VOICES * 4);
    let (rs_tx, rs_rx) = RingBuffer::new(MAX_VOICES * 2);
    let (mr_tx, mr_rx) = RingBuffer::new(8);
    let (ev_tx, ev_rx) = RingBuffer::new(1024);
    let shared = Arc::new(MixerShared::new(rate));
    let mixer = Mixer {
        voices: Vec::with_capacity(MAX_VOICES),
        commands: cmd_rx,
        retire: ret_tx,
        retire_src: rs_tx,
        events: ev_tx,
        shared: shared.clone(),
        mic: None,
        mic_retire: mr_tx,
        mic_level: 1.0,
        rate,
        smooth: 1.0 - (-1.0 / (0.005 * rate as f32)).exp(),
    };
    let handle = MixerHandle {
        commands: cmd_tx,
        shared,
        retire: ret_rx,
        retire_src: rs_rx,
        mic_retire: mr_rx,
        events: ev_rx,
    };
    (mixer, handle)
}

#[inline]
fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.85;
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = (a - KNEE) / (1.0 - KNEE);
        (KNEE + (1.0 - KNEE) * over.tanh()).copysign(x)
    }
}

impl Mixer {
    /// Sample rate.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    fn fade_len(&self) -> u32 {
        (STOP_FADE_MS * 0.001 * self.rate as f32) as u32
    }

    fn begin_fade(v: &mut Voice, len: u32) {
        if v.fade.is_none() {
            v.fade = Some(len.max(1));
            v.fade_len = len.max(1);
        }
    }

    fn apply_commands(&mut self) {
        let fade = self.fade_len();
        while let Ok(cmd) = self.commands.pop() {
            match cmd {
                Command::Play(spec) => {
                    match spec.mode {
                        PlaybackMode::Overlap => {}
                        PlaybackMode::Restart => {
                            for v in self.voices.iter_mut().filter(|v| v.sound == spec.sound) {
                                Self::begin_fade(v, fade);
                            }
                        }
                        PlaybackMode::StopOthers => {
                            for v in &mut self.voices {
                                Self::begin_fade(v, fade);
                            }
                        }
                    }
                    if self.voices.len() >= MAX_VOICES {
                        // Steal the oldest fading voice, else drop the new one.
                        if let Some(i) = self.voices.iter().position(|v| v.fade.is_some()) {
                            let old = self.voices.swap_remove(i);
                            let _ = self.events.push(MixerEvent::Finished {
                                voice: old.voice,
                                sound: old.sound,
                            });
                            self.retire_voice(old);
                        } else {
                            let _ = self.events.push(MixerEvent::Dropped { voice: spec.voice });
                            if let Err(rtrb::PushError::Full(src)) =
                                self.retire_src.push(spec.source)
                            {
                                drop(src); // only reachable if the control thread stalls
                            }
                            continue;
                        }
                    }
                    self.voices.push(Voice {
                        voice: spec.voice,
                        sound: spec.sound,
                        source: spec.source,
                        gain: spec.gain,
                        current: spec.gain,
                        looped: spec.looped,
                        fade: None,
                        fade_len: 1,
                        triggered: spec.triggered,
                        started: false,
                    });
                }
                Command::StopSound(s) => {
                    for v in self.voices.iter_mut().filter(|v| v.sound == s) {
                        Self::begin_fade(v, fade);
                    }
                }
                Command::StopVoice(id) => {
                    for v in self.voices.iter_mut().filter(|v| v.voice == id) {
                        Self::begin_fade(v, fade);
                    }
                }
                Command::StopAll => {
                    for v in &mut self.voices {
                        Self::begin_fade(v, fade);
                    }
                }
                Command::SetSoundGain(s, g) => {
                    for v in self.voices.iter_mut().filter(|v| v.sound == s) {
                        v.gain = g;
                    }
                }
                Command::SetMicInput(m) => {
                    if let Some(old) = std::mem::replace(&mut self.mic, m)
                        && let Err(rtrb::PushError::Full(old)) = self.mic_retire.push(old)
                    {
                        drop(old);
                    }
                }
            }
        }
    }

    fn retire_voice(&mut self, v: Voice) {
        if let Err(rtrb::PushError::Full(v)) = self.retire.push(v) {
            drop(v); // only reachable if the control thread stalls for a long time
        }
    }

    /// Render one block into `out` (interleaved, `channels` per frame).
    pub fn render(&mut self, out: &mut [f32], channels: usize) {
        self.shared.callbacks.fetch_add(1, Ordering::Relaxed);
        self.apply_commands();
        let channels = channels.max(1);
        out.fill(0.0);
        let frames = out.len() / channels;
        let master = f(&self.shared.master);
        let out_gain = f(&self.shared.output_gain);
        let paused = self.shared.paused.load(Ordering::Relaxed);
        let enabled = self.shared.enabled.load(Ordering::Relaxed);
        let smooth = self.smooth;

        if !paused {
            for v in &mut self.voices {
                if !v.started {
                    v.started = true;
                    let latency_us = v
                        .triggered
                        .map(|t| t.elapsed().as_micros().min(u128::from(u32::MAX)) as u32);
                    let _ = self.events.push(MixerEvent::Started {
                        voice: v.voice,
                        sound: v.sound,
                        latency_us,
                    });
                }
                let mut done = false;
                for i in 0..frames {
                    let (l, r) = match &mut v.source {
                        Source::Memory {
                            clip,
                            pos,
                            start,
                            end,
                        } => {
                            if *pos >= *end {
                                if v.looped && *end > *start {
                                    *pos = *start;
                                } else {
                                    done = true;
                                    break;
                                }
                            }
                            let idx = *pos * 2;
                            *pos += 1;
                            (clip.samples[idx], clip.samples[idx + 1])
                        }
                        Source::Stream {
                            rx,
                            finished,
                            played,
                            ..
                        } => {
                            if rx.slots() >= 2 {
                                let l = rx.pop().unwrap_or(0.0);
                                let r = rx.pop().unwrap_or(0.0);
                                *played += 1;
                                (l, r)
                            } else if finished.load(Ordering::Acquire) {
                                done = true;
                                break;
                            } else {
                                (0.0, 0.0) // decoder underrun: silence, keep going
                            }
                        }
                    };
                    v.current += (v.gain - v.current) * smooth;
                    let mut g = v.current * master;
                    if let Some(rem) = v.fade.as_mut() {
                        g *= *rem as f32 / v.fade_len as f32;
                        if *rem == 0 {
                            done = true;
                            break;
                        }
                        *rem -= 1;
                    }
                    let o = i * channels;
                    if channels == 1 {
                        out[o] += (l + r) * 0.5 * g;
                    } else {
                        out[o] += l * g;
                        out[o + 1] += r * g;
                    }
                }
                if done {
                    v.fade = Some(0);
                    v.looped = false;
                    if let Source::Memory { pos, end, .. } = &mut v.source {
                        *pos = *end;
                    }
                }
            }
        }

        // Retire finished voices (swap_remove keeps this O(n) without allocation).
        let mut i = 0;
        while i < self.voices.len() {
            let finished = self.voices[i].fade == Some(0)
                || matches!(&self.voices[i].source, Source::Memory { pos, end, .. } if *pos >= *end && !self.voices[i].looped);
            if finished {
                let v = self.voices.swap_remove(i);
                let _ = self.events.push(MixerEvent::Finished {
                    voice: v.voice,
                    sound: v.sound,
                });
                self.retire_voice(v);
            } else {
                i += 1;
            }
        }

        // Microphone passthrough.
        let mic_gain = f(&self.shared.mic_gain);
        let blocked = self.shared.block_voice.load(Ordering::Relaxed) && !self.voices.is_empty();
        let target_level = if blocked { 0.0 } else { 1.0 };
        if let Some(mic) = self.mic.as_mut() {
            let available = mic.slots() / 2;
            // Keep latency bounded: if the ring holds more than ~3 blocks, skip ahead.
            let excess = available.saturating_sub(frames * 3);
            for _ in 0..excess * 2 {
                let _ = mic.pop();
            }
            let n = (mic.slots() / 2).min(frames);
            for i in 0..n {
                let l = mic.pop().unwrap_or(0.0);
                let r = mic.pop().unwrap_or(0.0);
                self.mic_level += (target_level - self.mic_level) * smooth;
                let g = mic_gain * self.mic_level;
                let o = i * channels;
                if channels == 1 {
                    out[o] += (l + r) * 0.5 * g;
                } else {
                    out[o] += l * g;
                    out[o + 1] += r * g;
                }
            }
        }

        // Output gain, soft clip, peak, and channel fan-out beyond stereo.
        let gain = if enabled { out_gain } else { 0.0 };
        let mut peak = 0f32;
        for frame in out.chunks_exact_mut(channels) {
            for s in frame.iter_mut().take(2) {
                *s = soft_clip(*s * gain);
                peak = peak.max(s.abs());
            }
        }
        self.shared.peak.store(peak.to_bits(), Ordering::Relaxed);

        // Publish slots.
        for (slot, info) in self.shared.slots.iter().enumerate() {
            match self.voices.get(slot) {
                Some(v) => {
                    let (pos, len) = match &v.source {
                        Source::Memory {
                            pos, start, end, ..
                        } => ((pos - start.min(pos)) as u64, (end - start) as u64),
                        Source::Stream { played, total, .. } => (*played as u64, *total as u64),
                    };
                    info.sound.store(v.sound, Ordering::Relaxed);
                    info.pos.store(pos, Ordering::Relaxed);
                    info.len.store(len, Ordering::Relaxed);
                    info.voice.store(v.voice, Ordering::Release);
                }
                None => info.voice.store(0, Ordering::Release),
            }
        }
    }

    /// Number of active voices (test helper).
    pub fn active_voices(&self) -> usize {
        self.voices.len()
    }
}

/// Create a memory source for a clip, honoring a trim window in milliseconds.
pub fn memory_source(clip: Arc<Clip>, start_ms: u32, end_ms: Option<u32>) -> Source {
    let frames = clip.frames();
    let to_frames = |ms: u32| ((u64::from(ms) * u64::from(clip.rate)) / 1000) as usize;
    let start = to_frames(start_ms).min(frames);
    let end = end_ms.map(to_frames).unwrap_or(frames).clamp(start, frames);
    Source::Memory {
        clip,
        pos: start,
        start,
        end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(frames: usize, value: f32) -> Arc<Clip> {
        Arc::new(Clip {
            samples: vec![value; frames * 2].into_boxed_slice(),
            rate: 1000,
            lufs: None,
            peak: value,
        })
    }

    fn spec(voice: u64, sound: u64, c: Arc<Clip>, mode: PlaybackMode, looped: bool) -> VoiceSpec {
        VoiceSpec {
            voice,
            sound,
            source: memory_source(c, 0, None),
            gain: 1.0,
            looped,
            mode,
            triggered: None,
        }
    }

    fn drain(h: &mut MixerHandle) -> Vec<MixerEvent> {
        let mut v = Vec::new();
        h.housekeeping(&mut v);
        v
    }

    #[test]
    fn plays_to_end_and_reports() {
        let (mut m, mut h) = mixer(1000);
        h.send(Command::Play(spec(
            1,
            7,
            clip(10, 0.5),
            PlaybackMode::Overlap,
            false,
        )))
        .ok();
        let mut out = vec![0.0; 2 * 16];
        m.render(&mut out, 2);
        assert!((out[0] - 0.5).abs() < 1e-6);
        assert_eq!(out[2 * 12], 0.0);
        let ev = drain(&mut h);
        assert!(matches!(
            ev[0],
            MixerEvent::Started {
                voice: 1,
                sound: 7,
                ..
            }
        ));
        assert!(ev.contains(&MixerEvent::Finished { voice: 1, sound: 7 }));
        assert_eq!(m.active_voices(), 0);
    }

    #[test]
    fn overlap_sums_and_soft_clips() {
        let (mut m, mut h) = mixer(1000);
        for i in 0..3 {
            h.send(Command::Play(spec(
                i + 1,
                1,
                clip(100, 0.5),
                PlaybackMode::Overlap,
                false,
            )))
            .ok();
        }
        let mut out = vec![0.0; 2 * 4];
        m.render(&mut out, 2);
        assert_eq!(m.active_voices(), 3);
        assert!(out[0] < 1.0 && out[0] > 0.85, "{}", out[0]);
    }

    #[test]
    fn restart_fades_previous_instance_of_same_sound() {
        let (mut m, mut h) = mixer(1000);
        h.send(Command::Play(spec(
            1,
            1,
            clip(1000, 0.1),
            PlaybackMode::Restart,
            false,
        )))
        .ok();
        h.send(Command::Play(spec(
            2,
            2,
            clip(1000, 0.1),
            PlaybackMode::Restart,
            false,
        )))
        .ok();
        let mut out = vec![0.0; 2 * 4];
        m.render(&mut out, 2);
        h.send(Command::Play(spec(
            3,
            1,
            clip(1000, 0.1),
            PlaybackMode::Restart,
            false,
        )))
        .ok();
        let mut out = vec![0.0; 2 * 64];
        m.render(&mut out, 2);
        // voice 1 faded out (12 ms at 1 kHz = 12 samples), voices 2 and 3 remain
        assert_eq!(m.active_voices(), 2);
        assert!(drain(&mut h).contains(&MixerEvent::Finished { voice: 1, sound: 1 }));
    }

    #[test]
    fn stop_others_and_stop_all() {
        let (mut m, mut h) = mixer(1000);
        h.send(Command::Play(spec(
            1,
            1,
            clip(1000, 0.1),
            PlaybackMode::Overlap,
            false,
        )))
        .ok();
        h.send(Command::Play(spec(
            2,
            2,
            clip(1000, 0.1),
            PlaybackMode::Overlap,
            false,
        )))
        .ok();
        let mut out = vec![0.0; 2 * 4];
        m.render(&mut out, 2);
        h.send(Command::Play(spec(
            3,
            3,
            clip(1000, 0.1),
            PlaybackMode::StopOthers,
            false,
        )))
        .ok();
        let mut out = vec![0.0; 2 * 64];
        m.render(&mut out, 2);
        assert_eq!(m.active_voices(), 1);
        h.send(Command::StopAll).ok();
        m.render(&mut out, 2);
        assert_eq!(m.active_voices(), 0);
    }

    #[test]
    fn loop_wraps_until_stopped() {
        let (mut m, mut h) = mixer(1000);
        h.send(Command::Play(spec(
            1,
            1,
            clip(5, 0.2),
            PlaybackMode::Overlap,
            true,
        )))
        .ok();
        let mut out = vec![0.0; 2 * 50];
        m.render(&mut out, 2);
        assert!(out.iter().all(|s| (s - 0.2).abs() < 1e-6));
        h.send(Command::StopSound(1)).ok();
        m.render(&mut out, 2);
        assert_eq!(m.active_voices(), 0);
    }

    #[test]
    fn master_gain_trim_mono_and_disable() {
        let (mut m, mut h) = mixer(1000);
        h.shared.set_master(0.5);
        let c = clip(1000, 0.4);
        h.send(Command::Play(VoiceSpec {
            voice: 1,
            sound: 1,
            source: memory_source(c, 100, Some(200)),
            gain: 1.0,
            looped: false,
            mode: PlaybackMode::Overlap,
            triggered: None,
        }))
        .ok();
        let mut out = vec![0.0; 150];
        m.render(&mut out, 1);
        assert!((out[0] - 0.2).abs() < 1e-6);
        assert_eq!(out[120], 0.0, "trimmed to 100 frames");
        h.shared.enabled.store(false, Ordering::Relaxed);
        h.send(Command::Play(spec(
            2,
            1,
            clip(100, 0.4),
            PlaybackMode::Overlap,
            false,
        )))
        .ok();
        m.render(&mut out, 1);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn mic_passthrough_and_block_voice() {
        let (mut m, mut h) = mixer(1000);
        let (mut tx, rx) = RingBuffer::new(4096);
        h.send(Command::SetMicInput(Some(rx))).ok();
        h.shared.set_mic_gain(1.0);
        for _ in 0..64 {
            tx.push(0.3).ok();
        }
        let mut out = vec![0.0; 2 * 32];
        m.render(&mut out, 2);
        assert!((out[0] - 0.3).abs() < 1e-6);
        // block voice: mic fades to silence while a sound plays
        h.shared.block_voice.store(true, Ordering::Relaxed);
        h.send(Command::Play(spec(
            1,
            1,
            clip(10_000, 0.0),
            PlaybackMode::Overlap,
            false,
        )))
        .ok();
        for _ in 0..4000 {
            tx.push(0.3).ok();
        }
        let mut out = vec![0.0; 2 * 2000];
        m.render(&mut out, 2);
        assert!(out[2 * 1999].abs() < 0.01, "{}", out[2 * 1999]);
    }

    #[test]
    fn voice_limit_drops_cleanly() {
        let (mut m, mut h) = mixer(1000);
        for i in 0..(MAX_VOICES as u64 + 5) {
            while h
                .send(Command::Play(spec(
                    i + 1,
                    i + 1,
                    clip(1000, 0.0),
                    PlaybackMode::Overlap,
                    false,
                )))
                .is_err()
            {
                let mut out = vec![0.0; 2];
                m.render(&mut out, 2);
            }
        }
        let mut out = vec![0.0; 2];
        m.render(&mut out, 2);
        assert_eq!(m.active_voices(), MAX_VOICES);
        assert!(
            drain(&mut h)
                .iter()
                .any(|e| matches!(e, MixerEvent::Dropped { .. }))
        );
    }

    #[test]
    fn slots_publish_progress() {
        let (mut m, mut h) = mixer(1000);
        h.send(Command::Play(spec(
            9,
            4,
            clip(100, 0.0),
            PlaybackMode::Overlap,
            false,
        )))
        .ok();
        let mut out = vec![0.0; 2 * 25];
        m.render(&mut out, 2);
        assert_eq!(h.shared.active(), vec![(9, 4, 25, 100)]);
    }
}
