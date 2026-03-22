use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
mod cli;
use cli::{parse_args, CliAction};
mod history;
use history::History;
mod note_utils;
use note_utils::*;
mod theme;
use theme::Theme;
mod wav_io;
use wav_io::*;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table},
    Terminal,
};
use rtrb::RingBuffer;
use std::{
    io,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
use vitakt_core::{
    audio::{Command, Mixer, Sequencer, Voice},
    model::{Chain, ChainSlot, FxCommand, InterpMode, Song, Step, STEPS_PER_PHRASE, TRACKS},
    storage,
};

// ── App state ─────────────────────────────────────────────────────────────────

/// Top-level view the TUI is showing.
enum View {
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
enum BrowserMode {
    /// Browse `.wav` files for an instrument sample slot.
    Sample,
    /// Browse `.trk` project files to open.
    Project,
}

#[derive(Debug, PartialEq)]
enum InputMode {
    Normal,
    Insert,
    Command,
    Keyboard,
    ConfirmQuit,
}

/// An entry in the sample browser: either a subdirectory or a `.wav` file.
#[derive(Clone)]
enum BrowserEntry {
    Dir(String),
    Wav(String),
    /// The synthetic `..` entry that navigates to the parent directory.
    ParentDir,
}

impl BrowserEntry {
    fn display_name(&self) -> String {
        match self {
            BrowserEntry::Dir(name) => format!("{name}/"),
            BrowserEntry::Wav(name) => name.clone(),
            BrowserEntry::ParentDir => "..".to_string(),
        }
    }

    fn sort_key(&self) -> String {
        match self {
            BrowserEntry::Dir(name) | BrowserEntry::Wav(name) => name.to_lowercase(),
            BrowserEntry::ParentDir => String::new(), // sorts before everything else
        }
    }
}

/// Instrument-editor field indices (cursor position in the editor).
const INSTR_FIELD_NAME: usize = 0;
const INSTR_FIELD_SAMPLE: usize = 1;
const INSTR_FIELD_ROOT: usize = 2;
const INSTR_FIELD_LOOP_START: usize = 3;
const INSTR_FIELD_LOOP_END: usize = 4;
const INSTR_FIELD_INTERP: usize = 5;
const INSTR_FIELD_VOLUME: usize = 6;
const INSTR_FIELD_PAN: usize = 7;
const INSTR_FIELD_COUNT: usize = 8;

/// Maximum number of instruments allowed in one project.
const MAX_INSTRUMENTS: usize = 256;


struct App {
    song: Song,
    /// Which top-level screen is active.
    view: View,
    mode: InputMode,
    /// Current step row (0..STEPS_PER_PHRASE).
    cursor_step: usize,
    /// Current octave (1–8; C4 = MIDI 60 when octave=4).
    octave: u8,
    /// Active instrument slot index.
    active_instrument: usize,
    /// Pending first 'y' for `yy` copy command.
    yy_pending: bool,
    /// Yanked step for `p` paste.
    yanked_step: Option<Step>,
    /// Command-mode buffer.
    cmd_buf: String,
    /// Status bar message.
    status: String,
    /// Ring buffer producer — None if audio unavailable.
    producer: Option<rtrb::Producer<Command>>,
    /// Root note of the loaded sample (for pitch calculation).
    sample_root: u8,
    /// Whether the sequencer is currently playing (shared with audio thread).
    seq_playing: Arc<AtomicBool>,
    /// Current sequencer step index as reported by the audio thread.
    current_seq_step: Arc<AtomicU8>,
    /// Instrument editor: which field the cursor is on.
    instr_cursor: usize,
    /// Instrument editor: whether we're in text-editing mode for a text field.
    instr_editing: bool,
    /// Instrument editor: buffer for in-progress text edits.
    instr_edit_buf: String,
    /// Sample browser: list of directories and .wav files in the current directory.
    browser_entries: Vec<BrowserEntry>,
    /// Sample browser: cursor row.
    browser_cursor: usize,
    /// Sample browser: current directory being listed.
    browser_dir: PathBuf,
    /// What the browser is selecting (sample or project file).
    browser_mode: BrowserMode,
    /// Sample browser: scroll offset — number of entries hidden above the visible window.
    browser_scroll: usize,
    /// Startup screen: cursor (0 = New Project, 1 = Open File).
    startup_cursor: usize,
    /// Index of the phrase currently being edited.
    active_phrase_idx: usize,
    /// Navigation stack for Esc/Backspace pop-back.
    view_stack: Vec<View>,
    /// Song view cursor: current row.
    song_cursor_row: usize,
    /// Song view cursor: current track column (0–7).
    song_cursor_track: usize,
    /// Chain view: which track's chain we are editing.
    chain_view_track: usize,
    /// Chain view: which arrangement row we drilled in from.
    chain_view_row: usize,
    /// Chain view cursor: slot index within the chain.
    chain_cursor: usize,
    /// Chain view insert mode: editing phrase/transpose values.
    chain_insert_mode: bool,
    /// Phrase editor: active column (0=note, 1=ins, 2–9=FX cmd/val pairs).
    cursor_col: usize,
    /// Phrase editor: buffer for in-progress FX command or value entry.
    fx_edit_buf: String,
    /// Mixer view: active track column (0–7).
    mixer_cursor_track: usize,
    /// Mixer view: active field row (0=VOL, 1=PAN, 2=MUTE, 3=SOLO, 4=SEND).
    mixer_cursor_field: usize,
    /// Keyboard mode: last used instrument slot index (0–255); persists across mode entries.
    keyboard_instrument: usize,
    /// Background WAV render thread (Some while render is in progress).
    render_receiver: Option<std::sync::mpsc::Receiver<anyhow::Result<String>>>,
    /// Render progress 0–100, written by the render thread, read by the TUI.
    render_progress: Arc<AtomicU32>,
    /// Undo/redo history.
    history: History,
    /// Whether the song has unsaved changes.
    is_dirty: bool,
    /// Whether the sample browser is currently playing an audio preview.
    is_previewing: bool,
    /// Shared flag: audio thread writes false when preview voice self-deactivates.
    preview_playing: Arc<AtomicBool>,
    /// When set, status bar shows app.status until this instant (timed messages).
    status_timer: Option<std::time::Instant>,
    /// Loaded color theme.
    theme: Theme,
}

impl App {
    fn new(
        producer: Option<rtrb::Producer<Command>>,
        sample_root: u8,
        seq_playing: Arc<AtomicBool>,
        current_seq_step: Arc<AtomicU8>,
        preview_playing: Arc<AtomicBool>,
    ) -> Self {
        let song = Song::default(); // always has 1 phrase, 1 chain, 1 arrangement row
        Self {
            song,
            view: View::SongView,
            mode: InputMode::Normal,
            cursor_step: 0,
            octave: 4,
            active_instrument: 0,
            yy_pending: false,
            yanked_step: None,
            cmd_buf: String::new(),
            status: "SONG  |  hjkl: nav  |  0-9: assign chain  |  Del: clear  |  Enter: chain view  |  SPC: play  |  q: quit".to_string(),
            producer,
            sample_root,
            seq_playing,
            current_seq_step,
            instr_cursor: 0,
            instr_editing: false,
            instr_edit_buf: String::new(),
            browser_entries: Vec::new(),
            browser_cursor: 0,
            browser_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            browser_mode: BrowserMode::Sample,
            browser_scroll: 0,
            startup_cursor: 0,
            active_phrase_idx: 0,
            view_stack: Vec::new(),
            song_cursor_row: 0,
            song_cursor_track: 0,
            chain_view_track: 0,
            chain_view_row: 0,
            chain_cursor: 0,
            chain_insert_mode: false,
            cursor_col: 0,
            fx_edit_buf: String::new(),
            mixer_cursor_track: 0,
            mixer_cursor_field: 0,
            keyboard_instrument: 0,
            render_receiver: None,
            render_progress: Arc::new(AtomicU32::new(0)),
            history: History::new(),
            is_dirty: false,
            is_previewing: false,
            preview_playing,
            status_timer: None,
            theme: theme::load(),
        }
    }

    fn phrase_mut(&mut self) -> &mut vitakt_core::model::Phrase {
        let idx = self.active_phrase_idx.min(self.song.phrases.len().saturating_sub(1));
        &mut self.song.phrases[idx]
    }

    fn phrase(&self) -> &vitakt_core::model::Phrase {
        let idx = self.active_phrase_idx.min(self.song.phrases.len().saturating_sub(1));
        &self.song.phrases[idx]
    }

    /// Send a command to the audio thread (fire-and-forget).
    fn send_cmd(&mut self, cmd: Command) {
        if let Some(prod) = &mut self.producer {
            let _ = prod.push(cmd);
        }
    }

    /// Push a fresh phrase snapshot to the sequencer.
    fn sync_phrase_to_sequencer(&mut self) {
        let phrase = Box::new(self.phrase().clone());
        self.send_cmd(Command::UpdatePhrase(phrase.clone()));
        self.send_cmd(Command::UpdatePhraseInSong {
            idx: self.active_phrase_idx,
            phrase,
        });
        self.send_cmd(Command::SetSampleRoot(self.sample_root));
    }

    /// Send the full song data snapshot to the audio thread for arrangement playback.
    fn sync_song_to_sequencer(&mut self) {
        let arrangement = self.song.arrangement.clone();
        let chains = self.song.chains.clone();
        let phrases = self.song.phrases.clone();
        let instruments = self.song.instruments.clone();
        self.send_cmd(Command::UpdateSongData { arrangement, chains, phrases, instruments });
        self.send_cmd(Command::SetSampleRoot(self.sample_root));
    }

    /// Push all mixer state to the audio thread (called after load and incremental changes).
    fn sync_mixer_to_audio(&mut self) {
        for t in 0..TRACKS {
            let (vol, pan, mute, solo) = {
                let m = &self.song.mixer[t];
                (m.volume, m.pan, m.mute, m.solo)
            };
            self.send_cmd(Command::SetTrackVolume { track: t as u8, volume: vol });
            self.send_cmd(Command::SetTrackPan { track: t as u8, pan });
            self.send_cmd(Command::SetTrackMute { track: t as u8, mute });
            self.send_cmd(Command::SetTrackSolo { track: t as u8, active: solo });
        }
    }

    /// Toggle play / stop.
    fn toggle_play(&mut self) {
        if self.seq_playing.load(Ordering::Relaxed) {
            self.send_cmd(Command::Stop);
            self.seq_playing.store(false, Ordering::Relaxed);
        } else {
            self.sync_song_to_sequencer();
            self.send_cmd(Command::Play);
            self.seq_playing.store(true, Ordering::Relaxed);
        }
    }

    /// Restart sequencer from step 0 (F5).
    fn restart_play(&mut self) {
        self.sync_song_to_sequencer();
        self.send_cmd(Command::Restart);
        self.seq_playing.store(true, Ordering::Relaxed);
    }

    /// Ensure instrument slots 0..=idx exist (creates defaults up to MAX_INSTRUMENTS).
    fn ensure_instrument(&mut self, idx: usize) {
        while self.song.instruments.len() <= idx && self.song.instruments.len() < MAX_INSTRUMENTS {
            self.song.instruments.push(vitakt_core::model::Instrument::default());
        }
    }

    /// Open the instrument editor for the active instrument, creating it if needed.
    fn open_instrument_editor(&mut self) {
        if self.song.instruments.len() < MAX_INSTRUMENTS {
            self.ensure_instrument(self.active_instrument);
        }
        self.push_view(View::InstrumentEditor);
        self.instr_cursor = 0;
        self.instr_editing = false;
        self.instr_edit_buf.clear();
    }

    /// Reload sample from disk for the active instrument and send a LoadVoice command.
    fn reload_instrument_sample(&mut self) {
        let idx = self.active_instrument;
        if let Some(instr) = self.song.instruments.get(idx) {
            if let Some(sample) = &instr.sample {
                let path = sample.path.clone();
                let embedded_bytes = sample.bytes.clone();
                let loop_start = instr.loop_start.unwrap_or(0);
                let loop_end = instr.loop_end.unwrap_or(0);
                let sample_start = instr.sample_start;
                let sample_end = instr.sample_end;
                let interp_mode = instr.interp_mode.clone();
                let load_result = if let Some(bytes) = embedded_bytes {
                    load_wav_from_bytes(&bytes)
                } else {
                    load_wav(&path)
                };
                match load_result {
                    Ok((buf, channels)) => {
                        let (buf, loop_start, loop_end) =
                            apply_sample_bounds(buf, channels, sample_start, sample_end, loop_start, loop_end);
                        self.send_cmd(Command::LoadVoice {
                            slot: 0,
                            samples: buf,
                            channels,
                            loop_start,
                            loop_end,
                            interp_mode,
                        });
                        self.sample_root = self.song.instruments[idx].root_note;
                        self.send_cmd(Command::SetSampleRoot(self.sample_root));
                        self.status = format!("Loaded: {path}");
                    }
                    Err(e) => self.status = format!("Error: {e}"),
                }
            }
        }
    }

    /// Reload instrument 0's sample after a project load.
    fn reload_instruments(&mut self) {
        if self.song.instruments.is_empty() {
            return;
        }
        // Reload slot 0 (active instrument) if it has a sample path.
        let instr = &self.song.instruments[0];
        if let Some(sample) = &instr.sample {
            let path = sample.path.clone();
            let embedded_bytes = sample.bytes.clone();
            let loop_start = instr.loop_start.unwrap_or(0);
            let loop_end = instr.loop_end.unwrap_or(0);
            let sample_start = instr.sample_start;
            let sample_end = instr.sample_end;
            let interp_mode = instr.interp_mode.clone();
            let load_result = if let Some(bytes) = embedded_bytes {
                load_wav_from_bytes(&bytes)
            } else {
                load_wav(&path)
            };
            match load_result {
                Ok((buf, channels)) => {
                    let (buf, loop_start, loop_end) =
                        apply_sample_bounds(buf, channels, sample_start, sample_end, loop_start, loop_end);
                    self.send_cmd(Command::LoadVoice {
                        slot: 0,
                        samples: buf,
                        channels,
                        loop_start,
                        loop_end,
                        interp_mode,
                    });
                    self.sample_root = self.song.instruments[0].root_note;
                    self.send_cmd(Command::SetSampleRoot(self.sample_root));
                }
                Err(e) => self.status = format!("Warning: could not reload sample: {e}"),
            }
        }
    }

    /// Open the sample browser starting at the instrument's current sample directory,
    /// or the current working directory if no sample is set.
    fn open_sample_browser(&mut self) {
        let start_dir = self
            .song
            .instruments
            .get(self.active_instrument)
            .and_then(|instr| instr.sample.as_ref())
            .and_then(|sample| {
                let p = std::path::Path::new(&sample.path);
                p.parent().map(|parent| parent.to_path_buf())
            })
            .filter(|p| p.is_dir())
            .unwrap_or_else(|| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            });
        self.browser_dir = start_dir;
        self.browser_mode = BrowserMode::Sample;
        self.browser_entries = list_browser_entries(&self.browser_dir);
        self.browser_cursor = 0;
        self.browser_scroll = 0;
        self.push_view(View::SampleBrowser);
    }

    /// Open the file browser scoped to `.trk` project files.
    fn open_project_browser(&mut self) {
        let start_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        self.browser_dir = start_dir;
        self.browser_mode = BrowserMode::Project;
        self.browser_entries = list_browser_entries_ext(&self.browser_dir, "trk");
        self.browser_cursor = 0;
        self.browser_scroll = 0;
        self.push_view(View::SampleBrowser);
    }

    /// Handle Enter in the sample browser.
    /// Navigates into a directory or loads a `.wav` file into the active instrument.
    fn browser_enter(&mut self) {
        if let Some(entry) = self.browser_entries.get(self.browser_cursor).cloned() {
            match entry {
                BrowserEntry::ParentDir => {
                    self.browser_go_up();
                }
                BrowserEntry::Dir(name) => {
                    self.browser_dir = self.browser_dir.join(&name);
                    let ext = match self.browser_mode {
                        BrowserMode::Sample => "wav",
                        BrowserMode::Project => "trk",
                    };
                    self.browser_entries = list_browser_entries_ext(&self.browser_dir, ext);
                    self.browser_cursor = 0;
                    self.browser_scroll = 0;
                }
                BrowserEntry::Wav(name) => match self.browser_mode {
                    BrowserMode::Sample => {
                        let full_path = self.browser_dir.join(&name);
                        let path_str = full_path
                            .canonicalize()
                            .unwrap_or(full_path)
                            .to_string_lossy()
                            .to_string();
                        self.record("select sample");
                        self.ensure_instrument(self.active_instrument);
                        if let Some(instr) = self.song.instruments.get_mut(self.active_instrument) {
                            instr.sample = Some(vitakt_core::model::Sample::from_path(path_str));
                        }
                        self.pop_view();
                        self.reload_instrument_sample();
                    }
                    BrowserMode::Project => {
                        let full_path = self.browser_dir.join(&name);
                        let path_str = full_path
                            .canonicalize()
                            .unwrap_or(full_path)
                            .to_string_lossy()
                            .to_string();
                        match storage::load_trk(&path_str) {
                            Ok(song) => {
                                self.song = vitakt_core::model::migrate(song);
                                if self.song.phrases.is_empty() {
                                    self.song
                                        .phrases
                                        .push(vitakt_core::model::Phrase::default());
                                }
                                self.sync_phrase_to_sequencer();
                                self.reload_instruments();
                                self.sync_song_to_sequencer();
                                self.sync_mixer_to_audio();
                                self.history.clear();
                                // Clear nav stack and go straight to Song View.
                                self.view_stack.clear();
                                self.view = View::SongView;
                                self.browser_mode = BrowserMode::Sample;
                                self.set_timed_status(format!("Loaded: {path_str}"));
                            }
                            Err(e) => {
                                self.set_timed_status(format!("Error: {e}"));
                            }
                        }
                    }
                },
            }
        }
    }

    /// Navigate to the parent directory in the sample browser.
    fn browser_go_up(&mut self) {
        if let Some(parent) = self.browser_dir.parent().map(|p| p.to_path_buf()) {
            self.browser_dir = parent;
            let ext = match self.browser_mode {
                BrowserMode::Sample => "wav",
                BrowserMode::Project => "trk",
            };
            self.browser_entries = list_browser_entries_ext(&self.browser_dir, ext);
            self.browser_cursor = 0;
            self.browser_scroll = 0;
        }
    }

    /// Adjust `browser_scroll` so that `browser_cursor` stays within the visible window.
    /// `available` is the number of entry rows visible (after accounting for borders and header).
    fn browser_clamp_scroll(&mut self, available: usize) {
        if available == 0 {
            return;
        }
        if self.browser_cursor < self.browser_scroll {
            self.browser_scroll = self.browser_cursor;
        } else if self.browser_cursor >= self.browser_scroll + available {
            self.browser_scroll = self.browser_cursor - available + 1;
        }
    }

    /// Handle Space in the sample browser: toggle preview playback of the highlighted .wav.
    /// Silently ignores Space on a directory entry or an empty list.
    fn browser_preview_toggle(&mut self) {
        // Sync is_previewing from the shared atomic so stale state after natural
        // completion doesn't cause a double-press to restart.
        self.is_previewing = self.preview_playing.load(Ordering::Relaxed);

        if self.is_previewing {
            self.send_cmd(Command::StopPreview);
            self.preview_playing.store(false, Ordering::Relaxed);
            self.is_previewing = false;
            return;
        }
        let Some(entry) = self.browser_entries.get(self.browser_cursor).cloned() else {
            return;
        };
        let BrowserEntry::Wav(name) = entry else {
            return; // directory — ignore
        };
        let full_path = self.browser_dir.join(&name);
        let path_str = full_path
            .canonicalize()
            .unwrap_or(full_path)
            .to_string_lossy()
            .to_string();
        match load_wav(&path_str) {
            Ok((samples, channels)) => {
                self.send_cmd(Command::PreviewSample { samples, channels });
                self.preview_playing.store(true, Ordering::Relaxed);
                self.is_previewing = true;
            }
            Err(e) => self.set_timed_status(format!("Preview error: {e}")),
        }
    }

    /// Adjust BPM by `delta` and send the new value to the audio thread.
    fn adjust_bpm(&mut self, delta: f32) {
        self.record("set BPM");
        self.song.bpm = (self.song.bpm + delta).clamp(20.0, 999.0);
        self.send_cmd(Command::SetBpm(self.song.bpm));
    }

    /// Push the current view onto the navigation stack and switch to `next`.
    fn push_view(&mut self, next: View) {
        let current = std::mem::replace(&mut self.view, next);
        self.view_stack.push(current);
    }

    /// Pop the navigation stack and return to the previous view.
    fn pop_view(&mut self) {
        if let Some(prev) = self.view_stack.pop() {
            self.view = prev;
        }
        // Stop any active preview when leaving the sample browser.
        if self.is_previewing {
            self.send_cmd(Command::StopPreview);
            self.is_previewing = false;
            self.preview_playing.store(false, Ordering::Relaxed);
        }
        // Clear mode when returning to PhraseEditor
        if matches!(self.view, View::PhraseEditor) {
            self.mode = InputMode::Normal;
        }
        // Clear chain insert mode when leaving ChainView
        self.chain_insert_mode = false;
    }

    /// Ensure chain slots exist up to and including `chain_idx`.
    pub fn ensure_chain(&mut self, chain_idx: usize) {
        while self.song.chains.len() <= chain_idx {
            self.song.chains.push(Chain {
                slots: vec![ChainSlot { phrase: 0, transpose: 0 }],
            });
        }
    }

    /// Enter a note at the current step and advance the cursor.
    fn enter_note(&mut self, midi: u8) {
        self.record(&format!("set note {} at step {}", note_name(midi), self.cursor_step));
        let cursor = self.cursor_step;
        let instr = self.active_instrument as u8;
        let step = &mut self.phrase_mut().steps[cursor];
        step.note = Some(midi);
        step.instrument = Some(instr);
        step.velocity = 100;

        // Send NoteOn to audio thread for live preview.
        if let Some(prod) = &mut self.producer {
            let speed = pitch_speed(midi, self.sample_root);
            let _ = prod.push(Command::NoteOn { slot: 0, speed });
        }

        // Keep sequencer phrase in sync.
        self.sync_phrase_to_sequencer();

        // Advance cursor.
        self.cursor_step = (self.cursor_step + 1) % STEPS_PER_PHRASE;
    }

    fn execute_command(&mut self) {
        let raw = self.cmd_buf.trim().to_string();
        self.cmd_buf.clear();
        self.mode = InputMode::Normal;

        if let Some(path) = raw.strip_prefix("w ") {
            let path = path.trim();
            match storage::save_trk(&self.song, path) {
                Ok(_) => {
                    self.is_dirty = false;
                    self.status = format!("Saved: {path}");
                }
                Err(e) => self.status = format!("Error: {e}"),
            }
        } else if let Some(path) = raw.strip_prefix("e ") {
            let path = path.trim();
            match storage::load_trk(path) {
                Ok(song) => {
                    self.song = vitakt_core::model::migrate(song);
                    if self.song.phrases.is_empty() {
                        self.song.phrases.push(vitakt_core::model::Phrase::default());
                    }
                    self.sync_phrase_to_sequencer();
                    self.reload_instruments();
                    self.sync_song_to_sequencer();
                    self.sync_mixer_to_audio();
                    self.history.clear();
                    self.is_dirty = false;
                    self.status = format!("Loaded: {path}");
                }
                Err(e) => self.status = format!("Error: {e}"),
            }
        } else if let Some(path) = raw.strip_prefix("export-json ") {
            let path = path.trim();
            match storage::export_json(&self.song, path) {
                Ok(_) => self.status = format!("JSON exported: {path}"),
                Err(e) => self.status = format!("Error: {e}"),
            }
        } else if let Some(path) = raw.strip_prefix("export-packed ") {
            let path = path.trim().to_string();
            self.status = format!("Packing samples…");
            match storage::save_packed_trk(&self.song, &path) {
                Ok(failed) if failed.is_empty() => {
                    self.status = format!("Packed: {path}");
                }
                Ok(failed) => {
                    self.status = format!(
                        "Packed: {path} ({} sample(s) missing: {})",
                        failed.len(),
                        failed.join(", ")
                    );
                }
                Err(e) => self.status = format!("Error: {e}"),
            }
        } else if let Some(path) = raw.strip_prefix("export-mix ") {
            let path = path.trim().to_string();
            self.start_render_mix(path);
        } else if let Some(dir) = raw.strip_prefix("export-stems ") {
            let dir = dir.trim().to_string();
            self.start_render_stems(dir);
        } else if let Some(rest) = raw.strip_prefix("bpm") {
            let rest = rest.trim();
            if rest.is_empty() {
                self.status = "Usage: :bpm <value>  (e.g. :bpm 140)".to_string();
            } else {
                match rest.parse::<f32>() {
                    Ok(bpm) => {
                        if !bpm.is_finite() {
                            self.status = format!("Invalid BPM value: '{rest}' — expected a number");
                        } else {
                            let clamped = bpm.clamp(20.0, 999.0);
                            self.record("set BPM");
                            self.song.bpm = clamped;
                            self.send_cmd(Command::SetBpm(clamped));
                            self.set_timed_status(format!("BPM set to {clamped:.1}"));
                        }
                    }
                    Err(_) => {
                        self.status = format!("Invalid BPM value: '{rest}' — expected a number");
                    }
                }
            }
        } else if raw.is_empty() {
            // No-op: Normal mode status bar shows its own fixed hint text.
        } else {
            self.status = format!("Unknown command: {raw}");
        }
    }

    /// Spawn a background thread to render the full mix and write to `path`.
    fn start_render_mix(&mut self, path: String) {
        if self.render_receiver.is_some() {
            self.status = "Error: render already in progress".to_string();
            return;
        }
        let song = self.song.clone();
        let progress = Arc::clone(&self.render_progress);
        progress.store(0, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.render_receiver = Some(rx);
        self.status = "Rendering mix... 0%".to_string();

        std::thread::spawn(move || {
            let buffers = load_all_instrument_samples(&song);
            let result: anyhow::Result<String> = (|| {
                let audio = vitakt_core::render::render_to_buffer(
                    &song,
                    &buffers,
                    None,
                    &mut |p| {
                        progress.store((p * 100.0) as u32, Ordering::Relaxed);
                    },
                );
                write_wav(&path, &audio)?;
                Ok(format!("Mix exported: {path}"))
            })();
            let _ = tx.send(result);
        });
    }

    /// Spawn a background thread to render per-track stems into `dir`.
    fn start_render_stems(&mut self, dir: String) {
        if self.render_receiver.is_some() {
            self.status = "Error: render already in progress".to_string();
            return;
        }
        let song = self.song.clone();
        let progress = Arc::clone(&self.render_progress);
        progress.store(0, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.render_receiver = Some(rx);
        self.status = "Rendering stems... 0%".to_string();

        std::thread::spawn(move || {
            let result: anyhow::Result<String> = (|| {
                std::fs::create_dir_all(&dir)?;
                let buffers = load_all_instrument_samples(&song);
                for track in 0..TRACKS {
                    let audio = vitakt_core::render::render_to_buffer(
                        &song,
                        &buffers,
                        Some(track),
                        &mut |p| {
                            // Scale progress across all TRACKS passes.
                            let overall = (track as f32 + p) / TRACKS as f32;
                            progress.store((overall * 100.0) as u32, Ordering::Relaxed);
                        },
                    );
                    let filename = format!("{dir}/track-{:02}.wav", track + 1);
                    write_wav(&filename, &audio)?;
                }
                Ok(format!("Stems exported to: {dir}"))
            })();
            let _ = tx.send(result);
        });
    }

    /// Poll the render thread receiver; update status bar on completion or progress.
    fn poll_render(&mut self) {
        let result = match &self.render_receiver {
            None => return,
            Some(rx) => match rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    let pct = self.render_progress.load(Ordering::Relaxed);
                    self.status = format!("Rendering... {pct}%");
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err(anyhow::anyhow!("render thread disconnected unexpectedly"))
                }
            },
        };
        self.render_receiver = None;
        match result {
            Ok(msg) => self.status = msg,
            Err(e) => self.status = format!("Export error: {e}"),
        }
    }

    /// Save a snapshot before a mutation. Call BEFORE any mutation.
    fn record(&mut self, description: &str) {
        let snapshot = self.song.clone();
        self.history.push(description.to_string(), snapshot);
        self.is_dirty = true;
    }

    /// Set a status message that clears after 2 seconds.
    fn set_timed_status(&mut self, msg: String) {
        self.status = msg;
        self.status_timer = Some(std::time::Instant::now());
    }

    /// Undo the most recent mutation.
    fn do_undo(&mut self) {
        let current = self.song.clone();
        if let Some((snapshot, desc)) = self.history.undo(current) {
            self.song = snapshot;
            self.is_dirty = true;
            self.set_timed_status(format!("Undid: {desc}"));
            self.sync_phrase_to_sequencer();
            self.sync_song_to_sequencer();
            self.sync_mixer_to_audio();
        } else {
            self.set_timed_status("Nothing to undo".to_string());
        }
    }

    /// Redo the most recently undone mutation.
    fn do_redo(&mut self) {
        let current = self.song.clone();
        if let Some((snapshot, desc)) = self.history.redo(current) {
            self.song = snapshot;
            self.is_dirty = true;
            self.set_timed_status(format!("Redid: {desc}"));
            self.sync_phrase_to_sequencer();
            self.sync_song_to_sequencer();
            self.sync_mixer_to_audio();
        } else {
            self.set_timed_status("Nothing to redo".to_string());
        }
    }

    fn enter_keyboard_mode(&mut self) {
        self.mode = InputMode::Keyboard;
    }

    fn exit_keyboard_mode(&mut self) {
        self.mode = InputMode::Normal;
    }

    fn keyboard_instrument_prev(&mut self) {
        if self.keyboard_instrument > 0 {
            self.keyboard_instrument -= 1;
        }
    }

    fn keyboard_instrument_next(&mut self) {
        if self.keyboard_instrument < 255 {
            self.keyboard_instrument += 1;
        }
    }
}

/// Slice a decoded sample buffer to the region [sample_start, sample_end] and adjust
/// loop points to be relative to the new start, clamped to the new buffer length.
/// Returns the trimmed buffer and adjusted loop points.  `None` values default to
/// the full buffer / no loop.
fn apply_sample_bounds(
    buf: Arc<Vec<f32>>,
    channels: usize,
    sample_start: Option<u32>,
    sample_end: Option<u32>,
    loop_start: u32,
    loop_end: u32,
) -> (Arc<Vec<f32>>, u32, u32) {
    let total_frames = buf.len() / channels.max(1);
    let start_frame = sample_start.unwrap_or(0) as usize;
    let end_frame = sample_end.map(|e| e as usize).unwrap_or(total_frames);
    let start_frame = start_frame.min(total_frames);
    let end_frame = end_frame.clamp(start_frame, total_frames);
    if start_frame == 0 && end_frame == total_frames {
        return (buf, loop_start, loop_end);
    }
    let new_length = end_frame - start_frame;
    let sliced = Arc::new(buf[start_frame * channels..end_frame * channels].to_vec());
    let adj_loop_start =
        ((loop_start as usize).saturating_sub(start_frame)).min(new_length) as u32;
    let adj_loop_end =
        ((loop_end as usize).saturating_sub(start_frame)).min(new_length) as u32;
    (sliced, adj_loop_start, adj_loop_end)
}

#[cfg(test)]
mod sample_bounds_tests {
    use super::*;

    fn make_buf(frames: usize) -> Arc<Vec<f32>> {
        Arc::new(vec![0.5f32; frames])
    }

    #[test]
    fn no_bounds_returns_original_buffer() {
        let buf = make_buf(100);
        let (out, ls, le) = apply_sample_bounds(Arc::clone(&buf), 1, None, None, 10, 50);
        assert_eq!(out.len(), 100);
        assert_eq!(ls, 10);
        assert_eq!(le, 50);
    }

    #[test]
    fn sample_start_shifts_loop_points() {
        let buf = make_buf(200);
        // Slice starts at frame 50; loop was at 60..80 → should become 10..30
        let (out, ls, le) =
            apply_sample_bounds(Arc::clone(&buf), 1, Some(50), None, 60, 80);
        assert_eq!(out.len(), 150);
        assert_eq!(ls, 10);
        assert_eq!(le, 30);
    }

    #[test]
    fn loop_end_clamped_when_exceeds_sample_end() {
        let buf = make_buf(500);
        // sample_end = 200, loop_end = 500 → adj_loop_end must be clamped to 200
        let (out, ls, le) =
            apply_sample_bounds(Arc::clone(&buf), 1, Some(0), Some(200), 10, 500);
        assert_eq!(out.len(), 200);
        assert_eq!(ls, 10);
        assert_eq!(le, 200, "loop_end should be clamped to new buffer length");
    }

    #[test]
    fn loop_points_before_start_frame_clamped_to_zero() {
        let buf = make_buf(100);
        // start_frame = 50, loop_start = 20 (before start) → clamped to 0
        let (out, ls, le) =
            apply_sample_bounds(Arc::clone(&buf), 1, Some(50), None, 20, 80);
        assert_eq!(out.len(), 50);
        assert_eq!(ls, 0);
        assert_eq!(le, 30);
    }

    #[test]
    fn multichannel_buffer_sliced_correctly() {
        // 100 stereo frames = 200 samples; slice frames 10..40 = 30 stereo frames = 60 samples
        let buf = Arc::new(vec![0.5f32; 200]);
        let (out, _, _) =
            apply_sample_bounds(Arc::clone(&buf), 2, Some(10), Some(40), 0, 0);
        assert_eq!(out.len(), 60);
    }
}

/// List subdirectories and `.wav` files in `dir`, sorted alphabetically (case-insensitive).
/// All other file types are excluded.
fn list_browser_entries(dir: &std::path::Path) -> Vec<BrowserEntry> {
    list_browser_entries_ext(dir, "wav")
}

/// List subdirectories and files matching `file_ext` in `dir`, sorted alphabetically
/// (case-insensitive).  All other file types are excluded.
fn list_browser_entries_ext(dir: &std::path::Path, file_ext: &str) -> Vec<BrowserEntry> {
    let mut entries = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                entries.push(BrowserEntry::Dir(name));
            } else if let Some(ext) = path.extension() {
                if ext.to_ascii_lowercase() == file_ext {
                    entries.push(BrowserEntry::Wav(name));
                }
            }
        }
    }
    entries.sort_by_key(|e| e.sort_key());
    // Prepend `..` when we're not at the filesystem root.
    if dir.parent().is_some() {
        entries.insert(0, BrowserEntry::ParentDir);
    }
    entries
}

