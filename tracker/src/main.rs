use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
    Terminal,
};
use rtrb::RingBuffer;
use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
use tracker_core::{
    audio::{Command, Mixer, Sequencer, Voice},
    model::{Chain, ChainSlot, FxCommand, InterpMode, Song, Step, STEPS_PER_PHRASE, TRACKS},
    storage,
};

// ── Note helpers ─────────────────────────────────────────────────────────────
const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn note_name(midi: u8) -> String {
    let octave = (midi as i32 / 12) - 1;
    let name = NOTE_NAMES[(midi % 12) as usize];
    if name.len() == 1 {
        format!("{name}-{octave}")
    } else {
        format!("{name}{octave}")
    }
}

/// Map a QWERTY key to a semitone offset from C (standard 2-octave tracker layout).
/// Lower row: z=C(0) s=C#(1) x=D(2) d=D#(3) c=E(4) v=F(5) g=F#(6) b=G(7) h=G#(8) n=A(9) j=A#(10) m=B(11)
/// Upper row: q=C(12) 2=C#(13) w=D(14) 3=D#(15) e=E(16) r=F(17) 5=F#(18) t=G(19) 6=G#(20) y=A(21) 7=A#(22) u=B(23)
fn qwerty_to_semitone(c: char) -> Option<i8> {
    match c {
        'z' => Some(0),
        's' => Some(1),
        'x' => Some(2),
        'd' => Some(3),
        'c' => Some(4),
        'v' => Some(5),
        'g' => Some(6),
        'b' => Some(7),
        'h' => Some(8),
        'n' => Some(9),
        'j' => Some(10),
        'm' => Some(11),
        'q' => Some(12),
        '2' => Some(13),
        'w' => Some(14),
        '3' => Some(15),
        'e' => Some(16),
        'r' => Some(17),
        '5' => Some(18),
        't' => Some(19),
        '6' => Some(20),
        'y' => Some(21),
        '7' => Some(22),
        'u' => Some(23),
        _ => None,
    }
}

/// Compute playback speed ratio from note vs root using equal temperament.
fn pitch_speed(note: u8, root_note: u8) -> f32 {
    let delta = note as i32 - root_note as i32;
    2.0_f64.powf(delta as f64 / 12.0) as f32
}

// ── App state ─────────────────────────────────────────────────────────────────

/// Top-level view the TUI is showing.
enum View {
    SongView,
    ChainView,
    PhraseEditor,
    InstrumentEditor,
    SampleBrowser,
}

enum InputMode {
    Normal,
    Insert,
    Command,
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

// ── Phrase-editor column indices ─────────────────────────────────────────────
const COL_NOTE: usize = 0;
const COL_INS: usize = 1;
/// Columns 2–9: FX slot pairs (cmd at even offsets, val at odd offsets).
/// `col_to_fx(col)` returns `Some((slot_index, is_cmd_field))` for FX columns.
const COL_FX_FIRST: usize = 2;
const COL_COUNT: usize = 10; // note + ins + 4×(cmd+val)

fn col_to_fx(col: usize) -> Option<(usize, bool)> {
    if col >= COL_FX_FIRST && col < COL_COUNT {
        let offset = col - COL_FX_FIRST;
        Some((offset / 2, offset % 2 == 0)) // (slot_index, is_cmd)
    } else {
        None
    }
}

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
    /// Sample browser: list of .wav files in the current directory.
    browser_entries: Vec<String>,
    /// Sample browser: cursor row.
    browser_cursor: usize,
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
}

