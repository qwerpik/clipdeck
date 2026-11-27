# Changelog

## 0.2.0 — 2026-11-27

- **Recorder.** The red button in the bottom bar records what you hear (WASAPI loopback on Windows, the default output's monitor on PipeWire) or your microphone. Silence is trimmed, the level is normalized, and the WAV lands in your library under "Recordings". Settings → Recorder sets the folder, category, length limit and what the hotkey records. Also `clipdeck record [speakers|mic|stop]`.
- **Player controls:** pause/resume button, click-to-seek progress bar, previous/next, and "when a sound ends": stop, play the next one, or repeat.
- **Push-to-talk delay** and **karaoke delay** (mic only), and a **voice-activation sound** that opens Discord's voice activation before each sound.
- **Windows: in-microphone effect (beta).** Settings → Microphones puts Clipdeck's sounds straight into your real microphone, like Soundpad, without VB-CABLE. Opt-in; asks for administrator rights. VB-CABLE stays the default.
- Remote control: `seek`, `next`, `previous`, `record`; Soundpad `DoPlayNextSound`, `DoPlayPreviousSound`, `DoSeekMs`, `DoJumpMs`, `DoStartRecording[Speakers|Microphone]`, `DoStopRecording`, `GetRecordingPositionInMs`, `GetPlaybackPositionInMs`, `GetPlaybackDurationInMs`.
- Windows builds link the C runtime statically (no Visual C++ redistributable needed).

## 0.1.0 — 2026-11-21

First release.

- Virtual microphone: native PipeWire source "Clipdeck Mic" (Linux), VB-CABLE routing (Windows), real-mic passthrough with optional "mute my voice while a sound plays".
- Lock-free mixer with overlap / restart / stop-others modes, loop, trim, per-sound and master volume, soft clipper; EBU R128 loudness normalization.
- Material 3 Expressive UI (Slint): dynamic color from a seed, light/dark, spring motion and shape morphing, grid and list views, waveform trim editor, categories, favorites, recent, fuzzy search, drag and drop, watched folders, tray icon.
- Global hotkeys on Windows (RegisterHotKey), X11 and Wayland (GlobalShortcuts portal); CLI for compositor binds.
- `clipdeck` CLI and single-instance IPC; accepts Soundpad remote-control syntax.
- Soundpad `.spl` / `stats.spl` import and export, byte-compatible with Soundpad 4.0.35.
