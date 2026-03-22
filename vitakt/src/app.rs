use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU8},
    Arc,
};
use vitakt_core::{
    audio::Command,
    model::{Song, Step},
};

use crate::config::Config;
use crate::history::History;
use crate::theme::Theme;

// ── App state types ───────────────────────────────────────────────────────────

/// Top-level view the TUI is showing.
pub enum View {
    /// Startup screen shown when no project path is given on the CLI.
    Startup,
    SongView,
    ChainView,
    PhraseEditor,
    InstrumentEditor,
    SampleBrowser,
    Mixer,
}

/// What the file browser is selecting.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BrowserMode {
    /// Browse `.wav` files for an instrument sample slot.
    Sample,
    /// Browse `.trk` project files to open.
    Project,
}

#[derive(Debug, PartialEq)]
pub enum InputMode {
    Normal,
    Insert,
    Command,
    Keyboard,
    ConfirmQuit,
}

/// An entry in the sample browser: either a subdirectory or a `.wav` file.
#[derive(Clone)]
pub enum BrowserEntry {
    Dir(String),
    Wav(String),
    /// The synthetic `..` entry that navigates to the parent directory.
    ParentDir,
}

impl BrowserEntry {
    pub fn display_name(&self) -> String {
        match self {
            BrowserEntry::Dir(name) => format!("{name}/"),
            BrowserEntry::Wav(name) => name.clone(),
            BrowserEntry::ParentDir => "..".to_string(),
        }
    }

    pub fn sort_key(&self) -> String {
        match self {
            BrowserEntry::Dir(name) | BrowserEntry::Wav(name) => name.to_lowercase(),
            BrowserEntry::ParentDir => String::new(), // sorts before everything else
        }
    }
}

/// Instrument-editor field indices (cursor position in the editor).
pub const INSTR_FIELD_NAME: usize = 0;
pub const INSTR_FIELD_SAMPLE: usize = 1;
pub const INSTR_FIELD_ROOT: usize = 2;
pub const INSTR_FIELD_LOOP_START: usize = 3;
pub const INSTR_FIELD_LOOP_END: usize = 4;
pub const INSTR_FIELD_INTERP: usize = 5;
pub const INSTR_FIELD_VOLUME: usize = 6;
pub const INSTR_FIELD_PAN: usize = 7;
pub const INSTR_FIELD_COUNT: usize = 8;

/// Maximum number of instruments allowed in one project.
pub const MAX_INSTRUMENTS: usize = 256;

pub struct App {
    pub song: Song,
    /// Which top-level screen is active.
    pub view: View,
    pub mode: InputMode,
    /// Current step row (0..STEPS_PER_PHRASE).
    pub cursor_step: usize,
    /// Current octave (1–8; C4 = MIDI 60 when octave=4).
    pub octave: u8,
    /// Active instrument slot index.
    pub active_instrument: usize,
    /// Pending first 'y' for `yy` copy command.
    pub yy_pending: bool,
    /// Yanked step for `p` paste.
    pub yanked_step: Option<Step>,
    /// Command-mode buffer.
    pub cmd_buf: String,
    /// Status bar message.
    pub status: String,
    /// Ring buffer producer — None if audio unavailable.
    pub producer: Option<rtrb::Producer<Command>>,
    /// Root note of the loaded sample (for pitch calculation).
    pub sample_root: u8,
    /// Whether the sequencer is currently playing (shared with audio thread).
    pub seq_playing: Arc<AtomicBool>,
    /// Current sequencer step index as reported by the audio thread.
    pub current_seq_step: Arc<AtomicU8>,
    /// Instrument editor: which field the cursor is on.
    pub instr_cursor: usize,
    /// Instrument editor: whether we're in text-editing mode for a text field.
    pub instr_editing: bool,
    /// Instrument editor: buffer for in-progress text edits.
    pub instr_edit_buf: String,
    /// Sample browser: list of directories and .wav files in the current directory.
    pub browser_entries: Vec<BrowserEntry>,
    /// Sample browser: cursor row.
    pub browser_cursor: usize,
    /// Sample browser: current directory being listed.
    pub browser_dir: PathBuf,
    /// What the browser is selecting (sample or project file).
    pub browser_mode: BrowserMode,
    /// Sample browser: scroll offset — number of entries hidden above the visible window.
    pub browser_scroll: usize,
    /// Startup screen: cursor (0 = New Project, 1 = Open File).
    pub startup_cursor: usize,
    /// Index of the phrase currently being edited.
    pub active_phrase_idx: usize,
    /// Navigation stack for Esc/Backspace pop-back.
    pub view_stack: Vec<View>,
    /// Song view cursor: current row.
    pub song_cursor_row: usize,
    /// Song view cursor: current track column (0–7).
    pub song_cursor_track: usize,
    /// Chain view: which track's chain we are editing.
    pub chain_view_track: usize,
    /// Chain view: which arrangement row we drilled in from.
    pub chain_view_row: usize,
    /// Chain view cursor: slot index within the chain.
    pub chain_cursor: usize,
    /// Chain view insert mode: editing phrase/transpose values.
    pub chain_insert_mode: bool,
    /// Phrase editor: active column (0=note, 1=ins, 2–9=FX cmd/val pairs).
    pub cursor_col: usize,
    /// Phrase editor: buffer for in-progress FX command or value entry.
    pub fx_edit_buf: String,
    /// Mixer view: active track column (0–7).
    pub mixer_cursor_track: usize,
    /// Mixer view: active field row (0=VOL, 1=PAN, 2=MUTE, 3=SOLO, 4=SEND).
    pub mixer_cursor_field: usize,
    /// Keyboard mode: last used instrument slot index (0–255); persists across mode entries.
    pub keyboard_instrument: usize,
    /// Background WAV render thread (Some while render is in progress).
    pub render_receiver: Option<std::sync::mpsc::Receiver<anyhow::Result<String>>>,
    /// Render progress 0–100, written by the render thread, read by the TUI.
    pub render_progress: Arc<AtomicU32>,
    /// Undo/redo history.
    pub history: History,
    /// Whether the song has unsaved changes.
    pub is_dirty: bool,
    /// Whether the sample browser is currently playing an audio preview.
    pub is_previewing: bool,
    /// Shared flag: audio thread writes false when preview voice self-deactivates.
    pub preview_playing: Arc<AtomicBool>,
    /// When set, status bar shows app.status until this instant (timed messages).
    pub status_timer: Option<std::time::Instant>,
    /// Loaded color theme.
    pub theme: Theme,
    /// Global config loaded from `~/.config/vitakt/config.toml`.
    pub config: Config,
    /// Sample browser: whether the bookmark overlay is open.
    pub browser_show_bookmarks: bool,
    /// Sample browser: cursor index within the bookmark overlay list.
    pub browser_bookmark_cursor: usize,
    /// Set by `browser_launch_external`; tui.rs calls `terminal.clear()` before the next draw.
    pub needs_terminal_clear: bool,
}