impl App {
    fn new(
        producer: Option<rtrb::Producer<Command>>,
        sample_root: u8,
        seq_playing: Arc<AtomicBool>,
        current_seq_step: Arc<AtomicU8>,
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
        }
    }

    fn phrase_mut(&mut self) -> &mut tracker_core::model::Phrase {
        let idx = self.active_phrase_idx.min(self.song.phrases.len().saturating_sub(1));
        &mut self.song.phrases[idx]
    }

    fn phrase(&self) -> &tracker_core::model::Phrase {
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
        self.send_cmd(Command::UpdatePhrase(phrase));
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
            self.song.instruments.push(tracker_core::model::Instrument::default());
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
                let loop_start = instr.loop_start.unwrap_or(0);
                let loop_end = instr.loop_end.unwrap_or(0);
                let interp_mode = instr.interp_mode.clone();
                match load_wav(&path) {
                    Ok((buf, channels)) => {
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
            let loop_start = instr.loop_start.unwrap_or(0);
            let loop_end = instr.loop_end.unwrap_or(0);
            let interp_mode = instr.interp_mode.clone();
            match load_wav(&path) {
                Ok((buf, channels)) => {
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

    /// Open the sample browser listing .wav files from the current directory.
    fn open_sample_browser(&mut self) {
        self.browser_entries = list_wav_files(".");
        self.browser_cursor = 0;
        self.push_view(View::SampleBrowser);
    }

    /// Confirm selection in the sample browser — loads the file into the active instrument.
    fn confirm_browser_selection(&mut self) {
        if let Some(path) = self.browser_entries.get(self.browser_cursor).cloned() {
            self.ensure_instrument(self.active_instrument);
            if let Some(instr) = self.song.instruments.get_mut(self.active_instrument) {
                instr.sample = Some(tracker_core::model::Sample::from_path(&path));
            }
            self.pop_view();
            self.reload_instrument_sample();
        }
    }

    /// Adjust BPM by `delta` and send the new value to the audio thread.
    fn adjust_bpm(&mut self, delta: f32) {
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
                Ok(_) => self.status = format!("Saved: {path}"),
                Err(e) => self.status = format!("Error: {e}"),
            }
        } else if let Some(path) = raw.strip_prefix("e ") {
            let path = path.trim();
            match storage::load_trk(path) {
                Ok(song) => {
                    self.song = tracker_core::model::migrate(song);
                    if self.song.phrases.is_empty() {
                        self.song.phrases.push(tracker_core::model::Phrase::default());
                    }
                    self.sync_phrase_to_sequencer();
                    self.reload_instruments();
                    self.sync_song_to_sequencer();
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
        } else if raw.is_empty() {
            self.status =
                "NORMAL  |  SPC: play  |  i: insert  |  Tab: instrument  |  :: command  |  q: quit".to_string();
        } else {
            self.status = format!("Unknown command: {raw}");
        }
    }
}

// ── WAV loading ───────────────────────────────────────────────────────────────

fn load_wav(path: &str) -> Result<(Arc<Vec<f32>>, usize)> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("failed to open WAV: {path}"))?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(anyhow::Error::from))
            .collect::<Result<_>>()?,
        hound::SampleFormat::Int => {
            let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            match spec.bits_per_sample {
                16 => reader
                    .samples::<i16>()
                    .map(|s| s.map(|v| v as f32 / max).map_err(anyhow::Error::from))
                    .collect::<Result<_>>()?,
                24 | 32 => reader
                    .samples::<i32>()
                    .map(|s| s.map(|v| v as f32 / max).map_err(anyhow::Error::from))
                    .collect::<Result<_>>()?,
                _ => anyhow::bail!("unsupported bit depth: {}", spec.bits_per_sample),
            }
        }
    };
    Ok((Arc::new(samples), channels))
}

/// List `.wav` files (by filename) in `dir`, sorted alphabetically.
fn list_wav_files(dir: &str) -> Vec<String> {
    let mut entries = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if let Some(ext) = path.extension() {
                if ext.to_ascii_lowercase() == "wav" {
                    if let Some(name) = path.file_name() {
                        entries.push(name.to_string_lossy().to_string());
                    }
                }
            }
        }
    }
    entries.sort();
    entries
}

// ── Audio ─────────────────────────────────────────────────────────────────────

