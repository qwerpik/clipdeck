//! Decoded clips and the memory-bounded clip cache.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::decode;
use crate::error::Result;
use crate::loudness;

/// An immutable decoded clip: interleaved stereo `f32` at `rate`.
#[derive(Debug)]
pub struct Clip {
    /// Interleaved L/R samples.
    pub samples: Box<[f32]>,
    /// Sample rate.
    pub rate: u32,
    /// Integrated loudness (LUFS), if measurable.
    pub lufs: Option<f32>,
    /// Sample peak (linear).
    pub peak: f32,
}

impl Clip {
    /// Frames in the clip.
    pub fn frames(&self) -> usize {
        self.samples.len() / 2
    }
    /// Duration in ms.
    pub fn duration_ms(&self) -> u32 {
        (self.frames() as u64 * 1000 / u64::from(self.rate.max(1))) as u32
    }
    /// Build from decoded audio, measuring loudness.
    pub fn from_decoded(d: decode::Decoded) -> Clip {
        let lufs = loudness::integrated_lufs(&d.samples, d.rate);
        let peak = d.samples.iter().fold(0f32, |m, s| m.max(s.abs()));
        Clip {
            samples: d.samples.into_boxed_slice(),
            rate: d.rate,
            lufs,
            peak,
        }
    }
    /// Decode `path` at `rate`.
    pub fn load(path: &Path, rate: u32) -> Result<Clip> {
        Ok(Clip::from_decoded(decode::decode_to(path, rate)?))
    }
    /// Approximate heap size.
    pub fn bytes(&self) -> usize {
        self.samples.len() * std::mem::size_of::<f32>()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    rate: u32,
}

#[derive(Debug)]
struct Entry {
    clip: Arc<Clip>,
    stamp: (Option<SystemTime>, u64),
    last_used: u64,
}

/// LRU cache of decoded clips bounded by total bytes.
#[derive(Debug)]
pub struct ClipCache {
    entries: HashMap<Key, Entry>,
    budget: usize,
    used: usize,
    tick: u64,
}

fn file_stamp(path: &Path) -> (Option<SystemTime>, u64) {
    std::fs::metadata(path)
        .map(|m| (m.modified().ok(), m.len()))
        .unwrap_or((None, 0))
}

impl ClipCache {
    /// Cache holding at most `budget` bytes of samples.
    pub fn new(budget: usize) -> Self {
        ClipCache {
            entries: HashMap::new(),
            budget,
            used: 0,
            tick: 0,
        }
    }

    /// Cached clip if present and the file did not change on disk.
    pub fn get(&mut self, path: &Path, rate: u32) -> Option<Arc<Clip>> {
        self.tick += 1;
        let key = Key {
            path: path.to_path_buf(),
            rate,
        };
        let stamp = file_stamp(path);
        let fresh = self.entries.get(&key).map(|e| e.stamp == stamp)?;
        if !fresh {
            if let Some(e) = self.entries.remove(&key) {
                self.used -= e.clip.bytes();
            }
            return None;
        }
        let e = self.entries.get_mut(&key)?;
        e.last_used = self.tick;
        Some(e.clip.clone())
    }

    /// Insert a clip, evicting least recently used entries past the budget.
    /// Returns evicted clips so the caller can drop them off the audio thread.
    pub fn insert(&mut self, path: &Path, rate: u32, clip: Arc<Clip>) -> Vec<Arc<Clip>> {
        self.tick += 1;
        let key = Key {
            path: path.to_path_buf(),
            rate,
        };
        self.used += clip.bytes();
        if let Some(old) = self.entries.insert(
            key,
            Entry {
                clip,
                stamp: file_stamp(path),
                last_used: self.tick,
            },
        ) {
            self.used -= old.clip.bytes();
        }
        let mut evicted = Vec::new();
        while self.used > self.budget && self.entries.len() > 1 {
            let Some(oldest) = self
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            if let Some(e) = self.entries.remove(&oldest) {
                self.used -= e.clip.bytes();
                evicted.push(e.clip);
            }
        }
        evicted
    }

    /// Bytes in use.
    pub fn used_bytes(&self) -> usize {
        self.used
    }

    /// Number of clips.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.used = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clip(n: usize) -> Arc<Clip> {
        Arc::new(Clip {
            samples: vec![0.0; n].into_boxed_slice(),
            rate: 48_000,
            lufs: None,
            peak: 0.0,
        })
    }

    #[test]
    fn lru_eviction_by_bytes() {
        let mut c = ClipCache::new(1000);
        let a = Path::new("/nonexistent/a");
        let b = Path::new("/nonexistent/b");
        let d = Path::new("/nonexistent/c");
        assert!(c.insert(a, 48_000, clip(100)).is_empty());
        assert!(c.insert(b, 48_000, clip(100)).is_empty());
        assert!(c.get(a, 48_000).is_some());
        let evicted = c.insert(d, 48_000, clip(100));
        assert_eq!(evicted.len(), 1);
        assert!(c.get(b, 48_000).is_none());
        assert!(c.get(a, 48_000).is_some());
        assert_eq!(c.used_bytes(), 800);
        assert!(c.get(a, 44_100).is_none());
    }
}
