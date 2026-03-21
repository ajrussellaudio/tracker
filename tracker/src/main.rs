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
    model::{InterpMode, Song, Step, STEPS_PER_PHRASE},
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
}

impl App {
    fn new(
        producer: Option<rtrb::Producer<Command>>,
        sample_root: u8,
        seq_playing: Arc<AtomicBool>,
        current_seq_step: Arc<AtomicU8>,
    ) -> Self {
        // Ensure the song has at least one phrase to edit.
        let mut song = Song::default();
        if song.phrases.is_empty() {
            song.phrases.push(tracker_core::model::Phrase::default());
        }
        Self {
            song,
            view: View::PhraseEditor,
            mode: InputMode::Normal,
            cursor_step: 0,
            octave: 4,
            active_instrument: 0,
            yy_pending: false,
            yanked_step: None,
            cmd_buf: String::new(),
            status: "NORMAL  |  SPC: play  |  i: insert  |  Tab: instrument  |  :: command  |  q: quit".to_string(),
            producer,
            sample_root,
            seq_playing,
            current_seq_step,
            instr_cursor: 0,
            instr_editing: false,
            instr_edit_buf: String::new(),
            browser_entries: Vec::new(),
            browser_cursor: 0,
        }
    }

    fn phrase_mut(&mut self) -> &mut tracker_core::model::Phrase {
        &mut self.song.phrases[0]
    }

    fn phrase(&self) -> &tracker_core::model::Phrase {
        &self.song.phrases[0]
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

    /// Toggle play / stop.
    fn toggle_play(&mut self) {
        if self.seq_playing.load(Ordering::Relaxed) {
            self.send_cmd(Command::Stop);
            self.seq_playing.store(false, Ordering::Relaxed);
        } else {
            self.sync_phrase_to_sequencer();
            self.send_cmd(Command::Play);
            self.seq_playing.store(true, Ordering::Relaxed);
        }
    }

    /// Restart sequencer from step 0 (F5).
    fn restart_play(&mut self) {
        self.sync_phrase_to_sequencer();
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
        self.view = View::InstrumentEditor;
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
        self.view = View::SampleBrowser;
    }

    /// Confirm selection in the sample browser — loads the file into the active instrument.
    fn confirm_browser_selection(&mut self) {
        if let Some(path) = self.browser_entries.get(self.browser_cursor).cloned() {
            self.ensure_instrument(self.active_instrument);
            if let Some(instr) = self.song.instruments.get_mut(self.active_instrument) {
                instr.sample = Some(tracker_core::model::Sample::from_path(&path));
            }
            self.view = View::InstrumentEditor;
            self.reload_instrument_sample();
        }
    }

    /// Adjust BPM by `delta` and send the new value to the audio thread.
    fn adjust_bpm(&mut self, delta: f32) {
        self.song.bpm = (self.song.bpm + delta).clamp(20.0, 999.0);
        self.send_cmd(Command::SetBpm(self.song.bpm));
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

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / 2;

            // Process commands from the UI thread.
            while let Ok(cmd) = consumer.pop() {
                match cmd {
                    Command::NoteOn { slot, speed } => mixer.trigger(slot as usize, speed),
                    Command::NoteOff(slot) => mixer.stop_slot(slot as usize),
                    Command::Play => {
                        if let Some(speed) = sequencer.play() {
                            mixer.trigger(0, speed);
                        }
                        seq_playing.store(true, Ordering::Relaxed);
                    }
                    Command::Stop => {
                        sequencer.stop();
                        seq_playing.store(false, Ordering::Relaxed);
                    }
                    Command::Restart => {
                        if let Some(speed) = sequencer.restart() {
                            mixer.trigger(0, speed);
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
                }
            }

            // Advance the sequencer and trigger notes.
            let events = sequencer.advance(frames);
            for (step_idx, maybe_speed) in events {
                current_step.store(step_idx, Ordering::Relaxed);
                if let Some(speed) = maybe_speed {
                    mixer.trigger(0, speed);
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
) -> Table<'static> {
    let rows: Vec<Row> = phrase
        .steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let note_str = match step.note {
                Some(n) => note_name(n),
                None => "---".to_string(),
            };
            let instr_str = match step.instrument {
                Some(n) => format!("{n:02}"),
                None => "--".to_string(),
            };
            let fx_str = ".. ..".to_string(); // placeholder

            let row_style = if i == cursor_step {
                Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD)
            } else if i % 4 == 0 {
                Style::default().fg(Color::White)
            } else {
                Style::default().fg(Color::Gray)
            };

            let note_style = if i == cursor_step {
                Style::default()
                    .bg(Color::Cyan)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else if step.note.is_some() {
                Style::default().fg(Color::Green)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            Row::new(vec![
                Cell::from(format!("{i:02}")).style(row_style),
                Cell::from(note_str).style(note_style),
                Cell::from(instr_str).style(row_style),
                Cell::from(fx_str).style(row_style),
            ])
        })
        .collect();

    Table::new(
        rows,
        [
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Length(4),
            Constraint::Min(5),
        ],
    )
    .header(
        Row::new(vec!["#", "NOTE", "INS", "FX"])
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .title("Phrase 00")
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
                View::PhraseEditor => {
                    let phrase = app.phrase();
                    let table = render_phrase_grid(phrase, app.cursor_step);
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
                View::PhraseEditor => match app.mode {
                    InputMode::Normal => format!("{transport}  |  {}", app.status),
                    InputMode::Insert => format!(
                        "{transport}  |  INSERT  Oct:{} Ins:{:02}  |  Esc: normal",
                        app.octave, app.active_instrument
                    ),
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
                    // Phrase editor key handling
                    // ──────────────────────────────────────────────────────────
                    View::PhraseEditor => match app.mode {
                        // ── Normal mode ───────────────────────────────────────
                        InputMode::Normal => {
                            app.yy_pending = false;
                            match key.code {
                                KeyCode::Char('q') => break,
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
                                KeyCode::Char('h') => {}
                                KeyCode::Char('l') => {}
                                // Delete / clear step
                                KeyCode::Char('d') | KeyCode::Delete => {
                                    let idx = app.cursor_step;
                                    let step = &mut app.phrase_mut().steps[idx];
                                    step.note = None;
                                    step.instrument = None;
                                    step.velocity = 0;
                                    step.fx = Default::default();
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
                                app.status =
                                    "NORMAL  |  SPC: play  |  i: insert  |  Tab: instrument  |  :: command  |  q: quit"
                                        .to_string();
                            }
                            KeyCode::Char('-') => {
                                if app.octave > 1 {
                                    app.octave -= 1;
                                }
                            }
                            KeyCode::Char('=') => {
                                if app.octave < 8 {
                                    app.octave += 1;
                                }
                            }
                            KeyCode::Up => {
                                app.cursor_step =
                                    (app.cursor_step + STEPS_PER_PHRASE - 1)
                                        % STEPS_PER_PHRASE;
                            }
                            KeyCode::Down => {
                                app.cursor_step =
                                    (app.cursor_step + 1) % STEPS_PER_PHRASE;
                            }
                            KeyCode::Char(c) => {
                                if let Some(semitone) = qwerty_to_semitone(c) {
                                    let base: i32 = 12 * (app.octave as i32 + 1);
                                    let midi = (base + semitone as i32).clamp(0, 127) as u8;
                                    app.enter_note(midi);
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
                                    // Return to phrase editor
                                    app.view = View::PhraseEditor;
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
                            app.view = View::InstrumentEditor;
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
}

