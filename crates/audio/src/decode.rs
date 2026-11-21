//! File decoding to interleaved stereo `f32` at a target sample rate.

use std::fs::File;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::error::{AudioError, Result};
use crate::resample;

/// Decoded audio: interleaved stereo at `rate`.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    /// Interleaved L/R samples.
    pub samples: Vec<f32>,
    /// Sample rate.
    pub rate: u32,
}

impl Decoded {
    /// Frame count.
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }
    /// Duration in milliseconds.
    pub fn duration_ms(&self) -> u32 {
        if self.rate == 0 {
            return 0;
        }
        (self.frames() as u64 * 1000 / u64::from(self.rate)) as u32
    }
}

fn open_reader(path: &Path) -> Result<Box<dyn FormatReader>> {
    let file = File::open(path).map_err(|e| AudioError::Open {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| AudioError::Decode {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
}

/// Decode a whole file at its native rate into interleaved stereo.
pub fn decode_native(path: &Path) -> Result<Decoded> {
    decode_native_limited(path, None)
}

/// Like [`decode_native`], stopping after `max_frames` frames when given.
pub fn decode_native_limited(path: &Path, max_frames: Option<usize>) -> Result<Decoded> {
    let derr = |m: String| AudioError::Decode {
        path: path.to_path_buf(),
        message: m,
    };
    let mut format = open_reader(path)?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| derr("no audio track".into()))?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| derr("no audio codec parameters".into()))?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| derr(e.to_string()))?;

    let mut out: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut rate = params.sample_rate.unwrap_or(0);
    let mut errors = 0usize;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => {
                if out.is_empty() {
                    return Err(derr(e.to_string()));
                }
                tracing::debug!(%e, path = %path.display(), "packet read stopped");
                break;
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        let buf = match decoder.decode(&packet) {
            Ok(b) => b,
            Err(SymError::DecodeError(e)) => {
                errors += 1;
                tracing::debug!(%e, "skipping undecodable packet");
                if errors > 64 && out.is_empty() {
                    return Err(derr(e.to_string()));
                }
                continue;
            }
            Err(e) => {
                if out.is_empty() {
                    return Err(derr(e.to_string()));
                }
                break;
            }
        };
        let spec = buf.spec();
        rate = spec.rate();
        let channels = spec.channels().count().max(1);
        scratch.resize(buf.samples_interleaved(), 0.0);
        buf.copy_to_slice_interleaved(&mut scratch);
        to_stereo(&scratch, channels, &mut out);
        if let Some(max) = max_frames
            && out.len() / 2 >= max
        {
            out.truncate(max * 2);
            break;
        }
    }
    if rate == 0 {
        return Err(derr("unknown sample rate".into()));
    }
    if out.is_empty() {
        return Err(derr("no audio data".into()));
    }
    Ok(Decoded { samples: out, rate })
}

/// Decode and resample to `target_rate`.
pub fn decode_to(path: &Path, target_rate: u32) -> Result<Decoded> {
    let d = decode_native(path)?;
    if d.rate == target_rate {
        return Ok(d);
    }
    let samples = resample::resample_stereo(&d.samples, d.rate, target_rate)?;
    Ok(Decoded {
        samples,
        rate: target_rate,
    })
}

/// Probe the duration without decoding everything (falls back to decoding).
pub fn probe_duration_ms(path: &Path) -> Result<u32> {
    let format = open_reader(path)?;
    if let Some(track) = format.default_track(TrackType::Audio)
        && let (Some(tb), Some(n)) = (track.time_base, track.num_frames)
        && let Some(t) = tb.calc_time(symphonia::core::units::Timestamp::from(n as i64))
    {
        let ms = (t.as_secs_f64() * 1000.0).round() as u32;
        if ms > 0 {
            return Ok(ms);
        }
    }
    Ok(decode_native(path)?.duration_ms())
}

/// Append `channels`-interleaved samples to `out` as stereo (mono duplicated, extra channels
/// folded into L/R).
pub fn to_stereo(input: &[f32], channels: usize, out: &mut Vec<f32>) {
    match channels {
        1 => {
            out.reserve(input.len() * 2);
            for &s in input {
                out.push(s);
                out.push(s);
            }
        }
        2 => out.extend_from_slice(input),
        n => {
            out.reserve(input.len() / n * 2);
            for frame in input.chunks_exact(n) {
                // FL, FR, then fold the rest in at -3 dB, alternating sides (C/LFE go to both).
                let mut l = frame[0];
                let mut r = frame[1];
                for (i, &s) in frame.iter().enumerate().skip(2) {
                    let g = 0.707;
                    match i {
                        2 | 3 => {
                            l += s * g;
                            r += s * g;
                        }
                        _ if i % 2 == 0 => l += s * g,
                        _ => r += s * g,
                    }
                }
                out.push(l);
                out.push(r);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a 16-bit PCM WAV for tests.
    pub(crate) fn write_wav(path: &Path, rate: u32, channels: u16, frames: &[i16]) {
        let data_len = (frames.len() * 2) as u32;
        let mut b = Vec::new();
        b.extend_from_slice(b"RIFF");
        b.extend_from_slice(&(36 + data_len).to_le_bytes());
        b.extend_from_slice(b"WAVEfmt ");
        b.extend_from_slice(&16u32.to_le_bytes());
        b.extend_from_slice(&1u16.to_le_bytes());
        b.extend_from_slice(&channels.to_le_bytes());
        b.extend_from_slice(&rate.to_le_bytes());
        b.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        b.extend_from_slice(&(channels * 2).to_le_bytes());
        b.extend_from_slice(&16u16.to_le_bytes());
        b.extend_from_slice(b"data");
        b.extend_from_slice(&data_len.to_le_bytes());
        for s in frames {
            b.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(path, b).unwrap();
    }

    #[test]
    fn decodes_mono_wav_and_resamples() {
        let dir = std::env::temp_dir().join(format!("clipdeck-dec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("tone.wav");
        let frames: Vec<i16> = (0..44_100)
            .map(|i| ((i as f32 * 0.05).sin() * 16_000.0) as i16)
            .collect();
        write_wav(&p, 44_100, 1, &frames);
        let d = decode_native(&p).unwrap();
        assert_eq!(d.rate, 44_100);
        assert_eq!(d.frames(), 44_100);
        assert_eq!(d.samples[0], d.samples[1]);
        assert_eq!(d.duration_ms(), 1000);
        let r = decode_to(&p, 48_000).unwrap();
        assert_eq!(r.rate, 48_000);
        assert!(
            (r.frames() as i64 - 48_000).abs() < 64,
            "{} frames",
            r.frames()
        );
        assert_eq!(probe_duration_ms(&p).unwrap(), 1000);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_garbage_and_missing() {
        let dir = std::env::temp_dir().join(format!("clipdeck-dec2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("junk.mp3");
        std::fs::write(&p, b"definitely not audio").unwrap();
        assert!(matches!(decode_native(&p), Err(AudioError::Decode { .. })));
        assert!(matches!(
            decode_native(&dir.join("nope.wav")),
            Err(AudioError::Open { .. })
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn downmix_51() {
        let mut out = Vec::new();
        to_stereo(&[1.0, 0.5, 0.0, 0.0, 0.2, 0.4], 6, &mut out);
        assert_eq!(out.len(), 2);
        assert!((out[0] - (1.0 + 0.2 * 0.707)).abs() < 1e-6);
        assert!((out[1] - (0.5 + 0.4 * 0.707)).abs() < 1e-6);
    }
}
