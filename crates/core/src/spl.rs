//! Soundpad sound-list (`.spl`) and stats (`stats.spl`) interop.
//!
//! The format was recovered from Soundpad 4.0.35 (see `re/findings.md`, Q1): plain UTF-8 XML,
//! no BOM, LF line endings, two-space indentation, `& < > "` escaped and `'` written raw.
//! [`SplDocument`] mirrors the file faithfully so Soundpad-written files round-trip byte for
//! byte; [`SplDocument::to_library`] / [`SplDocument::from_library`] convert to our model.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use crate::error::{Error, Result};
use crate::hotkey::{Hotkey, Modifiers};
use crate::library::Library;
use crate::model::{Category, CategoryId, Sound, SoundId};

/// One `<Sound>` in the top-level list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SplSound {
    /// File identity (`serial-high-low`) or the absolute path for missing files.
    pub hash: Option<String>,
    /// Path; relative to the document's `rel` in user-saved lists.
    pub url: String,
    /// Artist metadata.
    pub artist: Option<String>,
    /// Title metadata.
    pub title: Option<String>,
    /// Duration text `m:ss`.
    pub duration: Option<String>,
    /// `RegisterHotKey` modifier bits (1 Alt, 2 Ctrl, 4 Shift, 8 Win).
    pub key_modifiers: Option<u32>,
    /// Win32 virtual-key code.
    pub key: Option<u32>,
    /// Alt+Numcode hotkey.
    pub num_code: Option<u32>,
    /// `YYYY-MM-DD`.
    pub added_on: Option<String>,
    /// `RRGGBB`.
    pub color: Option<String>,
    /// User display text.
    pub custom_tag: Option<String>,
    /// Pipe-format only: display tag (`GetSoundlist()` output).
    pub tag: Option<String>,
    /// Pipe-format only: play count.
    pub play_count: Option<u32>,
    /// Pipe-format only: last played.
    pub last_played_on: Option<String>,
}

/// One `<Category>`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SplCategory {
    /// `type` (1 = built-in "All sounds").
    pub kind: Option<u32>,
    /// Name.
    pub name: Option<String>,
    /// Stock icon name or base64 PNG.
    pub icon: Option<String>,
    /// `recordings="true"` marks the recordings category.
    pub recordings: Option<String>,
    /// `RRGGBB`.
    pub color: Option<String>,
    /// Category index for index hotkeys.
    pub index: Option<u32>,
    /// `"true"` when hidden.
    pub hidden: Option<String>,
    /// Random-play hotkey modifiers.
    pub key_modifiers: Option<u32>,
    /// Random-play hotkey key.
    pub key: Option<u32>,
    /// Numcode hotkey.
    pub num_code: Option<u32>,
    /// 0-based indices into [`SplDocument::sounds`].
    pub sounds: Vec<usize>,
    /// Nested categories.
    pub children: Vec<SplCategory>,
}

/// A whole `.spl` file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SplDocument {
    /// Directory relative `url`s are resolved against (user-saved lists).
    pub rel: Option<String>,
    /// Session files: path of the loaded user list.
    pub url: Option<String>,
    /// Session files: `"1"` when the loaded list was saved.
    pub saved: Option<String>,
    /// Sounds, in order.
    pub sounds: Vec<SplSound>,
    /// Category tree (`None` when the file had no `<Categories>`).
    pub categories: Option<Vec<SplCategory>>,
    /// Hotbar pages, each a list of `soundId`s (`None` when absent).
    pub hotbar: Option<Vec<Vec<u32>>>,
}

/// One entry of `stats.spl`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SplStat {
    /// File identity matching [`SplSound::hash`].
    pub hash: String,
    /// Times played.
    pub play_count: u32,
    /// Last played date.
    pub last_played_on: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Generic XML tree

#[derive(Debug, Default)]
struct Node {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

impl Node {
    fn attr(&self, k: &str) -> Option<String> {
        self.attrs
            .iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.clone())
    }
    fn attr_u32(&self, k: &str) -> Option<u32> {
        self.attr(k).and_then(|v| v.trim().parse().ok())
    }
}