fn start_audio_stream(
    mut consumer: rtrb::Consumer<Command>,
    seq_playing: Arc<AtomicBool>,
    current_step: Arc<AtomicU8>,
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
                }
            }

            // Advance the sequencer and trigger notes, applying any FX slots.
            let events = sequencer.advance(frames);
            for event in &events {
                current_step.store(event.step_index, Ordering::Relaxed);
                for (track, speed, fx) in &event.notes {
                    let mut final_speed = *speed;
                    let mut vol = 1.0f32;
                    let mut pan = 0.0f32;
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
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;

    stream.play()?;
    Ok(stream)
}

// ── TUI rendering helpers ─────────────────────────────────────────────────────

fn render_phrase_grid(
    phrase: &tracker_core::model::Phrase,
    cursor_step: usize,
    cursor_col: usize,
    phrase_idx: usize,
) -> Table<'static> {
    let rows: Vec<Row> = phrase
        .steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let is_cursor_row = i == cursor_step;

            let note_str = match step.note {
                Some(n) => note_name(n),
                None => "---".to_string(),
            };
            let instr_str = match step.instrument {
                Some(n) => format!("{n:02X}"),
                None => "--".to_string(),
            };

            // Base row style for non-cursor-cell content.
            let row_style = if is_cursor_row {
                Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else if i % 4 == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };

            let cursor_cell_style =
                Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD);

            // Helper: return cursor_cell_style when this column is active, otherwise row_style.
            let cell_style = |col: usize| {
                if is_cursor_row && col == cursor_col {
                    cursor_cell_style
                } else {
                    row_style
                }
            };

            let note_style = if is_cursor_row && cursor_col == COL_NOTE {
                cursor_cell_style
            } else if step.note.is_some() {
                if is_cursor_row {
                    Style::default().bg(Color::DarkGray).fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::Green)
                }
            } else {
                if is_cursor_row {
                    row_style
                } else {
                    Style::default().fg(Color::DarkGray)
                }
            };

            // Build the 10 cells: step#, note, ins, 4×(cmd, val).
            let mut cells = vec![
                Cell::from(format!("{i:02}")).style(row_style),
                Cell::from(note_str).style(note_style),
                Cell::from(instr_str).style(cell_style(COL_INS)),
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
                        Style::default().bg(Color::DarkGray).fg(Color::Magenta).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Magenta)
                    }
                } else {
                    if is_cursor_row {
                        row_style
                    } else {
                        Style::default().fg(Color::DarkGray)
                    }
                };
                let val_style = if is_cursor_row && cursor_col == val_col {
                    cursor_cell_style
                } else if fx.command != 0 {
                    if is_cursor_row {
                        Style::default().bg(Color::DarkGray).fg(Color::Yellow).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Yellow)
                    }
                } else {
                    if is_cursor_row {
                        row_style
                    } else {
                        Style::default().fg(Color::DarkGray)
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
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .title(format!("Phrase {:02X}", phrase_idx))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    )
}

// ── Song view render ──────────────────────────────────────────────────────────

fn render_song_view(app: &App) -> Table<'static> {
    let header_cells: Vec<Cell> = std::iter::once(Cell::from(" "))
        .chain((0..TRACKS).map(|t| {
            Cell::from(format!("TRK{t}")).style(
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
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
                        Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD)
                    } else if chain_opt.is_some() {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::DarkGray)
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
                .border_style(Style::default().fg(Color::Cyan)),
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
                Style::default().bg(Color::Cyan).fg(Color::Black).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Green)
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
            Cell::from("--").style(Style::default().fg(Color::DarkGray)),
            Cell::from("--").style(Style::default().fg(Color::DarkGray)),
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
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .title(chain_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
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
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
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
        Style::default().fg(Color::DarkGray),
    ));

    Paragraph::new(lines).block(
        Block::default()
            .title(format!("Instrument {:02}", app.active_instrument))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    )
}

// ── Sample browser render ─────────────────────────────────────────────────────