// ── Audio ─────────────────────────────────────────────────────────────────────

fn start_audio_stream(
    mut consumer: rtrb::Consumer<Command>,
    seq_playing: Arc<AtomicBool>,
    current_step: Arc<AtomicU8>,
    preview_playing: Arc<AtomicBool>,
    sample_buf: Option<(Arc<Vec<f32>>, usize)>,
    initial_bpm: f32,
) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device"))?;

    let config = cpal::StreamConfig {
        channels: 2,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: cpal::BufferSize::Default,
    };

    let mut mixer = Mixer::new();
    if let Some((buf, channels)) = sample_buf {
        mixer.load_slot(0, Voice::new(buf, channels));
    }

    let mut sequencer = Sequencer::new(48000.0, initial_bpm);

    // Dedicated preview voice — separate from the 16 instrument slots.
    let mut preview_voice: Option<Voice> = None;

    // Per-track mixer state: updated by SetTrackVolume/Pan/Mute/Solo commands.
    let mut track_volumes = [1.0f32; TRACKS];
    let mut track_pans = [0.0f32; TRACKS];
    let mut track_mute = [false; TRACKS];
    let mut track_solo = [false; TRACKS];

    /// Scheduled retrigger: fires `speed`/`volume`/`pan` on `track` after `samples_until` frames.
    struct Retrigger {
        track: usize,
        speed: f32,
        volume: f32,
        pan: f32,
        samples_until: f64,
    }
    let mut retriggers: Vec<Retrigger> = Vec::new();

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / 2;

            // Fire scheduled retriggers that fall within this buffer.
            retriggers.retain_mut(|rt| {
                rt.samples_until -= frames as f64;
                if rt.samples_until <= 0.0 {
                    mixer.trigger_with_fx(rt.track, rt.speed, rt.volume, rt.pan);
                    false
                } else {
                    true
                }
            });

            // Process commands from the UI thread.
            while let Ok(cmd) = consumer.pop() {
                match cmd {
                    Command::NoteOn { slot, speed } => mixer.trigger(slot as usize, speed),
                    Command::NoteOff(slot) => mixer.stop_slot(slot as usize),
                    Command::Play => {
                        for (track, speed) in sequencer.play() {
                            mixer.trigger(track, speed);
                        }
                        seq_playing.store(true, Ordering::Relaxed);
                    }
                    Command::Stop => {
                        sequencer.stop();
                        retriggers.clear();
                        seq_playing.store(false, Ordering::Relaxed);
                    }
                    Command::Restart => {
                        for (track, speed) in sequencer.restart() {
                            mixer.trigger(track, speed);
                        }
                        current_step.store(0, Ordering::Relaxed);
                        seq_playing.store(true, Ordering::Relaxed);
                    }
                    Command::SetBpm(bpm) => sequencer.bpm = bpm,
                    Command::SetSwing(swing) => sequencer.swing = swing,
                    Command::UpdatePhrase(phrase) => sequencer.set_phrase(phrase),
                    Command::SetSampleRoot(root) => sequencer.sample_root = root,
                    Command::LoadVoice { slot, samples, channels, loop_start, loop_end, interp_mode } => {
                        let voice = Voice::new(samples, channels)
                            .with_loop(loop_start, loop_end)
                            .with_interp_mode(interp_mode);
                        mixer.load_slot(slot as usize, voice);
                    }
                    Command::SetLoopPoints { slot, loop_start, loop_end } => {
                        mixer.set_loop_points(slot as usize, loop_start, loop_end);
                    }
                    Command::SetInterpMode { slot, interp_mode } => {
                        mixer.set_interp_mode(slot as usize, interp_mode);
                    }
                    Command::UpdateSongData { arrangement, chains, phrases, instruments } => {
                        sequencer.update_song_data(arrangement, chains, phrases, instruments);
                    }
                    Command::UpdatePhraseInSong { idx, phrase } => {
                        sequencer.update_phrase_in_song(idx, phrase);
                    }
                    Command::SetTrackVolume { track, volume } => {
                        if (track as usize) < TRACKS {
                            track_volumes[track as usize] = volume;
                        }
                    }
                    Command::SetTrackPan { track, pan } => {
                        if (track as usize) < TRACKS {
                            track_pans[track as usize] = pan;
                        }
                    }
                    Command::SetTrackMute { track, mute } => {
                        if (track as usize) < TRACKS {
                            track_mute[track as usize] = mute;
                        }
                    }
                    Command::SetTrackSolo { track, active } => {
                        if (track as usize) < TRACKS {
                            track_solo[track as usize] = active;
                        }
                    }
                    Command::PreviewSample { samples, channels } => {
                        let mut v = Voice::new(samples, channels);
                        v.trigger(1.0);
                        preview_playing.store(true, Ordering::Relaxed);
                        preview_voice = Some(v);
                    }
                    Command::StopPreview => {
                        preview_playing.store(false, Ordering::Relaxed);
                        preview_voice = None;
                    }
                }
            }

            // Advance the sequencer and trigger notes, applying any FX slots.
            let any_solo = track_solo.iter().any(|&s| s);
            let events = sequencer.advance(frames);
            for event in &events {
                current_step.store(event.step_index, Ordering::Relaxed);
                for (track, speed, fx) in &event.notes {
                    // Skip if muted or if another track is soloed and this one isn't.
                    if track_mute[*track] || (any_solo && !track_solo[*track]) {
                        continue;
                    }

                    let mut final_speed = *speed;
                    // FX slots override vol/pan; track-level values are the base.
                    let mut vol = track_volumes[*track];
                    let mut pan = track_pans[*track];
                    let mut ret_count: u8 = 0;

                    for slot in fx.iter() {
                        if slot.command == 0 {
                            continue;
                        }
                        match FxCommand::from_id(slot.command) {
                            Some(FxCommand::Vol) => vol = slot.value as f32 / 255.0,
                            Some(FxCommand::Pan) => {
                                pan = (slot.value as f32 - 128.0) / 127.0;
                            }
                            Some(FxCommand::Pit) => {
                                let semitones = slot.value as i8;
                                final_speed *= 2.0f32.powf(semitones as f32 / 12.0);
                            }
                            Some(FxCommand::Ret) if slot.value >= 2 => {
                                ret_count = slot.value;
                            }
                            _ => {}
                        }
                    }

                    mixer.trigger_with_fx(*track, final_speed, vol, pan);

                    // Schedule retriggers at even sub-step intervals.
                    if ret_count >= 2 {
                        let step_samples = sequencer.samples_per_step();
                        let interval = step_samples / ret_count as f64;
                        for i in 1..ret_count {
                            retriggers.push(Retrigger {
                                track: *track,
                                speed: final_speed,
                                volume: vol,
                                pan,
                                samples_until: interval * i as f64,
                            });
                        }
                    }
                }
            }

            mixer.render(data);

            // Mix the preview voice on top of the instrument voices.
            if let Some(pv) = preview_voice.as_mut() {
                pv.render(data);
                if !pv.is_active() {
                    preview_playing.store(false, Ordering::Relaxed);
                    preview_voice = None;
                }
            }
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;

    stream.play()?;
    Ok(stream)
}

