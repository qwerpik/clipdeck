//! Instant fuzzy search over sound names, categories and metadata.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

use crate::library::Library;
use crate::model::SoundId;

/// Reusable matcher (keeps its scratch buffers between keystrokes).
pub struct Searcher {
    matcher: Matcher,
    buf: Vec<char>,
}

impl std::fmt::Debug for Searcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Searcher")
    }
}

impl Default for Searcher {
    fn default() -> Self {
        Self::new()
    }
}

impl Searcher {
    /// New matcher.
    pub fn new() -> Self {
        Searcher {
            matcher: Matcher::new(Config::DEFAULT),
            buf: Vec::new(),
        }
    }

    /// Rank `candidates` (or all sounds) by fuzzy match against `query`.
    /// An empty query returns the candidates unchanged.
    pub fn search(
        &mut self,
        lib: &Library,
        query: &str,
        candidates: Option<&[SoundId]>,
    ) -> Vec<SoundId> {
        let all: Vec<SoundId>;
        let ids: &[SoundId] = match candidates {
            Some(c) => c,
            None => {
                all = lib.sounds.iter().map(|s| s.id).collect();
                &all
            }
        };
        let q = query.trim();
        if q.is_empty() {
            return ids.to_vec();
        }
        let pattern = Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart);
        let mut scored: Vec<(u32, usize, SoundId)> = Vec::new();
        let mut hay = String::new();
        for (order, id) in ids.iter().enumerate() {
            let Some(s) = lib.sound(*id) else { continue };
            hay.clear();
            hay.push_str(&s.name);
            // Weaker fields after the name: categories, artist/title, file name.
            for c in &s.categories {
                if let Some(cat) = lib.category(*c) {
                    hay.push(' ');
                    hay.push_str(&cat.name);
                }
            }
            if !s.artist.is_empty() {
                hay.push(' ');
                hay.push_str(&s.artist);
            }
            if !s.title.is_empty() {
                hay.push(' ');
                hay.push_str(&s.title);
            }
            if let Some(h) = &s.hotkey {
                hay.push(' ');
                hay.push_str(h);
            }
            let name_score =
                pattern.score(Utf32Str::new(&s.name, &mut self.buf), &mut self.matcher);
            let full_score = pattern.score(Utf32Str::new(&hay, &mut self.buf), &mut self.matcher);
            let score = match (name_score, full_score) {
                (Some(n), _) => n.saturating_mul(2),
                (None, Some(f)) => f,
                (None, None) => continue,
            };
            scored.push((score, order, *id));
        }
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, id)| id).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Sound;

    #[test]
    fn ranks_name_matches_first_and_filters() {
        let mut lib = Library::new();
        let a = lib.insert_sound(Sound::from_path(SoundId(0), "/s/air horn.mp3"));
        let b = lib.insert_sound(Sound::from_path(SoundId(0), "/s/bruh.mp3"));
        let c = lib.insert_sound(Sound::from_path(SoundId(0), "/s/horn of gondor.mp3"));
        let cat = lib.add_category("airport", None);
        lib.sound_mut(b).unwrap().categories.push(cat);
        let mut s = Searcher::new();
        let r = s.search(&lib, "horn", None);
        assert_eq!(r.len(), 2);
        assert!(r.contains(&a) && r.contains(&c));
        assert_eq!(s.search(&lib, "", None), vec![a, b, c]);
        assert!(s.search(&lib, "airport", None).contains(&b));
        assert!(s.search(&lib, "zzzz", None).is_empty());
    }

    #[test]
    fn thousand_sounds_is_fast() {
        let mut lib = Library::new();
        for i in 0..2000 {
            lib.insert_sound(Sound::from_path(
                SoundId(0),
                format!("/s/sound effect {i}.mp3"),
            ));
        }
        let mut s = Searcher::new();
        let t = std::time::Instant::now();
        let r = s.search(&lib, "effect 19", None);
        assert!(!r.is_empty());
        assert!(
            t.elapsed().as_millis() < 200,
            "search took {:?}",
            t.elapsed()
        );
    }
}
