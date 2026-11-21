//! Waveform peaks for the trim editor.

use std::path::Path;

use crate::decode;
use crate::error::Result;

/// Min/max peaks per bucket over the whole file (mono mix).
#[derive(Debug, Clone, PartialEq)]
pub struct Waveform {
    /// `(min, max)` per bucket in -1..1.
    pub buckets: Vec<(f32, f32)>,
    /// Duration of the file in ms.
    pub duration_ms: u32,
}

/// Compute `n` buckets for `path`.
pub fn compute(path: &Path, n: usize) -> Result<Waveform> {
    let d = decode::decode_native(path)?;
    Ok(Waveform {
        buckets: peaks(&d.samples, n),
        duration_ms: d.duration_ms(),
    })
}

/// Min/max of interleaved stereo in `n` buckets.
pub fn peaks(stereo: &[f32], n: usize) -> Vec<(f32, f32)> {
    let frames = stereo.len() / 2;
    let n = n.max(1);
    let mut out = vec![(0f32, 0f32); n];
    if frames == 0 {
        return out;
    }
    for (i, f) in stereo.as_chunks::<2>().0.iter().enumerate() {
        let b = i * n / frames;
        let m = (f[0] + f[1]) * 0.5;
        let e = &mut out[b.min(n - 1)];
        e.0 = e.0.min(m);
        e.1 = e.1.max(m);
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn buckets() {
        let s = [0.5, 0.5, -0.5, -0.5, 1.0, 1.0, 0.0, 0.0];
        let p = super::peaks(&s, 2);
        assert_eq!(p, vec![(-0.5, 0.5), (0.0, 1.0)]);
    }
}
