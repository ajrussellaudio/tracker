# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

#### Core sequencer
- Workspace scaffold, TUI shell, and silent audio engine (#2)
- Core song data model with bincode and JSON save/load (#17)
- BPM sequencer with sample-accurate timing, swing, and transport controls (#6)
- Song and Chain arrangement views with a full 8-track sequencer (#8)
- Phrase editor with QWERTY piano input and live note preview (#5)
- FX slots in the phrase editor: VOL, PAN, PIT (pitch), and RET (retrigger) commands; 2D phrase cursor (#9)
- Live phrase editing while the song arrangement is playing (#39)
- Visible playback head cursor in the Phrase Editor (#37)
- `o`/`O` keys to insert rows and slots in Song and Chain views (#43)

#### Instruments and audio
- Instrument editor with sample browser, loop points, and Hermite interpolation (#7)
- Filesystem sample browser with full directory navigation (#36)
- Space key in the sample browser to toggle wav file preview without committing to a selection (#54)
- Keyboard mode (`/`) for playing QWERTY notes live without recording to a pattern (#41)
- Mixer view with per-track volume, pan, mute, solo, and FX send controls (#10)
- Offline WAV renderer: `:export-mix` for a full stereo mix and `:export-stems` for per-track stems (#11)
- Self-contained project export (`:export-packed`) that bundles sample files into a single archive (#14)

#### UX and editing
- Undo/redo stack with full snapshot history (#12)
- Quit confirmation modal when quitting with unsaved changes; quits immediately when clean (#53)
- `:bpm <value>` command for setting BPM directly from the command line (#51)
- Status bar redesign: mode label shown first; Insert mode uses a distinct background colour (#42)
- Startup screen with project info and key bindings (#40)
- Theme system: load custom colours from `~/.config/tracker/theme.toml` (#14)

### Changed

- BPM arrow-key shortcut (←/→) now shown in the status bar hint

### Documentation

- README with installation instructions, quick-start guide, and full feature reference (#27)
