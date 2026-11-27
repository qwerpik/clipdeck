//! The recorder: capture a stereo stream into memory, then trim, normalize and save a WAV.
//!
//! The capture stream is opened *inside* the recorder thread (cpal streams are not `Send`
//! everywhere), through an [`OpenCapture`] closure supplied by the platform layer: the
//! microphone, WASAPI loopback on Windows, or the default sink's monitor on PipeWire.

use std::any::Any;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use rtrb::Consumer;

/// A live capture: a guard that keeps the stream alive, the consumer end of its interleaved
/// stereo ring, and the ring's sample rate.
pub type Capture = (Box<dyn Any>, Consumer<f32>, u32);

/// Opens the capture stream on the recorder thread.
pub type OpenCapture = Box<dyn FnOnce() -> Result<Capture, String> + Send>;

/// What to do with the audio once recording stops.
#[derive(Debug, Clone)]
pub struct RecordOptions {
    /// Output file (`.wav`).
    pub path: PathBuf,
    /// Cut silence at both ends.
    pub trim_silence: bool,
    /// Scale so the peak sits at -1 dBFS (at most +30 dB).
    pub normalize: bool,
    /// Stop on its own after this many seconds (0 = 1 hour).
    pub max_seconds: u32,
}

/// A saved recording.
#[derive(Debug, Clone, PartialEq)]
pub struct Finished {
    /// File written.
    pub path: PathBuf,
    /// Length after trimming.
    pub duration_ms: u64,
}

#[derive(Debug, Default)]
struct Shared {
    frames: AtomicU64,
    peak: AtomicU32,
    rate: AtomicU32,
    done: AtomicBool,
}

/// A running recording. Dropping it without [`Recorder::stop`] discards the audio.
pub struct Recorder {
    stop: Arc<AtomicBool>,
    discard: Arc<AtomicBool>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<Result<Finished, String>>>,
}

impl std::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Recorder")
            .field("elapsed_ms", &self.elapsed_ms())
            .finish()
    }
}

/// Silence threshold for trimming (-50 dBFS).
const SILENCE: f32 = 0.003_16;
/// Audio kept around the first/last loud sample when trimming.
const TRIM_PAD_MS: u64 = 100;
/// Normalization target peak (-1 dBFS).
const TARGET_PEAK: f32 = 0.891;
/// Normalization never boosts more than +30 dB.
const MAX_BOOST: f32 = 31.6;

impl Recorder {
    /// Open the capture and start recording. Fails if the stream cannot be opened.
    pub fn start(open: OpenCapture, opts: RecordOptions) -> Result<Recorder, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let discard = Arc::new(AtomicBool::new(false));
        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        let (s, d, sh) = (stop.clone(), discard.clone(), shared.clone());
        let thread = std::thread::Builder::new()
            .name("clipdeck-recorder".into())
            .spawn(move || run(open, opts, &s, &d, &sh, &ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(())) => Ok(Recorder {
                stop,
                discard,
                shared,
                thread: Some(thread),
            }),
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                stop.store(true, Ordering::Release);
                discard.store(true, Ordering::Release);
                Err("the recording device did not start".into())
            }
        }
    }

    /// Recorded time so far.
    pub fn elapsed_ms(&self) -> u64 {
        let rate = u64::from(self.shared.rate.load(Ordering::Relaxed).max(1));
        self.shared.frames.load(Ordering::Relaxed) * 1000 / rate
    }

    /// Peak level since the last call (0..1).
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.shared.peak.swap(0, Ordering::Relaxed))
    }

    /// True once the recording stopped by itself (length limit or device lost).
    pub fn is_done(&self) -> bool {
        self.shared.done.load(Ordering::Acquire)
    }

    /// Stop, process and save. Blocks while the file is written.
    pub fn stop(mut self) -> Result<Finished, String> {
        self.stop.store(true, Ordering::Release);
        match self.thread.take().map(JoinHandle::join) {
            Some(Ok(r)) => r,
            Some(Err(_)) => Err("the recorder crashed".into()),
            None => Err("not recording".into()),
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(t) = self.thread.take() {
            self.discard.store(true, Ordering::Release);
            self.stop.store(true, Ordering::Release);
            let _ = t.join();
        }
    }
}