fn start_to_node(e: &BytesStart<'_>) -> Result<Node> {
    let name = e.name().as_ref().to_string();
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|err| Error::Spl(err.to_string()))?;
        let k = a.key.as_ref().to_string();
        let v = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| Error::Spl(err.to_string()))?
            .into_owned();
        attrs.push((k, v));
    }
    Ok(Node {
        name,
        attrs,
        children: Vec::new(),
    })
}

fn parse_tree(text: &str) -> Result<Node> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut reader = Reader::from_str(text);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<Node> = Vec::new();
    let mut root: Option<Node> = None;
    loop {
        match reader.read_event().map_err(|e| Error::Spl(e.to_string()))? {
            Event::Start(e) => stack.push(start_to_node(&e)?),
            Event::Empty(e) => {
                let node = start_to_node(&e)?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => root = Some(node),
                }
            }
            Event::End(_) => {
                let node = stack
                    .pop()
                    .ok_or_else(|| Error::Spl("unbalanced tags".into()))?;
                match stack.last_mut() {
                    Some(parent) => parent.children.push(node),
                    None => root = Some(node),
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err(Error::Spl("unexpected end of file".into()));
    }
    root.ok_or_else(|| Error::Spl("empty document".into()))
}

// ---------------------------------------------------------------------------------------------
// Parsing

impl SplDocument {
    /// Parse `.spl` text (also accepts Soundpad's pipe `GetSoundlist()` output).
    pub fn parse(text: &str) -> Result<SplDocument> {
        let root = parse_tree(text)?;
        if root.name != "Soundlist" {
            return Err(Error::Spl(format!(
                "root element is <{}>, expected <Soundlist>",
                root.name
            )));
        }
        let mut doc = SplDocument {
            rel: root.attr("rel"),
            url: root.attr("url"),
            saved: root.attr("saved"),
            ..Default::default()
        };
        for child in &root.children {
            match child.name.as_str() {
                "Sound" | "SoundFile" => doc.sounds.push(parse_sound(child)),
                "Categories" => {
                    let cats = child
                        .children
                        .iter()
                        .filter(|c| c.name == "Category")
                        .map(parse_category)
                        .collect();
                    doc.categories = Some(cats);
                }
                "Hotbar" => {
                    let pages = child
                        .children
                        .iter()
                        .filter(|p| p.name == "HotbarPage")
                        .map(|p| {
                            p.children
                                .iter()
                                .filter(|i| i.name == "HotbarItem")
                                .filter_map(|i| i.attr_u32("soundId"))
                                .collect()
                        })
                        .collect();
                    doc.hotbar = Some(pages);
                }
                _ => {}
            }
        }
        Ok(doc)
    }

    /// Read and parse a file.
    pub fn load(path: &Path) -> Result<SplDocument> {
        let bytes = std::fs::read(path).map_err(|e| Error::io(path, e))?;
        let text = String::from_utf8_lossy(&bytes);
        SplDocument::parse(&text)
    }
}

fn parse_sound(n: &Node) -> SplSound {
    SplSound {
        hash: n.attr("hash"),
        url: n.attr("url").unwrap_or_default(),
        artist: n.attr("artist"),
        title: n.attr("title"),
        duration: n.attr("duration"),
        key_modifiers: n
            .attr_u32("keyModifiers")
            .or_else(|| n.attr_u32("hotkeyKeyModifiers")),
        key: n.attr_u32("key").or_else(|| n.attr_u32("hotkeyKey")),
        num_code: n
            .attr_u32("numCode")
            .or_else(|| n.attr_u32("hotkeyNumCode")),
        added_on: n.attr("addedOn"),
        color: n.attr("color"),
        custom_tag: n.attr("customTag"),
        tag: n.attr("tag"),
        play_count: n.attr_u32("playCount"),
        last_played_on: n.attr("lastPlayedOn"),
    }
}

fn parse_category(n: &Node) -> SplCategory {
    let mut c = SplCategory {
        kind: n.attr_u32("type"),
        name: n.attr("name"),
        icon: n.attr("icon"),
        recordings: n.attr("recordings"),
        color: n.attr("color"),
        index: n.attr_u32("index"),
        hidden: n.attr("hidden"),
        key_modifiers: n.attr_u32("keyModifiers"),
        key: n.attr_u32("key"),
        num_code: n.attr_u32("numCode"),
        ..Default::default()
    };
    for ch in &n.children {
        match ch.name.as_str() {
            "Sound" => {
                if let Some(id) = ch.attr_u32("id") {
                    c.sounds.push(id as usize);
                }
            }
            "Category" => c.children.push(parse_category(ch)),
            _ => {}
        }
    }
    c
}

// ---------------------------------------------------------------------------------------------
// Writing

/// Escape an attribute value exactly like Soundpad 4.0.35.
pub fn escape_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c => out.push(c),
        }
    }
    out
}

