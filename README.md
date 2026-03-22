# tracker

A CLI/TUI sample-based music tracker written in Rust. Arrange samples into phrases, chain phrases into sequences, and mix 8 independent tracks — all from the terminal.

## Installation

### Pre-built binary (macOS Apple Silicon)

Download the latest release, extract the archive, and move the binary onto your `PATH`:

```bash
curl -L https://github.com/ajrussellaudio/tracker/releases/latest/download/tracker-aarch64-apple-darwin.tar.xz \
  | tar -xJ
mv tracker /usr/local/bin/
```

Then launch with:

```bash
tracker
```

### Build from source

**Prerequisites:**
- Rust stable toolchain (`rustup` recommended)
- A system audio device (ALSA on Linux, CoreAudio on macOS, WASAPI on Windows)

```bash
git clone https://github.com/ajrussellaudio/tracker
cd tracker
cargo build --release
```

The binary is at `target/release/tracker`. Run it with `cargo run --release` during development.

## Quick Start

Launch with no arguments to see the startup screen. If installed from the pre-built binary:

```bash
tracker
```

Or, when running from source:

```bash
cargo run --release
```

On the startup screen, choose **New Project** to open a blank song, or **Open File** to browse for a `.trk` project file.

To open a project directly (useful in scripts or shell aliases), pass the path as a positional argument:

```bash
tracker path/to/my-song.trk
# or from source:
cargo run --release -- path/to/my-song.trk
```

On launch you are placed in **Song View**. The status bar at the bottom shows the current BPM and transport state. Press `Space` to play, `q` to quit.

---

## Tutorial

<!-- TODO: write narrative tutorial here -->

---

## Feature Reference

### Song View

The default view. An 8-track × N-row arrangement grid. Each cell holds a chain index (hex `00`–`ff`). The sequencer plays rows top-to-bottom, loops to row 0.

**Navigation to:** default on launch

| Key | Action |
|-----|--------|
| `h` / `l` | Move cursor left / right (tracks) |
| `j` / `↓` | Move cursor down (auto-appends a new row at end) |
| `k` / `↑` | Move cursor up |
| `0`–`9`, `a`–`f` | Assign chain index (hex) to cell |
| `x` / `Delete` | Clear cell |
| `o` | Append new empty row |
| `Enter` | Open Chain View for this cell's chain |
| `Tab` / `F3` | Jump to Phrase Editor |
| `F2` | Jump to Mixer View |
| `F4` | Jump to Instrument Editor |
| `Space` | Play / stop |
| `F5` | Restart playback from step 0 |
| `←` / `→` | Decrease / increase BPM by 1 |
| `u` | Undo |
| `Ctrl+R` | Redo |
| `:` | Enter Command Mode |
| `q` | Quit |

---

### Chain View

A chain is an ordered list of phrase slots with per-slot semitone transpose. Open it by pressing `Enter` on a Song View cell.

**Navigation to:** `Enter` on a Song View cell

#### Normal Mode

| Key | Action |
|-----|--------|
| `j` / `↓` | Move cursor down |
| `k` / `↑` | Move cursor up |
| `h` / `←` | Decrease phrase index for current slot |
| `l` / `→` | Increase phrase index for current slot |
| `,` | Decrease transpose by 1 semitone |
| `.` | Increase transpose by 1 semitone |
| `i` | Enter Insert Mode |
| `a` | Append new slot |
| `d` / `Delete` | Delete current slot |
| `Enter` | Open Phrase Editor for this slot's phrase |
| `Esc` / `Backspace` / `F1` | Return to Song View |
| `u` | Undo |
| `Ctrl+R` | Redo |

#### Insert Mode

| Key | Action |
|-----|--------|
| `h` / `←` | Decrease phrase index |
| `l` / `→` | Increase phrase index |
| `,` | Decrease transpose by 1 semitone |
| `.` | Increase transpose by 1 semitone |
| `Esc` | Return to Normal Mode |

---

### Phrase Editor

A 16-step grid with columns: **NOTE**, **INST** (instrument index), and four **FX** pairs (command + value).

**Navigation to:** `Enter` on a Chain View slot · `Tab` or `F3` from Song View

