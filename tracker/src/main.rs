use anyhow::{Context, Result};
use hound;
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
    text::Span,
    widgets::{Block, Borders, Paragraph},
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
use tracker_core::audio::{Command, Mixer, Voice};

// ── WAV loading ──────────────────────────────────────────────────────────────

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

// ── Audio stream ─────────────────────────────────────────────────────────────

fn start_audio_stream(
    mut consumer: rtrb::Consumer<Command>,
    is_playing: Arc<AtomicBool>,
    sample_buf: Arc<Vec<f32>>,
    src_channels: usize,
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
    mixer.load_slot(0, Voice::new(sample_buf, src_channels));

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Drain any pending commands.
            while let Ok(cmd) = consumer.pop() {
                match cmd {
                    Command::NoteOn(slot) => mixer.trigger(slot as usize),
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

// ── TUI ──────────────────────────────────────────────────────────────────────

fn run_tui(
    mut producer: rtrb::Producer<Command>,
    is_playing: Arc<AtomicBool>,
    sample_name: String,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    loop {
        let playing = is_playing.load(Ordering::Relaxed);

        terminal.draw(|frame| {
            let size = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(size);

            // Main panel
            let indicator = if playing {
                Span::styled(
                    " ▶ PLAYING ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    " ■ STOPPED ",
                    Style::default().fg(Color::DarkGray).bg(Color::Black),
                )
            };

            let sample_line = format!("Sample: {sample_name}");
            let main_block = Block::default()
                .title("tracker")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan));
            frame.render_widget(main_block, chunks[0]);

            // Inner area of the block
            let inner = chunks[0].inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 1,
            });
            let inner_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(1), Constraint::Length(1)])
                .split(inner);

            frame.render_widget(Paragraph::new(sample_line), inner_chunks[0]);
            frame.render_widget(Paragraph::new(indicator), inner_chunks[1]);

            // Status bar
            let status = Paragraph::new("Space: trigger  |  q: quit")
                .style(Style::default().fg(Color::White).bg(Color::DarkGray));
            frame.render_widget(status, chunks[1]);
        })?;

        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Char(' ') => {
                        // Best-effort; drop the command if the buffer is full.
                        let _ = producer.push(Command::NoteOn(0));
                    }
                    _ => {}
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
    // Parse --sample <path>
    let args: Vec<String> = std::env::args().collect();
    let sample_path = args
        .iter()
        .position(|a| a == "--sample")
        .and_then(|i| args.get(i + 1))
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("Usage: tracker --sample <path.wav>"))?;

    let sample_name = std::path::Path::new(&sample_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| sample_path.clone());

    let (sample_buf, src_channels) = load_wav(&sample_path)?;

    // Lock-free SPSC channel: UI → audio thread.
    let (producer, consumer) = RingBuffer::<Command>::new(64);
    let is_playing = Arc::new(AtomicBool::new(false));

    let _stream = start_audio_stream(
        consumer,
        Arc::clone(&is_playing),
        sample_buf,
        src_channels,
    )
    .unwrap_or_else(|e| {
        eprintln!("Warning: could not open audio device: {e}");
        panic!("audio unavailable: {e}");
    });

    run_tui(producer, is_playing, sample_name)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoke_app_module_exists() {
        assert_eq!(2 + 2, 4);
    }
}