fn attr(out: &mut String, k: &str, v: &Option<String>) {
    if let Some(v) = v {
        let _ = write!(out, " {k}=\"{}\"", escape_attr(v));
    }
}
fn attr_num(out: &mut String, k: &str, v: Option<u32>) {
    if let Some(v) = v {
        let _ = write!(out, " {k}=\"{v}\"");
    }
}

impl SplDocument {
    /// Serialize in Soundpad's exact layout.
    pub fn to_xml(&self) -> String {
        let mut o = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<Soundlist");
        attr(&mut o, "rel", &self.rel);
        attr(&mut o, "url", &self.url);
        attr(&mut o, "saved", &self.saved);
        if self.sounds.is_empty() && self.categories.is_none() && self.hotbar.is_none() {
            o.push_str("/>\n");
            return o;
        }
        o.push_str(">\n");
        for s in &self.sounds {
            o.push_str("  <Sound");
            attr(&mut o, "hash", &s.hash);
            attr(&mut o, "url", &Some(s.url.clone()));
            attr(&mut o, "artist", &s.artist);
            attr(&mut o, "title", &s.title);
            attr(&mut o, "duration", &s.duration);
            if s.key.is_some() {
                attr_num(&mut o, "keyModifiers", s.key_modifiers.or(Some(0)));
                attr_num(&mut o, "key", s.key);
            }
            attr_num(&mut o, "numCode", s.num_code);
            attr(&mut o, "addedOn", &s.added_on);
            attr(&mut o, "color", &s.color);
            attr(&mut o, "customTag", &s.custom_tag);
            o.push_str("/>\n");
        }
        if let Some(cats) = &self.categories {
            if cats.is_empty() {
                o.push_str("  <Categories/>\n");
            } else {
                o.push_str("  <Categories>\n");
                for c in cats {
                    write_category(&mut o, c, 2);
                }
                o.push_str("  </Categories>\n");
            }
        }
        if let Some(pages) = &self.hotbar {
            if pages.is_empty() {
                o.push_str("  <Hotbar/>\n");
            } else {
                o.push_str("  <Hotbar>\n");
                for p in pages {
                    if p.is_empty() {
                        o.push_str("    <HotbarPage/>\n");
                    } else {
                        o.push_str("    <HotbarPage>\n");
                        for id in p {
                            let _ = writeln!(o, "      <HotbarItem soundId=\"{id}\"/>");
                        }
                        o.push_str("    </HotbarPage>\n");
                    }
                }
                o.push_str("  </Hotbar>\n");
            }
        }
        o.push_str("</Soundlist>\n");
        o
    }

    /// Write to disk atomically.
    pub fn save(&self, path: &Path) -> Result<()> {
        crate::paths::write_atomic(path, self.to_xml().as_bytes())
    }
}

fn write_category(o: &mut String, c: &SplCategory, depth: usize) {
    let pad = "  ".repeat(depth);
    let _ = write!(o, "{pad}<Category");
    attr_num(o, "type", c.kind);
    attr(o, "name", &c.name);
    attr(o, "icon", &c.icon);
    attr(o, "recordings", &c.recordings);
    attr(o, "color", &c.color);
    attr_num(o, "index", c.index);
    attr(o, "hidden", &c.hidden);
    if c.key.is_some() {
        attr_num(o, "keyModifiers", c.key_modifiers.or(Some(0)));
        attr_num(o, "key", c.key);
    }
    attr_num(o, "numCode", c.num_code);
    if c.sounds.is_empty() && c.children.is_empty() {
        o.push_str("/>\n");
        return;
    }
    o.push_str(">\n");
    for id in &c.sounds {
        let _ = writeln!(o, "{pad}  <Sound id=\"{id}\"/>");
    }
    for ch in &c.children {
        write_category(o, ch, depth + 1);
    }
    let _ = writeln!(o, "{pad}</Category>");
}

