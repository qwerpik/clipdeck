# Soundpad → Clipdeck parity table

Status: `todo` / `wip` / `done` / `intentional delta`. Evidence refs point into `findings.md` (Qn) and `samples/`.

## Data and formats

| Soundpad behavior | Evidence | Clipdeck crate / module | Status |
|---|---|---|---|
| `.spl` = UTF-8 XML, no BOM, LF, 2-space indent, trailing newline | Q1, `samples/spl/02-rich.saved-by-soundpad.spl` | `core::spl::write` | done |
| `<Sound>` attrs + order `hash,url,artist,title,duration,keyModifiers,key,addedOn,color,customTag` | Q1, `FUN_1401f0710` | `core::spl` | done |
| Legacy `<SoundFile>` accepted on load | Q1, `FUN_1401ea590` | `core::spl::read` | done |
| User lists: root `rel`, relative `url`; load tries current dir then `rel` | Q1, sample 02 | `core::spl` | done |
| Session list root `url` + `saved` | sample 03 | `core::spl` (import only) | done |
| `hash` = volume serial–index high–index low; missing file ⇒ absolute path | Q1, sample 01 vs inodes | `core::spl::FileIdentity` (Windows: `GetFileInformationByHandle`; Linux: `st_dev`-`0`-`st_ino`) | done |
| `stats.spl` (`playCount`, `lastPlayedOn` keyed by `hash`) | Q1, `FUN_140209ef0` | `core::spl::stats` | done |
| Categories: nested tree, `Sound@id` = 0-based index, one category per sound | Q1, sample 02 | `core::library` | done; **intentional delta**: Clipdeck allows multi-category, export keeps the first |
| Built-in "All sounds" category `type="1"` | sample 01 | `core::spl` | done |
| Hotbar pages/items (`HotbarPage/HotbarItem@soundId`) | Q1 | `core::spl` (round-trip only) | done |
| Escaping: `& < > "` escaped, `'` raw | Q1 | `core::spl::write` | done |
| Golden round-trip tests on `re/samples/spl/*` | Q1 test plan | `core/tests/spl_golden.rs` | done |

## Settings

| Soundpad behavior | Evidence | Clipdeck | Status |
|---|---|---|---|
| Registry `HKCU\Software\Leppsoft\Soundpad\MainFrame\*` | Q2, `samples/registry/after-first-exit.user.reg.txt` | `core::config` (TOML) | **intentional delta** (TOML via `directories`) |
| Soundpad settings migration (map registry keys) | Q2 | `core::migrate::soundpad` (Windows only) | todo (phase 2) |
| `playVolume` default 50 | Q2 | `core::config` | **intentional delta** (master 80 %) |
| Session restore (`MainFrame\session`: selection, scroll, play position) | Q2 | `ui` | todo |
| Daily list backup `backup-soundlist-YYYY-MM-DD.spl` | Q1 | `core::library::backup` | todo |

## Audio

