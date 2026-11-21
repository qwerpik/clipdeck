//! Sample-rate conversion: high quality (rubato FFT) for clips, allocation-free linear
//! interpolation for live microphone capture.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::error::{AudioError, Result};

/// Resample interleaved stereo `input` from `from` Hz to `to` Hz (whole clip, offline).
pub fn resample_stereo(input: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    if from == to || input.is_empty() {
        return Ok(input.to_vec());
    }
    let frames = input.len() / 2;
    let mut r = Fft::<f32>::new(from as usize, to as usize, 1024, 2, FixedSync::Input)
        .map_err(|e| AudioError::Resample(e.to_string()))?;
    let adapter = InterleavedSlice::new(&input[..frames * 2], 2, frames)
        .map_err(|e| AudioError::Resample(e.to_string()))?;
    let out = r
        .process_all(&adapter, frames, None)
        .map_err(|e| AudioError::Resample(e.to_string()))?;
    Ok(out.take_data())
}

/// Streaming linear resampler for interleaved stereo; no allocation after construction.
#[derive(Debug, Clone)]
pub struct LinearResampler {
    step: f64,
    pos: f64,
    prev: [f32; 2],
    have_prev: bool,
}

impl LinearResampler {
    /// Convert from `from` Hz to `to` Hz.
    pub fn new(from: u32, to: u32) -> Self {
        LinearResampler {
            step: f64::from(from) / f64::from(to.max(1)),
            pos: 0.0,
            prev: [0.0; 2],
            have_prev: false,
        }
    }

    /// Whether the rates are equal (pass-through).
    pub fn is_identity(&self) -> bool {
        (self.step - 1.0).abs() < 1e-9
    }

    /// Feed stereo frames, calling `emit(l, r)` for each output frame.
    pub fn process(&mut self, input: &[f32], mut emit: impl FnMut(f32, f32)) {
        if self.is_identity() {
            for f in input.as_chunks::<2>().0 {
                emit(f[0], f[1]);
            }
            return;
        }
        for f in input.as_chunks::<2>().0 {
            let cur = [f[0], f[1]];
            if !self.have_prev {
                self.prev = cur;
                self.have_prev = true;
                continue;
            }
            // Emit every output sample whose position falls between prev (0.0) and cur (1.0).
            while self.pos < 1.0 {
                let t = self.pos as f32;
                emit(
                    self.prev[0] + (cur[0] - self.prev[0]) * t,
                    self.prev[1] + (cur[1] - self.prev[1]) * t,
                );
                self.pos += self.step;
            }
            self.pos -= 1.0;
            self.prev = cur;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_resample_length_and_dc() {
        let input = vec![0.5f32; 44_100 * 2];
        let out = resample_stereo(&input, 44_100, 48_000).unwrap();
        let frames = out.len() / 2;
        assert!((frames as i64 - 48_000).abs() < 64, "{frames}");
        let mid = out[frames];
        assert!((mid - 0.5).abs() < 0.01, "{mid}");
    }

    #[test]
    fn linear_ratio() {
        let mut r = LinearResampler::new(48_000, 44_100);
        let input = vec![0.25f32; 48_000 * 2];
        let mut n = 0usize;
        r.process(&input, |l, rr| {
            assert!((l - 0.25).abs() < 1e-6 && (rr - 0.25).abs() < 1e-6);
            n += 1;
        });
        assert!((n as i64 - 44_100).abs() <= 2, "{n}");
        let mut id = LinearResampler::new(48_000, 48_000);
        let mut m = 0;
        id.process(&input[..20], |_, _| m += 1);
        assert_eq!(m, 10);
    }
}