// ---------------------------------------------------------------------------------------------
// stats.spl

/// Parse `stats.spl`.
pub fn parse_stats(text: &str) -> Result<Vec<SplStat>> {
    let root = parse_tree(text)?;
    if root.name != "Stats" {
        return Err(Error::Spl(format!(
            "root element is <{}>, expected <Stats>",
            root.name
        )));
    }
    Ok(root
        .children
        .iter()
        .filter(|n| n.name == "Sound")
        .filter_map(|n| {
            Some(SplStat {
                hash: n.attr("hash")?,
                play_count: n.attr_u32("playCount").unwrap_or(0),
                last_played_on: n.attr("lastPlayedOn"),
            })
        })
        .collect())
}

/// Serialize `stats.spl`.
pub fn write_stats(stats: &[SplStat]) -> String {
    let mut o = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    if stats.is_empty() {
        o.push_str("<Stats/>\n");
        return o;
    }
    o.push_str("<Stats>\n");
    for s in stats {
        o.push_str("  <Sound");
        attr(&mut o, "hash", &Some(s.hash.clone()));
        let _ = write!(o, " playCount=\"{}\"", s.play_count);
        attr(&mut o, "lastPlayedOn", &s.last_played_on);
        o.push_str("/>\n");
    }
    o.push_str("</Stats>\n");
    o
}

// ---------------------------------------------------------------------------------------------
// Library conversion

/// Format milliseconds like Soundpad (`m:ss`, `h:mm:ss` past an hour).
pub fn format_duration(ms: u32) -> String {
    let s = ms / 1000 + u32::from(ms % 1000 >= 500);
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Parse `m:ss` / `h:mm:ss` into milliseconds.
pub fn parse_duration(text: &str) -> Option<u32> {
    let parts: Vec<u32> = text
        .trim()
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let secs = match parts.as_slice() {
        [m, s] => m * 60 + s,
        [h, m, s] => h * 3600 + m * 60 + s,
        [s] => *s,
        _ => return None,
    };
    Some(secs * 1000)
}

/// Resolve a `url` against the list's `rel` and the directory the list now lives in.
/// Soundpad tries the list's current directory first, then `rel` (findings Q1).
pub fn resolve_url(url: &str, rel: Option<&str>, list_dir: Option<&Path>) -> PathBuf {
    let native = url.replace('\\', std::path::MAIN_SEPARATOR_STR);
    let p = PathBuf::from(&native);
    if is_absolute_any(url) {
        return p;
    }
    if let Some(dir) = list_dir {
        let candidate = dir.join(&p);
        if candidate.exists() || rel.is_none() {
            return candidate;
        }
    }
    match rel {
        Some(r) => PathBuf::from(r.replace('\\', std::path::MAIN_SEPARATOR_STR)).join(p),
        None => p,
    }
}

/// Absolute on either OS (`C:\…`, `\\server\…`, `/…`).
fn is_absolute_any(url: &str) -> bool {
    let b = url.as_bytes();
    url.starts_with('/')
        || url.starts_with("\\\\")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && (b[2] == b'\\' || b[2] == b'/'))
}

/// How an imported list maps onto the library.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ImportReport {
    /// Sounds created.
    pub added: usize,
    /// Sounds skipped because the path was already in the library (metadata merged).
    pub merged: usize,
    /// Sounds whose file does not exist on this machine.
    pub missing: usize,
    /// Categories created.
    pub categories: usize,
}

