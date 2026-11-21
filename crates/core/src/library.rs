//! The persistent sound library.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::model::{Category, CategoryId, Sound, SoundId, is_audio_file};

/// All sounds and categories.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Library {
    /// Format version of `library.toml`.
    #[serde(default = "current_version")]
    pub version: u32,
    /// Next id to hand out (sounds and categories share the counter).
    #[serde(default)]
    pub next_id: u64,
    /// Categories, in display order.
    #[serde(default, rename = "category")]
    pub categories: Vec<Category>,
    /// Sounds, in display order.
    #[serde(default, rename = "sound")]
    pub sounds: Vec<Sound>,
}

fn current_version() -> u32 {
    1
}

/// Outcome of adding files.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct AddReport {
    /// Newly added sounds.
    pub added: Vec<SoundId>,
    /// Paths that were already in the library.
    pub duplicates: usize,
    /// Paths skipped because they are not audio files.
    pub unsupported: usize,
}

impl Library {
    /// Empty library.
    pub fn new() -> Self {
        Library {
            version: 1,
            next_id: 1,
            ..Default::default()
        }
    }

    /// Load `library.toml`; missing file → empty library.
    pub fn load(path: &Path) -> Result<Library> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut lib: Library = toml::from_str(&text).map_err(|e| Error::Parse {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                })?;
                lib.repair();
                Ok(lib)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Library::new()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    /// Save atomically; once per day the previous file is copied into `backup_dir`.
    pub fn save(&self, path: &Path, backup_dir: Option<&Path>) -> Result<()> {
        if let Some(dir) = backup_dir
            && path.exists()
        {
            let backup = dir.join(format!("library-{}.toml", crate::date::today()));
            if !backup.exists() {
                std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
                std::fs::copy(path, &backup).map_err(|e| Error::io(&backup, e))?;
                prune_backups(dir, 14);
            }
        }
        let text = toml::to_string(self).map_err(|e| Error::Parse {
            path: path.to_path_buf(),
            message: e.to_string(),
        })?;
        crate::paths::write_atomic(path, text.as_bytes())
    }

    /// Fix ids/counters after loading a hand-edited or imported file.
    pub fn repair(&mut self) {
        let max = self
            .sounds
            .iter()
            .map(|s| s.id.0)
            .chain(self.categories.iter().map(|c| c.id.0))
            .max()
            .unwrap_or(0);
        if self.next_id <= max {
            self.next_id = max + 1;
        }
        let mut seen = HashSet::new();
        for s in &mut self.sounds {
            if !seen.insert(s.id) {
                s.id = SoundId(self.next_id);
                self.next_id += 1;
                seen.insert(s.id);
            }
            s.volume = s.volume.clamp(0.0, 2.0);
        }
        let cats: HashSet<CategoryId> = self.categories.iter().map(|c| c.id).collect();
        for s in &mut self.sounds {
            s.categories.retain(|c| cats.contains(c));
        }
        for c in &mut self.categories {
            if c.parent.is_some_and(|p| !cats.contains(&p) || p == c.id) {
                c.parent = None;
            }
        }
        if self.version == 0 {
            self.version = 1;
        }
    }