fn run(
    open: OpenCapture,
    opts: RecordOptions,
    stop: &AtomicBool,
    discard: &AtomicBool,
    shared: &Shared,
    ready: &std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<Finished, String> {
    let (guard, mut rx, rate) = match open() {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }
    };
    shared.rate.store(rate, Ordering::Relaxed);
    let _ = ready.send(Ok(()));
    let max_s = if opts.max_seconds == 0 {
        3600
    } else {
        opts.max_seconds
    };
    let max_samples = u64::from(max_s) * u64::from(rate) * 2;
    let mut samples: Vec<f32> = Vec::with_capacity(rate as usize * 2 * 10);
    loop {
        let stopping = stop.load(Ordering::Acquire);
        let mut peak = f32::from_bits(shared.peak.load(Ordering::Relaxed));
        while let Ok(v) = rx.pop() {
            if samples.len() as u64 >= max_samples {
                break;
            }
            peak = peak.max(v.abs());
            samples.push(v);
        }
        shared.peak.store(peak.to_bits(), Ordering::Relaxed);
        shared
            .frames
            .store(samples.len() as u64 / 2, Ordering::Relaxed);
        if samples.len() as u64 >= max_samples {
            shared.done.store(true, Ordering::Release);
        }
        if stopping {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    drop(guard);
    if discard.load(Ordering::Acquire) {
        return Err("discarded".into());
    }
    if samples.len() % 2 == 1 {
        samples.pop();
    }
    finish(samples, rate, &opts)
}

/// Trim, normalize and write `samples` (interleaved stereo).
pub fn finish(mut samples: Vec<f32>, rate: u32, opts: &RecordOptions) -> Result<Finished, String> {
    if opts.trim_silence {
        let (a, b) = trim_bounds(&samples, rate);
        if a >= b {
            return Err("The recording was silent, so nothing was saved".into());
        }
        samples.truncate(b);
        samples.drain(..a);
    }
    if samples.is_empty() {
        return Err("Nothing was recorded".into());
    }
    if opts.normalize {
        normalize(&mut samples);
    }
    write_wav(&opts.path, &samples, rate, 2)
        .map_err(|e| format!("Cannot save the recording to {}: {e}", opts.path.display()))?;
    Ok(Finished {
        path: opts.path.clone(),
        duration_ms: samples.len() as u64 / 2 * 1000 / u64::from(rate.max(1)),
    })
}

/// Sample range `[start, end)` (stereo-aligned) without leading/trailing silence, with
/// [`TRIM_PAD_MS`] of padding. `start >= end` when everything is silent.
pub fn trim_bounds(samples: &[f32], rate: u32) -> (usize, usize) {
    let first = samples.iter().position(|v| v.abs() > SILENCE);
    let last = samples.iter().rposition(|v| v.abs() > SILENCE);
    let (Some(first), Some(last)) = (first, last) else {
        return (0, 0);
    };
    let pad = (u64::from(rate) * TRIM_PAD_MS / 1000) as usize * 2;
    let start = (first / 2 * 2).saturating_sub(pad);
    let end = ((last / 2 + 1) * 2 + pad).min(samples.len() / 2 * 2);
    (start, end)
}

/// Scale so the peak sits at -1 dBFS, boosting at most +30 dB.
pub fn normalize(samples: &mut [f32]) {
    let peak = samples.iter().fold(0f32, |m, v| m.max(v.abs()));
    if peak <= 0.0 {
        return;
    }
    let gain = (TARGET_PEAK / peak).min(MAX_BOOST);
    for v in samples {
        *v *= gain;
    }
}

/// Write 16-bit PCM WAV.
pub fn write_wav(path: &Path, samples: &[f32], rate: u32, channels: u16) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data_len = u32::try_from(samples.len() * 2)
        .map_err(|_| std::io::Error::other("recording too long for WAV"))?;
    let tmp = path.with_extension("wav.part");
    let mut f = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    let block = channels * 2;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&channels.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * u32::from(block)).to_le_bytes())?;
    f.write_all(&block.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for v in samples {
        let s = (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

/// A file name like `Recording 2026-11-27 21-05-33.wav` that does not exist yet in `dir`.
pub fn unique_path(dir: &Path, stamp: &str) -> PathBuf {
    let base = format!("Recording {stamp}");
    let mut p = dir.join(format!("{base}.wav"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{base} ({n}).wav"));
        n += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, amp: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = amp * (i as f32 * 0.05).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn trims_silence_with_padding() {
        let rate = 1000;
        let mut s = vec![0.0; 2 * 500];
        s.extend(tone(300, 0.5));
        s.extend(vec![0.0; 2 * 700]);
        let (a, b) = trim_bounds(&s, rate);
        assert_eq!(a % 2, 0);
        assert_eq!(b % 2, 0);
        // 100 ms pad = 100 frames at 1 kHz.
        assert!((395 * 2..=401 * 2).contains(&a), "{a}");
        assert!((899 * 2..=902 * 2).contains(&b), "{b}");
        assert_eq!(trim_bounds(&[0.0; 64], rate), (0, 0));
    }

    #[test]
    fn normalizes_to_minus_one_db_with_a_boost_cap() {
        let mut s = tone(1000, 0.25);
        normalize(&mut s);
        let peak = s.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!((peak - TARGET_PEAK).abs() < 1e-3);
        let mut quiet = tone(1000, 0.0001);
        normalize(&mut quiet);
        let peak = quiet.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak < 0.0001 * MAX_BOOST + 1e-6);
    }

    #[test]
    fn records_from_a_ring_and_writes_a_decodable_wav() {
        let dir = std::env::temp_dir().join(format!("clipdeck-rec-{}", std::process::id()));
        let path = dir.join("r.wav");
        let open: OpenCapture = Box::new(|| {
            let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(48_000 * 2);
            for v in vec![0.0; 2 * 4800].into_iter().chain(tone(9600, 0.5)) {
                let _ = tx.push(v);
            }
            Ok((Box::new(tx) as Box<dyn Any>, rx, 48_000))
        });
        let rec = Recorder::start(
            open,
            RecordOptions {
                path: path.clone(),
                trim_silence: true,
                normalize: true,
                max_seconds: 10,
            },
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(80));
        assert!(rec.elapsed_ms() >= 290, "{}", rec.elapsed_ms());
        let done = rec.stop().unwrap();
        // 200 ms tone + 100 ms lead-in pad (the tail pad is cut by the end of the data).
        assert!(
            (290..=310).contains(&done.duration_ms),
            "{}",
            done.duration_ms
        );
        let clip = crate::clip::Clip::load(&path, 48_000).unwrap();
        assert_eq!(clip.frames() as u64 / 48, done.duration_ms);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn silent_recording_is_not_saved() {
        let open: OpenCapture = Box::new(|| {
            let (mut tx, rx) = rtrb::RingBuffer::<f32>::new(4800);
            for _ in 0..4800 {
                let _ = tx.push(0.0);
            }
            Ok((Box::new(tx) as Box<dyn Any>, rx, 48_000))
        });
        let path = std::env::temp_dir().join("clipdeck-silent-never.wav");
        let rec = Recorder::start(
            open,
            RecordOptions {
                path: path.clone(),
                trim_silence: true,
                normalize: false,
                max_seconds: 0,
            },
        )
        .unwrap();
        assert!(rec.stop().is_err());
        assert!(!path.exists());
    }

    #[test]
    fn unique_names() {
        let dir = std::env::temp_dir().join(format!("clipdeck-uniq-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = unique_path(&dir, "x");
        std::fs::write(&a, b"").unwrap();
        let b = unique_path(&dir, "x");
        assert_ne!(a, b);
        assert!(b.to_string_lossy().ends_with("Recording x (2).wav"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