impl SplDocument {
    /// Merge this list into `lib`. `list_path` is where the `.spl` was read from (for `rel`).
    pub fn import_into(
        &self,
        lib: &mut Library,
        list_path: Option<&Path>,
        stats: &[SplStat],
    ) -> ImportReport {
        let mut report = ImportReport::default();
        let list_dir = list_path.and_then(Path::parent);
        let stats: HashMap<&str, &SplStat> = stats.iter().map(|s| (s.hash.as_str(), s)).collect();
        let mut ids: Vec<SoundId> = Vec::with_capacity(self.sounds.len());
        for s in &self.sounds {
            let path = resolve_url(&s.url, self.rel.as_deref(), list_dir);
            if let Some(existing) = lib.sounds.iter().find(|x| x.path == path).map(|x| x.id) {
                ids.push(existing);
                report.merged += 1;
                continue;
            }
            let mut sound = Sound::from_path(SoundId(0), path);
            sound.artist = s.artist.clone().unwrap_or_default();
            sound.title = s.title.clone().unwrap_or_default();
            sound.name = s
                .custom_tag
                .clone()
                .filter(|t| !t.is_empty())
                .or_else(|| s.tag.clone().filter(|t| !t.is_empty()))
                .unwrap_or_else(|| display_tag(&sound.artist, &sound.title, &sound.name));
            sound.duration_ms = s.duration.as_deref().and_then(parse_duration);
            if let Some(key) = s.key {
                sound.hotkey =
                    Hotkey::from_win32(Modifiers::from_bits(s.key_modifiers.unwrap_or(0)), key)
                        .map(|h| h.to_string());
            }
            sound.num_code = s.num_code;
            sound.color = s.color.clone();
            if let Some(d) = &s.added_on {
                sound.added_on = d.clone();
            }
            sound.file_hash = s.hash.clone().unwrap_or_default();
            if let Some(st) = s.hash.as_deref().and_then(|h| stats.get(h)) {
                sound.play_count = st.play_count;
                sound.last_played_on = st.last_played_on.clone().unwrap_or_default();
            } else {
                sound.play_count = s.play_count.unwrap_or(0);
                sound.last_played_on = s.last_played_on.clone().unwrap_or_default();
            }
            if !sound.file_exists() {
                report.missing += 1;
            }
            ids.push(lib.insert_sound(sound));
            report.added += 1;
        }
        if let Some(cats) = &self.categories {
            for c in cats {
                import_category(c, None, lib, &ids, &mut report);
            }
        }
        report
    }

    /// Build a list from the library. With `list_path` the list is written the way Soundpad
    /// saves user lists: `rel` = the list's directory and `url`s relative where possible.
    pub fn from_library(lib: &Library, list_path: Option<&Path>) -> SplDocument {
        let rel_dir = list_path.and_then(Path::parent).map(Path::to_path_buf);
        let index: HashMap<SoundId, usize> = lib
            .sounds
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id, i))
            .collect();
        let sounds = lib
            .sounds
            .iter()
            .map(|s| {
                let (key_modifiers, key) = s
                    .hotkey
                    .as_deref()
                    .and_then(|h| h.parse::<Hotkey>().ok())
                    .and_then(|h| h.to_win32())
                    .map(|(m, k)| (Some(m.bits()), Some(k)))
                    .unwrap_or((None, None));
                let url = match &rel_dir {
                    Some(dir) => match s.path.strip_prefix(dir) {
                        Ok(r) => r.to_string_lossy().replace('/', "\\"),
                        Err(_) => s.path.to_string_lossy().into_owned(),
                    },
                    None => s.path.to_string_lossy().into_owned(),
                };
                let derived =
                    display_tag(&s.artist, &s.title, &crate::model::display_name(&s.path));
                SplSound {
                    hash: Some(if s.file_hash.is_empty() {
                        s.path.to_string_lossy().into_owned()
                    } else {
                        s.file_hash.clone()
                    }),
                    url,
                    artist: Some(s.artist.clone()),
                    title: Some(s.title.clone()),
                    duration: Some(s.duration_ms.map(format_duration).unwrap_or_default()),
                    key_modifiers,
                    key,
                    num_code: s.num_code,
                    added_on: Some(s.added_on.clone()),
                    color: s.color.clone(),
                    custom_tag: (s.name != derived).then(|| s.name.clone()),
                    ..Default::default()
                }
            })
            .collect();

        // Soundpad allows one category per sound: the first category wins on export.
        let mut placed: HashMap<SoundId, CategoryId> = HashMap::new();
        for s in &lib.sounds {
            if let Some(c) = s.categories.first() {
                placed.insert(s.id, *c);
            }
        }
        let mut cats = vec![SplCategory {
            kind: Some(1),
            icon: Some("stock_icon_cat_list".into()),
            hidden: Some("true".into()),
            ..Default::default()
        }];
        for c in lib.categories.iter().filter(|c| c.parent.is_none()) {
            cats.push(export_category(lib, c, &placed, &index));
        }
        let uncategorized: Vec<usize> = lib
            .sounds
            .iter()
            .filter(|s| !placed.contains_key(&s.id))
            .map(|s| index[&s.id])
            .collect();
        if !uncategorized.is_empty() {
            cats.push(SplCategory {
                name: Some("My Sounds".into()),
                icon: Some("stock_icon_cat_home".into()),
                sounds: uncategorized,
                ..Default::default()
            });
        }
        SplDocument {
            rel: rel_dir.map(|d| d.to_string_lossy().into_owned()),
            sounds,
            categories: Some(cats),
            hotbar: Some(vec![Vec::new()]),
            ..Default::default()
        }
    }

    /// Play statistics for `stats.spl`, keyed like [`SplSound::hash`].
    pub fn stats_from_library(lib: &Library) -> Vec<SplStat> {
        lib.sounds
            .iter()
            .filter(|s| s.play_count > 0)
            .map(|s| SplStat {
                hash: if s.file_hash.is_empty() {
                    s.path.to_string_lossy().into_owned()
                } else {
                    s.file_hash.clone()
                },
                play_count: s.play_count,
                last_played_on: (!s.last_played_on.is_empty()).then(|| s.last_played_on.clone()),
            })
            .collect()
    }
}

