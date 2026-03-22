# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added

- `vitakt::braille` module: pure `render_waveform(samples, width, height, handles)` function that converts a downsampled amplitude buffer into ratatui `Line`s using Braille Unicode characters (U+2800–U+28FF); the four edit handles (`sample_start`, `sample_end`, `loop_start`, `loop_end`) are overlaid as coloured vertical bars with the active handle highlighted in yellow (#79)
- `vitakt-core::waveform::decode_and_downsample`: decodes a `.wav` file to a normalised `f32` amplitude buffer, mixes down to mono, and downsamples to a target display width using peak-per-window (#74)
- `Instrument` gains `sample_start` and `sample_end` fields (`Option<u32>`); audio playback now honours these boundaries, slicing the sample buffer accordingly; loop points are adjusted relative to the new start; song format bumped to v4 with automatic migration from v3 (#75)
- Global config system: `~/.config/vitakt/config.toml` loaded at startup into `app.config`; supports `bookmarks` (Vec<String>) and `file_browser` (Option<String>); `Config::save()` atomically persists changes; `config.example.toml` ships with the repo (#76)
- Sample browser bookmarks: `b` opens a modal overlay listing saved bookmark directories (existing paths only); arrow keys navigate, Enter jumps to the selected directory, `Esc` dismisses; `B` adds the current directory as a new bookmark and immediately persists it to `config.toml` (#78)
- External file browser integration: pressing `e` in the sample browser suspends the vitakt TUI, launches the command configured in `file_browser` (e.g. `yazi`, `ranger`, `mc`) with `VITAKT_CHOOSER_FILE` set to a temp file path, and loads the selected `.wav` into the active instrument on exit; non-`.wav` selections are silently ignored; `e` does nothing when `file_browser` is unset; README documents setup for yazi, ranger, and mc (#80)
- Scrolling viewport in the sample browser: long directory listings now scroll so the cursor is always visible; viewport height is derived from the terminal size at draw time (#72)
- `..` parent directory entry appears at the top of every non-root directory in the sample browser; pressing Enter on it navigates up just like Backspace (#73)

### Changed

- Extract `note_utils`, `wav_io`, and `cli` modules from `vitakt/src/main.rs` (Phase 1 of #84 refactor): pure note/column helpers, WAV encode/decode helpers, and CLI argument parsing now live in their own modules (#84)
- Extract `app`, `render`, and `audio_stream` modules from `vitakt/src/main.rs` (Phase 2 refactor): app state types and enums live in `app.rs`; all 7 TUI render functions live in `render.rs`; `start_audio_stream` lives in `audio_stream.rs` (#92)
- Extract `browser`, `commands`, `app_core`, `input`, and `tui` modules from `vitakt/src/main.rs` (Phase 3 refactor): browser logic in `browser.rs`; command execution and instrument-editor helpers in `commands.rs`; core `App` impl methods in `app_core.rs`; all key-event handlers in `input.rs`; TUI event loop in `tui.rs`; `main.rs` reduced to module declarations, `main()`, and the test suite (#93)

- Rename Cargo workspace and crate names from `tracker`/`tracker-core` to `vitakt`/`vitakt-core`; rename source directories accordingly (#62)
- Rename user-visible strings, config path (`~/.config/vitakt/theme.toml`), usage messages, and internal test fixtures from `tracker` to `vitakt`; add one-time migration that copies `~/.config/tracker/theme.toml` to the new location on first run (#63)
- Update README, CHANGELOG, and `theme.example.toml` to reflect the `vitakt` name (#64)
- Update `ralph/project.md` repo slug and remote URL to `ajrussellaudio/vitakt`; update CHANGELOG issue links to the new repo URL (#65)

## [0.1.1] - 2026-03-22

### Changed

- Shell installer script added to releases — install with a single `curl` command

## [0.1.0] - 2026-03-22

### Added

#### Core sequencer
- Workspace scaffold, TUI shell, and silent audio engine ([#2](https://github.com/ajrussellaudio/vitakt/issues/2))
- Core song data model with bincode and JSON save/load ([#17](https://github.com/ajrussellaudio/vitakt/issues/17))
- BPM sequencer with sample-accurate timing, swing, and transport controls ([#6](https://github.com/ajrussellaudio/vitakt/issues/6))
- Song and Chain arrangement views with a full 8-track sequencer ([#8](https://github.com/ajrussellaudio/vitakt/issues/8))
- Phrase editor with QWERTY piano input and live note preview ([#5](https://github.com/ajrussellaudio/vitakt/issues/5))
- FX slots in the phrase editor: VOL, PAN, PIT (pitch), and RET (retrigger) commands; 2D phrase cursor ([#9](https://github.com/ajrussellaudio/vitakt/issues/9))
- Live phrase editing while the song arrangement is playing ([#39](https://github.com/ajrussellaudio/vitakt/issues/39))
- Visible playback head cursor in the Phrase Editor ([#37](https://github.com/ajrussellaudio/vitakt/issues/37))
- `o`/`O` keys to insert rows and slots in Song and Chain views ([#43](https://github.com/ajrussellaudio/vitakt/issues/43))

#### Instruments and audio
- Instrument editor with sample browser, loop points, and Hermite interpolation ([#7](https://github.com/ajrussellaudio/vitakt/issues/7))
- Filesystem sample browser with full directory navigation ([#36](https://github.com/ajrussellaudio/vitakt/issues/36))
- Space key in the sample browser to toggle wav file preview without committing to a selection ([#54](https://github.com/ajrussellaudio/vitakt/issues/54))
- Keyboard mode (`/`) for playing QWERTY notes live without recording to a pattern ([#41](https://github.com/ajrussellaudio/vitakt/issues/41))
- Mixer view with per-track volume, pan, mute, solo, and FX send controls ([#10](https://github.com/ajrussellaudio/vitakt/issues/10))
- Offline WAV renderer: `:export-mix` for a full stereo mix and `:export-stems` for per-track stems ([#11](https://github.com/ajrussellaudio/vitakt/issues/11))
- Self-contained project export (`:export-packed`) that bundles sample files into a single archive ([#14](https://github.com/ajrussellaudio/vitakt/issues/14))

#### UX and editing
- Undo/redo stack with full snapshot history ([#12](https://github.com/ajrussellaudio/vitakt/issues/12))
- Quit confirmation modal when quitting with unsaved changes; quits immediately when clean ([#53](https://github.com/ajrussellaudio/vitakt/issues/53))
- `:bpm <value>` command for setting BPM directly from the command line ([#51](https://github.com/ajrussellaudio/vitakt/issues/51))
- Status bar redesign: mode label shown first; Insert mode uses a distinct background colour ([#42](https://github.com/ajrussellaudio/vitakt/issues/42))
- Startup screen with project info and key bindings ([#40](https://github.com/ajrussellaudio/vitakt/issues/40))
- Theme system: load custom colours from `~/.config/vitakt/theme.toml` ([#14](https://github.com/ajrussellaudio/vitakt/issues/14))

### Changed

- BPM arrow-key shortcut (←/→) now shown in the status bar hint ([#50](https://github.com/ajrussellaudio/vitakt/issues/50))

### Documentation

- README with installation instructions, quick-start guide, and full feature reference ([#27](https://github.com/ajrussellaudio/vitakt/issues/27))