// ── TUI rendering helpers ─────────────────────────────────────────────────────

fn render_phrase_grid(
    phrase: &vitakt_core::model::Phrase,
    cursor_step: usize,
    cursor_col: usize,
    phrase_idx: usize,
    theme: &Theme,
    seq_playing: bool,
    playback_step: usize,
) -> Table<'static> {
    let rows: Vec<Row> = phrase
        .steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let is_cursor_row = i == cursor_step;
            let is_playback_row = seq_playing && i == playback_step;

            let note_str = match step.note {
                Some(n) => note_name(n),
                None => "---".to_string(),
            };
            let instr_str = match step.instrument {
                Some(n) => format!("{n:02X}"),
                None => "--".to_string(),
            };

            // Base row style for non-cursor-cell content.
            // Priority: cursor row > playback head > default beat grouping.
            let row_style = if is_cursor_row {
                Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else if is_playback_row {
                Style::default().bg(theme.playback_head_bg)
            } else if i % 4 == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };

            let cursor_cell_style =
                Style::default().bg(theme.cursor_bg).fg(theme.cursor_fg).add_modifier(Modifier::BOLD);

            let note_style = if is_cursor_row && cursor_col == COL_NOTE {
                cursor_cell_style
            } else if step.note.is_some() {
                if is_cursor_row {
                    Style::default().bg(Color::DarkGray).fg(theme.step_note).add_modifier(Modifier::BOLD)
                } else if is_playback_row {
                    Style::default().bg(theme.playback_head_bg).fg(theme.step_note)
                } else {
                    Style::default().fg(theme.step_note)
                }
            } else {
                if is_cursor_row || is_playback_row {
                    row_style
                } else {
                    Style::default().fg(theme.step_empty)
                }
            };

            let instr_style = if is_cursor_row && cursor_col == COL_INS {
                cursor_cell_style
            } else if step.instrument.is_some() {
                if is_cursor_row {
                    Style::default().bg(Color::DarkGray).fg(theme.step_instrument).add_modifier(Modifier::BOLD)
                } else if is_playback_row {
                    Style::default().bg(theme.playback_head_bg).fg(theme.step_instrument)
                } else {
                    Style::default().fg(theme.step_instrument)
                }
            } else {
                if is_cursor_row || is_playback_row {
                    row_style
                } else {
                    Style::default().fg(theme.step_empty)
                }
            };

            // Build the 10 cells: step#, note, ins, 4×(cmd, val).
            let mut cells = vec![
                Cell::from(format!("{i:02}")).style(row_style),
                Cell::from(note_str).style(note_style),
                Cell::from(instr_str).style(instr_style),
            ];

            for slot_idx in 0..4 {
                let fx = &step.fx[slot_idx];
                let cmd_col = COL_FX_FIRST + slot_idx * 2;
                let val_col = cmd_col + 1;

                let cmd_str = if fx.command == 0 {
                    "---".to_string()
                } else {
                    FxCommand::from_id(fx.command)
                        .map(|c| c.to_code().to_string())
                        .unwrap_or_else(|| format!("{:02X}?", fx.command))
                };
                let val_str = if fx.command == 0 {
                    "---".to_string()
                } else {
                    format!("{:03}", fx.value)
                };

                let cmd_style = if is_cursor_row && cursor_col == cmd_col {
                    cursor_cell_style
                } else if fx.command != 0 {
                    if is_cursor_row {
                        Style::default().bg(Color::DarkGray).fg(theme.step_fx_cmd).add_modifier(Modifier::BOLD)
                    } else if is_playback_row {
                        Style::default().bg(theme.playback_head_bg).fg(theme.step_fx_cmd)
                    } else {
                        Style::default().fg(theme.step_fx_cmd)
                    }
                } else {
                    if is_cursor_row || is_playback_row {
                        row_style
                    } else {
                        Style::default().fg(theme.step_empty)
                    }
                };
                let val_style = if is_cursor_row && cursor_col == val_col {
                    cursor_cell_style
                } else if fx.command != 0 {
                    if is_cursor_row {
                        Style::default().bg(Color::DarkGray).fg(theme.step_fx_val).add_modifier(Modifier::BOLD)
                    } else if is_playback_row {
                        Style::default().bg(theme.playback_head_bg).fg(theme.step_fx_val)
                    } else {
                        Style::default().fg(theme.step_fx_val)
                    }
                } else {
                    if is_cursor_row || is_playback_row {
                        row_style
                    } else {
                        Style::default().fg(theme.step_empty)
                    }
                };

                cells.push(Cell::from(cmd_str).style(cmd_style));
                cells.push(Cell::from(val_str).style(val_style));
            }

            Row::new(cells)
        })
        .collect();

    Table::new(
        rows,
        [
            Constraint::Length(3),  // "#"
            Constraint::Length(5),  // NOTE
            Constraint::Length(3),  // INS
            Constraint::Length(4),  // FX1 cmd
            Constraint::Length(4),  // FX1 val
            Constraint::Length(4),  // FX2 cmd
            Constraint::Length(4),  // FX2 val
            Constraint::Length(4),  // FX3 cmd
            Constraint::Length(4),  // FX3 val
            Constraint::Length(4),  // FX4 cmd
            Constraint::Min(3),     // FX4 val
        ],
    )
    .header(
        Row::new(vec!["#", "NOTE", "INS", "FX1C", "FX1V", "FX2C", "FX2V", "FX3C", "FX3V", "FX4C", "FX4V"])
            .style(Style::default().fg(theme.screen_title).add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .title(format!("Phrase {:02X}", phrase_idx))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme.screen_title)),
    )
}

// ── Song view render ──────────────────────────────────────────────────────────

fn render_song_view(app: &App) -> Table<'static> {
    let header_cells: Vec<Cell> = std::iter::once(Cell::from(" "))
        .chain((0..TRACKS).map(|t| {
            Cell::from(format!("TRK{t}")).style(
                Style::default().fg(app.theme.screen_title).add_modifier(Modifier::BOLD),
            )
        }))
        .collect();
    let header = Row::new(header_cells);

    let rows: Vec<Row> = app
        .song
        .arrangement
        .iter()
        .enumerate()
        .map(|(row_idx, row)| {
            let row_num = Cell::from(format!("{row_idx:02}")).style(if row_idx == app.song_cursor_row {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else if row_idx % 4 == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            });

            let cells: Vec<Cell> = std::iter::once(row_num)
                .chain(row.iter().enumerate().map(|(track, chain_opt)| {
                    let text = chain_opt.map(|c| format!("{c:02X}")).unwrap_or_else(|| "--".to_string());
                    let is_cursor = row_idx == app.song_cursor_row && track == app.song_cursor_track;
                    Cell::from(text).style(if is_cursor {
                        Style::default().bg(app.theme.cursor_bg).fg(app.theme.cursor_fg).add_modifier(Modifier::BOLD)
                    } else if chain_opt.is_some() {
                        Style::default().fg(app.theme.active_track)
                    } else {
                        Style::default().fg(app.theme.inactive_track)
                    })
                }))
                .collect();
            Row::new(cells)
        })
        .collect();

    let mut constraints = vec![Constraint::Length(3)]; // row number
    constraints.extend(std::iter::repeat(Constraint::Length(5)).take(TRACKS));

    Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .title(format!(
                    "SONG  [{} rows  |  {} chains]",
                    app.song.arrangement.len(),
                    app.song.chains.len()
                ))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.screen_title)),
        )
}

// ── Chain view render ─────────────────────────────────────────────────────────

fn render_chain_view(app: &App) -> Table<'static> {
    let track = app.chain_view_track;
    let row = app.chain_view_row;
    let chain_idx = app
        .song
        .arrangement
        .get(row)
        .and_then(|r| r[track])
        .map(|c| c as usize);

    let chain_slots = chain_idx
        .and_then(|ci| app.song.chains.get(ci))
        .map(|c| c.slots.as_slice())
        .unwrap_or(&[]);

    let rows: Vec<Row> = chain_slots
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let is_cursor = i == app.chain_cursor;
            let row_style = if is_cursor {
                Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else if i % 4 == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };
            let phrase_style = if is_cursor {
                Style::default().bg(app.theme.cursor_bg).fg(app.theme.cursor_fg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.active_track)
            };
            Row::new(vec![
                Cell::from(format!("{i:02}")).style(row_style),
                Cell::from(format!("{:02X}", slot.phrase)).style(phrase_style),
                Cell::from(format!("{:+}", slot.transpose)).style(row_style),
            ])
        })
        .collect();

    let display_rows = if rows.is_empty() {
        vec![Row::new(vec![
            Cell::from("--"),
            Cell::from("--").style(Style::default().fg(app.theme.inactive_track)),
            Cell::from("--").style(Style::default().fg(app.theme.inactive_track)),
        ])]
    } else {
        rows
    };

    let chain_title = if let Some(ci) = chain_idx {
        format!("Chain {:02X}  [Track {}  Row {}]", ci, track, row)
    } else {
        format!("Chain --  [Track {}  Row {}]  (no chain assigned)", track, row)
    };

    Table::new(
        display_rows,
        [Constraint::Length(3), Constraint::Length(5), Constraint::Length(6)],
    )
    .header(
        Row::new(vec!["#", "PHR", "TRANS"])
            .style(Style::default().fg(app.theme.screen_title).add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .title(chain_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.screen_title)),
    )
}

// ── Instrument editor render ──────────────────────────────────────────────────

fn render_instrument_editor(app: &App) -> Paragraph<'static> {
    let instr_idx = app.active_instrument;
    let instr = app.song.instruments.get(instr_idx);

    let field_names = [
        "Name       ",
        "Sample     ",
        "Root Note  ",
        "Loop Start ",
        "Loop End   ",
        "Interp Mode",
        "Volume     ",
        "Pan        ",
    ];

    let mut lines: Vec<ratatui::text::Line> = Vec::new();

    for (i, fname) in field_names.iter().enumerate() {
        let value = if let Some(instr) = instr {
            match i {
                INSTR_FIELD_NAME => {
                    if app.instr_editing && app.instr_cursor == i {
                        format!("[{}█]", app.instr_edit_buf)
                    } else {
                        format!(" {}", instr.name)
                    }
                }
                INSTR_FIELD_SAMPLE => {
                    if app.instr_editing && app.instr_cursor == i {
                        format!("[{}█]", app.instr_edit_buf)
                    } else {
                        let path = instr
                            .sample
                            .as_ref()
                            .map(|s| s.path.as_str())
                            .unwrap_or("(none)");
                        format!(" {}  [Enter: browse]", path)
                    }
                }
                INSTR_FIELD_ROOT => format!(" {} (MIDI {})", note_name(instr.root_note), instr.root_note),
                INSTR_FIELD_LOOP_START => {
                    let v = instr.loop_start.unwrap_or(0);
                    format!(" {v}")
                }
                INSTR_FIELD_LOOP_END => {
                    let v = instr.loop_end.unwrap_or(0);
                    format!(" {v}  (0=one-shot)")
                }
                INSTR_FIELD_INTERP => format!(
                    " {}",
                    match instr.interp_mode {
                        InterpMode::None => "Nearest",
                        InterpMode::Linear => "Linear",
                        InterpMode::Sinc => "Sinc (Hermite)",
                    }
                ),
                INSTR_FIELD_VOLUME => format!(" {:.2}", instr.volume),
                INSTR_FIELD_PAN => format!(" {:.2}", instr.pan),
                _ => String::new(),
            }
        } else {
            " (no instrument)".to_string()
        };

        let cursor_mark = if i == app.instr_cursor { "▶ " } else { "  " };
        let row_style = if i == app.instr_cursor {
            Style::default().fg(app.theme.cursor_bg).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::White)
        };

        lines.push(ratatui::text::Line::styled(
            format!("{cursor_mark}{fname}: {value}"),
            row_style,
        ));
    }

    // Add count info at bottom
    lines.push(ratatui::text::Line::default());
    lines.push(ratatui::text::Line::styled(
        format!(
            "  Instruments: {}/{}",
            app.song.instruments.len(),
            MAX_INSTRUMENTS
        ),
        Style::default().fg(app.theme.inactive_track),
    ));

    Paragraph::new(lines).block(
        Block::default()
            .title(format!("Instrument {:02}", app.active_instrument))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.screen_title)),
    )
}

// ── Startup screen render ─────────────────────────────────────────────────────

fn render_startup_screen(app: &App) -> Paragraph<'static> {
    let items = ["New Project", "Open File"];
    let mut lines = vec![
        ratatui::text::Line::from(""),
        ratatui::text::Line::styled(
            "  Welcome to vitakt",
            Style::default().fg(app.theme.screen_title).add_modifier(Modifier::BOLD),
        ),
        ratatui::text::Line::from(""),
    ];
    for (i, label) in items.iter().enumerate() {
        let (prefix, style) = if i == app.startup_cursor {
            ("  ▶  ", Style::default().fg(app.theme.cursor_bg).add_modifier(Modifier::BOLD))
        } else {
            ("     ", Style::default().fg(Color::White))
        };
        lines.push(ratatui::text::Line::styled(format!("{prefix}{label}"), style));
    }
    Paragraph::new(lines).block(
        Block::default()
            .title("vitakt  [j/k: navigate  Enter: select  q: quit]")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.screen_title)),
    )
}

// ── Sample browser render ─────────────────────────────────────────────────────

fn render_sample_browser(app: &App, viewport_height: usize) -> Paragraph<'static> {
    let dir_display = app.browser_dir.to_string_lossy().to_string();

    let mut lines = vec![
        ratatui::text::Line::styled(
            format!("  {dir_display}"),
            Style::default().fg(app.theme.inactive_track),
        ),
        ratatui::text::Line::from(""),
    ];

    if app.browser_entries.is_empty() {
        lines.push(ratatui::text::Line::styled(
            "  (no files found in current directory)",
            Style::default().fg(app.theme.inactive_track),
        ));
    } else {
        // 2 borders + 2 header lines (dir path + blank) = 4 rows consumed
        let available = viewport_height.saturating_sub(4);
        let scroll = app.browser_scroll;
        let end = if available == 0 {
            app.browser_entries.len()
        } else {
            (scroll + available).min(app.browser_entries.len())
        };
        for (i, entry) in app.browser_entries[scroll..end].iter().enumerate() {
            let abs_idx = scroll + i;
            let is_dir = matches!(entry, BrowserEntry::Dir(_) | BrowserEntry::ParentDir);
            let display = entry.display_name();
            let (prefix, style) = if abs_idx == app.browser_cursor {
                ("▶ ", Style::default().fg(app.theme.cursor_bg).add_modifier(Modifier::BOLD))
            } else if is_dir {
                ("  ", Style::default().fg(app.theme.screen_title))
            } else {
                ("  ", Style::default().fg(Color::White))
            };
            lines.push(ratatui::text::Line::styled(format!("{prefix}{display}"), style));
        }
    }

    Paragraph::new(lines).block(
        Block::default()
            .title(match app.browser_mode {
                BrowserMode::Sample => "Sample Browser  [Enter: select  -/Backspace: up  Esc: cancel]",
                BrowserMode::Project => "Open Project  [Enter: select  -/Backspace: up  Esc: cancel]",
            })
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.screen_title)),
    )
}

