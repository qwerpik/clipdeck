//! EBU R128 loudness measurement, normalization gain, and the on-disk loudness cache.

use std::collections::HashMap;
use std::path::Path;

use ebur128::{EbuR128, Mode};

/// Integrated loudness of interleaved stereo audio in LUFS (`None` for silence / too short).
pub fn integrated_lufs(stereo: &[f32], rate: u32) -> Option<f32> {
    if stereo.len() < 2 || rate == 0 {
        return None;
    }
    let mut m = EbuR128::new(2, rate, Mode::I).ok()?;
    m.add_frames_f32(stereo).ok()?;
    let l = m.loudness_global().ok()?;
    (l.is_finite() && l > -70.0).then_some(l as f32)
}

/// Linear gain that brings a clip of `lufs` to `target`, capped at +12 dB and so the sample
/// `peak` stays at or below -1 dBFS.
pub fn normalization_gain(lufs: Option<f32>, peak: f32, target: f32) -> f32 {
    let Some(l) = lufs else { return 1.0 };
    let db = (target - l).clamp(-30.0, 12.0);
    let mut g = 10f32.powf(db / 20.0);
    let ceiling = 10f32.powf(-1.0 / 20.0);
    if peak > 0.0 && peak * g > ceiling && g > 1.0 {
        g = (ceiling / peak).max(1.0);
    }
    g
}

/// Persistent `path|mtime|size → LUFS` cache so files are analyzed once.
#[derive(Debug, Default)]
pub struct LoudnessCache {
    map: HashMap<String, f32>,
    dirty: bool,
}

/// Cache key for a file (path + modification time + size).
pub fn file_key(path: &Path) -> Option<String> {
    let md = std::fs::metadata(path).ok()?;
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(format!("{}|{}|{}", path.display(), mtime, md.len()))
}

impl LoudnessCache {
    /// Load from a `key\tlufs` text file (missing/corrupt lines ignored).
    pub fn load(path: &Path) -> Self {
        let mut map = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Some((k, v)) = line.rsplit_once('\t')
                    && let Ok(v) = v.parse::<f32>()
                {
                    map.insert(k.to_string(), v);
                }
            }
        }
        LoudnessCache { map, dirty: false }
    }

    /// Save if changed.
    pub fn save(&mut self, path: &Path) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        let mut s = String::new();
        for (k, v) in &self.map {
            s.push_str(k);
            s.push('\t');
            s.push_str(&format!("{v:.2}\n"));
        }
        if let Some(d) = path.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, s)?;
        std::fs::rename(tmp, path)?;
        self.dirty = false;
        Ok(())
    }

    /// Cached loudness for a file, if its key matches.
    pub fn get(&self, path: &Path) -> Option<f32> {
        self.map.get(&file_key(path)?).copied()
    }

    /// Record loudness for a file.
    pub fn put(&mut self, path: &Path, lufs: f32) {
        if let Some(k) = file_key(path) {
            self.map.insert(k, lufs);
            self.dirty = true;
        }
    }

    /// Entries.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, rate: u32, secs: f32) -> Vec<f32> {
        let n = (rate as f32 * secs) as usize;
        let mut v = Vec::with_capacity(n * 2);
        for i in 0..n {
            let s = amp * (i as f32 * 2.0 * std::f32::consts::PI * 1000.0 / rate as f32).sin();
            v.push(s);
            v.push(s);
        }
        v
    }

    #[test]
    fn louder_signal_measures_louder_and_gain_moves_toward_target() {
        let quiet = integrated_lufs(&sine(0.05, 48_000, 3.0), 48_000).unwrap();
        let loud = integrated_lufs(&sine(0.5, 48_000, 3.0), 48_000).unwrap();
        assert!((loud - quiet - 20.0).abs() < 0.5, "{quiet} {loud}");
        assert!(integrated_lufs(&vec![0.0; 96_000], 48_000).is_none());
        let g = normalization_gain(Some(-26.0), 0.1, -16.0);
        assert!((g - 3.162).abs() < 0.01);
        // peak limited: 0.9 peak cannot be raised by +10 dB
        let g2 = normalization_gain(Some(-26.0), 0.9, -16.0);
        assert!(g2 * 0.9 <= 0.9 && g2 >= 1.0);
        assert!(normalization_gain(Some(-6.0), 1.0, -16.0) < 0.4);
        assert_eq!(normalization_gain(None, 1.0, -16.0), 1.0);
    }

    #[test]
    fn cache_persists() {
        let dir = std::env::temp_dir().join(format!("clipdeck-loud-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.wav");
        std::fs::write(&f, b"x").unwrap();
        let cache_file = dir.join("loudness.tsv");
        let mut c = LoudnessCache::default();
        c.put(&f, -18.5);
        c.save(&cache_file).unwrap();
        let c2 = LoudnessCache::load(&cache_file);
        assert_eq!(c2.get(&f), Some(-18.5));
        std::fs::write(&f, b"changed size").unwrap();
        assert_eq!(c2.get(&f), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