#### Normal Mode

| Key | Action |
|-----|--------|
| `h` / `l` | Move cursor left / right (columns) |
| `j` / `↓` | Move cursor down (wraps) |
| `k` / `↑` | Move cursor up (wraps) |
| `i` | Enter Insert Mode |
| `d` / `Delete` | Clear current step or FX slot |
| `y` `y` | Yank (copy) current step |
| `p` | Paste yanked step |
| `Tab` | Open Instrument Editor |
| `Space` | Play / stop |
| `F5` | Restart playback from step 0 |
| `←` / `→` | Decrease / increase BPM by 1 |
| `u` | Undo |
| `Ctrl+R` | Redo |
| `:` | Enter Command Mode |
| `F1` | Jump to Song View |
| `Esc` | Return to previous view |
| `q` | Quit |

#### Insert Mode — Note Entry (QWERTY Piano Layout)

The keyboard maps to two piano octave rows. Use `-` / `=` to shift the current octave (1–8). Pressing a note key enters the note and advances the cursor.

**Lower row (white/black keys, octave 0–relative):**

| Key | Note |
|-----|------|
| `z` | C |
| `s` | C# |
| `x` | D |
| `d` | D# |
| `c` | E |
| `v` | F |
| `g` | F# |
| `b` | G |
| `h` | G# |
| `n` | A |
| `j` | A# |
| `m` | B |

**Upper row (+1 octave):**

| Key | Note |
|-----|------|
| `q` | C |
| `2` | C# |
| `w` | D |
| `3` | D# |
| `e` | E |
| `r` | F |
| `5` | F# |
| `t` | G |
| `6` | G# |
| `y` | A |
| `7` | A# |
| `u` | B |

**Other Insert Mode keys:**

| Key | Action |
|-----|--------|
| `-` | Decrease octave |
| `=` | Increase octave |
| `↑` / `↓` / `←` / `→` | Move cursor (clears FX edit buffer) |
| `Delete` / `Backspace` | Backspace FX edit buffer, or clear FX slot |
| `Esc` | Return to Normal Mode |

#### Insert Mode — FX Entry

On an FX **command** column: type up to 3 letters (e.g. `VOL`). On matching 3 chars the cursor advances to the value column automatically.

On an FX **value** column: type up to 3 decimal digits (0–255). On 3 digits the cursor advances to the next step automatically. Press `Enter` to confirm early.

---

### Instrument Editor

Eight fields describing how a sample is played.

**Navigation to:** `Tab` from Phrase Editor · `F4` from Song View

#### Normal Mode

| Key | Action |
|-----|--------|
| `j` / `↓` | Move to next field |
| `k` / `↑` | Move to previous field |
| `i` | Edit Name (text mode) · open Sample Browser (Sample field) · increment other fields |
| `Enter` | Open Sample Browser (on Sample field) |
| `h` / `←` | Decrement numeric or cycle mode field |
| `l` / `→` | Increment numeric or cycle mode field |
| `u` | Undo |
| `Ctrl+R` | Redo |
| `Esc` | Return to previous view |

**Fields:**

| # | Name | Range / Values |
|---|------|---------------|
| 1 | Name | Free text |
| 2 | Sample | File path — browse with `Enter` or `i` |
| 3 | Root Note | MIDI pitch 0–127 (default 60 = C4) |
| 4 | Loop Start | Sample frame offset |
| 5 | Loop End | Sample frame offset (0 = one-shot) |
| 6 | Interp Mode | None (nearest) → Linear → Sinc (Hermite cubic) |
| 7 | Volume | 0.0–1.0, ±0.05 per step |
| 8 | Pan | −1.0 (left) to +1.0 (right), ±0.05 per step |

#### Text Edit Mode (Name field)

| Key | Action |
|-----|--------|
| Any char | Append to name |
| `Backspace` | Delete last character |
| `Enter` | Confirm |
| `Esc` | Cancel |

---

### Sample Browser

Lists `.wav` files and subdirectories in the current browsing directory.

**Navigation to:** `Enter` or `i` on the Sample field in Instrument Editor