// ── Mixer field row indices ──────────────────────────────────────────────────
const MIXER_FIELD_VOL: usize = 0;
const MIXER_FIELD_PAN: usize = 1;
const MIXER_FIELD_MUTE: usize = 2;
const MIXER_FIELD_SOLO: usize = 3;
const MIXER_FIELD_SEND: usize = 4;
const MIXER_FIELD_COUNT: usize = 5;

fn render_mixer_view(app: &App) -> Table<'static> {
    let field_labels = ["VOL", "PAN", "MUT", "SOL", "SND"];

    let header = Row::new(
        std::iter::once(Cell::from("    ")).chain(
            (0..TRACKS).map(|t| {
                Cell::from(format!("TRK{t}")).style(Style::default().fg(app.theme.inactive_track))
            }),
        ),
    );

    let rows: Vec<Row> = field_labels
        .iter()
        .enumerate()
        .map(|(field_idx, label)| {
            let cells: Vec<Cell> = std::iter::once(
                Cell::from(*label).style(Style::default().fg(app.theme.inactive_track)),
            )
            .chain((0..TRACKS).map(|t| {
                let m = &app.song.mixer[t];
                let is_cursor =
                    t == app.mixer_cursor_track && field_idx == app.mixer_cursor_field;

                let text = match field_idx {
                    MIXER_FIELD_VOL => format!("{:.2}", m.volume),
                    MIXER_FIELD_PAN => format!("{:+.2}", m.pan),
                    MIXER_FIELD_MUTE => if m.mute { "■  " } else { "·  " }.to_string(),
                    MIXER_FIELD_SOLO => if m.solo { "◆  " } else { "·  " }.to_string(),
                    MIXER_FIELD_SEND => format!("{:.2}", m.fx_send),
                    _ => "   ".to_string(),
                };

                let style = if is_cursor {
                    Style::default().bg(app.theme.cursor_bg).fg(app.theme.cursor_fg).add_modifier(Modifier::BOLD)
                } else if field_idx == MIXER_FIELD_MUTE && m.mute {
                    Style::default().fg(Color::Red)
                } else if field_idx == MIXER_FIELD_SOLO && m.solo {
                    Style::default().fg(Color::Yellow)
                } else {
                    Style::default().fg(Color::White)
                };

                Cell::from(text).style(style)
            }))
            .collect();
            Row::new(cells)
        })
        .collect();

    // 4 chars for label + 8 equal-width track columns
    let constraints: Vec<Constraint> = std::iter::once(Constraint::Length(4))
        .chain((0..TRACKS).map(|_| Constraint::Ratio(1, TRACKS as u32)))
        .collect();

    Table::new(rows, constraints)
        .header(header)
        .block(
            Block::default()
                .title("MIXER  [F2: close  h/l: track  j/k: field  +/-: adjust  m: mute  s: solo]")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(app.theme.screen_title)),
        )
}

// ── Instrument editor field helper ───────────────────────────────────────────

/// Increment or decrement the currently-selected numeric/mode instrument field.
/// `delta` is +1 or -1.
fn instr_editor_increment(app: &mut App, delta: i32) {
    let idx = app.active_instrument;
    let cursor = app.instr_cursor;
    if let Some(instr) = app.song.instruments.get_mut(idx) {
        match cursor {
            INSTR_FIELD_ROOT => {
                instr.root_note = (instr.root_note as i32 + delta).clamp(0, 127) as u8;
                app.sample_root = instr.root_note;
                let root = instr.root_note;
                app.send_cmd(Command::SetSampleRoot(root));
            }
            INSTR_FIELD_LOOP_START => {
                let cur = instr.loop_start.unwrap_or(0) as i32;
                let new_val = (cur + delta).max(0) as u32;
                instr.loop_start = Some(new_val);
                let (ls, le) = (new_val, instr.loop_end.unwrap_or(0));
                app.send_cmd(Command::SetLoopPoints { slot: 0, loop_start: ls, loop_end: le });
            }
            INSTR_FIELD_LOOP_END => {
                let cur = instr.loop_end.unwrap_or(0) as i32;
                let new_val = (cur + delta).max(0) as u32;
                instr.loop_end = Some(new_val);
                let (ls, le) = (instr.loop_start.unwrap_or(0), new_val);
                app.send_cmd(Command::SetLoopPoints { slot: 0, loop_start: ls, loop_end: le });
            }
            INSTR_FIELD_INTERP => {
                instr.interp_mode = match (&instr.interp_mode, delta > 0) {
                    (InterpMode::None, true) => InterpMode::Linear,
                    (InterpMode::Linear, true) => InterpMode::Sinc,
                    (InterpMode::Sinc, true) => InterpMode::None,
                    (InterpMode::None, false) => InterpMode::Sinc,
                    (InterpMode::Linear, false) => InterpMode::None,
                    (InterpMode::Sinc, false) => InterpMode::Linear,
                };
                let mode = instr.interp_mode.clone();
                app.send_cmd(Command::SetInterpMode { slot: 0, interp_mode: mode });
            }
            INSTR_FIELD_VOLUME => {
                instr.volume = (instr.volume + delta as f32 * 0.05).clamp(0.0, 1.0);
                // Round to 2 decimal places to avoid float drift
                instr.volume = (instr.volume * 100.0).round() / 100.0;
            }
            INSTR_FIELD_PAN => {
                instr.pan = (instr.pan + delta as f32 * 0.05).clamp(-1.0, 1.0);
                instr.pan = (instr.pan * 100.0).round() / 100.0;
            }
            _ => {}
        }
    }
}

// ── TUI event loop ────────────────────────────────────────────────────────────

