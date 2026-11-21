# Soundpad reverse-engineering findings (M1, first pass)

Target: **Soundpad 4.0.35** (Leppsoft), trial/demo build from leppsoft.com, downloaded 2026-11-21.

| Artifact | SHA-256 / note |
|---|---|
| `Soundpad_x64-4.0.35.msi` | WiX 5.0.2 MSI, `target/dl/` (gitignored) |
| `Soundpad.exe` (MSI file `GuiExe`) | `b24006b87d2e6f60d81eb9ae172d2c63e6b25879800fa70c2527cd377ec29c6b`, PE32+ x86-64, 12.2 MB |
| `SoundpadService.exe`, `UniteFx.dll` (+ `UniteFx-ARM.dll`), `TTS.dll` | also in `target/bin/` |

Evidence labels used below:

- **observed**: seen while Soundpad 4.0.35 ran under Wine 11.20 in the throwaway prefix `re/wine/prefix`, with a reproducible command. Wine is not Windows; anything touching the audio driver extension cannot be observed there (see Q5).
- **static**: read from the binary (strings, PE imports/resources, Ghidra 12.1.4 decompilation of the project in `re/ghidra/proj`). Addresses are Ghidra VAs with image base `0x140000000`.
- **inferred**: a conclusion drawn from observed/static facts that was not itself exercised.
- **unknown**: not established yet.

REA evidence IDs: PE resources `ev_253d3c24689668b8d1b2837f12774eda9c6aa2f3ca024d4142595e810c70e950`; Ghidra overview `ev_0be00827be76136b8d9ed745320b34b375a9d41d65a7dbfdbb6701fabcdcaa7e` (24,570 procedures, 17,512 strings).

## Corrections to the build prompt's assumptions

1. **Soundpad 4.x is not Delphi/VCL and not 32-bit.** It is 64-bit **C++ / wxWidgets 3.2** built with MSVC (strings `3.2 (wchar_t,Visual C++ 1900,wx containers,compatible with 3.0)`, source paths `D:\dev\UniteFx\Gui\*.cpp`). There are **no `RDFM` form resources**: the only resources are icons, bitmaps, the stock wx `WXWINDOWMENU`, version info and a manifest (static, `inspect-pe-resources`). The UI is built in code. The UI spec was taken from the **running app's live control tree** (custom Win32 inspector `tools/winctl.c`) plus the gettext catalogs. That turned out to be more complete than form resources would have been.
2. **There is no `speaker.dll`/`mic.dll`.** The microphone path is a **user-mode Audio Processing Object** (`UniteFx.dll`) injected into the real microphone's endpoint (Q5).
3. The REA Ghidra provider was not installed. Fixed by installing Ghidra 12.1.4 + Temurin JDK 21 under `~/.local/opt/`. The REA MCP server only picks up `GHIDRA_INSTALL_DIR` after a Claude Code restart, and the REA CLI re-analyzes the 12 MB binary on every call (~4.5 min). So queries go through a persistent headless Ghidra project with `re/ghidra/gh.sh` (~10 s per query).

## Q1. Sound-list format (`.spl`)

**Plain UTF-8 XML. No binary header, no checksum, no BOM, LF line endings, 2-space indent, trailing newline** (observed, `od -c samples/spl/02-rich.saved-by-soundpad.spl`).

The same element/attribute vocabulary is used in three files:

| File | Written when | Paths | Root attributes |
|---|---|---|---|
| user list (`File > Save sound list`, `DoSaveSoundlist()`) | on save | **relative** to `rel` if the sound is under that dir | `rel="<dir of the .spl>"` |
| session list `%APPDATA%\Leppsoft\soundlist.spl` | on exit | absolute | `url="<path of loaded user list>" saved="0|1"` (only when a user list is loaded) |
| daily backup `%APPDATA%\Leppsoft\backup-soundlist-YYYY-MM-DD.spl` | first save of a day | absolute | as session |
| stats `%APPDATA%\Leppsoft\stats.spl` | on exit | — | root `<Stats>` |

All observed (`samples/spl/01-*`, `02-*`, `03-*`), loader/writer static: loader `FUN_1401ea590`, list writer `FUN_1401ee0a0`, sound writer `FUN_1401f0710`, category writer `FUN_1401ef2a0`, category reader `FUN_1401eccb0`, hotbar reader `FUN_1401eddf0`, stats reader/writer `FUN_140209900`/`FUN_140209ef0`.