    fn alloc_id(&mut self) -> u64 {
        if self.next_id == 0 {
            self.next_id = 1;
        }
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Add audio files (directories are scanned recursively). Duplicates are skipped.
    pub fn add_paths<I, P>(&mut self, paths: I, category: Option<CategoryId>) -> AddReport
    where
        I: IntoIterator<Item = P>,
        P: AsRef<Path>,
    {
        let mut report = AddReport::default();
        let existing: HashSet<PathBuf> = self.sounds.iter().map(|s| s.path.clone()).collect();
        let mut files = Vec::new();
        for p in paths {
            collect_files(p.as_ref(), &mut files, 0);
        }
        let mut seen = existing;
        for f in files {
            if !is_audio_file(&f) {
                report.unsupported += 1;
                continue;
            }
            let f = std::fs::canonicalize(&f).unwrap_or(f);
            if !seen.insert(f.clone()) {
                report.duplicates += 1;
                continue;
            }
            let id = SoundId(self.alloc_id());
            let mut s = Sound::from_path(id, f);
            if let Some(c) = category {
                s.categories.push(c);
            }
            self.sounds.push(s);
            report.added.push(id);
        }
        report
    }

    /// Insert a fully built sound with a fresh id; returns the id.
    pub fn insert_sound(&mut self, mut sound: Sound) -> SoundId {
        sound.id = SoundId(self.alloc_id());
        let id = sound.id;
        self.sounds.push(sound);
        id
    }

    /// Create a category.
    pub fn add_category(&mut self, name: &str, parent: Option<CategoryId>) -> CategoryId {
        let id = CategoryId(self.alloc_id());
        self.categories.push(Category {
            id,
            name: name.to_string(),
            parent,
            color: None,
            hidden: false,
            hotkey: None,
            icon: None,
            index: None,
        });
        id
    }

    /// Remove a category; its sounds stay in the library, sub-categories move up.
    pub fn remove_category(&mut self, id: CategoryId) {
        let parent = self.category(id).and_then(|c| c.parent);
        self.categories.retain(|c| c.id != id);
        for c in &mut self.categories {
            if c.parent == Some(id) {
                c.parent = parent;
            }
        }
        for s in &mut self.sounds {
            s.categories.retain(|c| *c != id);
        }
    }

    /// Remove sounds by id.
    pub fn remove_sounds(&mut self, ids: &[SoundId]) {
        let set: HashSet<SoundId> = ids.iter().copied().collect();
        self.sounds.retain(|s| !set.contains(&s.id));
    }

    /// Look up a sound.
    pub fn sound(&self, id: SoundId) -> Option<&Sound> {
        self.sounds.iter().find(|s| s.id == id)
    }

    /// Mutable look-up.
    pub fn sound_mut(&mut self, id: SoundId) -> Option<&mut Sound> {
        self.sounds.iter_mut().find(|s| s.id == id)
    }

    /// Look up a category.
    pub fn category(&self, id: CategoryId) -> Option<&Category> {
        self.categories.iter().find(|c| c.id == id)
    }

    /// Mutable category look-up.
    pub fn category_mut(&mut self, id: CategoryId) -> Option<&mut Category> {
        self.categories.iter_mut().find(|c| c.id == id)
    }

    /// Resolve a user reference: numeric id, 1-based position (`#3`), exact name, then fuzzy.
    pub fn resolve(&self, query: &str) -> Option<SoundId> {
        let q = query.trim();
        if let Some(pos) = q.strip_prefix('#')
            && let Ok(n) = pos.parse::<usize>()
        {
            return self.sounds.get(n.checked_sub(1)?).map(|s| s.id);
        }
        if let Ok(n) = q.parse::<u64>()
            && let Some(s) = self.sound(SoundId(n))
        {
            return Some(s.id);
        }
        if let Some(s) = self.sounds.iter().find(|s| s.name.eq_ignore_ascii_case(q)) {
            return Some(s.id);
        }
        let mut search = crate::search::Searcher::new();
        search.search(self, q, None).first().copied()
    }

    /// Sounds in a category, including its sub-categories.
    pub fn sounds_in_category(&self, id: CategoryId) -> Vec<SoundId> {
        let mut ids = vec![id];
        let mut i = 0;
        while i < ids.len() {
            let cur = ids[i];
            ids.extend(
                self.categories
                    .iter()
                    .filter(|c| c.parent == Some(cur))
                    .map(|c| c.id),
            );
            i += 1;
        }
        let set: HashSet<CategoryId> = ids.into_iter().collect();
        self.sounds
            .iter()
            .filter(|s| s.categories.iter().any(|c| set.contains(c)))
            .map(|s| s.id)
            .collect()
    }

    /// Favorites.
    pub fn favorites(&self) -> Vec<SoundId> {
        self.sounds
            .iter()
            .filter(|s| s.favorite)
            .map(|s| s.id)
            .collect()
    }

    /// Most recently played first (only sounds that were played).
    pub fn recent(&self, limit: usize) -> Vec<SoundId> {
        let mut v: Vec<&Sound> = self.sounds.iter().filter(|s| s.play_count > 0).collect();
        v.sort_by(|a, b| {
            b.last_played_on
                .cmp(&a.last_played_on)
                .then(b.play_count.cmp(&a.play_count))
        });
        v.into_iter().take(limit).map(|s| s.id).collect()
    }

    /// Count a play.
    pub fn record_play(&mut self, id: SoundId) {
        if let Some(s) = self.sound_mut(id) {
            s.play_count = s.play_count.saturating_add(1);
            s.last_played_on = crate::date::today();
        }
    }

    /// Sounds whose files are missing.
    pub fn missing(&self) -> Vec<SoundId> {
        self.sounds
            .iter()
            .filter(|s| !s.file_exists())
            .map(|s| s.id)
            .collect()
    }

    /// Try to relink missing files by file name below `root`. Returns how many were fixed.
    pub fn relink_missing(&mut self, root: &Path) -> usize {
        let mut files = Vec::new();
        collect_files(root, &mut files, 0);
        let mut fixed = 0;
        for s in &mut self.sounds {
            if s.file_exists() {
                continue;
            }
            let Some(name) = s.path.file_name() else {
                continue;
            };
            if let Some(found) = files.iter().find(|f| f.file_name() == Some(name)) {
                s.path = found.clone();
                fixed += 1;
            }
        }
        fixed
    }

    /// Move a sound to a new position in the display order.
    pub fn move_sound(&mut self, id: SoundId, to: usize) {
        if let Some(from) = self.sounds.iter().position(|s| s.id == id) {
            let s = self.sounds.remove(from);
            let to = to.min(self.sounds.len());
            self.sounds.insert(to, s);
        }
    }

    /// Hotkey conflicts: pairs of sounds sharing a hotkey.
    pub fn hotkey_owner(&self, hotkey: &str, except: Option<SoundId>) -> Option<SoundId> {
        self.sounds
            .iter()
            .find(|s| {
                Some(s.id) != except
                    && s.hotkey
                        .as_deref()
                        .is_some_and(|h| h.eq_ignore_ascii_case(hotkey))
            })
            .map(|s| s.id)
    }
}

fn collect_files(path: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 16 {
        return;
    }
    if path.is_dir() {
        let Ok(rd) = std::fs::read_dir(path) else {
            return;
        };
        let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        for e in entries {
            collect_files(&e, out, depth + 1);
        }
    } else {
        out.push(path.to_path_buf());
    }
}

fn prune_backups(dir: &Path, keep: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("library-"))
        })
        .collect();
    files.sort();
    while files.len() > keep {
        let old = files.remove(0);
        let _ = std::fs::remove_file(old);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("clipdeck-lib-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn add_dedupe_save_load() {
        let d = tmpdir("add");
        std::fs::write(d.join("a.mp3"), b"x").unwrap();
        std::fs::write(d.join("b.wav"), b"x").unwrap();
        std::fs::write(d.join("notes.txt"), b"x").unwrap();
        let mut lib = Library::new();
        let r = lib.add_paths([&d], None);
        assert_eq!(r.added.len(), 2);
        assert_eq!(r.unsupported, 1);
        let r2 = lib.add_paths([d.join("a.mp3")], None);
        assert_eq!(r2.duplicates, 1);
        let cat = lib.add_category("Memes", None);
        lib.sound_mut(r.added[0]).unwrap().categories.push(cat);
        lib.record_play(r.added[0]);
        let file = d.join("library.toml");
        lib.save(&file, Some(&d.join("bk"))).unwrap();
        lib.save(&file, Some(&d.join("bk"))).unwrap();
        let back = Library::load(&file).unwrap();
        assert_eq!(back, lib);
        assert_eq!(back.sounds_in_category(cat), vec![r.added[0]]);
        assert_eq!(back.recent(5), vec![r.added[0]]);
        assert!(d.join("bk").read_dir().unwrap().count() == 1);
        std::fs::remove_dir_all(&d).ok();
    }

    #[test]
    fn resolve_by_id_position_and_name() {
        let mut lib = Library::new();
        let a = lib.insert_sound(Sound::from_path(SoundId(0), "/x/Air Horn.mp3"));
        let b = lib.insert_sound(Sound::from_path(SoundId(0), "/x/sad trombone.mp3"));
        assert_eq!(lib.resolve(&a.0.to_string()), Some(a));
        assert_eq!(lib.resolve("#2"), Some(b));
        assert_eq!(lib.resolve("air horn"), Some(a));
        assert_eq!(lib.resolve("trombne"), Some(b));
        assert_eq!(lib.resolve("#9"), None);
    }

    #[test]
    fn nested_categories_and_removal() {
        let mut lib = Library::new();
        let top = lib.add_category("Top", None);
        let sub = lib.add_category("Sub", Some(top));
        let s = lib.insert_sound(Sound::from_path(SoundId(0), "/x/a.mp3"));
        lib.sound_mut(s).unwrap().categories.push(sub);
        assert_eq!(lib.sounds_in_category(top), vec![s]);
        lib.remove_category(top);
        assert_eq!(lib.category(sub).unwrap().parent, None);
        lib.remove_category(sub);
        assert!(lib.sound(s).unwrap().categories.is_empty());
    }

    #[test]
    fn repair_fixes_duplicate_ids_and_dangling_refs() {
        let mut lib = Library::new();
        lib.sounds.push(Sound::from_path(SoundId(5), "/a.mp3"));
        lib.sounds.push(Sound::from_path(SoundId(5), "/b.mp3"));
        lib.sounds[0].categories.push(CategoryId(99));
        lib.next_id = 0;
        lib.repair();
        assert_ne!(lib.sounds[0].id, lib.sounds[1].id);
        assert!(lib.sounds[0].categories.is_empty());
        assert!(lib.next_id > 5);
    }
}