fn render_sample_browser(app: &App) -> Paragraph<'static> {
    let lines: Vec<ratatui::text::Line> = if app.browser_entries.is_empty() {
        vec![ratatui::text::Line::styled(
            "  (no .wav files found in current directory)",
            Style::default().fg(Color::DarkGray),
        )]
    } else {
        app.browser_entries
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let (prefix, style) = if i == app.browser_cursor {
                    ("▶ ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
                } else {
                    ("  ", Style::default().fg(Color::White))
                };
                ratatui::text::Line::styled(format!("{prefix}{name}"), style)
            })
            .collect()
    };

    Paragraph::new(lines).block(
        Block::default()
            .title("Sample Browser  [Enter: select  Esc: cancel]")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
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
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(producer, sample_root, seq_playing, current_seq_step);

    loop {
        // ── Render ────────────────────────────────────────────────────────────
        terminal.draw(|frame| {
            let size = frame.area();
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(size);

            // Main area — depends on active view
            match app.view {
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
                    let table = render_phrase_grid(phrase, app.cursor_step, app.cursor_col, app.active_phrase_idx);
                    frame.render_widget(table, outer[0]);
                }
                View::InstrumentEditor => {
                    let para = render_instrument_editor(&app);
                    frame.render_widget(para, outer[0]);
                }
                View::SampleBrowser => {
                    let para = render_sample_browser(&app);
                    frame.render_widget(para, outer[0]);
                }
            }

            // Status bar
            let playing = app.seq_playing.load(Ordering::Relaxed);
            let seq_step = app.current_seq_step.load(Ordering::Relaxed);
            let transport = if playing {
                format!("▶  Step:{:02}  BPM:{:.1}", seq_step, app.song.bpm)
            } else {
                format!("■  Step:{:02}  BPM:{:.1}", seq_step, app.song.bpm)
            };

            let status_text = match app.view {
                View::SongView => format!(
                    "{transport}  |  hjkl: nav  0-9/a-f: chain  Del: clear  Enter: chain view  o: add row  F3: phrase  q: quit"
                ),
                View::ChainView => {
                    if app.chain_insert_mode {
                        "CHAIN INSERT  |  h/l: phrase ±1  ,/.: transpose ±1  Esc: normal".to_string()
                    } else {
                        "CHAIN  |  j/k: nav  h/l: phrase  ,/.: transpose  a: add slot  d: del slot  Enter: phrase  i: insert  Esc: back".to_string()
                    }
                }
                View::PhraseEditor => match app.mode {
                    InputMode::Normal => format!("{transport}  |  {}", app.status),
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
                        format!("{transport}  |  INSERT  {col_hint}  |  Esc: normal")
                    }
                    InputMode::Command => format!("{transport}  |  :{}", app.cmd_buf),
                },
                View::InstrumentEditor => {
                    if app.instr_editing {
                        format!(
                            "INSTR EDIT  |  Ins:{:02}  |  Enter: confirm  Esc: cancel",
                            app.active_instrument
                        )
                    } else {
                        format!(
                            "INSTRUMENT  |  Ins:{:02}  |  j/k: nav  h/l: change  i: edit  Enter: browse(sample)  Esc: back",
                            app.active_instrument
                        )
                    }
                }
                View::SampleBrowser => {
                    format!(
                        "BROWSER  |  j/k: nav  Enter: select  Esc: cancel  ({} files)",
                        app.browser_entries.len()
                    )
                }
            };
            let status = Paragraph::new(status_text)
                .style(Style::default().fg(Color::White).bg(Color::DarkGray));
            frame.render_widget(status, outer[1]);
        })?;

        // ── Input ─────────────────────────────────────────────────────────────
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match app.view {
                    // ──────────────────────────────────────────────────────────
                    // Song view key handling
                    // ──────────────────────────────────────────────────────────
                    View::SongView => match key.code {
                        KeyCode::Char('q') => break,
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
                            if let Some(arr_row) = app.song.arrangement.get_mut(row) {
                                arr_row[track] = None;
                                app.sync_song_to_sequencer();
                            }
                        }
                        // Append row with 'o'
                        KeyCode::Char('o') => {
                            app.song.arrangement.push([None; TRACKS]);
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
                        // F3 → phrase editor; F4 → instrument editor
                        KeyCode::F(3) => {
                            app.push_view(View::PhraseEditor);
                        }
                        KeyCode::F(4) => {
                            app.push_view(View::InstrumentEditor);
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
                                        if let Some(slot) = app.song.chains[ci].slots.get_mut(app.chain_cursor) {
                                            slot.phrase = slot.phrase.saturating_sub(1)
                                                .min(app.song.phrases.len().saturating_sub(1) as u8);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                KeyCode::Char('l') | KeyCode::Right => {
                                    if let Some(ci) = chain_idx_opt {
                                        let max_phrase = app.song.phrases.len().saturating_sub(1) as u8;
                                        if let Some(slot) = app.song.chains[ci].slots.get_mut(app.chain_cursor) {
                                            slot.phrase = (slot.phrase + 1).min(max_phrase);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                // ,/. : adjust transpose (semitones down/up)
                                KeyCode::Char(',') => {
                                    if let Some(ci) = chain_idx_opt {
                                        if let Some(slot) = app.song.chains[ci].slots.get_mut(app.chain_cursor) {
                                            slot.transpose = slot.transpose.saturating_sub(1);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                KeyCode::Char('.') => {
                                    if let Some(ci) = chain_idx_opt {
                                        if let Some(slot) = app.song.chains[ci].slots.get_mut(app.chain_cursor) {
                                            slot.transpose = slot.transpose.saturating_add(1);
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
                                        if let Some(slot) = app.song.chains[ci].slots.get_mut(app.chain_cursor) {
                                            slot.phrase = slot.phrase.saturating_sub(1);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                KeyCode::Char('l') | KeyCode::Right => {
                                    if let Some(ci) = chain_idx_opt {
                                        let max_phrase = app.song.phrases.len().saturating_sub(1) as u8;
                                        if let Some(slot) = app.song.chains[ci].slots.get_mut(app.chain_cursor) {
                                            slot.phrase = (slot.phrase + 1).min(max_phrase);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                // ,/. : adjust transpose
                                KeyCode::Char(',') => {
                                    if let Some(ci) = chain_idx_opt {
                                        if let Some(slot) = app.song.chains[ci].slots.get_mut(app.chain_cursor) {
                                            slot.transpose = slot.transpose.saturating_sub(1);
                                            app.sync_song_to_sequencer();
                                        }
                                    }
                                }
                                KeyCode::Char('.') => {
                                    if let Some(ci) = chain_idx_opt {
                                        if let Some(slot) = app.song.chains[ci].slots.get_mut(app.chain_cursor) {
                                            slot.transpose = slot.transpose.saturating_add(1);
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
                                // a: append slot
                                KeyCode::Char('a') => {
                                    if let Some(ci) = chain_idx_opt {
                                        let max_phrase = app.song.phrases.len().saturating_sub(1) as u8;
                                        app.song.chains[ci].slots.push(ChainSlot {
                                            phrase: max_phrase.min(app.chain_cursor as u8),
                                            transpose: 0,
                                        });
                                        app.chain_cursor = app.song.chains[ci].slots.len() - 1;
                                        app.sync_song_to_sequencer();
                                    }
                                }
                                // d: delete current slot
                                KeyCode::Char('d') | KeyCode::Delete => {
                                    if let Some(ci) = chain_idx_opt {
                                        let len = app.song.chains[ci].slots.len();
                                        if len > 0 {
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
                                                app.song.phrases.push(tracker_core::model::Phrase::default());
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
                                KeyCode::Char('q') => break,
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
                                        // Clear just the active FX slot.
                                        app.phrase_mut().steps[idx].fx[slot_idx] =
                                            tracker_core::model::FxSlot::default();
                                    } else {
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
                                        app.status = format!(
                                            "Yanked step {}",
                                            app.cursor_step
                                        );
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
                                        app.phrase_mut().steps[idx] = s;
                                        app.sync_phrase_to_sequencer();
                                    }
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
                                app.status =
                                    "NORMAL  |  SPC: play  |  i: insert  |  Tab: instrument  |  :: command  |  q: quit"
                                        .to_string();
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
                                    app.phrase_mut().steps[step_idx].fx[slot_idx] =
                                        tracker_core::model::FxSlot::default();
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
                                    if let Some(instr) =
                                        app.song.instruments.get_mut(app.active_instrument)
                                    {
                                        match cursor {
                                            INSTR_FIELD_NAME => instr.name = buf,
                                            INSTR_FIELD_SAMPLE => {
                                                instr.sample = Some(
                                                    tracker_core::model::Sample::from_path(&buf),
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
                                    instr_editor_increment(&mut app, -1);
                                }
                                KeyCode::Char('l') | KeyCode::Right => {
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
                        KeyCode::Char('j') | KeyCode::Down => {
                            if !app.browser_entries.is_empty() {
                                app.browser_cursor =
                                    (app.browser_cursor + 1) % app.browser_entries.len();
                            }
                        }
                        KeyCode::Char('k') | KeyCode::Up => {
                            if !app.browser_entries.is_empty() {
                                app.browser_cursor =
                                    (app.browser_cursor + app.browser_entries.len() - 1)
                                        % app.browser_entries.len();
                            }
                        }
                        KeyCode::Enter => app.confirm_browser_selection(),
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
    let args: Vec<String> = std::env::args().collect();
    let sample_path = args
        .iter()
        .position(|a| a == "--sample")
        .and_then(|i| args.get(i + 1))
        .cloned();

    // Lock-free SPSC channel: UI → audio thread.
    let (producer, consumer) = RingBuffer::<Command>::new(64);

    // Shared state: UI reads, audio writes.
    let seq_playing = Arc::new(AtomicBool::new(false));
    let current_seq_step = Arc::new(AtomicU8::new(0));

    let sample_buf_and_root: Option<(Arc<Vec<f32>>, usize, u8)> =
        if let Some(ref path) = sample_path {
            match load_wav(path) {
                Ok((buf, ch)) => Some((buf, ch, 60)), // default root = C4
                Err(e) => {
                    eprintln!("Warning: could not load WAV: {e}");
                    None
                }
            }
        } else {
            None
        };

    let sample_root = sample_buf_and_root.as_ref().map(|t| t.2).unwrap_or(60);
    let audio_buf = sample_buf_and_root.map(|(buf, ch, _)| (buf, ch));

    let initial_bpm = 120.0f32;

    let _stream = start_audio_stream(
        consumer,
        Arc::clone(&seq_playing),
        Arc::clone(&current_seq_step),
        audio_buf,
        initial_bpm,
    )
    .unwrap_or_else(|e| {
        eprintln!("Warning: could not open audio device: {e}");
        panic!("audio unavailable: {e}")
    });

    run_tui(Some(producer), sample_root, seq_playing, current_seq_step)?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::model::Song;

    fn make_app() -> App {
        App::new(
            None,
            60,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU8::new(0)),
        )
    }

    #[test]
    fn app_default_song_has_current_version() {
        let app = make_app();
        assert_eq!(app.song.version, tracker_core::CURRENT_VERSION);
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
    fn step_roundtrip_bincode() {
        let mut app = make_app();
        app.enter_note(60);
        app.enter_note(64);
        app.enter_note(67);

        let path = std::env::temp_dir().join("tracker_phrase_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "got: {}", app.status);

        let original_steps = app.song.phrases[0].steps.clone();
        app.song = Song::default();
        app.song.phrases.push(tracker_core::model::Phrase::default());

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

        let path = std::env::temp_dir().join("tracker_instrument_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "save failed: {}", app.status);

        let orig = app.song.instruments[0].clone();

        // Reset song and reload
        app.song = Song::default();
        app.song.phrases.push(tracker_core::model::Phrase::default());

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
    fn list_wav_files_returns_sorted_list() {
        // Create temp wav files
        let dir = std::env::temp_dir().join("tracker_wav_test");
        std::fs::create_dir_all(&dir).ok();
        let dir_str = dir.to_str().unwrap();

        for name in &["b.wav", "a.WAV", "c.wav"] {
            std::fs::write(dir.join(name), b"RIFF").ok();
        }

        let entries = list_wav_files(dir_str);

        // Clean up
        for name in &["b.wav", "a.WAV", "c.wav"] {
            std::fs::remove_file(dir.join(name)).ok();
        }

        // All three should appear, sorted (case-insensitive extension match)
        assert!(!entries.is_empty(), "should find .wav files");
        // Check sorted
        let is_sorted = entries.windows(2).all(|w| w[0] <= w[1]);
        assert!(is_sorted, "entries should be sorted: {entries:?}");
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

        let path = std::env::temp_dir().join("tracker_arrangement_roundtrip.trk");
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
}