### Grammar (as written by 4.0.35)

```xml
<?xml version="1.0" encoding="UTF-8"?>
<Soundlist [rel=".."] [url=".." saved="1"]>
  <Sound hash=".." url=".." artist=".." title=".." duration="m:ss" [keyModifiers="N" key="VK"] [numCode="N"] addedOn="YYYY-MM-DD" [color="RRGGBB"] [customTag=".."]/>
  ...
  <Categories>
    <Category type="1" icon="stock_icon_cat_list" hidden="true"/>          <!-- built-in "All sounds" -->
    <Category name=".." icon="stock_icon_cat_home|stock_icon_cat_dir|<base64 PNG?>" [color="RRGGBB"] [index="NN"] [hidden="true"] [keyModifiers="N" key="VK"] [numCode] [recordings="true"]>
      <Sound id="<0-based position in the top-level Sound list>"/>
      <Category ...> ... </Category>                                       <!-- nesting allowed -->
    </Category>
  </Categories>
  <Hotbar>
    <HotbarPage> <HotbarItem soundId=".."/> ... </HotbarPage>
  </Hotbar>
</Soundlist>
```

Attribute order as written: Sound `hash, url, artist, title, duration, keyModifiers, key, addedOn, color, customTag` (observed). Category `name|type, icon, color, index, hidden, keyModifiers, key` (observed for the subset present).

### Field semantics

| Field | Meaning | Evidence |
|---|---|---|
| `hash` | `"<VolumeSerial>-<nFileIndexHigh>-<nFileIndexLow>"` from `GetFileInformationByHandle` (decimal). Used to re-find moved files. **If the file is missing, `hash` = the absolute path string.** Existing `hash` values are kept as loaded (a bogus `1-2-3` round-tripped unchanged) | observed: inode numbers 8009101..8009104 = `hash` low parts in sample 01, Wine volume serial `0x43000000`=1124073472 on C:, `0` on Z:. `GetFileInformationByHandle` import, static |
| `url` | path; relative to root `rel` in user-saved lists | observed sample 02 |
| `rel` | directory the list was saved in. On load: if `rel` ≠ the list's current dir, Soundpad first tries `<current dir>\<url>` and falls back to `<rel>\<url>` (portable lists) | loader lines ~555-720 of `FUN_1401ea590` decompile, static; fallback order **inferred** from the decompile |
| `artist`, `title`, `duration` | metadata cache. **Re-read from the file's tags when the sound is played**, overwriting the stored values (`Some & Artist` became `""`, `title` became the file stem `plain`, `duration` `0:02`→`0:03`) | observed, `samples/spl/03-session-after-load-and-play.soundlist.spl` |
| `customTag` | user-set display text. Display tag = `customTag` ‖ `artist - title` ‖ `title` ‖ file stem | observed via pipe `GetSoundlist()` `tag=` (`samples/spl/02-rich.pipe-GetSoundlist.xml`) |
| `key`, `keyModifiers` | Win32 VK code + `RegisterHotKey` modifier bits (`2`=Ctrl observed → `Ctrl+F1` for key 112; 1=Alt, 4=Shift, 8=Win **inferred** from the API) | observed `hotkey="Ctrl+F1"` |
| `numCode` | Alt+Numcode hotkey number. A `numCode="42"` in the input was **dropped** on save while "Enable Alt+Numcode hotkeys" was off | observed; reason **inferred** |
| `color` | `%06X` RRGGBB | static (`L"%06X"` in writers), observed round-trip |
| `addedOn` | `YYYY-MM-DD` | observed |
| `playCount`, `lastPlayedOn` | **not in the list**: stored in `stats.spl` as `<Stats><Sound hash=".." playCount="N" lastPlayedOn=".."/></Stats>` keyed by `hash` | static `FUN_140209900`/`FUN_140209ef0`; an observed `stats.spl` is still the empty `<Stats/>` (playback under Wine did not bump the count, cause **unknown**) |
| `Category/Sound@id` | 0-based index into the top-level `<Sound>` list. **A sound belongs to at most one user category**: a second reference was silently dropped | observed (sample 02 input had `id=0` in two categories) |
| nested `Category@hidden` | dropped on save (only kept on the built-in type-1 category) | observed |
| `Category` without `icon` | gets `icon="stock_icon_cat_dir"` | observed |
| `<SoundFile>` | legacy alias of `<Sound>`, accepted on load | static (`FUN_1401ea590` compares both) |
| `:new:` | special path argument meaning "new empty list" | static |
| hotbar items | the two `HotbarItem`s in the input were dropped (trial or invalid `soundId`: **unknown**) | observed |

