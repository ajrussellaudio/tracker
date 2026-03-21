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
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tracker_core::{
    audio::{Command, Mixer, Voice},
    model::{Song, Step, STEPS_PER_PHRASE},
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

enum InputMode {
    Normal,
    Insert,
    Command,
}

struct App {
    song: Song,
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
}

impl App {
    fn new(producer: Option<rtrb::Producer<Command>>, sample_root: u8) -> Self {
        // Ensure the song has at least one phrase to edit.
        let mut song = Song::default();
        if song.phrases.is_empty() {
            song.phrases.push(tracker_core::model::Phrase::default());
        }
        Self {
            song,
            mode: InputMode::Normal,
            cursor_step: 0,
            octave: 4,
            active_instrument: 0,
            yy_pending: false,
            yanked_step: None,
            cmd_buf: String::new(),
            status: "NORMAL  |  i: insert  |  :: command  |  q: quit".to_string(),
            producer,
            sample_root,
        }
    }

    fn phrase_mut(&mut self) -> &mut tracker_core::model::Phrase {
        &mut self.song.phrases[0]
    }

    fn phrase(&self) -> &tracker_core::model::Phrase {
        &self.song.phrases[0]
    }

    /// Enter a note at the current step and advance the cursor.
    fn enter_note(&mut self, midi: u8) {
        let cursor = self.cursor_step;
        let instr = self.active_instrument as u8;
        let step = &mut self.phrase_mut().steps[cursor];
        step.note = Some(midi);
        step.instrument = Some(instr);
        step.velocity = 100;

        // Send NoteOn to audio thread.
        if let Some(prod) = &mut self.producer {
            let speed = pitch_speed(midi, self.sample_root);
            let _ = prod.push(Command::NoteOn { slot: 0, speed });
        }

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
            self.status = "NORMAL  |  i: insert  |  :: command  |  q: quit".to_string();
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

// ── Audio ─────────────────────────────────────────────────────────────────────

fn start_audio_stream(
    mut consumer: rtrb::Consumer<Command>,
    is_playing: Arc<AtomicBool>,
    sample_buf: Option<(Arc<Vec<f32>>, usize)>,
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

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            while let Ok(cmd) = consumer.pop() {
                match cmd {
                    Command::NoteOn { slot, speed } => mixer.trigger(slot as usize, speed),
                    Command::NoteOff(slot) => mixer.stop_slot(slot as usize),
                }
            }
            mixer.render(data);
            is_playing.store(mixer.any_active(), Ordering::Relaxed);
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

// ── TUI event loop ────────────────────────────────────────────────────────────

fn run_tui(
    producer: Option<rtrb::Producer<Command>>,
    sample_root: u8,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(producer, sample_root);

    loop {
        // ── Render ────────────────────────────────────────────────────────────
        terminal.draw(|frame| {
            let size = frame.area();
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(size);

            // Main area — phrase grid
            let phrase = app.phrase();
            let table = render_phrase_grid(phrase, app.cursor_step);
            frame.render_widget(table, outer[0]);

            // Status bar
            let status_text = match app.mode {
                InputMode::Normal => app.status.clone(),
                InputMode::Insert => format!(
                    "INSERT  |  Oct:{} Ins:{:02}  |  Esc: normal",
                    app.octave, app.active_instrument
                ),
                InputMode::Command => format!(":{}", app.cmd_buf),
            };
            let status = Paragraph::new(status_text)
                .style(Style::default().fg(Color::White).bg(Color::DarkGray));
            frame.render_widget(status, outer[1]);
        })?;

        // ── Input ─────────────────────────────────────────────────────────────
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match app.mode {
                    // ── Normal mode ───────────────────────────────────────────
                    InputMode::Normal => {
                        app.yy_pending = false; // reset on any unrecognised key below
                        match key.code {
                            KeyCode::Char('q') => break,
                            KeyCode::Char(':') => {
                                app.mode = InputMode::Command;
                                app.cmd_buf.clear();
                            }
                            KeyCode::Char('i') => {
                                app.mode = InputMode::Insert;
                            }
                            // Vim navigation
                            KeyCode::Char('j') | KeyCode::Down => {
                                app.cursor_step =
                                    (app.cursor_step + 1) % STEPS_PER_PHRASE;
                            }
                            KeyCode::Char('k') | KeyCode::Up => {
                                app.cursor_step =
                                    (app.cursor_step + STEPS_PER_PHRASE - 1)
                                        % STEPS_PER_PHRASE;
                            }
                            KeyCode::Char('h') | KeyCode::Left => {}
                            KeyCode::Char('l') | KeyCode::Right => {}
                            // Delete / clear step
                            KeyCode::Char('d') | KeyCode::Delete => {
                                let idx = app.cursor_step;
                                let step = &mut app.phrase_mut().steps[idx];
                                step.note = None;
                                step.instrument = None;
                                step.velocity = 0;
                                step.fx = Default::default();
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
                                    return Ok(()); // don't reset yy_pending below
                                }
                            }
                            // p — paste step
                            KeyCode::Char('p') => {
                                if let Some(s) = app.yanked_step.clone() {
                                    let idx = app.cursor_step;
                                    app.phrase_mut().steps[idx] = s;
                                }
                            }
                            _ => {}
                        }
                        // Reset yy_pending unless we just set it
                        if !matches!(key.code, KeyCode::Char('y')) {
                            app.yy_pending = false;
                        }
                    }

                    // ── Insert mode ───────────────────────────────────────────
                    InputMode::Insert => match key.code {
                        KeyCode::Esc => {
                            app.mode = InputMode::Normal;
                            app.status =
                                "NORMAL  |  i: insert  |  :: command  |  q: quit"
                                    .to_string();
                        }
                        // Octave shift
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
                        // Navigation in insert mode
                        KeyCode::Up => {
                            app.cursor_step =
                                (app.cursor_step + STEPS_PER_PHRASE - 1)
                                    % STEPS_PER_PHRASE;
                        }
                        KeyCode::Down => {
                            app.cursor_step =
                                (app.cursor_step + 1) % STEPS_PER_PHRASE;
                        }
                        // QWERTY piano keys
                        KeyCode::Char(c) => {
                            if let Some(semitone) = qwerty_to_semitone(c) {
                                // Base C for this octave: MIDI = 12 * (octave + 1)
                                let base: i32 = 12 * (app.octave as i32 + 1);
                                let midi = (base + semitone as i32).clamp(0, 127) as u8;
                                app.enter_note(midi);
                            }
                        }
                        _ => {}
                    },

                    // ── Command mode ──────────────────────────────────────────
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
    let is_playing = Arc::new(AtomicBool::new(false));

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

    let _stream = start_audio_stream(consumer, Arc::clone(&is_playing), audio_buf)
        .unwrap_or_else(|e| {
            eprintln!("Warning: could not open audio device: {e}");
            panic!("audio unavailable: {e}")
        });

    run_tui(Some(producer), sample_root)?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tracker_core::model::Song;

    fn make_app() -> App {
        App::new(None, 60)
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
}

