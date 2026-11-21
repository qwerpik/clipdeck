//! Golden-file tests against `.spl` files written by the real Soundpad 4.0.35
//! (captured under Wine, see `re/findings.md` Q1).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};

use clipdeck_core::library::Library;
use clipdeck_core::model::{Sound, SoundId};
use clipdeck_core::spl::{self, SplDocument, SplStat};

fn golden(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name);
    std::fs::read_to_string(p).unwrap()
}

/// Soundpad-written files must re-serialize byte for byte.
#[test]
fn soundpad_files_roundtrip_byte_for_byte() {
    for name in ["01-default.spl", "02-rich-user-list.spl", "03-session.spl"] {
        let text = golden(name);
        let doc = SplDocument::parse(&text).unwrap();
        assert_eq!(doc.to_xml(), text, "{name} did not round-trip");
    }
}

#[test]
fn stats_roundtrip() {
    let text = golden("stats-empty.spl");
    assert!(spl::parse_stats(&text).unwrap().is_empty());
    assert_eq!(spl::write_stats(&[]), text);
    let stats = vec![SplStat {
        hash: "1-2-3".into(),
        play_count: 4,
        last_played_on: Some("2026-11-21".into()),
    }];
    let written = spl::write_stats(&stats);
    assert_eq!(spl::parse_stats(&written).unwrap(), stats);
}

#[test]
fn rich_user_list_semantics() {
    let doc = SplDocument::parse(&golden("02-rich-user-list.spl")).unwrap();
    assert_eq!(
        doc.rel.as_deref(),
        Some("Z:\\home\\user\\Projects\\soundpad\\re\\wine\\media")
    );
    assert_eq!(doc.sounds.len(), 4);
    assert_eq!(doc.sounds[0].url, "plain.mp3");
    assert_eq!(doc.sounds[0].key, Some(112));
    assert_eq!(doc.sounds[0].key_modifiers, Some(2));
    assert_eq!(doc.sounds[2].url, "Zażółć gęślą & <jaźń> \"q\" 'a'.mp3");
    let cats = doc.categories.as_ref().unwrap();
    assert_eq!(cats[0].kind, Some(1));
    assert_eq!(cats[1].name.as_deref(), Some("Memes"));
    assert_eq!(cats[1].children[0].name.as_deref(), Some("Sub & Nested"));
    assert_eq!(cats[1].children[0].sounds, vec![1]);

    let mut lib = Library::new();
    let report = doc.import_into(&mut lib, Some(Path::new("/nonexistent/list.spl")), &[]);
    assert_eq!(report.added, 4);
    assert_eq!(report.categories, 3); // Memes, Sub & Nested, Other
    let plain = &lib.sounds[0];
    assert_eq!(plain.name, "my tag");
    assert_eq!(plain.hotkey.as_deref(), Some("Ctrl+F1"));
    assert_eq!(plain.color.as_deref(), Some("FF0000"));
    assert_eq!(plain.duration_ms, Some(2000));
    assert_eq!(lib.sounds[2].name, "Ünïcødé - Ⓣitle ✓");
    assert_eq!(lib.sounds[1].name, "notify ding");
    let memes = lib.categories.iter().find(|c| c.name == "Memes").unwrap();
    assert_eq!(memes.hotkey.as_deref(), Some("Shift+F2"));
    assert_eq!(memes.index, Some(5));
}

#[test]
fn pipe_output_is_importable() {
    let doc = SplDocument::parse(&golden("02-rich-pipe.xml")).unwrap();
    let mut lib = Library::new();
    doc.import_into(&mut lib, None, &[]);
    assert_eq!(lib.sounds.len(), 4);
    assert_eq!(lib.sounds[0].hotkey.as_deref(), Some("Ctrl+F1"));
    assert_eq!(lib.sounds[0].name, "my tag");
}

/// Our export of an imported Soundpad list must match what Soundpad itself wrote for the same
/// input (modulo `rel`, which is the list's own directory).
#[test]
fn export_matches_soundpad_writer() {
    let soundpad = golden("02-rich-user-list.spl");
    let doc = SplDocument::parse(&soundpad).unwrap();
    let list_dir = PathBuf::from("/media/sounds");
    let mut lib = Library::new();
    doc.import_into(&mut lib, Some(&list_dir.join("in.spl")), &[]);
    // Pretend the files live next to the list on this machine (the golden `rel` is a Wine path).
    for s in &mut lib.sounds {
        let name = s
            .path
            .to_string_lossy()
            .rsplit(['/', '\\'])
            .next()
            .unwrap()
            .to_string();
        s.path = list_dir.join(name);
    }
    let mut ours = SplDocument::from_library(&lib, Some(&list_dir.join("out.spl")));
    assert_eq!(ours.rel.as_deref(), Some("/media/sounds"));
    ours.rel = doc.rel.clone();
    assert_eq!(ours.to_xml(), soundpad);
}

#[test]
fn export_relative_paths_and_reimport() {
    let dir = std::env::temp_dir().join(format!("clipdeck-spl-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/boom.mp3"), b"x").unwrap();
    let mut lib = Library::new();
    let mut s = Sound::from_path(SoundId(0), dir.join("sub/boom.mp3"));
    s.hotkey = Some("Ctrl+Alt+B".into());
    s.duration_ms = Some(61_400);
    lib.insert_sound(s);
    lib.insert_sound(Sound::from_path(SoundId(0), "/elsewhere/abs.wav"));
    let list = dir.join("list.spl");
    let doc = SplDocument::from_library(&lib, Some(&list));
    let text = doc.to_xml();
    assert!(text.contains("url=\"sub\\boom.mp3\""), "{text}");
    assert!(text.contains("duration=\"1:01\""));
    assert!(text.contains("keyModifiers=\"3\" key=\"66\""));
    assert!(text.contains("<Category name=\"My Sounds\" icon=\"stock_icon_cat_home\">"));
    doc.save(&list).unwrap();

    // Moving the folder keeps relative entries working (current dir wins over `rel`).
    let moved = dir.with_extension("moved");
    let _ = std::fs::remove_dir_all(&moved);
    std::fs::rename(&dir, &moved).unwrap();
    let back = SplDocument::load(&moved.join("list.spl")).unwrap();
    let mut lib2 = Library::new();
    back.import_into(&mut lib2, Some(&moved.join("list.spl")), &[]);
    assert_eq!(lib2.sounds[0].path, moved.join("sub").join("boom.mp3"));
    assert!(lib2.sounds[0].file_exists());
    assert_eq!(lib2.sounds[0].hotkey.as_deref(), Some("Ctrl+Alt+B"));
    assert_eq!(lib2.sounds[1].path, PathBuf::from("/elsewhere/abs.wav"));
    std::fs::remove_dir_all(&moved).ok();
}

#[test]
fn rejects_garbage() {
    assert!(SplDocument::parse("").is_err());
    assert!(SplDocument::parse("<Other/>").is_err());
    assert!(SplDocument::parse("<Soundlist><Sound url=\"a\"").is_err());
    assert!(SplDocument::parse("not xml at all").is_err());
    assert!(spl::parse_stats("<Soundlist/>").is_err());
}

#[test]
fn durations() {
    assert_eq!(spl::format_duration(2_400), "0:02");
    assert_eq!(spl::format_duration(2_600), "0:03");
    assert_eq!(spl::format_duration(3_723_000), "1:02:03");
    assert_eq!(spl::parse_duration("1:02:03"), Some(3_723_000));
    assert_eq!(spl::parse_duration("0:03"), Some(3_000));
    assert_eq!(spl::parse_duration(""), None);
}