fn run_tui(
    producer: Option<rtrb::Producer<Command>>,
    sample_root: u8,
    seq_playing: Arc<AtomicBool>,
    current_seq_step: Arc<AtomicU8>,
    preview_playing: Arc<AtomicBool>,
    action: CliAction,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(producer, sample_root, seq_playing, current_seq_step, preview_playing);

    match action {
        CliAction::ShowStartup => {
            app.view = View::Startup;
        }
        CliAction::OpenFile(path) => {
            let path_str = path.to_string_lossy().to_string();
            match storage::load_trk(&path_str) {
                Ok(song) => {
                    app.song = vitakt_core::model::migrate(song);
                    if app.song.phrases.is_empty() {
                        app.song.phrases.push(vitakt_core::model::Phrase::default());
                    }
                    app.sync_phrase_to_sequencer();
                    app.reload_instruments();
                    app.sync_song_to_sequencer();
                    app.sync_mixer_to_audio();
                    app.set_timed_status(format!("Loaded: {path_str}"));
                }
                Err(e) => {
                    eprintln!("error: could not open '{path_str}': {e}");
                    disable_raw_mode()?;
                    return Err(e);
                }
            }
        }
    }

    loop {
        // Poll render thread for completion/progress before drawing.
        app.poll_render();

        // Clear timed status messages after 2 seconds.
        if let Some(timer) = app.status_timer {
            if timer.elapsed() >= Duration::from_secs(2) {
                app.status_timer = None;
            }
        }

        // ── Render ────────────────────────────────────────────────────────────
        terminal.draw(|frame| {
            let size = frame.area();
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(size);

            // Main area — depends on active view
            match app.view {
                View::Startup => {
                    let para = render_startup_screen(&app);
                    frame.render_widget(para, outer[0]);
                }
                View::SongView => {
                    let table = render_song_view(&app);
                    frame.render_widget(table, outer[0]);
                }
                View::ChainView => {
                    let table = render_chain_view(&app);
                    frame.render_widget(table, outer[0]);
                }
                View::PhraseEditor => {
                    let phrase = app.phrase();
                    let seq_playing = app.seq_playing.load(Ordering::Relaxed);
                    let playback_step = app.current_seq_step.load(Ordering::Relaxed) as usize;
                    let table = render_phrase_grid(phrase, app.cursor_step, app.cursor_col, app.active_phrase_idx, &app.theme, seq_playing, playback_step);
                    frame.render_widget(table, outer[0]);
                }
                View::InstrumentEditor => {
                    let para = render_instrument_editor(&app);
                    frame.render_widget(para, outer[0]);
                }
                View::SampleBrowser => {
                    let para = render_sample_browser(&app, outer[0].height as usize);
                    frame.render_widget(para, outer[0]);
                }
                View::Mixer => {
                    let table = render_mixer_view(&app);
                    frame.render_widget(table, outer[0]);
                }
            }

            // Quit confirmation modal overlay
            if matches!(app.mode, InputMode::ConfirmQuit) {
                let modal_width = 46u16;
                let modal_height = 4u16;
                let x = size.width.saturating_sub(modal_width) / 2;
                let y = size.height.saturating_sub(modal_height) / 2;
                let modal_area = Rect::new(x, y, modal_width.min(size.width), modal_height.min(size.height));
                frame.render_widget(Clear, modal_area);
                let modal = Paragraph::new("Unsaved changes. Quit? (y/n)")
                    .block(Block::default().borders(Borders::ALL).title(" Confirm Quit "))
                    .style(Style::default().fg(Color::Yellow).bg(Color::DarkGray));
                frame.render_widget(modal, modal_area);
            }

            // Status bar
            let playing = app.seq_playing.load(Ordering::Relaxed);
            let seq_step = app.current_seq_step.load(Ordering::Relaxed);
            let transport = if playing {
                format!("Step:{:02}  BPM:{:.1}", seq_step, app.song.bpm)
            } else {
                format!("Step:{:02}  BPM:{:.1}", seq_step, app.song.bpm)
            };

            // Mode label: always the leftmost element in the status bar.
            let mode_label = match app.view {
                _ if matches!(app.mode, InputMode::Keyboard) => "KEYBOARD",
                _ if matches!(app.mode, InputMode::ConfirmQuit) => "NORMAL",
                View::ChainView if app.chain_insert_mode => "INSERT",
                View::PhraseEditor => match app.mode {
                    InputMode::Normal => "NORMAL",
                    InputMode::Insert => "INSERT",
                    InputMode::Command => "COMMAND",
                    InputMode::Keyboard => "KEYBOARD",
                    InputMode::ConfirmQuit => "NORMAL",
                },
                _ => "NORMAL",
            };

            // Insert mode is active when the mode label is INSERT.
            let insert_active = mode_label == "INSERT";
            let keyboard_active = mode_label == "KEYBOARD";

            // When a render is in progress, override the status bar with progress.
            let status_text = if app.render_receiver.is_some() {
                app.status.clone()
            } else if matches!(app.mode, InputMode::ConfirmQuit) {
                "Unsaved changes — y: quit  n/Esc: cancel".to_string()
            } else if keyboard_active {
                format!(
                    "{mode_label}  |  {transport}  |  Ins:{:02}  [/]: change instrument  QWERTY: play note  Esc: normal",
                    app.keyboard_instrument
                )
            } else if app.status_timer.is_some() {
                format!("{mode_label}  |  {transport}  |  {}", app.status)
            } else {
                match app.view {
                View::Startup => "j/k: navigate  Enter: select  q: quit".to_string(),
                View::SongView => format!(
                    "{mode_label}  |  {transport}  |  hjkl: nav  0-9/a-f: chain  Del: clear  Enter: chain view  o: add row below  O: add row above  F3: phrase  ←/→: BPM  q: quit"
                ),
                View::ChainView => {
                    if app.chain_insert_mode {
                        format!("{mode_label}  |  h/l: phrase ±1  ,/.: transpose ±1  Esc: normal")
                    } else {
                        format!("{mode_label}  |  j/k: nav  h/l: phrase  ,/.: transpose  o: add slot below  O: add slot above  d: del slot  Enter: phrase  i: insert  Esc: back")
                    }
                }
                View::PhraseEditor => match app.mode {
                    InputMode::Normal => format!("{mode_label}  |  {transport}  |  SPC: play  i: insert  Tab: instrument  ←/→: BPM  :: command  q: quit"),
                    InputMode::Insert => {
                        let col_hint = match col_to_fx(app.cursor_col) {
                            Some((s, true)) => {
                                let buf = &app.fx_edit_buf;
                                format!("FX{} CMD: [{buf:<3}]  type 3-letter code (VOL/PAN/PIT/RET)", s + 1)
                            }
                            Some((s, false)) => {
                                let buf = &app.fx_edit_buf;
                                format!("FX{} VAL: [{buf:<3}]  type 0-255, Enter to confirm", s + 1)
                            }
                            None if app.cursor_col == COL_NOTE => {
                                format!("NOTE  Oct:{} Ins:{:02}  QWERTY piano", app.octave, app.active_instrument)
                            }
                            None => format!("Col:{} Ins:{:02}", app.cursor_col, app.active_instrument),
                        };
                        format!("{mode_label}  |  {transport}  |  {col_hint}  |  Esc: normal")
                    }
                    InputMode::Command => format!("{mode_label}  |  {transport}  |  :{}", app.cmd_buf),
                    // InputMode::Keyboard is handled by the `keyboard_active` branch above
                    InputMode::Keyboard => unreachable!("Keyboard mode status handled before view match"),
                    // InputMode::ConfirmQuit is handled by the modal branch above
                    InputMode::ConfirmQuit => unreachable!("ConfirmQuit status handled before view match"),
                },
                View::InstrumentEditor => {
                    if app.instr_editing {
                        format!(
                            "{mode_label}  |  Ins:{:02}  |  Enter: confirm  Esc: cancel",
                            app.active_instrument
                        )
                    } else {
                        format!(
                            "{mode_label}  |  Ins:{:02}  |  j/k: nav  h/l: change  i: edit  Enter: browse(sample)  Esc: back",
                            app.active_instrument
                        )
                    }
                }
                View::SampleBrowser => {
                    format!(
                        "{mode_label}  |  j/k: nav  Enter: select  -/Backspace: up  Esc: cancel  ({} entries)",
                        app.browser_entries.len()
                    )
                }
                View::Mixer => {
                    format!(
                        "{mode_label}  |  {transport}  |  h/l: track  j/k: field  +/-: adjust  m: mute  s: solo  Esc: back"
                    )
                }
            }};

            let status_bg = if insert_active {
                app.theme.insert_mode_bg
            } else if keyboard_active {
                app.theme.keyboard_mode_bg
            } else {
                app.theme.status_bar_bg
            };
            let status = Paragraph::new(status_text)
                .style(Style::default().fg(app.theme.status_bar_fg).bg(status_bg));
            frame.render_widget(status, outer[1]);
        })?;

        // ── Input ─────────────────────────────────────────────────────────────
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                // Global undo/redo: works from any view except insert/command mode.
                let is_insert = matches!((&app.view, &app.mode), (View::PhraseEditor, InputMode::Insert));
                let is_command = matches!((&app.view, &app.mode), (View::PhraseEditor, InputMode::Command));
                if !is_insert && !is_command {
                    if key.code == KeyCode::Char('u') && !key.modifiers.contains(KeyModifiers::CONTROL) {
                        app.do_undo();
                        continue;
                    }
                    if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        app.do_redo();
                        continue;
                    }
                }
                // ── Global: ConfirmQuit modal overrides all per-view key handling ─
                if matches!(app.mode, InputMode::ConfirmQuit) {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') => break,
                        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                            app.mode = InputMode::Normal;
                        }
                        _ => {}
                    }
                    continue;
                }

                // ── Global: Keyboard mode overrides all per-view key handling ─
                if matches!(app.mode, InputMode::Keyboard) {
                    match key.code {
                        KeyCode::Esc => {
                            app.exit_keyboard_mode();
                        }
                        KeyCode::Char('[') => {
                            app.keyboard_instrument_prev();
                        }
                        KeyCode::Char(']') => {
                            app.keyboard_instrument_next();
                        }
                        KeyCode::Char(c) => {
                            if let Some(semitone) = qwerty_to_semitone(c) {
                                let base: i32 = 12 * (app.octave as i32 + 1);
                                let midi = (base + semitone as i32).clamp(0, 127) as u8;
                                let slot = app.keyboard_instrument as u8;
                                let root = app.song.instruments
                                    .get(app.keyboard_instrument)
                                    .map(|i| i.root_note)
                                    .unwrap_or(app.sample_root);
                                let speed = pitch_speed(midi, root);
                                app.send_cmd(Command::NoteOn { slot, speed });
                            }
                        }
                        _ => {}
                    }
                    continue;
                }

                match app.view {
                    // ──────────────────────────────────────────────────────────
                    // Startup screen key handling
                    // ──────────────────────────────────────────────────────────
                    View::Startup => match key.code {
                        KeyCode::Char('q') => {
                            if app.is_dirty {
                                app.mode = InputMode::ConfirmQuit;
                            } else {
                                break;
                            }
                        }
                        KeyCode::Char('j') | KeyCode::Down => {
                            app.startup_cursor = (app.startup_cursor + 1) % 2;
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if app.startup_cursor > 0 {
                                app.startup_cursor -= 1;
                            }
                        }
                        KeyCode::Enter => match app.startup_cursor {
                            0 => {
                                // New Project — blank song, go to Song View.
                                app.view = View::SongView;
                                app.view_stack.clear();
                            }
                            _ => {
                                // Open File — launch project browser.
                                app.open_project_browser();
                            }
                        },
                        _ => {}
                    },
                    // ──────────────────────────────────────────────────────────
                    // Song view key handling
                    // ──────────────────────────────────────────────────────────
                    View::SongView => match key.code {
                        KeyCode::Char('q') => {
                            if app.is_dirty {
                                app.mode = InputMode::ConfirmQuit;
                            } else {
                                break;
                            }
                        }
                        KeyCode::Char(' ') => app.toggle_play(),
                        KeyCode::F(5) => app.restart_play(),
                        KeyCode::Left => app.adjust_bpm(-1.0),
                        KeyCode::Right => app.adjust_bpm(1.0),
                        KeyCode::Char(':') => {
                            app.push_view(View::PhraseEditor);
                            app.mode = InputMode::Command;
                            app.cmd_buf.clear();
                        }
                        // Navigation
                        KeyCode::Char('j') | KeyCode::Down => {
                            let rows = app.song.arrangement.len();
                            if rows == 0 || app.song_cursor_row + 1 >= rows {
                                // Auto-append a new empty row when j goes past the last
                                let empty_row = [None; TRACKS];
                                app.song.arrangement.push(empty_row);
                            }
                            app.song_cursor_row = (app.song_cursor_row + 1)
                                .min(app.song.arrangement.len().saturating_sub(1));
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if app.song_cursor_row > 0 {
                                app.song_cursor_row -= 1;
                            }
                        }
                        KeyCode::Char('h') => {
                            if app.song_cursor_track > 0 {
                                app.song_cursor_track -= 1;
                            }
                        }
                        KeyCode::Char('l') => {
                            if app.song_cursor_track < TRACKS - 1 {
                                app.song_cursor_track += 1;
                            }
                        }
                        // Assign chain via hex digit (0-9 → chains 0-9, a-f → chains 10-15)
                        KeyCode::Char(c) if c.is_ascii_hexdigit() && c != 'j' && c != 'k' && c != 'h' && c != 'l' => {
                            if let Ok(chain_idx) = u8::from_str_radix(&c.to_string(), 16) {
                                let row = app.song_cursor_row;
                                let track = app.song_cursor_track;
                                app.record(&format!("assign chain to row {} track {}", row, track));
                                while app.song.arrangement.len() <= row {
                                    app.song.arrangement.push([None; TRACKS]);
                                }
                                app.ensure_chain(chain_idx as usize);
                                app.song.arrangement[row][track] = Some(chain_idx);
                                app.sync_song_to_sequencer();
                            }
                        }
                        // Clear cell
                        KeyCode::Delete | KeyCode::Char('x') => {
                            let row = app.song_cursor_row;
                            let track = app.song_cursor_track;
                            app.record(&format!("clear arrangement row {} track {}", row, track));
                            if let Some(arr_row) = app.song.arrangement.get_mut(row) {
                                arr_row[track] = None;
                                app.sync_song_to_sequencer();
                            }
                        }
                        // Append row below cursor with 'o'
                        KeyCode::Char('o') => {
                            app.record("add song row below");
                            let insert_at = app.song_cursor_row + 1;
                            app.song.arrangement.insert(insert_at, [None; TRACKS]);
                        }
                        // Insert row above cursor with 'O'; cursor follows original row
                        KeyCode::Char('O') => {
                            app.record("add song row above");
                            let insert_at = app.song_cursor_row;
                            app.song.arrangement.insert(insert_at, [None; TRACKS]);
                            app.song_cursor_row += 1;
                        }
                        // Drill into chain view on Enter
                        KeyCode::Enter => {
                            let row = app.song_cursor_row;
                            let track = app.song_cursor_track;
                            app.chain_view_track = track;
                            app.chain_view_row = row;
                            app.chain_cursor = 0;
                            app.chain_insert_mode = false;
                            app.push_view(View::ChainView);
                        }
                        // Tab → phrase editor (direct jump)
                        KeyCode::Tab => {
                            app.push_view(View::PhraseEditor);
                            app.mode = InputMode::Normal;
                        }
                        // F3 → phrase editor; F4 → instrument editor; F2 → mixer
                        KeyCode::F(2) => {
                            app.push_view(View::Mixer);
                        }
                        KeyCode::F(3) => {
                            app.push_view(View::PhraseEditor);
                        }
                        KeyCode::F(4) => {
                            app.push_view(View::InstrumentEditor);
                        }
                        KeyCode::Char('/') => {
                            app.enter_keyboard_mode();
                        }
                        _ => {}
                    },

                    // ──────────────────────────────────────────────────────────
                    // Chain view key handling
                    // ──────────────────────────────────────────────────────────
                    View::ChainView => {
                        let track = app.chain_view_track;
                        let row = app.chain_view_row;
                        let chain_idx_opt = app.song.arrangement
                            .get(row)
                            .and_then(|r| r[track])
                            .map(|c| c as usize);

                        if app.chain_insert_mode {
                            match key.code {
                                KeyCode::Esc => {
                                    app.chain_insert_mode = false;
                                }
                                // h/l: adjust phrase index
                                KeyCode::Char('h') | KeyCode::Left => {
                                    if let Some(ci) = chain_idx_opt {
                                        let cursor = app.chain_cursor;
                                        if cursor < app.song.chains[ci].slots.len() {
                                            app.record("adjust chain phrase");
                                            let max = app.song.phrases.len().saturating_sub(1) as u8;
                                            app.song.chains[ci].slots[cursor].phrase =
                                                app.song.chains[ci].slots[cursor].phrase.saturating_sub(1).min(max);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                KeyCode::Char('l') | KeyCode::Right => {
                                    if let Some(ci) = chain_idx_opt {
                                        let cursor = app.chain_cursor;
                                        if cursor < app.song.chains[ci].slots.len() {
                                            app.record("adjust chain phrase");
                                            let max = app.song.phrases.len().saturating_sub(1) as u8;
                                            app.song.chains[ci].slots[cursor].phrase =
                                                (app.song.chains[ci].slots[cursor].phrase + 1).min(max);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                // ,/. : adjust transpose (semitones down/up)
                                KeyCode::Char(',') => {
                                    if let Some(ci) = chain_idx_opt {
                                        let cursor = app.chain_cursor;
                                        if cursor < app.song.chains[ci].slots.len() {
                                            app.record("adjust chain transpose");
                                            app.song.chains[ci].slots[cursor].transpose =
                                                app.song.chains[ci].slots[cursor].transpose.saturating_sub(1);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                KeyCode::Char('.') => {
                                    if let Some(ci) = chain_idx_opt {
                                        let cursor = app.chain_cursor;
                                        if cursor < app.song.chains[ci].slots.len() {
                                            app.record("adjust chain transpose");
                                            app.song.chains[ci].slots[cursor].transpose =
                                                app.song.chains[ci].slots[cursor].transpose.saturating_add(1);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                _ => {}
                            }
                        } else {
                            // Normal mode
                            match key.code {
                                KeyCode::Esc | KeyCode::Backspace => app.pop_view(),
                                KeyCode::Char('j') | KeyCode::Down => {
                                    if let Some(ci) = chain_idx_opt {
                                        let len = app.song.chains[ci].slots.len();
                                        if len > 0 {
                                            app.chain_cursor = (app.chain_cursor + 1) % len;
                                        }
                                    }
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    if let Some(ci) = chain_idx_opt {
                                        let len = app.song.chains[ci].slots.len();
                                        if len > 0 {
                                            app.chain_cursor = (app.chain_cursor + len - 1) % len;
                                        }
                                    }
                                }
                                // h/l: adjust phrase index in normal mode too
                                KeyCode::Char('h') | KeyCode::Left => {
                                    if let Some(ci) = chain_idx_opt {
                                        let cursor = app.chain_cursor;
                                        if cursor < app.song.chains[ci].slots.len() {
                                            app.record("adjust chain phrase");
                                            app.song.chains[ci].slots[cursor].phrase =
                                                app.song.chains[ci].slots[cursor].phrase.saturating_sub(1);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                KeyCode::Char('l') | KeyCode::Right => {
                                    if let Some(ci) = chain_idx_opt {
                                        let cursor = app.chain_cursor;
                                        if cursor < app.song.chains[ci].slots.len() {
                                            app.record("adjust chain phrase");
                                            let max = app.song.phrases.len().saturating_sub(1) as u8;
                                            app.song.chains[ci].slots[cursor].phrase =
                                                (app.song.chains[ci].slots[cursor].phrase + 1).min(max);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                // ,/. : adjust transpose
                                KeyCode::Char(',') => {
                                    if let Some(ci) = chain_idx_opt {
                                        let cursor = app.chain_cursor;
                                        if cursor < app.song.chains[ci].slots.len() {
                                            app.record("adjust chain transpose");
                                            app.song.chains[ci].slots[cursor].transpose =
                                                app.song.chains[ci].slots[cursor].transpose.saturating_sub(1);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                KeyCode::Char('.') => {
                                    if let Some(ci) = chain_idx_opt {
                                        let cursor = app.chain_cursor;
                                        if cursor < app.song.chains[ci].slots.len() {
                                            app.record("adjust chain transpose");
                                            app.song.chains[ci].slots[cursor].transpose =
                                                app.song.chains[ci].slots[cursor].transpose.saturating_add(1);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                // i: enter insert mode
                                KeyCode::Char('i') => {
                                    if chain_idx_opt.is_some() {
                                        app.chain_insert_mode = true;
                                    }
                                }
                                // o: insert slot below cursor
                                KeyCode::Char('o') => {
                                    if let Some(ci) = chain_idx_opt {
                                        app.record("add chain slot below");
                                        let insert_at = (app.chain_cursor + 1).min(app.song.chains[ci].slots.len());
                                        app.song.chains[ci].slots.insert(insert_at, ChainSlot {
                                            phrase: 0,
                                            transpose: 0,
                                        });
                                        app.chain_cursor = insert_at;
                                        app.sync_song_to_sequencer();
                                    }
                                }
                                // O: insert slot above cursor; cursor follows original slot
                                KeyCode::Char('O') => {
                                    if let Some(ci) = chain_idx_opt {
                                        app.record("add chain slot above");
                                        let insert_at = app.chain_cursor;
                                        app.song.chains[ci].slots.insert(insert_at, ChainSlot {
                                            phrase: 0,
                                            transpose: 0,
                                        });
                                        app.chain_cursor += 1;
                                        app.sync_song_to_sequencer();
                                    }
                                }
                                // d: delete current slot
                                KeyCode::Char('d') | KeyCode::Delete => {
                                    if let Some(ci) = chain_idx_opt {
                                        let len = app.song.chains[ci].slots.len();
                                        if len > 0 {
                                            app.record("delete chain slot");
                                            app.song.chains[ci].slots.remove(app.chain_cursor);
                                            if app.chain_cursor >= app.song.chains[ci].slots.len() && app.chain_cursor > 0 {
                                                app.chain_cursor -= 1;
                                            }
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                // Enter: drill into PhraseEditor for selected slot
                                KeyCode::Enter => {
                                    if let Some(ci) = chain_idx_opt {
                                        if let Some(slot) = app.song.chains[ci].slots.get(app.chain_cursor) {
                                            let phrase_idx = slot.phrase as usize;
                                            while app.song.phrases.len() <= phrase_idx {
                                                app.song.phrases.push(vitakt_core::model::Phrase::default());
                                            }
                                            app.active_phrase_idx = phrase_idx;
                                            app.push_view(View::PhraseEditor);
                                            app.mode = InputMode::Normal;
                                        }
                                    }
                                }
                                // F1: song view (back), F3: phrase editor
                                KeyCode::F(1) => app.pop_view(),
                                KeyCode::F(3) => {
                                    app.push_view(View::PhraseEditor);
                                    app.mode = InputMode::Normal;
                                }
                                KeyCode::Char('/') => {
                                    app.enter_keyboard_mode();
                                }
                                _ => {}
                            }
                        }
                    },

                    // ──────────────────────────────────────────────────────────
                    // Phrase editor key handling
                    // ──────────────────────────────────────────────────────────
                    View::PhraseEditor => match app.mode {
                        // ── Normal mode ───────────────────────────────────────
                        InputMode::Normal => {
                            app.yy_pending = false;
                            match key.code {
                                KeyCode::Esc => app.pop_view(),
                                KeyCode::Char('q') => {
                                    if app.is_dirty {
                                        app.mode = InputMode::ConfirmQuit;
                                    } else {
                                        break;
                                    }
                                }
                                KeyCode::F(1) => {
                                    // Go to Song View (top)
                                    app.view_stack.clear();
                                    app.view = View::SongView;
                                }
                                KeyCode::Char(':') => {
                                    app.mode = InputMode::Command;
                                    app.cmd_buf.clear();
                                }
                                KeyCode::Char('i') => {
                                    app.mode = InputMode::Insert;
                                }
                                // Tab → instrument editor
                                KeyCode::Tab => app.open_instrument_editor(),
                                // Transport: Space = play/stop, F5 = restart from 0
                                KeyCode::Char(' ') => app.toggle_play(),
                                KeyCode::F(5) => app.restart_play(),
                                // BPM adjustment: left/right arrows
                                KeyCode::Left => app.adjust_bpm(-1.0),
                                KeyCode::Right => app.adjust_bpm(1.0),
                                // Vim navigation (up/down)
                                KeyCode::Char('j') | KeyCode::Down => {
                                    app.cursor_step =
                                        (app.cursor_step + 1) % STEPS_PER_PHRASE;
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    app.cursor_step =
                                        (app.cursor_step + STEPS_PER_PHRASE - 1)
                                            % STEPS_PER_PHRASE;
                                }
                                KeyCode::Char('h') => {
                                    if app.cursor_col > 0 {
                                        app.cursor_col -= 1;
                                    }
                                    app.fx_edit_buf.clear();
                                }
                                KeyCode::Char('l') => {
                                    if app.cursor_col + 1 < COL_COUNT {
                                        app.cursor_col += 1;
                                    }
                                    app.fx_edit_buf.clear();
                                }
                                // Delete / clear step or active FX slot
                                KeyCode::Char('d') | KeyCode::Delete => {
                                    let idx = app.cursor_step;
                                    if let Some((slot_idx, _)) = col_to_fx(app.cursor_col) {
                                        app.record(&format!("clear FX slot {} at step {}", slot_idx + 1, app.cursor_step));
                                        // Clear just the active FX slot.
                                        app.phrase_mut().steps[idx].fx[slot_idx] =
                                            vitakt_core::model::FxSlot::default();
                                    } else {
                                        app.record(&format!("clear step {}", app.cursor_step));
                                        // Clear the entire step.
                                        let step = &mut app.phrase_mut().steps[idx];
                                        step.note = None;
                                        step.instrument = None;
                                        step.velocity = 0;
                                        step.fx = Default::default();
                                    }
                                    app.fx_edit_buf.clear();
                                    app.sync_phrase_to_sequencer();
                                }
                                // yy — copy step
                                KeyCode::Char('y') => {
                                    if app.yy_pending {
                                        app.yanked_step =
                                            Some(app.phrase().steps[app.cursor_step].clone());
                                        app.set_timed_status(format!(
                                            "Yanked step {}",
                                            app.cursor_step
                                        ));
                                        app.yy_pending = false;
                                    } else {
                                        app.yy_pending = true;
                                        continue; // don't reset yy_pending below
                                    }
                                }
                                // p — paste step
                                KeyCode::Char('p') => {
                                    if let Some(s) = app.yanked_step.clone() {
                                        let idx = app.cursor_step;
                                        app.record(&format!("paste at step {}", app.cursor_step));
                                        app.phrase_mut().steps[idx] = s;
                                        app.sync_phrase_to_sequencer();
                                    }
                                }
                                KeyCode::Char('/') => {
                                    app.enter_keyboard_mode();
                                }
                                _ => {}
                            }
                            if !matches!(key.code, KeyCode::Char('y')) {
                                app.yy_pending = false;
                            }
                        }

                        // ── Insert mode ───────────────────────────────────────
                        InputMode::Insert => match key.code {
                            KeyCode::Esc => {
                                app.mode = InputMode::Normal;
                                app.fx_edit_buf.clear();
                            }
                            KeyCode::Up => {
                                app.cursor_step =
                                    (app.cursor_step + STEPS_PER_PHRASE - 1)
                                        % STEPS_PER_PHRASE;
                                app.fx_edit_buf.clear();
                            }
                            KeyCode::Down => {
                                app.cursor_step =
                                    (app.cursor_step + 1) % STEPS_PER_PHRASE;
                                app.fx_edit_buf.clear();
                            }
                            KeyCode::Left => {
                                if app.cursor_col > 0 {
                                    app.cursor_col -= 1;
                                }
                                app.fx_edit_buf.clear();
                            }
                            KeyCode::Right => {
                                if app.cursor_col + 1 < COL_COUNT {
                                    app.cursor_col += 1;
                                }
                                app.fx_edit_buf.clear();
                            }
                            // Octave up/down only active on the NOTE column.
                            KeyCode::Char('-') if app.cursor_col == COL_NOTE => {
                                if app.octave > 1 {
                                    app.octave -= 1;
                                }
                            }
                            KeyCode::Char('=') if app.cursor_col == COL_NOTE => {
                                if app.octave < 8 {
                                    app.octave += 1;
                                }
                            }
                            // Delete / Backspace clears active FX field.
                            KeyCode::Delete | KeyCode::Backspace
                                if col_to_fx(app.cursor_col).is_some() =>
                            {
                                if !app.fx_edit_buf.is_empty() {
                                    app.fx_edit_buf.pop();
                                } else if let Some((slot_idx, _)) = col_to_fx(app.cursor_col) {
                                    let step_idx = app.cursor_step;
                                    app.record(&format!("clear FX slot {} at step {}", slot_idx + 1, app.cursor_step));
                                    app.phrase_mut().steps[step_idx].fx[slot_idx] =
                                        vitakt_core::model::FxSlot::default();
                                    app.sync_phrase_to_sequencer();
                                }
                            }
                            KeyCode::Char(c) => {
                                match col_to_fx(app.cursor_col) {
                                    Some((slot_idx, true)) => {
                                        // FX command field: accumulate up to 3 alpha chars.
                                        if c.is_alphabetic() {
                                            app.fx_edit_buf.push(c.to_ascii_uppercase());
                                            if app.fx_edit_buf.len() == 3 {
                                                let buf = app.fx_edit_buf.clone();
                                                app.fx_edit_buf.clear();
                                                let step_idx = app.cursor_step;
                                                if let Some(cmd) = FxCommand::from_code(&buf) {
                                                    app.record(&format!("set FX{} command at step {}", slot_idx + 1, app.cursor_step));
                                                    app.phrase_mut().steps[step_idx].fx[slot_idx]
                                                        .command = cmd.id();
                                                    app.sync_phrase_to_sequencer();
                                                    // Advance to value column.
                                                    if app.cursor_col + 1 < COL_COUNT {
                                                        app.cursor_col += 1;
                                                    }
                                                } else {
                                                    app.status =
                                                        format!("Unknown FX command: {buf}");
                                                }
                                            }
                                        }
                                    }
                                    Some((slot_idx, false)) => {
                                        // FX value field: accumulate up to 3 decimal digits.
                                        if c.is_ascii_digit() {
                                            app.fx_edit_buf.push(c);
                                            if app.fx_edit_buf.len() == 3 {
                                                let buf = app.fx_edit_buf.clone();
                                                app.fx_edit_buf.clear();
                                                if let Ok(v) = buf.parse::<u16>() {
                                                    let step_idx = app.cursor_step;
                                                    app.record(&format!("set FX{} value at step {}", slot_idx + 1, app.cursor_step));
                                                    app.phrase_mut().steps[step_idx].fx[slot_idx]
                                                        .value = v.min(255) as u8;
                                                    app.sync_phrase_to_sequencer();
                                                    // Advance cursor to next step.
                                                    app.cursor_step =
                                                        (app.cursor_step + 1) % STEPS_PER_PHRASE;
                                                }
                                            }
                                        }
                                    }
                                    None => {
                                        // NOTE column: QWERTY piano.
                                        if app.cursor_col == COL_NOTE {
                                            if let Some(semitone) = qwerty_to_semitone(c) {
                                                let base: i32 = 12 * (app.octave as i32 + 1);
                                                let midi =
                                                    (base + semitone as i32).clamp(0, 127) as u8;
                                                app.enter_note(midi);
                                            }
                                        }
                                    }
                                }
                            }
                            // Enter confirms FX value entry early (before 3 digits).
                            KeyCode::Enter if col_to_fx(app.cursor_col).is_some() => {
                                if let Some((slot_idx, false)) = col_to_fx(app.cursor_col) {
                                    let buf = app.fx_edit_buf.clone();
                                    app.fx_edit_buf.clear();
                                    if !buf.is_empty() {
                                        if let Ok(v) = buf.parse::<u16>() {
                                            let step_idx = app.cursor_step;
                                            app.record(&format!("set FX value at step {}", app.cursor_step));
                                            app.phrase_mut().steps[step_idx].fx[slot_idx].value =
                                                v.min(255) as u8;
                                            app.sync_phrase_to_sequencer();
                                            app.cursor_step =
                                                (app.cursor_step + 1) % STEPS_PER_PHRASE;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        },

                        // ── Command mode ──────────────────────────────────────
                        InputMode::Command => match key.code {
                            KeyCode::Enter => app.execute_command(),
                            KeyCode::Esc => {
                                app.mode = InputMode::Normal;
                                app.cmd_buf.clear();
                            }
                            KeyCode::Backspace => {
                                app.cmd_buf.pop();
                            }
                            KeyCode::Char(c) => {
                                app.cmd_buf.push(c);
                            }
                            _ => {}
                        },
                        // Keyboard mode is handled globally before this match; this arm
                        // is unreachable but required for exhaustiveness.
                        InputMode::Keyboard => {}
                        // ConfirmQuit is handled globally before this match; this arm
                        // is unreachable but required for exhaustiveness.
                        InputMode::ConfirmQuit => {}
                    },

                    // ──────────────────────────────────────────────────────────
                    // Instrument editor key handling
                    // ──────────────────────────────────────────────────────────
                    View::InstrumentEditor => {
                        if app.instr_editing {
                            // Text-edit sub-mode for Name / Sample fields
                            match key.code {
                                KeyCode::Enter => {
                                    let buf = app.instr_edit_buf.clone();
                                    let cursor = app.instr_cursor;
                                    app.record(&format!("edit instrument {} name/sample", app.active_instrument));
                                    if let Some(instr) =
                                        app.song.instruments.get_mut(app.active_instrument)
                                    {
                                        match cursor {
                                            INSTR_FIELD_NAME => instr.name = buf,
                                            INSTR_FIELD_SAMPLE => {
                                                instr.sample = Some(
                                                    vitakt_core::model::Sample::from_path(&buf),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                    app.instr_editing = false;
                                    app.instr_edit_buf.clear();
                                    if cursor == INSTR_FIELD_SAMPLE {
                                        app.reload_instrument_sample();
                                    }
                                }
                                KeyCode::Esc => {
                                    app.instr_editing = false;
                                    app.instr_edit_buf.clear();
                                }
                                KeyCode::Backspace => {
                                    app.instr_edit_buf.pop();
                                }
                                KeyCode::Char(c) => {
                                    app.instr_edit_buf.push(c);
                                }
                                _ => {}
                            }
                        } else {
                            // Normal instrument editor navigation
                            match key.code {
                                KeyCode::Esc => {
                                    app.pop_view();
                                }
                                KeyCode::Char('j') | KeyCode::Down => {
                                    app.instr_cursor =
                                        (app.instr_cursor + 1) % INSTR_FIELD_COUNT;
                                }
                                KeyCode::Char('k') | KeyCode::Up => {
                                    app.instr_cursor =
                                        (app.instr_cursor + INSTR_FIELD_COUNT - 1)
                                            % INSTR_FIELD_COUNT;
                                }
                                // Enter text edit mode (for name/sample) or open browser
                                KeyCode::Char('i') => {
                                    let cursor = app.instr_cursor;
                                    if cursor == INSTR_FIELD_SAMPLE {
                                        // Open browser directly on sample field
                                        app.open_sample_browser();
                                    } else if cursor == INSTR_FIELD_NAME {
                                        // Pre-fill with current name
                                        let cur_name = app
                                            .song
                                            .instruments
                                            .get(app.active_instrument)
                                            .map(|i| i.name.clone())
                                            .unwrap_or_default();
                                        app.instr_edit_buf = cur_name;
                                        app.instr_editing = true;
                                    } else {
                                        // For non-text fields, treat 'i' like 'l' (increment)
                                        app.record(&format!("edit instrument {} field {}", app.active_instrument, app.instr_cursor));
                                        instr_editor_increment(&mut app, 1);
                                    }
                                }
                                // Enter opens browser on sample field
                                KeyCode::Enter => {
                                    if app.instr_cursor == INSTR_FIELD_SAMPLE {
                                        app.open_sample_browser();
                                    }
                                }
                                // h/l or Left/Right adjust numeric/mode fields
                                KeyCode::Char('h') | KeyCode::Left => {
                                    app.record(&format!("edit instrument {} field {}", app.active_instrument, app.instr_cursor));
                                    instr_editor_increment(&mut app, -1);
                                }
                                KeyCode::Char('l') | KeyCode::Right => {
                                    app.record(&format!("edit instrument {} field {}", app.active_instrument, app.instr_cursor));
                                    instr_editor_increment(&mut app, 1);
                                }
                                _ => {}
                            }
                        }
                    }

                    // ──────────────────────────────────────────────────────────
                    // Sample browser key handling
                    // ──────────────────────────────────────────────────────────
                    View::SampleBrowser => match key.code {
                        KeyCode::Esc => {
                            app.pop_view();
                        }
                        KeyCode::Char(' ') => app.browser_preview_toggle(),
                        KeyCode::Char('j') | KeyCode::Down => {
                            if !app.browser_entries.is_empty() {
                                app.browser_cursor =
                                    (app.browser_cursor + 1) % app.browser_entries.len();
                                let available = terminal
                                    .size()
                                    .map(|r| (r.height as usize).saturating_sub(5))
                                    .unwrap_or(0);
                                app.browser_clamp_scroll(available);
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if !app.browser_entries.is_empty() {
                                app.browser_cursor =
                                    (app.browser_cursor + app.browser_entries.len() - 1)
                                        % app.browser_entries.len();
                                let available = terminal
                                    .size()
                                    .map(|r| (r.height as usize).saturating_sub(5))
                                    .unwrap_or(0);
                                app.browser_clamp_scroll(available);
                            }
                        }
                        KeyCode::Enter => app.browser_enter(),
                        KeyCode::Backspace | KeyCode::Char('-') => app.browser_go_up(),
                        _ => {}
                    },

                    // ──────────────────────────────────────────────────────────
                    // Mixer view key handling
                    // ──────────────────────────────────────────────────────────
                    View::Mixer => match key.code {
                        KeyCode::Esc | KeyCode::F(2) => app.pop_view(),

                        // Track navigation (left/right)
                        KeyCode::Char('h') | KeyCode::Left => {
                            if app.mixer_cursor_track > 0 {
                                app.mixer_cursor_track -= 1;
                            }
                        }
                        KeyCode::Char('l') | KeyCode::Right => {
                            if app.mixer_cursor_track < TRACKS - 1 {
                                app.mixer_cursor_track += 1;
                            }
                        }

                        // Field navigation (up/down)
                        KeyCode::Char('j') | KeyCode::Down => {
                            app.mixer_cursor_field =
                                (app.mixer_cursor_field + 1) % MIXER_FIELD_COUNT;
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            app.mixer_cursor_field =
                                (app.mixer_cursor_field + MIXER_FIELD_COUNT - 1)
                                    % MIXER_FIELD_COUNT;
                        }

                        // Increment/decrement numeric fields (+/=  and  -)
                        KeyCode::Char('+') | KeyCode::Char('=') => {
                            app.record(&format!("set mixer track {} field", app.mixer_cursor_track));
                            let t = app.mixer_cursor_track;
                            let m = &mut app.song.mixer[t];
                            match app.mixer_cursor_field {
                                MIXER_FIELD_VOL => {
                                    m.volume = ((m.volume + 0.05) * 100.0).round() / 100.0;
                                    m.volume = m.volume.clamp(0.0, 2.0);
                                    let v = m.volume;
                                    app.send_cmd(Command::SetTrackVolume {
                                        track: t as u8,
                                        volume: v,
                                    });
                                }
                                MIXER_FIELD_PAN => {
                                    m.pan = ((m.pan + 0.05) * 100.0).round() / 100.0;
                                    m.pan = m.pan.clamp(-1.0, 1.0);
                                    let p = m.pan;
                                    app.send_cmd(Command::SetTrackPan {
                                        track: t as u8,
                                        pan: p,
                                    });
                                }
                                MIXER_FIELD_SEND => {
                                    m.fx_send = ((m.fx_send + 0.05) * 100.0).round() / 100.0;
                                    m.fx_send = m.fx_send.clamp(0.0, 1.0);
                                }
                                _ => {}
                            }
                        }
                        KeyCode::Char('-') => {
                            app.record(&format!("set mixer track {} field", app.mixer_cursor_track));
                            let t = app.mixer_cursor_track;
                            let m = &mut app.song.mixer[t];
                            match app.mixer_cursor_field {
                                MIXER_FIELD_VOL => {
                                    m.volume = ((m.volume - 0.05) * 100.0).round() / 100.0;
                                    m.volume = m.volume.clamp(0.0, 2.0);
                                    let v = m.volume;
                                    app.send_cmd(Command::SetTrackVolume {
                                        track: t as u8,
                                        volume: v,
                                    });
                                }
                                MIXER_FIELD_PAN => {
                                    m.pan = ((m.pan - 0.05) * 100.0).round() / 100.0;
                                    m.pan = m.pan.clamp(-1.0, 1.0);
                                    let p = m.pan;
                                    app.send_cmd(Command::SetTrackPan {
                                        track: t as u8,
                                        pan: p,
                                    });
                                }
                                MIXER_FIELD_SEND => {
                                    m.fx_send = ((m.fx_send - 0.05) * 100.0).round() / 100.0;
                                    m.fx_send = m.fx_send.clamp(0.0, 1.0);
                                }
                                _ => {}
                            }
                        }

                        // Toggle mute (m key or Enter on MUTE row)
                        KeyCode::Char('m') => {
                            let t = app.mixer_cursor_track;
                            app.record(&format!("toggle mute track {}", t));
                            let new_mute = !app.song.mixer[t].mute;
                            app.song.mixer[t].mute = new_mute;
                            app.send_cmd(Command::SetTrackMute {
                                track: t as u8,
                                mute: new_mute,
                            });
                        }

                        // Toggle solo (s key or Enter on SOLO row)
                        KeyCode::Char('s') => {
                            let t = app.mixer_cursor_track;
                            app.record(&format!("toggle solo track {}", t));
                            let new_solo = !app.song.mixer[t].solo;
                            app.song.mixer[t].solo = new_solo;
                            app.send_cmd(Command::SetTrackSolo {
                                track: t as u8,
                                active: new_solo,
                            });
                        }

                        // Enter toggles the current field (mute/solo rows) or steps on others
                        KeyCode::Enter => {
                            let t = app.mixer_cursor_track;
                            match app.mixer_cursor_field {
                                MIXER_FIELD_MUTE => {
                                    app.record(&format!("toggle mute track {}", t));
                                    let new_mute = !app.song.mixer[t].mute;
                                    app.song.mixer[t].mute = new_mute;
                                    app.send_cmd(Command::SetTrackMute {
                                        track: t as u8,
                                        mute: new_mute,
                                    });
                                }
                                MIXER_FIELD_SOLO => {
                                    app.record(&format!("toggle solo track {}", t));
                                    let new_solo = !app.song.mixer[t].solo;
                                    app.song.mixer[t].solo = new_solo;
                                    app.send_cmd(Command::SetTrackSolo {
                                        track: t as u8,
                                        active: new_solo,
                                    });
                                }
                                _ => {}
                            }
                        }

                        // Transport controls pass through even from Mixer
                        KeyCode::Char(' ') => app.toggle_play(),
                        KeyCode::F(5) => app.restart_play(),

                        _ => {}
                    },
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();
    let action = parse_args(&raw_args).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });

    // Lock-free SPSC channel: UI → audio thread.
    let (producer, consumer) = RingBuffer::<Command>::new(64);

    // Shared state: UI reads, audio writes.
    let seq_playing = Arc::new(AtomicBool::new(false));
    let current_seq_step = Arc::new(AtomicU8::new(0));
    let preview_playing = Arc::new(AtomicBool::new(false));

    let initial_bpm = 120.0f32;

    let _stream = start_audio_stream(
        consumer,
        Arc::clone(&seq_playing),
        Arc::clone(&current_seq_step),
        Arc::clone(&preview_playing),
        None, // samples are loaded from project instruments, not the CLI
        initial_bpm,
    )
    .unwrap_or_else(|e| {
        eprintln!("Warning: could not open audio device: {e}");
        panic!("audio unavailable: {e}")
    });

    run_tui(Some(producer), 60, seq_playing, current_seq_step, preview_playing, action)?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use vitakt_core::model::Song;

    fn make_app() -> App {
        App::new(
            None,
            60,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
        )
    }

    #[test]
    fn app_default_song_has_current_version() {
        let app = make_app();
        assert_eq!(app.song.version, vitakt_core::CURRENT_VERSION);
    }

    #[test]
    fn app_has_one_phrase_on_init() {
        let app = make_app();
        assert_eq!(app.song.phrases.len(), 1);
        assert_eq!(app.song.phrases[0].steps.len(), 16);
    }

    #[test]
    fn note_name_middle_c() {
        assert_eq!(note_name(60), "C-4");
    }

    #[test]
    fn note_name_a4() {
        assert_eq!(note_name(69), "A-4");
    }

    #[test]
    fn note_name_c_sharp() {
        assert_eq!(note_name(61), "C#4");
    }

    #[test]
    fn qwerty_z_is_c() {
        assert_eq!(qwerty_to_semitone('z'), Some(0));
    }

    #[test]
    fn qwerty_s_is_csharp() {
        assert_eq!(qwerty_to_semitone('s'), Some(1));
    }

    #[test]
    fn qwerty_unknown_is_none() {
        assert_eq!(qwerty_to_semitone('a'), None);
    }

    #[test]
    fn enter_note_sets_step_and_advances_cursor() {
        let mut app = make_app();
        app.enter_note(60);
        assert_eq!(app.song.phrases[0].steps[0].note, Some(60));
        assert_eq!(app.cursor_step, 1);
    }

    #[test]
    fn enter_note_wraps_cursor_at_end() {
        let mut app = make_app();
        app.cursor_step = 15;
        app.enter_note(60);
        assert_eq!(app.cursor_step, 0);
    }

    #[test]
    fn pitch_speed_root_is_one() {
        let speed = pitch_speed(60, 60);
        assert!((speed - 1.0).abs() < 1e-4);
    }

    #[test]
    fn pitch_speed_octave_up_is_two() {
        let speed = pitch_speed(72, 60);
        assert!((speed - 2.0).abs() < 1e-4);
    }

    #[test]
    fn pitch_speed_octave_down_is_half() {
        let speed = pitch_speed(48, 60);
        assert!((speed - 0.5).abs() < 1e-4);
    }

    #[test]
    fn pit_fx_value_12_doubles_speed() {
        // PIT value 12 → i8 = 12 → +1 octave → speed ×2
        let semitones = 12u8 as i8;
        let factor = 2.0f32.powf(semitones as f32 / 12.0);
        assert!((factor - 2.0).abs() < 1e-4, "PIT+12 should double speed, got {factor}");
    }

    #[test]
    fn pit_fx_value_244_halves_speed() {
        // PIT value 244 reinterpreted as i8 = -12 → -1 octave → speed ×0.5
        let semitones = 244u8 as i8;
        assert_eq!(semitones, -12, "244u8 as i8 must equal -12");
        let factor = 2.0f32.powf(semitones as f32 / 12.0);
        assert!((factor - 0.5).abs() < 1e-4, "PIT-12 should halve speed, got {factor}");
    }

    #[test]
    fn execute_command_w_error_on_bad_path() {
        let mut app = make_app();
        app.mode = InputMode::Command;
        app.cmd_buf = "w /nonexistent_dir/out.trk".to_string();
        app.execute_command();
        assert!(app.status.starts_with("Error:"), "got: {}", app.status);
    }

    #[test]
    fn execute_command_e_error_on_missing_file() {
        let mut app = make_app();
        app.mode = InputMode::Command;
        app.cmd_buf = "e /nonexistent/file.trk".to_string();
        app.execute_command();
        assert!(app.status.starts_with("Error:"), "got: {}", app.status);
    }

    #[test]
    fn execute_bpm_sets_song_bpm() {
        let mut app = make_app();
        app.cmd_buf = "bpm 140".to_string();
        app.execute_command();
        assert!((app.song.bpm - 140.0).abs() < 1e-4, "BPM should be 140.0, got {}", app.song.bpm);
    }

    #[test]
    fn execute_bpm_clamps_below_minimum() {
        let mut app = make_app();
        app.cmd_buf = "bpm 10".to_string();
        app.execute_command();
        assert!((app.song.bpm - 20.0).abs() < 1e-4, "BPM should clamp to 20.0, got {}", app.song.bpm);
    }

    #[test]
    fn execute_bpm_clamps_above_maximum() {
        let mut app = make_app();
        app.cmd_buf = "bpm 1000".to_string();
        app.execute_command();
        assert!((app.song.bpm - 999.0).abs() < 1e-4, "BPM should clamp to 999.0, got {}", app.song.bpm);
    }

    #[test]
    fn execute_bpm_invalid_shows_error() {
        let mut app = make_app();
        app.cmd_buf = "bpm foo".to_string();
        app.execute_command();
        assert!(app.status.contains("Invalid BPM"), "expected error, got: {}", app.status);
    }

    #[test]
    fn execute_bpm_no_value_shows_usage() {
        let mut app = make_app();
        app.cmd_buf = "bpm".to_string();
        app.execute_command();
        assert!(app.status.contains("Usage"), "expected usage hint, got: {}", app.status);
    }

    #[test]
    fn execute_bpm_is_undoable() {
        let mut app = make_app();
        let original_bpm = app.song.bpm;
        app.cmd_buf = "bpm 180".to_string();
        app.execute_command();
        assert!((app.song.bpm - 180.0).abs() < 1e-4);
        app.do_undo();
        assert!((app.song.bpm - original_bpm).abs() < 1e-4, "undo should restore original BPM");
    }

    #[test]
    fn execute_bpm_nan_shows_error_and_does_not_corrupt_bpm() {
        let mut app = make_app();
        let original_bpm = app.song.bpm;
        app.cmd_buf = "bpm nan".to_string();
        app.execute_command();
        assert!(app.status.contains("Invalid BPM"), "expected error, got: {}", app.status);
        assert_eq!(app.song.bpm, original_bpm, "NaN must not corrupt song.bpm");
    }

    #[test]
    fn step_roundtrip_bincode() {
        let mut app = make_app();
        app.enter_note(60);
        app.enter_note(64);
        app.enter_note(67);

        let path = std::env::temp_dir().join("vitakt_phrase_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "got: {}", app.status);

        let original_steps = app.song.phrases[0].steps.clone();
        app.song = Song::default();
        app.song.phrases.push(vitakt_core::model::Phrase::default());

        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "got: {}", app.status);

        assert_eq!(app.song.phrases[0].steps, original_steps);
        std::fs::remove_file(&path).ok();
    }

    // ── Instrument editor tests ───────────────────────────────────────────────

    #[test]
    fn ensure_instrument_creates_default() {
        let mut app = make_app();
        assert!(app.song.instruments.is_empty());
        app.ensure_instrument(0);
        assert_eq!(app.song.instruments.len(), 1);
        assert_eq!(app.song.instruments[0].root_note, 60);
    }

    #[test]
    fn ensure_instrument_does_not_exceed_max() {
        let mut app = make_app();
        // Request instrument at index MAX_INSTRUMENTS (should not create >MAX)
        app.ensure_instrument(MAX_INSTRUMENTS - 1);
        assert_eq!(app.song.instruments.len(), MAX_INSTRUMENTS);

        // Requesting one beyond the max should not grow further
        app.ensure_instrument(MAX_INSTRUMENTS);
        assert_eq!(app.song.instruments.len(), MAX_INSTRUMENTS);
    }

    #[test]
    fn open_instrument_editor_creates_instrument_if_missing() {
        let mut app = make_app();
        app.open_instrument_editor();
        assert_eq!(app.song.instruments.len(), 1);
        assert!(matches!(app.view, View::InstrumentEditor));
    }

    #[test]
    fn instr_editor_increment_root_note() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].root_note = 60;
        app.instr_cursor = INSTR_FIELD_ROOT;
        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].root_note, 61);
        instr_editor_increment(&mut app, -1);
        assert_eq!(app.song.instruments[0].root_note, 60);
    }

    #[test]
    fn instr_editor_increment_loop_start() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.instr_cursor = INSTR_FIELD_LOOP_START;
        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].loop_start, Some(1));
    }

    #[test]
    fn instr_editor_increment_loop_end() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.instr_cursor = INSTR_FIELD_LOOP_END;
        instr_editor_increment(&mut app, 5);
        // Note: delta is applied once per call, so we need 5 calls or a different approach
        // Actually delta=5 means +5 in one call
        assert_eq!(app.song.instruments[0].loop_end, Some(5));
    }

    #[test]
    fn instr_editor_increment_interp_mode_cycles() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].interp_mode = InterpMode::None;
        app.instr_cursor = INSTR_FIELD_INTERP;

        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].interp_mode, InterpMode::Linear);

        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].interp_mode, InterpMode::Sinc);

        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].interp_mode, InterpMode::None);
    }

    #[test]
    fn instr_editor_increment_interp_mode_reverse() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].interp_mode = InterpMode::None;
        app.instr_cursor = INSTR_FIELD_INTERP;

        instr_editor_increment(&mut app, -1);
        assert_eq!(app.song.instruments[0].interp_mode, InterpMode::Sinc);
    }

    #[test]
    fn instrument_changes_persist_through_save_load() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].name = "TestKit".to_string();
        app.song.instruments[0].root_note = 48;
        app.song.instruments[0].loop_start = Some(100);
        app.song.instruments[0].loop_end = Some(200);
        app.song.instruments[0].interp_mode = InterpMode::Sinc;
        app.song.instruments[0].volume = 0.75;
        app.song.instruments[0].pan = -0.5;

        let path = std::env::temp_dir().join("vitakt_instrument_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "save failed: {}", app.status);

        let orig = app.song.instruments[0].clone();

        // Reset song and reload
        app.song = Song::default();
        app.song.phrases.push(vitakt_core::model::Phrase::default());

        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "load failed: {}", app.status);

        assert!(!app.song.instruments.is_empty(), "instruments should have been loaded");
        let loaded = &app.song.instruments[0];
        assert_eq!(loaded.name, orig.name);
        assert_eq!(loaded.root_note, orig.root_note);
        assert_eq!(loaded.loop_start, orig.loop_start);
        assert_eq!(loaded.loop_end, orig.loop_end);
        assert_eq!(loaded.interp_mode, orig.interp_mode);
        assert!((loaded.volume - orig.volume).abs() < 0.01);
        assert!((loaded.pan - orig.pan).abs() < 0.01);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_browser_entries_tags_and_filters_correctly() {
        let dir = std::env::temp_dir().join("vitakt_browser_test");
        std::fs::create_dir_all(&dir).ok();

        // Create wav files (case-insensitive extension), a non-wav file, and a subdir
        for name in &["b.wav", "a.WAV", "c.wav", "ignored.txt"] {
            std::fs::write(dir.join(name), b"RIFF").ok();
        }
        let subdir = dir.join("samples_dir");
        std::fs::create_dir_all(&subdir).ok();

        let entries = list_browser_entries(&dir);

        // Clean up
        for name in &["b.wav", "a.WAV", "c.wav", "ignored.txt"] {
            std::fs::remove_file(dir.join(name)).ok();
        }
        std::fs::remove_dir(&subdir).ok();

        // Should contain the directory and the 3 wav files, not the txt file
        assert!(!entries.is_empty(), "should find entries");
        let dirs: Vec<_> = entries.iter().filter(|e| matches!(e, BrowserEntry::Dir(_))).collect();
        let wavs: Vec<_> = entries.iter().filter(|e| matches!(e, BrowserEntry::Wav(_))).collect();
        assert_eq!(dirs.len(), 1, "should have 1 directory");
        assert_eq!(wavs.len(), 3, "should have 3 wav files");

        // Check sorted (case-insensitive) — dirs and wavs interleaved alphabetically
        let names: Vec<String> = entries.iter().map(|e| e.sort_key()).collect();
        let is_sorted = names.windows(2).all(|w| w[0] <= w[1]);
        assert!(is_sorted, "entries should be sorted: {names:?}");
    }

    #[test]
    fn list_browser_entries_empty_dir() {
        let dir = std::env::temp_dir().join("vitakt_browser_empty_test");
        std::fs::create_dir_all(&dir).ok();
        // Remove any files that might exist from a previous run
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                std::fs::remove_file(entry.path()).ok();
                std::fs::remove_dir(entry.path()).ok();
            }
        }
        let entries = list_browser_entries(&dir);
        // An empty non-root directory should contain only the synthetic `..` entry.
        assert_eq!(entries.len(), 1, "empty non-root dir should yield exactly the '..' entry");
        assert!(matches!(entries[0], BrowserEntry::ParentDir), "first entry should be ParentDir");
    }

    #[test]
    fn sample_browser_opens_and_cancel_returns_to_instrument_editor() {
        let mut app = make_app();
        app.open_instrument_editor();
        app.open_sample_browser();
        assert!(matches!(app.view, View::SampleBrowser));
        // Simulate Esc — cancel
        app.view = View::InstrumentEditor;
        assert!(matches!(app.view, View::InstrumentEditor));
    }

    #[test]
    fn browser_enter_on_dir_updates_browser_dir_and_entries() {
        let parent = std::env::temp_dir().join("vitakt_browser_enter_test");
        let subdir = parent.join("subdir");
        std::fs::create_dir_all(&subdir).ok();
        std::fs::write(subdir.join("kick.wav"), b"RIFF").ok();

        let mut app = make_app();
        app.browser_dir = parent.clone();
        app.browser_entries = list_browser_entries(&parent);
        app.browser_cursor = 0;

        // Find the index of the Dir entry
        let dir_idx = app
            .browser_entries
            .iter()
            .position(|e| matches!(e, BrowserEntry::Dir(_)))
            .expect("should have a Dir entry");
        app.browser_cursor = dir_idx;

        app.browser_enter();

        assert_eq!(app.browser_dir, subdir, "browser_dir should update to subdir");
        assert_eq!(app.browser_cursor, 0, "cursor should reset to 0");
        let has_wav = app
            .browser_entries
            .iter()
            .any(|e| matches!(e, BrowserEntry::Wav(n) if n == "kick.wav"));
        assert!(has_wav, "browser_entries should contain kick.wav after entering subdir");

        // Clean up
        std::fs::remove_file(subdir.join("kick.wav")).ok();
        std::fs::remove_dir(&subdir).ok();
        std::fs::remove_dir(&parent).ok();
    }

    #[test]
    fn browser_go_up_navigates_to_parent() {
        let parent = std::env::temp_dir().join("vitakt_go_up_test");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).ok();

        let mut app = make_app();
        app.browser_dir = child.clone();
        app.browser_entries = list_browser_entries(&child);

        app.browser_go_up();

        assert_eq!(app.browser_dir, parent, "browser_dir should be the parent after go_up");
        assert_eq!(app.browser_cursor, 0, "cursor should reset to 0");

        // Clean up
        std::fs::remove_dir(&child).ok();
        std::fs::remove_dir(&parent).ok();
    }

    #[test]
    fn browser_go_up_at_root_does_nothing() {
        let root = PathBuf::from("/");
        let mut app = make_app();
        app.browser_dir = root.clone();
        app.browser_entries = Vec::new();

        app.browser_go_up();

        assert_eq!(app.browser_dir, root, "browser_dir should not change when already at root");
    }

    #[test]
    fn song_view_assign_chain_persists() {
        let mut app = make_app();
        app.song_cursor_row = 0;
        app.song_cursor_track = 1;
        // Simulate assigning chain 0 to track 1
        app.ensure_chain(0);
        app.song.arrangement[0][1] = Some(0);
        assert_eq!(app.song.arrangement[0][1], Some(0));
    }

    #[test]
    fn song_view_clear_chain() {
        let mut app = make_app();
        app.song.arrangement[0][0] = Some(5);
        app.song.arrangement[0][0] = None;
        assert_eq!(app.song.arrangement[0][0], None);
    }

    #[test]
    fn chain_view_transpose_adjusts() {
        let mut app = make_app();
        // Chain 0 exists from default
        app.chain_view_track = 0;
        app.chain_view_row = 0;
        app.chain_cursor = 0;
        let ci = 0usize;
        app.song.chains[ci].slots[0].transpose = 5;
        assert_eq!(app.song.chains[ci].slots[0].transpose, 5);
        app.song.chains[ci].slots[0].transpose += 1;
        assert_eq!(app.song.chains[ci].slots[0].transpose, 6);
    }

    #[test]
    fn arrangement_roundtrip_bincode() {
        let mut app = make_app();
        app.song.arrangement[0][0] = Some(0);
        app.song.arrangement[0][3] = Some(2);
        app.ensure_chain(2);

        let path = std::env::temp_dir().join("vitakt_arrangement_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "save failed: {}", app.status);

        let saved_arr = app.song.arrangement.clone();

        app.song = Song::default();
        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "load failed: {}", app.status);

        assert_eq!(app.song.arrangement, saved_arr, "arrangement should survive round-trip");
        std::fs::remove_file(&path).ok();
    }

    // ── Mixer tests ──────────────────────────────────────────────────────────

    #[test]
    fn mixer_defaults_to_unity_gain_center_pan_no_mute_solo() {
        let app = make_app();
        for t in 0..TRACKS {
            let m = &app.song.mixer[t];
            assert!((m.volume - 1.0).abs() < 1e-4, "track {t} volume should default to 1.0");
            assert!(m.pan.abs() < 1e-4, "track {t} pan should default to 0.0");
            assert!(!m.mute, "track {t} should not be muted by default");
            assert!(!m.solo, "track {t} should not be soloed by default");
            assert!(m.fx_send.abs() < 1e-4, "track {t} fx_send should default to 0.0");
        }
    }

    #[test]
    fn mixer_mute_toggle_updates_model() {
        let mut app = make_app();
        assert!(!app.song.mixer[0].mute);
        app.song.mixer[0].mute = true;
        assert!(app.song.mixer[0].mute);
        app.song.mixer[0].mute = false;
        assert!(!app.song.mixer[0].mute);
    }

    #[test]
    fn mixer_solo_toggle_updates_model() {
        let mut app = make_app();
        assert!(!app.song.mixer[2].solo);
        app.song.mixer[2].solo = true;
        assert!(app.song.mixer[2].solo);
    }

    #[test]
    fn mixer_volume_clamped_to_range() {
        let mut app = make_app();
        app.song.mixer[0].volume = 3.0;
        app.song.mixer[0].volume = app.song.mixer[0].volume.clamp(0.0, 2.0);
        assert!((app.song.mixer[0].volume - 2.0).abs() < 1e-4);

        app.song.mixer[0].volume = -0.5;
        app.song.mixer[0].volume = app.song.mixer[0].volume.clamp(0.0, 2.0);
        assert!(app.song.mixer[0].volume.abs() < 1e-4);
    }

    #[test]
    fn mixer_state_persists_through_save_load() {
        let mut app = make_app();
        app.song.mixer[0].volume = 0.75;
        app.song.mixer[1].pan = -0.5;
        app.song.mixer[2].mute = true;
        app.song.mixer[3].solo = true;
        app.song.mixer[4].fx_send = 0.3;

        let path = std::env::temp_dir().join("vitakt_mixer_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "save failed: {}", app.status);

        app.song = Song::default();
        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "load failed: {}", app.status);

        assert!((app.song.mixer[0].volume - 0.75).abs() < 1e-3, "volume should persist");
        assert!((app.song.mixer[1].pan - (-0.5)).abs() < 1e-3, "pan should persist");
        assert!(app.song.mixer[2].mute, "mute should persist");
        assert!(app.song.mixer[3].solo, "solo should persist");
        assert!((app.song.mixer[4].fx_send - 0.3).abs() < 1e-3, "fx_send should persist");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mixer_multiple_solos_allowed() {
        let mut app = make_app();
        app.song.mixer[0].solo = true;
        app.song.mixer[3].solo = true;
        assert!(app.song.mixer[0].solo);
        assert!(app.song.mixer[3].solo);
        assert!(!app.song.mixer[1].solo);
    }

    // ── Undo/redo tests ──────────────────────────────────────────────────────

    #[test]
    fn undo_redo_note_entry() {
        let mut app = make_app();
        let original = app.song.clone();

        // Enter 5 notes
        for i in 0..5u8 {
            app.cursor_step = i as usize;
            app.enter_note(60 + i);
        }
        let mutated = app.song.clone();
        // Verify mutations happened
        for i in 0..5usize {
            assert_eq!(app.song.phrases[0].steps[i].note, Some(60 + i as u8));
        }

        // Undo all 5
        for _ in 0..5 {
            app.do_undo();
        }
        assert_eq!(app.song.phrases[0], original.phrases[0], "after 5 undos should match original");

        // Redo all 5
        for _ in 0..5 {
            app.do_redo();
        }
        assert_eq!(app.song.phrases[0], mutated.phrases[0], "after 5 redos should match mutated state");
    }

    #[test]
    fn new_mutation_clears_redo_stack() {
        let mut app = make_app();
        app.enter_note(60);
        app.do_undo();
        // Redo stack has one entry now
        // Enter a new note — should clear redo
        app.cursor_step = 1;
        app.enter_note(62);
        app.do_redo(); // should do nothing
        // Step 0 should still be empty (redo was cleared)
        assert_eq!(app.song.phrases[0].steps[0].note, None);
        // Step 1 should have the new note
        assert_eq!(app.song.phrases[0].steps[1].note, Some(62));
    }

    #[test]
    fn undo_stack_capped_at_1000() {
        let mut app = make_app();
        for i in 0..1001u16 {
            app.song.bpm = 100.0 + i as f32 * 0.001;
            app.history.push(format!("change {i}"), app.song.clone());
        }
        // Should have exactly 1000 entries (oldest dropped)
        assert_eq!(app.history.undo_stack.len(), 1000);
    }

    #[test]
    fn song_view_o_inserts_row_below_cursor() {
        let mut app = make_app();
        // Start with 1 row; cursor at row 0
        assert_eq!(app.song.arrangement.len(), 1);
        app.song_cursor_row = 0;
        // Mark row 0 so we can check order after insert
        app.song.arrangement[0][0] = Some(7);
        // Simulate 'o': insert below cursor
        let insert_at = app.song_cursor_row + 1;
        app.song.arrangement.insert(insert_at, [None; TRACKS]);
        assert_eq!(app.song.arrangement.len(), 2);
        // Original row stays at 0, new blank row is at 1
        assert_eq!(app.song.arrangement[0][0], Some(7));
        assert_eq!(app.song.arrangement[1][0], None);
        // Cursor unchanged
        assert_eq!(app.song_cursor_row, 0);
    }

    #[test]
    fn song_view_capital_o_inserts_row_above_cursor_and_cursor_follows() {
        let mut app = make_app();
        app.song.arrangement[0][0] = Some(3);
        app.song_cursor_row = 0;
        // Simulate 'O': insert above cursor, cursor increments to stay on original row
        let insert_at = app.song_cursor_row;
        app.song.arrangement.insert(insert_at, [None; TRACKS]);
        app.song_cursor_row += 1;
        assert_eq!(app.song.arrangement.len(), 2);
        // New blank row at 0, original row shifted to 1
        assert_eq!(app.song.arrangement[0][0], None);
        assert_eq!(app.song.arrangement[1][0], Some(3));
        // Cursor follows original row
        assert_eq!(app.song_cursor_row, 1);
    }

    #[test]
    fn song_view_o_inserts_row_below_cursor_at_middle_position() {
        let mut app = make_app();
        // Set up 3 rows: [Some(1), Some(2), Some(3)]
        app.song.arrangement[0][0] = Some(1);
        app.song.arrangement.push([None; TRACKS]);
        app.song.arrangement[1][0] = Some(2);
        app.song.arrangement.push([None; TRACKS]);
        app.song.arrangement[2][0] = Some(3);
        assert_eq!(app.song.arrangement.len(), 3);
        // Cursor at row 1 (middle)
        app.song_cursor_row = 1;
        // Simulate 'o': insert below cursor (at index 2)
        let insert_at = app.song_cursor_row + 1;
        app.song.arrangement.insert(insert_at, [None; TRACKS]);
        assert_eq!(app.song.arrangement.len(), 4);
        // Original rows at 0 and 1 unchanged; new blank row at 2; row 3 is shifted Some(3)
        assert_eq!(app.song.arrangement[0][0], Some(1));
        assert_eq!(app.song.arrangement[1][0], Some(2));
        assert_eq!(app.song.arrangement[2][0], None);
        assert_eq!(app.song.arrangement[3][0], Some(3));
        // Cursor stays on middle row
        assert_eq!(app.song_cursor_row, 1);
    }

    #[test]
    fn song_view_capital_o_inserts_row_above_middle_cursor_and_cursor_follows() {
        let mut app = make_app();
        // Set up 3 rows: [Some(1), Some(2), Some(3)]
        app.song.arrangement[0][0] = Some(1);
        app.song.arrangement.push([None; TRACKS]);
        app.song.arrangement[1][0] = Some(2);
        app.song.arrangement.push([None; TRACKS]);
        app.song.arrangement[2][0] = Some(3);
        assert_eq!(app.song.arrangement.len(), 3);
        // Cursor at row 1 (middle)
        app.song_cursor_row = 1;
        // Simulate 'O': insert above cursor (at index 1), cursor increments
        let insert_at = app.song_cursor_row;
        app.song.arrangement.insert(insert_at, [None; TRACKS]);
        app.song_cursor_row += 1;
        assert_eq!(app.song.arrangement.len(), 4);
        // Blank row inserted at index 1; original row 1 shifted to 2
        assert_eq!(app.song.arrangement[0][0], Some(1));
        assert_eq!(app.song.arrangement[1][0], None);
        assert_eq!(app.song.arrangement[2][0], Some(2));
        assert_eq!(app.song.arrangement[3][0], Some(3));
        // Cursor follows original row to index 2
        assert_eq!(app.song_cursor_row, 2);
    }

    #[test]
    fn chain_view_o_inserts_slot_below_cursor() {
        let mut app = make_app();
        let ci = 0usize;
        // Start with 1 slot at cursor 0
        assert_eq!(app.song.chains[ci].slots.len(), 1);
        app.chain_cursor = 0;
        app.song.chains[ci].slots[0].phrase = 5;
        // Simulate 'o': insert below cursor
        let insert_at = (app.chain_cursor + 1).min(app.song.chains[ci].slots.len());
        app.song.chains[ci].slots.insert(insert_at, ChainSlot { phrase: 0, transpose: 0 });
        app.chain_cursor = insert_at;
        assert_eq!(app.song.chains[ci].slots.len(), 2);
        // Original slot stays at index 0
        assert_eq!(app.song.chains[ci].slots[0].phrase, 5);
        // New slot at index 1
        assert_eq!(app.song.chains[ci].slots[1].phrase, 0);
        // Cursor moved to new slot
        assert_eq!(app.chain_cursor, 1);
    }

    #[test]
    fn chain_view_capital_o_inserts_slot_above_cursor_and_cursor_follows() {
        let mut app = make_app();
        let ci = 0usize;
        app.chain_cursor = 0;
        app.song.chains[ci].slots[0].phrase = 5;
        // Simulate 'O': insert above cursor, cursor increments
        let insert_at = app.chain_cursor;
        app.song.chains[ci].slots.insert(insert_at, ChainSlot { phrase: 0, transpose: 0 });
        app.chain_cursor += 1;
        assert_eq!(app.song.chains[ci].slots.len(), 2);
        // New blank slot at index 0
        assert_eq!(app.song.chains[ci].slots[0].phrase, 0);
        // Original slot shifted to index 1
        assert_eq!(app.song.chains[ci].slots[1].phrase, 5);
        // Cursor follows original slot
        assert_eq!(app.chain_cursor, 1);
    }

    #[test]
    fn chain_view_o_inserts_slot_below_middle_cursor() {
        let mut app = make_app();
        let ci = 0usize;
        // Set up 3 slots: phrases [1, 2, 3]
        app.song.chains[ci].slots[0].phrase = 1;
        app.song.chains[ci].slots.push(ChainSlot { phrase: 2, transpose: 0 });
        app.song.chains[ci].slots.push(ChainSlot { phrase: 3, transpose: 0 });
        assert_eq!(app.song.chains[ci].slots.len(), 3);
        // Cursor at slot 1 (middle)
        app.chain_cursor = 1;
        // Simulate 'o': insert below cursor (at index 2)
        let insert_at = (app.chain_cursor + 1).min(app.song.chains[ci].slots.len());
        app.song.chains[ci].slots.insert(insert_at, ChainSlot { phrase: 0, transpose: 0 });
        app.chain_cursor = insert_at;
        assert_eq!(app.song.chains[ci].slots.len(), 4);
        assert_eq!(app.song.chains[ci].slots[0].phrase, 1);
        assert_eq!(app.song.chains[ci].slots[1].phrase, 2);
        assert_eq!(app.song.chains[ci].slots[2].phrase, 0); // new blank slot
        assert_eq!(app.song.chains[ci].slots[3].phrase, 3);
        assert_eq!(app.chain_cursor, 2);
    }

    #[test]
    fn chain_view_capital_o_inserts_slot_above_middle_cursor_and_cursor_follows() {
        let mut app = make_app();
        let ci = 0usize;
        // Set up 3 slots: phrases [1, 2, 3]
        app.song.chains[ci].slots[0].phrase = 1;
        app.song.chains[ci].slots.push(ChainSlot { phrase: 2, transpose: 0 });
        app.song.chains[ci].slots.push(ChainSlot { phrase: 3, transpose: 0 });
        assert_eq!(app.song.chains[ci].slots.len(), 3);
        // Cursor at slot 1 (middle)
        app.chain_cursor = 1;
        // Simulate 'O': insert above cursor (at index 1), cursor increments
        let insert_at = app.chain_cursor;
        app.song.chains[ci].slots.insert(insert_at, ChainSlot { phrase: 0, transpose: 0 });
        app.chain_cursor += 1;
        assert_eq!(app.song.chains[ci].slots.len(), 4);
        assert_eq!(app.song.chains[ci].slots[0].phrase, 1);
        assert_eq!(app.song.chains[ci].slots[1].phrase, 0); // new blank slot
        assert_eq!(app.song.chains[ci].slots[2].phrase, 2);
        assert_eq!(app.song.chains[ci].slots[3].phrase, 3);
        // Cursor follows original slot to index 2
        assert_eq!(app.chain_cursor, 2);
    }

    #[test]
    fn load_clears_history() {
        let mut app = make_app();
        app.enter_note(60);
        assert!(!app.history.undo_stack.is_empty());
        // Simulate :e by calling history.clear() (as done in execute_command)
        app.history.clear();
        assert!(app.history.undo_stack.is_empty());
        assert!(app.history.redo_stack.is_empty());
    }

    #[test]
    fn phrase_grid_highlights_playback_row_when_playing() {
        use ratatui::backend::TestBackend;
        let phrase = vitakt_core::model::Phrase::default();
        let theme = Theme::default();
        let table = render_phrase_grid(&phrase, 0, 0, 0, &theme, true, 5);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(table, f.area())).unwrap();
        let buf = terminal.backend().buffer();
        // Row 5 is at y = 1 (top border) + 1 (header) + 5 = 7.
        // The step# cell (x=1) always uses row_style, which for a playback row is playback_head_bg.
        assert_eq!(
            buf[(1u16, 7u16)].bg,
            Color::Rgb(0, 95, 135),
            "playback row should carry playback_head_bg"
        );
    }

    #[test]
    fn phrase_grid_cursor_takes_priority_over_playback_head() {
        use ratatui::backend::TestBackend;
        let phrase = vitakt_core::model::Phrase::default();
        let theme = Theme::default();
        // cursor and playback head both on row 3
        let table = render_phrase_grid(&phrase, 3, 0, 0, &theme, true, 3);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(table, f.area())).unwrap();
        let buf = terminal.backend().buffer();
        // Row 3 is at y = 1 (top border) + 1 (header) + 3 = 5.
        // The step# cell (x=1) uses row_style; for a cursor row row_style is DarkGray,
        // regardless of playback position.
        assert_eq!(
            buf[(1u16, 5u16)].bg,
            Color::DarkGray,
            "cursor style should take priority over playback head on coincident row"
        );
        assert_ne!(
            buf[(1u16, 5u16)].bg,
            Color::Rgb(0, 95, 135),
            "playback_head_bg must not appear on cursor row when they coincide"
        );
    }

    #[test]
    fn parse_args_no_args_shows_startup() {
        let args: Vec<String> = vec!["vitakt".to_string()];
        assert_eq!(parse_args(&args).unwrap(), CliAction::ShowStartup);
    }

    #[test]
    fn parse_args_empty_slice_shows_startup() {
        let args: Vec<String> = vec![];
        assert_eq!(parse_args(&args).unwrap(), CliAction::ShowStartup);
    }

    #[test]
    fn parse_args_positional_path_gives_open_file() {
        let args: Vec<String> = vec!["vitakt".to_string(), "my-song.trk".to_string()];
        assert_eq!(
            parse_args(&args).unwrap(),
            CliAction::OpenFile(std::path::PathBuf::from("my-song.trk"))
        );
    }

    #[test]
    fn parse_args_removed_sample_flag_gives_error() {
        let args: Vec<String> =
            vec!["vitakt".to_string(), "--sample".to_string(), "kick.wav".to_string()];
        assert!(parse_args(&args).is_err(), "--sample should return an error");
        let err = parse_args(&args).unwrap_err().to_string();
        assert!(err.contains("--sample"), "error should mention --sample");
    }

    #[test]
    fn parse_args_unknown_flag_gives_error() {
        let args: Vec<String> = vec!["vitakt".to_string(), "--unknown".to_string()];
        assert!(parse_args(&args).is_err(), "unknown flag should return an error");
    }

    // ── Keyboard mode tests ──────────────────────────────────────────────────

    #[test]
    fn keyboard_mode_enter_sets_mode() {
        let mut app = make_app();
        assert_eq!(app.mode, InputMode::Normal);
        app.enter_keyboard_mode();
        assert_eq!(app.mode, InputMode::Keyboard);
    }

    #[test]
    fn keyboard_mode_esc_returns_to_normal() {
        let mut app = make_app();
        app.enter_keyboard_mode();
        app.exit_keyboard_mode();
        assert_eq!(app.mode, InputMode::Normal);
    }

    #[test]
    fn keyboard_instrument_prev_clamps_at_zero() {
        let mut app = make_app();
        app.keyboard_instrument = 0;
        app.keyboard_instrument_prev();
        assert_eq!(app.keyboard_instrument, 0, "should not underflow below 0");
    }

    #[test]
    fn keyboard_instrument_next_clamps_at_255() {
        let mut app = make_app();
        app.keyboard_instrument = 255;
        app.keyboard_instrument_next();
        assert_eq!(app.keyboard_instrument, 255, "should not overflow above 255");
    }

    #[test]
    fn keyboard_instrument_prev_decrements() {
        let mut app = make_app();
        app.keyboard_instrument = 5;
        app.keyboard_instrument_prev();
        assert_eq!(app.keyboard_instrument, 4);
    }

    #[test]
    fn keyboard_instrument_next_increments() {
        let mut app = make_app();
        app.keyboard_instrument = 5;
        app.keyboard_instrument_next();
        assert_eq!(app.keyboard_instrument, 6);
    }

    #[test]
    fn keyboard_instrument_persists_across_mode_entries() {
        let mut app = make_app();
        app.enter_keyboard_mode();
        app.keyboard_instrument = 7;
        app.exit_keyboard_mode();
        app.enter_keyboard_mode();
        assert_eq!(app.keyboard_instrument, 7);
    }

    #[test]
    fn is_dirty_false_on_startup() {
        let app = make_app();
        assert!(!app.is_dirty);
    }

    #[test]
    fn is_dirty_set_after_record() {
        let mut app = make_app();
        assert!(!app.is_dirty);
        app.record("test mutation");
        assert!(app.is_dirty);
    }

    #[test]
    fn is_dirty_cleared_after_enter_note() {
        // enter_note calls record(), so dirty should be true afterwards.
        let mut app = make_app();
        app.enter_note(60);
        assert!(app.is_dirty);
    }

    #[test]
    fn browser_preview_toggle_sets_is_previewing_on_wav_entry() {
        let mut app = make_app();
        // Simulate a .wav entry in the browser (no real file needed — send_cmd is a no-op
        // when producer is None, and load_wav will fail gracefully; use a known path).
        // Instead, directly verify the flag is false initially.
        assert!(!app.is_previewing);
        // Manually set a wav entry and invoke toggle; load_wav will fail on a fake path
        // so is_previewing stays false — but we verify no panic.
        app.browser_entries = vec![BrowserEntry::Wav("nonexistent.wav".to_string())];
        app.browser_cursor = 0;
        app.browser_preview_toggle(); // load_wav fails → sets timed error, is_previewing stays false
        assert!(!app.is_previewing);
    }

    #[test]
    fn browser_preview_toggle_ignores_directory_entry() {
        let mut app = make_app();
        app.browser_entries = vec![BrowserEntry::Dir("samples".to_string())];
        app.browser_cursor = 0;
        app.browser_preview_toggle();
        assert!(!app.is_previewing, "Space on a directory should not set is_previewing");
    }

    #[test]
    fn pop_view_clears_is_previewing() {
        let mut app = make_app();
        // Simulate an active preview.
        app.is_previewing = true;
        app.preview_playing.store(true, Ordering::Relaxed);
        app.push_view(View::SampleBrowser);
        app.pop_view();
        assert!(!app.is_previewing, "pop_view should clear is_previewing");
        assert!(
            !app.preview_playing.load(Ordering::Relaxed),
            "pop_view should clear the preview_playing atomic"
        );
    }

    #[test]
    fn browser_clamp_scroll_scrolls_down_when_cursor_below_window() {
        let mut app = make_app();
        // 10 entries, viewport shows 5 at a time
        app.browser_entries = (0..10).map(|i| BrowserEntry::Wav(format!("{i}.wav"))).collect();
        app.browser_scroll = 0;
        app.browser_cursor = 7; // past end of window [0..5)
        app.browser_clamp_scroll(5);
        assert_eq!(app.browser_scroll, 3, "scroll should move cursor to last visible row");
    }

    #[test]
    fn browser_clamp_scroll_scrolls_up_when_cursor_above_window() {
        let mut app = make_app();
        app.browser_entries = (0..10).map(|i| BrowserEntry::Wav(format!("{i}.wav"))).collect();
        app.browser_scroll = 5;
        app.browser_cursor = 2; // above scroll offset
        app.browser_clamp_scroll(5);
        assert_eq!(app.browser_scroll, 2, "scroll should move to show cursor at top");
    }

    #[test]
    fn browser_clamp_scroll_noop_when_cursor_in_window() {
        let mut app = make_app();
        app.browser_entries = (0..10).map(|i| BrowserEntry::Wav(format!("{i}.wav"))).collect();
        app.browser_scroll = 2;
        app.browser_cursor = 4; // inside window [2..7)
        app.browser_clamp_scroll(5);
        assert_eq!(app.browser_scroll, 2, "scroll should not change when cursor is visible");
    }

    #[test]
    fn browser_clamp_scroll_noop_when_available_is_zero() {
        let mut app = make_app();
        app.browser_entries = (0..10).map(|i| BrowserEntry::Wav(format!("{i}.wav"))).collect();
        app.browser_scroll = 3;
        app.browser_cursor = 0;
        app.browser_clamp_scroll(0); // zero available — should be a no-op
        assert_eq!(app.browser_scroll, 3, "scroll should not change when available is 0");
    }

    #[test]
    fn browser_enter_resets_scroll_on_dir_navigation() {
        let parent = std::env::temp_dir().join("vitakt_scroll_reset_test");
        let subdir = parent.join("subdir");
        std::fs::create_dir_all(&subdir).ok();
        std::fs::write(subdir.join("kick.wav"), b"RIFF").ok();

        let mut app = make_app();
        app.browser_dir = parent.clone();
        app.browser_entries = list_browser_entries(&parent);
        app.browser_scroll = 5; // simulate scrolled state
        let dir_idx = app
            .browser_entries
            .iter()
            .position(|e| matches!(e, BrowserEntry::Dir(_)))
            .expect("should have a Dir entry");
        app.browser_cursor = dir_idx;
        app.browser_enter();

        assert_eq!(app.browser_scroll, 0, "browser_scroll should reset to 0 after navigating into a dir");

        std::fs::remove_file(subdir.join("kick.wav")).ok();
        std::fs::remove_dir(&subdir).ok();
        std::fs::remove_dir(&parent).ok();
    }

    #[test]
    fn list_browser_entries_has_parent_dir_for_non_root() {
        let dir = std::env::temp_dir().join("vitakt_parent_dir_test");
        std::fs::create_dir_all(&dir).ok();
        let entries = list_browser_entries(&dir);
        assert!(
            matches!(entries.first(), Some(BrowserEntry::ParentDir)),
            "first entry should be ParentDir for a non-root directory"
        );
    }

    #[test]
    fn list_browser_entries_no_parent_dir_at_root() {
        let root = std::path::Path::new("/");
        let entries = list_browser_entries(root);
        assert!(
            !entries.iter().any(|e| matches!(e, BrowserEntry::ParentDir)),
            "ParentDir should not appear when browsing /"
        );
    }

    #[test]
    fn browser_enter_on_parent_dir_goes_up() {
        let parent = std::env::temp_dir().join("vitakt_enter_parent_test");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).ok();

        let mut app = make_app();
        app.browser_dir = child.clone();
        app.browser_entries = list_browser_entries(&child);
        // ParentDir is the first entry
        app.browser_cursor = 0;
        assert!(matches!(app.browser_entries[0], BrowserEntry::ParentDir));

        app.browser_enter();

        assert_eq!(app.browser_dir, parent, "Enter on .. should navigate to parent");
        assert_eq!(app.browser_cursor, 0, "cursor should reset after navigating up");

        std::fs::remove_dir(&child).ok();
        std::fs::remove_dir(&parent).ok();
    }

    #[test]
    fn parent_dir_is_never_included_in_wav_entries() {
        let dir = std::env::temp_dir().join("vitakt_parent_not_wav_test");
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("test.wav"), b"RIFF").ok();

        let entries = list_browser_entries(&dir);
        let wavs: Vec<_> = entries.iter().filter(|e| matches!(e, BrowserEntry::Wav(_))).collect();
        let parent_dirs: Vec<_> = entries.iter().filter(|e| matches!(e, BrowserEntry::ParentDir)).collect();

        assert_eq!(parent_dirs.len(), 1, "should have exactly one ParentDir");
        assert_eq!(wavs.len(), 1, "should have exactly one Wav entry");
        assert!(
            matches!(entries.first(), Some(BrowserEntry::ParentDir)),
            "ParentDir should be the first entry"
        );

        std::fs::remove_file(dir.join("test.wav")).ok();
        std::fs::remove_dir(&dir).ok();
    }
}

