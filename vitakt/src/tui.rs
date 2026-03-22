use crate::app::{App, InputMode, View};
use crate::cli::CliAction;
use crate::input;
use crate::note_utils::{col_to_fx, COL_NOTE};
use crate::render::{
    render_chain_view, render_instrument_editor, render_mixer_view, render_phrase_grid,
    render_sample_browser, render_song_view, render_startup_screen, render_waveform_editor,
};
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
    Terminal,
};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};
use vitakt_core::{audio::Command, model, storage};

pub fn run_tui(
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
                    app.song = model::migrate(song);
                    if app.song.phrases.is_empty() {
                        app.song.phrases.push(model::Phrase::default());
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
        app.poll_render();

        if let Some(timer) = app.status_timer {
            if timer.elapsed() >= Duration::from_secs(2) {
                app.status_timer = None;
            }
        }

        if app.needs_terminal_clear {
            app.needs_terminal_clear = false;
            terminal.clear()?;
        }

        terminal.draw(|frame| {
            let size = frame.area();
            let outer = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(size);

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
                    let playback_step =
                        app.current_seq_step.load(Ordering::Relaxed) as usize;
                    let table = render_phrase_grid(
                        phrase,
                        app.cursor_step,
                        app.cursor_col,
                        app.active_phrase_idx,
                        &app.theme,
                        seq_playing,
                        playback_step,
                    );
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
                View::WaveformEditor => {
                    let w = outer[0].width.saturating_sub(2) as usize;
                    let h = outer[0].height.saturating_sub(2) as usize;
                    let lines = render_waveform_editor(&app, w, h);
                    let para = ratatui::widgets::Paragraph::new(lines).block(
                        ratatui::widgets::Block::default()
                            .title(" Waveform Editor  [Esc: back] ")
                            .borders(ratatui::widgets::Borders::ALL)
                            .border_style(
                                ratatui::style::Style::default().fg(app.theme.screen_title),
                            ),
                    );
                    frame.render_widget(para, outer[0]);
                }
            }

            // Quit confirmation modal overlay
            if matches!(app.mode, InputMode::ConfirmQuit) {
                let modal_width = 46u16;
                let modal_height = 4u16;
                let x = size.width.saturating_sub(modal_width) / 2;
                let y = size.height.saturating_sub(modal_height) / 2;
                let modal_area = Rect::new(
                    x,
                    y,
                    modal_width.min(size.width),
                    modal_height.min(size.height),
                );
                frame.render_widget(Clear, modal_area);
                let modal = Paragraph::new("Unsaved changes. Quit? (y/n)")
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Confirm Quit "),
                    )
                    .style(Style::default().fg(Color::Yellow).bg(Color::DarkGray));
                frame.render_widget(modal, modal_area);
            }

            // Bookmark overlay (shown over the sample browser)
            if app.browser_show_bookmarks {
                let valid_bookmarks: Vec<String> = app
                    .config
                    .bookmarks
                    .iter()
                    .filter(|p| std::path::Path::new(p.as_str()).is_dir())
                    .cloned()
                    .collect();
                let modal_width = (size.width * 2 / 3).max(40).min(size.width);
                let modal_height = (valid_bookmarks.len() as u16 + 4).min(size.height);
                let x = size.width.saturating_sub(modal_width) / 2;
                let y = size.height.saturating_sub(modal_height) / 2;
                let modal_area = Rect::new(x, y, modal_width, modal_height);
                frame.render_widget(Clear, modal_area);
                let mut bm_lines: Vec<ratatui::text::Line<'static>> = Vec::new();
                for (i, path) in valid_bookmarks.iter().enumerate() {
                    let (prefix, style) = if i == app.browser_bookmark_cursor {
                        (
                            "▶ ",
                            Style::default()
                                .fg(app.theme.cursor_bg)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        ("  ", Style::default().fg(Color::White))
                    };
                    bm_lines.push(ratatui::text::Line::styled(
                        format!("{prefix}{path}"),
                        style,
                    ));
                }
                let bm_para = Paragraph::new(bm_lines)
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Bookmarks  [Enter: go  Esc: cancel] ")
                            .border_style(Style::default().fg(app.theme.screen_title)),
                    )
                    .style(Style::default().bg(Color::DarkGray));
                frame.render_widget(bm_para, modal_area);
            }

            // Status bar
            let playing = app.seq_playing.load(Ordering::Relaxed);
            let seq_step = app.current_seq_step.load(Ordering::Relaxed);
            let transport = if playing {
                format!("Step:{:02}  BPM:{:.1}", seq_step, app.song.bpm)
            } else {
                format!("Step:{:02}  BPM:{:.1}", seq_step, app.song.bpm)
            };

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

            let insert_active = mode_label == "INSERT";
            let keyboard_active = mode_label == "KEYBOARD";

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
                            format!(
                                "{mode_label}  |  h/l: phrase ±1  ,/.: transpose ±1  Esc: normal"
                            )
                        } else {
                            format!(
                                "{mode_label}  |  j/k: nav  h/l: phrase  ,/.: transpose  o: add slot below  O: add slot above  d: del slot  Enter: phrase  i: insert  Esc: back"
                            )
                        }
                    }
                    View::PhraseEditor => match app.mode {
                        InputMode::Normal => format!(
                            "{mode_label}  |  {transport}  |  SPC: play  i: insert  Tab: instrument  ←/→: BPM  :: command  q: quit"
                        ),
                        InputMode::Insert => {
                            let col_hint = match col_to_fx(app.cursor_col) {
                                Some((s, true)) => {
                                    let buf = &app.fx_edit_buf;
                                    format!(
                                        "FX{} CMD: [{buf:<3}]  type 3-letter code (VOL/PAN/PIT/RET)",
                                        s + 1
                                    )
                                }
                                Some((s, false)) => {
                                    let buf = &app.fx_edit_buf;
                                    format!(
                                        "FX{} VAL: [{buf:<3}]  type 0-255, Enter to confirm",
                                        s + 1
                                    )
                                }
                                None if app.cursor_col == COL_NOTE => {
                                    format!(
                                        "NOTE  Oct:{} Ins:{:02}  QWERTY piano",
                                        app.octave, app.active_instrument
                                    )
                                }
                                None => format!(
                                    "Col:{} Ins:{:02}",
                                    app.cursor_col, app.active_instrument
                                ),
                            };
                            format!("{mode_label}  |  {transport}  |  {col_hint}  |  Esc: normal")
                        }
                        InputMode::Command => {
                            format!("{mode_label}  |  {transport}  |  :{}", app.cmd_buf)
                        }
                        InputMode::Keyboard => {
                            unreachable!("Keyboard mode status handled before view match")
                        }
                        InputMode::ConfirmQuit => {
                            unreachable!("ConfirmQuit status handled before view match")
                        }
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
                        let external_hint = if app.config.file_browser.is_some() {
                            "  e: external"
                        } else {
                            ""
                        };
                        if app.browser_searching {
                            format!(
                                "{mode_label}  |  Type to search  Enter: confirm  Esc: clear  ({} entries)",
                                app.browser_entries.len()
                            )
                        } else {
                            format!(
                                "{mode_label}  |  j/k: nav  Enter: select  -/Backspace: up  /: search  n/N: next/prev match  b: bookmarks  B: bookmark here{external_hint}  Esc: cancel  ({} entries)",
                                app.browser_entries.len()
                            )
                        }
                    }
                    View::Mixer => {
                        format!(
                            "{mode_label}  |  {transport}  |  h/l: track  j/k: field  +/-: adjust  m: mute  s: solo  Esc: back"
                        )
                    }
                    View::WaveformEditor => {
                        format!(
                            "{mode_label}  |  Ins:{:02}  |  Esc: back to instrument editor",
                            app.active_instrument
                        )
                    }
                }
            };

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

        // Input handling
        if event::poll(Duration::from_millis(16))? {
            if let Event::Key(key) = event::read()? {
                let height = terminal.size().map(|r| r.height).unwrap_or(24);
                if input::handle_input(&mut app, key, height) {
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
