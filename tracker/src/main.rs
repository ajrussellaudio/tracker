use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph},
    Terminal,
};
use std::{io, time::Duration};
use tracker_core::{model::Song, storage};

// ── App state ────────────────────────────────────────────────────────────────

enum InputMode {
    Normal,
    Command,
}

struct App {
    song: Song,
    mode: InputMode,
    /// Characters typed after `:` in Command mode.
    cmd_buf: String,
    /// Message shown in the status bar (cleared on the next Normal keypress).
    status: String,
}

impl App {
    fn new() -> Self {
        Self {
            song: Song::default(),
            mode: InputMode::Normal,
            cmd_buf: String::new(),
            status: "Ready  |  q: quit  |  :: command".to_string(),
        }
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
            self.status = "Ready  |  q: quit  |  :: command".to_string();
        } else {
            self.status = format!("Unknown command: {raw}");
        }
    }
}

// ── Audio ─────────────────────────────────────────────────────────────────────

fn start_silence_stream() -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device"))?;

    let mut supported_configs_range = device.supported_output_configs()?;
    let supported_config = supported_configs_range
        .next()
        .ok_or_else(|| anyhow::anyhow!("no supported output config"))?
        .with_max_sample_rate();

    let config = cpal::StreamConfig {
        channels: 2,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: cpal::BufferSize::Default,
    };

    // Try the preferred 48 kHz / f32 stereo config; fall back to whatever the
    // device reports if the preferred config is not supported.
    let stream = device
        .build_output_stream(
            &config,
            |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                data.fill(0.0);
            },
            |err| eprintln!("audio stream error: {err}"),
            None,
        )
        .or_else(|_| {
            device.build_output_stream(
                &supported_config.into(),
                |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    data.fill(0.0);
                },
                |err| eprintln!("audio stream error: {err}"),
                None,
            )
        })?;

    stream.play()?;
    Ok(stream)
}

// ── TUI ───────────────────────────────────────────────────────────────────────

fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    loop {
        // ── Render ────────────────────────────────────────────────────────────
        terminal.draw(|frame| {
            let size = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(size);

            let main_block = Block::default()
                .title(format!("tracker — {}", app.song.name.as_str().if_empty("(untitled)")))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan));
            frame.render_widget(main_block, chunks[0]);

            let status_text = match app.mode {
                InputMode::Normal => app.status.clone(),
                InputMode::Command => format!(":{}", app.cmd_buf),
            };
            let status = Paragraph::new(status_text)
                .style(Style::default().fg(Color::White).bg(Color::DarkGray));
            frame.render_widget(status, chunks[1]);
        })?;

        // ── Input ─────────────────────────────────────────────────────────────
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match app.mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char(':') => {
                            app.mode = InputMode::Command;
                            app.cmd_buf.clear();
                        }
                        _ => {}
                    },
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
    let _stream = start_silence_stream().unwrap_or_else(|e| {
        eprintln!("Warning: could not open audio device: {e}");
        panic!("audio unavailable: {e}")
    });

    run_tui()?;
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

trait StrExt {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str;
}
impl StrExt for str {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.is_empty() { fallback } else { self }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_default_song_has_current_version() {
        let app = App::new();
        assert_eq!(app.song.version, tracker_core::CURRENT_VERSION);
    }

    #[test]
    fn execute_command_w_error_on_bad_path() {
        let mut app = App::new();
        app.mode = InputMode::Command;
        app.cmd_buf = "w /nonexistent_dir/out.trk".to_string();
        app.execute_command();
        assert!(app.status.starts_with("Error:"), "got: {}", app.status);
    }

    #[test]
    fn execute_command_e_error_on_missing_file() {
        let mut app = App::new();
        app.mode = InputMode::Command;
        app.cmd_buf = "e /nonexistent/file.trk".to_string();
        app.execute_command();
        assert!(app.status.starts_with("Error:"), "got: {}", app.status);
    }

    #[test]
    fn execute_command_roundtrip() {
        let mut app = App::new();
        app.song.name = "roundtrip test".to_string();
        app.song.bpm = 99.0;

        let path = std::env::temp_dir().join("tracker_tui_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        // Save
        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "got: {}", app.status);

        // Clobber in-memory state
        app.song = Song::default();

        // Load
        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "got: {}", app.status);

        assert_eq!(app.song.name, "roundtrip test");
        assert_eq!(app.song.bpm, 99.0);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn smoke_app_module_exists() {
        assert_eq!(2 + 2, 4);
    }
}