| Soundpad behavior | Evidence | Clipdeck | Status |
|---|---|---|---|
| Mic injection into the **real** mic via user-mode APO + shared memory | Q5 | `crates/micfx` (APO DLL) + `platform::windows::micfx` | **done**: own in-mic APO (chains the device's effect), VB-CABLE as fallback; CI e2e on VB-CABLE's mic |
| Linux equivalent | — | `platform::linux::pwmic` (PipeWire `Audio/Source/Virtual`) | done: PipeWire source "Clipdeck Mic" |
| Play modes default / speakers / mic per trigger; hotkey cycles modes | Q4, prefs | `audio::engine` routing flags | done (route per trigger: `--mic` / `--monitor`, default route setting) |
| Allow resume in another play mode | prefs | `audio::engine` | todo |
| Auto stop for hotkey-played sounds | Q3 | `audio::engine` | todo |
| Continue playback / repeat file / repeat list | registry keys | `ui::app::AppCore::after_end`, bottom-bar toggle | **done** (stop / next / repeat; repeat list = next with wrap) |
| Normalization: dB target (59–107 dB), dynamic or fixed (−30…+30 dB) | Q4 | `audio::loudness` | **intentional delta**: LUFS (EBU R128), migration maps dB target (open question 2) |
| Block voice while playing on mic | Q4 | mixer mic duck; APO `FLAG_DUCK_MIC` | **done** |
| Voice-activation sound before playback | Q4 | `AppCore::play_activation` + per-request mic delay | **done** |
| PTT delay ms / karaoke mic delay ms | Q4 | `VoiceSpec::delay`, `engine::delay_frames` | **done** |
| Duck other apps (mute / −80% / −50%) | Q4 | `platform` | todo (phase 2, Windows only first) |
| Decoders: MF + FLAC/Opus/Vorbis | Q4/Q8 | `audio::decode` (symphonia + opus) | done (symphonia: MP3, AAC/M4A, FLAC, Ogg Vorbis, WAV, ALAC) |
| Metadata refresh on play | Q1 | `audio::decode::probe` | todo |
| Mixing math / buffer / limiter | Q4 **unknown** | `audio::mixer` | done (own lock-free mixer, soft clipper) |

## Hotkeys and automation

| Soundpad behavior | Evidence | Clipdeck | Status |
|---|---|---|---|
| Per-sound `RegisterHotKey` (VK + MOD bits) | Q3 | `platform::Hotkeys` | done |
| Low-level hook: L/R modifiers, pass-through, numpad codes | Q3 | `platform::windows::hook` | todo |
| Alt+Numcode / Ctrl+Alt+Index hotkeys, category index 01–99 | Q3, prefs | `platform::Hotkeys` | todo |
| Special hotkeys (stop, pause, next, random, …) | Q3 | `platform::Hotkeys` | todo |
| Auto Keys (PTT emulation via `keybd_event`, conditions by app/fullscreen) | Q3 | `platform::autokeys` | todo (phase 2) |
| Wayland | — | XDG GlobalShortcuts portal | **intentional delta** (platform) |

## Interop

| Soundpad behavior | Evidence | Clipdeck | Status |
|---|---|---|---|
| Pipe `\\.\pipe\sp_remote_control`, text commands, `R-xxx` codes | Q9 | `cli` + IPC server | done: Soundpad grammar on Clipdeck's socket/pipe |
| `-rc <cmd>` CLI | Q9 | `clipdeck rc '<cmd>'` | done (`clipdeck rc`) |
| `.spl` file association, open list on launch | Q9 | packaging (M7) | todo |
| Single instance forwards args | Q9 | `ui` + IPC | done |

## UI

| Soundpad behavior | Evidence | Clipdeck | Status |
|---|---|---|---|
| Preferences tabs Audio/Hotkeys/Interface/Devices/Recorder/Editor/Notifications/Updates | Q6, `samples/ui/preferences-controls.txt` | `ui::settings` (grouping kept, M3 visuals) | todo |
| Recorder: speakers / microphone, trim silence, normalize, max length, target category | Q6 prefs, Q9 `DoStartRecording*` | `audio::record`, `platform::record_capture`, `AppCore::start_recording` | **done**; WAV only (**intentional delta**: no M4A encoder), insert position = end |
| Toolbar pause, seek slider, previous/next | Q6 | bottom bar, `Command::Seek`, `DoSeekMs`/`DoJumpMs` | **done** (seek on in-memory clips; streamed long files ignore it) |
| Sound table columns Index/Tag/Duration/Hotkey (+Artist/Title/Added/Last played/Play count/Path) | Q6 | `ui` list view | todo |
| Category tree with counts | Q6 | `ui` nav rail | **intentional delta** (nav rail + chips) |
| Status-bar "File not found: <path>" | Q10 | snackbar | **intentional delta** |
| Notification sounds (hotkeys on/off, play mode, hotbar page) | strings | `ui::notify` | todo (phase 2) |
| Trial limits | Q7 | — | **intentional delta**: none |
