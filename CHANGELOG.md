# Changelog

## 0.1.0 — 2026-11-21

First release.

- Virtual microphone: native PipeWire source "Clipdeck Mic" (Linux), VB-CABLE routing (Windows), real-mic passthrough with optional "mute my voice while a sound plays".
- Lock-free mixer with overlap / restart / stop-others modes, loop, trim, per-sound and master volume, soft clipper; EBU R128 loudness normalization.
- Material 3 Expressive UI (Slint): dynamic color from a seed, light/dark, spring motion and shape morphing, grid and list views, waveform trim editor, categories, favorites, recent, fuzzy search, drag and drop, watched folders, tray icon.
- Global hotkeys on Windows (RegisterHotKey), X11 and Wayland (GlobalShortcuts portal); CLI for compositor binds.
- `clipdeck` CLI and single-instance IPC; accepts Soundpad remote-control syntax.
- Soundpad `.spl` / `stats.spl` import and export, byte-compatible with Soundpad 4.0.35.
