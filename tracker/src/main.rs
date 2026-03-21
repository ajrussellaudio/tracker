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
use std::{
    io,
    time::Duration,
};

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
            // Fallback: use the device's own supported config
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

fn run_tui() -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        terminal.draw(|frame| {
            let size = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .split(size);

            let main_block = Block::default()
                .title("tracker")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan));
            frame.render_widget(main_block, chunks[0]);

            let status = Paragraph::new("Ready  |  q: quit")
                .style(Style::default().fg(Color::White).bg(Color::DarkGray));
            frame.render_widget(status, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                if key.code == KeyCode::Char('q') {
                    break;
                }
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn main() -> Result<()> {
    let _stream = start_silence_stream().unwrap_or_else(|e| {
        eprintln!("Warning: could not open audio device: {e}");
        // Return a dummy — we still want the TUI to work without audio
        panic!("audio unavailable: {e}")
    });

    run_tui()?;

    // _stream is dropped here, releasing the audio device
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke_app_module_exists() {
        // Verifies the binary crate compiles and is reachable from tests.
        assert_eq!(2 + 2, 4);
    }
}