Escaping: `&amp; &lt; &gt; &quot;` escaped, `'` written raw (`&apos;` accepted on input). Non-ASCII written as raw UTF-8 (observed).

Samples (≥3, different content): `samples/spl/01-default-first-run.soundlist.spl`, `samples/spl/02-rich.saved-by-soundpad.spl` (input: `samples/spl/input/02-rich.spl`), `samples/spl/03-session-after-load-and-play.soundlist.spl`, plus `*.stats.spl`.

**Round-trip test plan** (golden files, `clipdeck-core::spl`):
1. Parse each sample → model → serialize → byte-compare with the sample (writer parity).
2. Parse `input/02-rich.spl` → serialize as a *user list* saved to `wine/media/` → must equal `02-rich.saved-by-soundpad.spl` except `hash` values (host-dependent). Hash is injected through a trait so the test can pin it.
3. Fuzz: random attribute subsets/orders, XML escapes, unicode, missing files → parse(serialize(x)) == x.
4. Live parity (manual, Windows or this Wine prefix): load our exported list in Soundpad (`tools/launch.sh <path>`), then `tools/rc.sh 'GetSoundlist()'` must list the same sounds/tags/hotkeys.

## Q2. Settings storage

**Registry, `HKCU\Software\Leppsoft\Soundpad\...`** via `wxRegConfig` (observed, `samples/registry/after-first-exit.user.reg.txt`, 134 lines of keys with defaults after the first run). Sub-keys: `MainFrame` (~90 values), `MainFrame\Hotbar`, `MainFrame\session` (scroll/selection/play position restore), `MainFrame\SoundTable` & `CategoryTable` (`col%d/pos%d/width%d`), `MainFrame\AutoKeys`, `ConfigFrame` (prefs window geometry + last tab). The full key-name list is static (strings block next to `/MainFrame`, `target/sp-strings-utf16.txt`).

Machine-wide: `HKLM\SOFTWARE\Leppsoft\Soundpad\InstallDir`, `HKLM\SOFTWARE\Leppsoft\UniteFx\*` (driver-extension state per capture device: `Mode`, `AutoMode`, `Version`, `CaptureDevices`, `DisableDrmProtection`, `FilterBlockVoice`, `LogLevel`, `SetupDate`, `Standalone`) (observed, `samples/registry/after-install.system.reg.txt`).