/// Soundpad's derived tag: `artist - title`, else `title`, else the file name.
pub fn display_tag(artist: &str, title: &str, fallback: &str) -> String {
    match (artist.is_empty(), title.is_empty()) {
        (false, false) => format!("{artist} - {title}"),
        (true, false) => title.to_string(),
        _ => fallback.to_string(),
    }
}

fn import_category(
    c: &SplCategory,
    parent: Option<CategoryId>,
    lib: &mut Library,
    ids: &[SoundId],
    report: &mut ImportReport,
) {
    if c.kind == Some(1) {
        return; // built-in "All sounds"
    }
    let name = c.name.clone().unwrap_or_else(|| "Category".into());
    let id = match lib
        .categories
        .iter()
        .find(|x| x.name == name && x.parent == parent)
    {
        Some(x) => x.id,
        None => {
            report.categories += 1;
            let id = lib.add_category(&name, parent);
            if let Some(cat) = lib.category_mut(id) {
                apply_category_attrs(cat, c);
            }
            id
        }
    };
    for &i in &c.sounds {
        if let Some(sid) = ids.get(i)
            && let Some(s) = lib.sound_mut(*sid)
            && !s.categories.contains(&id)
        {
            s.categories.push(id);
        }
    }
    for ch in &c.children {
        import_category(ch, Some(id), lib, ids, report);
    }
}

fn apply_category_attrs(cat: &mut Category, c: &SplCategory) {
    cat.color = c.color.clone();
    cat.hidden = c.hidden.as_deref() == Some("true");
    cat.icon = c.icon.clone();
    cat.index = c.index;
    if let Some(key) = c.key {
        cat.hotkey = Hotkey::from_win32(Modifiers::from_bits(c.key_modifiers.unwrap_or(0)), key)
            .map(|h| h.to_string());
    }
}

fn export_category(
    lib: &Library,
    c: &Category,
    placed: &HashMap<SoundId, CategoryId>,
    index: &HashMap<SoundId, usize>,
) -> SplCategory {
    let (key_modifiers, key) = c
        .hotkey
        .as_deref()
        .and_then(|h| h.parse::<Hotkey>().ok())
        .and_then(|h| h.to_win32())
        .map(|(m, k)| (Some(m.bits()), Some(k)))
        .unwrap_or((None, None));
    SplCategory {
        name: Some(c.name.clone()),
        icon: Some(
            c.icon
                .clone()
                .unwrap_or_else(|| "stock_icon_cat_dir".into()),
        ),
        color: c.color.clone(),
        index: c.index,
        hidden: (c.hidden && c.parent.is_none()).then(|| "true".into()),
        key_modifiers,
        key,
        sounds: lib
            .sounds
            .iter()
            .filter(|s| placed.get(&s.id) == Some(&c.id))
            .map(|s| index[&s.id])
            .collect(),
        children: lib
            .categories
            .iter()
            .filter(|x| x.parent == Some(c.id))
            .map(|x| export_category(lib, x, placed, index))
            .collect(),
        ..Default::default()
    }
}