| Key | Action |
|-----|--------|
| `j` / `↓` | Move cursor down (wraps) |
| `k` / `↑` | Move cursor up (wraps) |
| `Enter` | Navigate into directory, or load selected file into active instrument |
| `-` / `Backspace` | Navigate to parent directory |
| `Esc` | Cancel and return to previous view |

---

### Startup Screen

Shown when `tracker` is launched with no arguments.

| Key | Action |
|-----|--------|
| `j` / `↓` | Move selection down |
| `k` / `↑` | Move selection up |
| `Enter` | Confirm selection |
| `q` | Quit |

**Options:**

| Option | Effect |
|--------|--------|
| New Project | Open a blank song in Song View |
| Open File | Browse for a `.trk` file to open |

---

### Keyboard Mode

A performance mode that lets you play notes on the QWERTY piano without writing anything to the pattern.

**Navigation to:** `/` from Song View, Chain View, or Phrase Editor (Normal Mode)

| Key | Action |
|-----|--------|
| QWERTY piano | Play the corresponding note on the active instrument |
| `[` | Decrement active instrument index (clamped at 0) |
| `]` | Increment active instrument index (clamped at 255) |
| `Esc` | Return to Normal mode |

The active instrument index is remembered when leaving and re-entering Keyboard mode. The status bar displays `KEYBOARD` and uses the `keyboard_mode_bg` theme colour.

---

### Mixer View

Per-track volume, pan, mute, solo, and FX send for all 8 tracks.

**Navigation to:** `F2` from Song View

| Key | Action |
|-----|--------|
| `h` / `←` | Move to previous track |
| `l` / `→` | Move to next track |
| `j` / `↓` | Move to next field (wraps: VOL → PAN → MUT → SOL → SND) |
| `k` / `↑` | Move to previous field (wraps) |
| `+` / `=` | Increment current field |
| `-` | Decrement current field |
| `m` | Toggle mute for current track |
| `s` | Toggle solo for current track |
| `Space` | Play / stop |
| `F5` | Restart playback from step 0 |
| `u` | Undo |
| `Ctrl+R` | Redo |
| `Esc` / `F2` | Return to previous view |

**Fields:**

| Field | Range | Notes |
|-------|-------|-------|
| VOL | 0.0–2.0 | Default 1.0 |
| PAN | −1.0–+1.0 | Default 0.0 (centre) |
| MUT | on / off | Silences track; shown in red |
| SOL | on / off | Silences all other tracks; shown in yellow |
| SND | 0.0–1.0 | FX send amount |

---

### Transport Controls

Available from Song View and Phrase Editor (Normal Mode).

| Key | Action |
|-----|--------|
| `Space` | Toggle play / stop |
| `F5` | Restart playback from step 0 |
| `←` / `→` | Decrease / increase BPM by 1 |

---

### Command Mode

Enter with `:` from Phrase Editor or Song View Normal Mode. Type the command and press `Enter` to execute. `Esc` cancels.

| Command | Action |
|---------|--------|
| `:w <path>` | Save project to `.trk` file |
| `:e <path>` | Load project from `.trk` file (clears undo history) |
| `:export-json <path>` | Export project as JSON |
| `:export-mix <path>` | Render full stereo mix to WAV (48 kHz f32, runs in background) |
| `:export-stems <dir>` | Render one WAV per track into directory (runs in background) |

Progress for async exports is shown in the status bar.

---

### FX Commands

Used in the FX command/value columns of the Phrase Editor.

| Code | Effect | Value range |
|------|--------|-------------|
| `VOL` | Set step volume | 0–255 (maps to 0.0–1.0) |
| `PAN` | Set stereo pan | 0–255 (128 = centre, 0 = full left, 255 = full right) |
| `PIT` | Pitch shift by semitones | Signed byte: 0–127 = up, 128–255 = down |
| `RET` | Retrigger note at sub-step intervals | Value ≥ 2 = number of repeats |

---

## File Formats

| Extension | Format | Notes |
|-----------|--------|-------|
| `.trk` | bincode (binary) | Default save format; compact and fast |
| `.json` | JSON | Human-readable export via `:export-json` |

**Compatibility:** v1 `.trk` files are not compatible with v2+. Load errors are surfaced in the status bar.