Data files: `%APPDATA%\Leppsoft\` (`soundlist.spl`, `stats.spl`, daily backups; `license.dat` static).

Notable defaults (observed): `playVolume=50`, `playMode=0`, `hotkeysEnabled=1`, `autoKeysEnabled=1`, `theme=0`, `bitrateMp3=320`, `seekSliderFps=60` (trackbar 10–200), Hotbar `cols=6 rows=2 pages=1`.

Portable mode: **none found**. No INI/portable strings; settings are always in HKCU (static, **inferred**). The relative-path `rel` mechanism is the only portability feature.

## Q3. Hotkeys

- APIs (static imports): `RegisterHotKey`/`UnregisterHotKey` for per-sound and special hotkeys, plus `SetWindowsHookExW` (`SetWindowsHookEx(wxKeyboardHook)`) with `GetAsyncKeyState`/`GetKeyState` for left/right modifier distinction, numpad Alt+Numcode and Ctrl+Alt+Index entry, and **"Pass hotkeys"** (pass-through instead of blocking). No raw input.
- Hotkey storage: per sound `key`+`keyModifiers` in the list (Q1). Category random-play hotkeys likewise. Special hotkeys (stop, pause, next…) in the registry (`/MainFrame/sameHotkeys`, `hotkeys`). Names in strings: `pauseResume, playSelected, playNext, playPrevious, randomPlayback(All), selectNext/Previous(Category), recordBeginSpeakers…`
- Conflicts: `Hotkey is already in use. Overwrite?`, `Conflicting hotkey configuration detected.`, option `Same hotkeys in all categories`, `Allow left and right modifiers` (static strings; UI observed).
- **Auto Keys (push-to-talk emulation):** `keybd_event` (static import). Modes `Press before and after` / `Press and hold`. Conditions `Always` / `Foreground application is fullscreen` / `Certain application is running` / `Certain application is in foreground`. Option `Press only if sound is played by hotkey`. Modifiers are blocked unless `Allow potentially dangerous keys` (string). Whether the previous PTT state is restored: **unknown**.
- "Auto stop (recommended)": stops a hotkey-triggered sound even if repeat/continue is on (string, observed in prefs).

## Q4. Audio path

- Decoding: **Media Foundation SourceReader** (`MFCreateSourceReaderFromURL/ByteStream`) plus bundled libFLAC, Opus and Vorbis (license strings). Encoding for recorder/editor via MF transcode (`MFCreateTranscodeProfile`), M4A/WAV/MP3 output (static).
- Real-time thread uses MMCSS (`AvSetMmThreadCharacteristicsW`, static).
- Playback device: one selectable render device, `Default` + endpoints (observed combo). Mic injection: Q5.
- Play modes: `default` / `speakers` / `microphone` per trigger (`DoPlaySound(i,speakers,mic)`). Hotkey cycles `all play modes | default and speakers | default and microphone` (observed combo). `Allow resume in another play mode`.
- **Normalization is not LUFS.** It is a dB-level scheme: `normalizeVolume`, `averageSoundfileDecibel`, `normalizeTargetDecibel`, `fixedVolumeAdjustmentDecibel`. UI: "Voice volume" 59–107 dB in 3 dB steps (measure your voice), "Dynamic volume adjustment (recommended)" vs "Fixed volume adjustment" −30…+30 dB in 3 dB steps (observed combos). Algorithm internals: **unknown** (next decompile target: xrefs of `averageSoundfileDecibel`).
- Other audio options (observed prefs): Block voice while playing on mic, voice-activation sound (`sounds\white_noise.mp3`), Push-to-Talk delay (ms), Karaoke mic delay (ms), duck other apps (mute / −80% / −50%), via `UserDuckingPreference`.
- Mixing math, buffer size, sample format, limiter, loop/overlap semantics: **unknown**. Under Wine only the speaker path runs (`DoPlaySound` → `PLAYING`, `GetPlaybackDurationInMs()=3029`). Next: decompile the render thread (xrefs of `AvSetMmThreadCharacteristicsW`) and `UniteFx.dll`'s `APOProcess`.

## Q5. Virtual mic on Windows

**User-mode APO, not a kernel driver, not a virtual device** (observed in the install diff, `samples/registry/after-install.system.reg.txt`):
- `UniteFx.dll` is copied to `C:\Windows\System32\UniteFx1.8.0.dll`, registered as COM CLSID `{27384E53-9860-0AC3-9519-C60EBCAA2C71}` (`ThreadingModel=Both`) and as an APO (`AudioEngine\AudioProcessingObjects\{…}`, `APOInterface0`=IAudioProcessingObject, Flags `0xe`).
- On the chosen **real capture endpoint**, `FxProperties` gets `{d04e05a6-…},7` (PKEY_FX_EndpointEffectClsid) = UniteFx and `{d3993a3f-…},7` = `AUDIO_SIGNALPROCESSINGMODE_DEFAULT`. The original EFX is presumably chained ("restores devices it was installed on").
- The GUI feeds PCM to the APO, which runs inside `audiodg.exe`, through **global shared memory** `Global\UniteFxMappingObject1.8.0` with events/mutexes `Global\UniteFx{Callback,ControlCallback,StopCallback,TransferMutex,RegisterMapping…}1.8.0` (static strings in `UniteFx.dll`). `SoundpadService.exe` ("Soundpad Client Service") brokers this (`Local\SoundpadService{Init,Callback,Callback2,Mem}`).
- Install side effects: requires admin, may disable Windows Audio DRM validation (`DisableProtectedAudioDG`), restarts `AudioSrv` or asks for a reboot.
- Under Wine the APO cannot load (`Failed to restore setup [Code 80040154]`, REGDB_E_CLASSNOTREG, observed).

**Consequence for Clipdeck:** Soundpad's observable result is that *every app that uses your normal microphone* hears the sounds, with no device switch in Discord. VB-CABLE gives a different result: the user must pick `CABLE Output` and loses their real mic unless we mix it in. A kernel driver is not needed to match Soundpad. A **user-mode APO DLL** is: COM in-proc, which Rust can implement with the `windows` crate's COM support. It needs admin install and an audiodg restart. Recommendation: ship VB-CABLE routing in M5 as planned behind the `VirtualMic` trait, and record "APO mode" as a later backend (see `parity.md`).

## Q6. UI inventory

- Main window `Soundpad (Trial)`, 769×720 default (observed, `samples/ui/mainframe-controls.txt`). Menus `&File &Edit &Play &Window &Help` (+ `&View`, `Hot&bar`, `Effe&cts` in strings). Toolbar: play, play-on-speakers, play-on-mic, pause, stop, previous, next, time `0:00`, seek slider, volume slider. Left: category tree (`Category | #`), right: sound table (`Index | Tag | Duration | Hotkey`, more columns in strings: Artist, Title, Added on, Last played on, Play count, File path). Bottom: status bar and info panel (`Index:`, `Sounds:`, `Selected:`, `Play count:`). Hidden panels: search bar, TTS panel (Speed/Pitch), recorder, hotbar, color tool.
- **Preferences** (568×736, tabs **Audio, Hotkeys, Interface, Devices, Recorder, Editor, Notifications, Updates**): every control, combo item, slider range and checkbox state is in `samples/ui/preferences-controls.txt` (observed, 415 lines).
- All 776 UI strings with Polish translations were decoded from the shipped gettext catalogs (`tools/mo2po.py`-style decode of `target/x64/*Mo`). They are Leppsoft's translations, so they stay local (`samples/i18n/` is gitignored) and are not redistributed here.
- Dialogs seen: `Soundpad Setup` (APO error / "not configured properly"), `Unsaved changes` ("Save current sound list?" Yes/No/Cancel), `Language` confirmation.
- Keyboard shortcuts: in menu strings (`\tCtrl+E` Edit file, `\tShift+F2` Rename…). Full menu tree with accelerators: **todo**. Cross-process `GetMenu` returns nothing under Wine. Next: read the menu-building function or use the strings.

## Q7. Editions and gating

- Trial text (static): *"You have a trial license, which limits the files you can play to %d and the files you can add to %d."* + `Trial limit reached.` / `Demo limit reached.`; title suffix ` (Trial)`; pipe `IsTrial()` → `1` (observed).
- Pipe commands blocked in trial: `DoAddSound` → `R-403: Command not available in trial version.` (observed). Loading lists, playing, saving work (observed).
- License: `license.dat`, serial registration, Steam edition (`Licensed to Steam ID`, `steam://rungameid/629520`). The numeric limits and the exact gating check: **unknown** (next: xrefs of the trial string).
- Clipdeck has no editions, so this is informational only.

## Q8. Playback / decoder details

- File dialogs: `All supported audio files`; save formats WAV, M4A, MP3 (static). The full input extension list is built at runtime (MF + FLAC/Ogg/Opus): **unknown**.
- Recorder: save as M4A/WAV, auto-normalize recordings to 59–107 dB, trim silence, max length 300 s default, insert at beginning/end, target category (observed prefs).
- Editor: cut, crop, fade-in/out, reverse, undo/redo, play selection, external editor with param (strings + prefs).
- `duration` stored as `m:ss` text. Metadata re-read on play (Q1).

## Q9. Interop surface

- **Named pipe `\\.\pipe\sp_remote_control`**, message mode, request = command text, response = text/XML (observed with `tools/sprc.c`). Remote-control version `1.1.2`. Responses `R-200` OK, `R-204`, `R-400` bad params, `R-403` not allowed (trial / command line), `R-404` not found, `R-409`, `R-500`. Commands (static list, many observed): `DoPlaySound(i[,spk,mic])`, `DoPlaySoundFromCategory`, `DoPlayRandomSound[FromCategory]`, `DoPlayPrevious/Next/Selected/CurrentAgain/PreviouslyPlayedSound`, `DoStopSound`, `DoTogglePause`, `DoToggleMute`, `DoSeekMs`, `DoJumpMs`, `DoAddSound`, `DoAddCategory`, `DoRemoveCategory`, `DoRemoveSelectedEntries`, `DoSearch`, `DoResetSearch`, `DoSelect{NextHit,PreviousHit,Category,Index,NextCategory,PreviousCategory}`, `DoScrollTo/By`, `DoUndo/Redo`, `DoStartRecording[Speakers|Microphone]`, `DoStopRecording`, `DoSaveSoundlist`, `Get{Soundlist,Categories,Category,SoundFileCount,PlaybackPositionInMs,PlaybackDurationInMs,RecordingPositionInMs,RecordingPeak,PlayStatus(PLAYING|PAUSED|STOPPED|SEEKING),StatusBarText,TitleText,Volume,Version,RemoteControlVersion}`, `SetVolume(0..100)`, `IsAlive`, `IsTrial`, `IsMuted`.
- CLI (static, wxCmdLineParser long names): `remote-control` (`-rc <cmd>`), `install`, `uninstall`, `uninstall-device`, `restore`, `restore-immediately`, `winaudio` (restart AudioSrv), `clean` (reset prefs), `license`, `troubleshooting`, `version`, `silent`, `cloud` (`-c <url>`, URL-scheme call). Short forms seen in strings: `-rc`, `-i`, `-u`, `-s -r`, `-s -ri`, `-s -w`, `-c`. The other short letters are **inferred**. Positional `<file.spl>` observed: it loads the list, asking to save the current one first.
- File association `.spl` → ProgID ending in `.Soundlist` (exact prefix **inferred**: `Soundpad.Soundlist`), per-user under `HKCU\Software\Classes` (`registerFileExtension`), plus a URL protocol (`URL Protocol`, `" -c "%1"`) (static).
- Single instance: second launch forwards to the first (observed: a second launch with a file loaded it into the running window). Mechanism (pipe vs wx IPC): **unknown**.
- Cloud: `https://api.meowpad.me/v2/soundpad/` sound downloads (static).

## Q10. Robustness

| Case | Soundpad (observed unless noted) | Clipdeck should |
|---|---|---|
| Missing file in list | kept; `hash` falls back to the path; playing shows `File not found: <path>` in the status bar; `Find dead entries` + `Fix file location` exist (strings) | same, plus non-blocking badge and bulk relink by `hash`/filename |
| Moved list with relative paths | resolves against the current dir, then falls back to `rel` (static) | same |
| Default sound paths when started from another cwd | first run under Wine with cwd=`re/` put the bundled sounds at `<cwd>\sounds\...` (observed, `GetSoundlist` before first save) | resolve bundled assets from the exe dir |
| Duplicate category membership | silently dropped | warn on import |
| Corrupt list | loader returns failure (`Could not open…` path in decompile); UI message **unknown** | refuse + keep backup, never overwrite |
| No audio device / device change / 1000+ items / read-only dir | **unknown** (`Can't write to the directory.`, `sp-write-test-%lld` probe file string, static) | covered in our tests |

## Open questions that would change the design

1. Mic path on Windows: VB-CABLE (simple, different UX) vs our own user-mode APO (Soundpad's UX, admin install). Linux PipeWire is unaffected.
2. Normalization: copy Soundpad's dB-target scheme for parity, or do LUFS (spec) with a Soundpad-compatible import mapping. Recommend LUFS internally, mapping `normalizeTargetDecibel` on settings migration.
3. One category per sound (Soundpad) vs tags/multi-category (spec). Export to `.spl` must flatten to one.

## How to reproduce

```bash
# static
re/ghidra/gh.sh /tmp/out.c '=lastPlayedOn' fn:1401ee0a0      # decompile by string / address
python3 -I re/tools/peread.py re/target/bin/Soundpad.exe 1408c23f8
# dynamic (hidden Hyprland monitor GLASS + Wine virtual desktop "spdesk")
re/tools/launch.sh 'Z:\path\to\list.spl' &   # start Soundpad
re/tools/rc.sh 'GetSoundlist()'              # pipe client
re/tools/wc.sh tree                          # live control tree incl. combo items
```
