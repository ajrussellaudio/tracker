use ratatui::{
    layout::Constraint,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table},
};
use vitakt_core::model::{FxCommand, InterpMode, TRACKS};

use crate::app::{
    App, BrowserEntry, BrowserMode, INSTR_FIELD_INTERP, INSTR_FIELD_LOOP_END,
    INSTR_FIELD_LOOP_START, INSTR_FIELD_NAME, INSTR_FIELD_ROOT, INSTR_FIELD_SAMPLE,
    INSTR_FIELD_VOLUME, INSTR_FIELD_PAN, MAX_INSTRUMENTS,
};
use crate::note_utils::{note_name, COL_FX_FIRST, COL_INS, COL_NOTE};
use crate::theme::Theme;

// ── Mixer field row indices ───────────────────────────────────────────────────
pub const MIXER_FIELD_VOL: usize = 0;
pub const MIXER_FIELD_PAN: usize = 1;
pub const MIXER_FIELD_MUTE: usize = 2;
pub const MIXER_FIELD_SOLO: usize = 3;
pub const MIXER_FIELD_SEND: usize = 4;
pub const MIXER_FIELD_COUNT: usize = 5;

// ── TUI rendering helpers ─────────────────────────────────────────────────────

pub fn render_phrase_grid(
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

pub fn render_song_view(app: &App) -> Table<'static> {
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

pub fn render_chain_view(app: &App) -> Table<'static> {
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

pub fn render_instrument_editor(app: &App) -> Paragraph<'static> {
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

pub fn render_startup_screen(app: &App) -> Paragraph<'static> {
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

pub fn render_sample_browser(app: &App, viewport_height: usize) -> Paragraph<'static> {
    let dir_display = app.browser_dir.to_string_lossy().to_string();

    let search_active = app.browser_searching || !app.browser_search_query.is_empty();

    let search_line = if search_active {
        let cursor_char = if app.browser_searching { "▌" } else { "" };
        let match_info = if !app.browser_search_query.is_empty() {
            if app.browser_search_matches.is_empty() {
                " (no matches)".to_string()
            } else {
                format!(
                    " ({}/{})",
                    app.browser_search_idx + 1,
                    app.browser_search_matches.len()
                )
            }
        } else {
            String::new()
        };
        ratatui::text::Line::styled(
            format!("  /{}{}{}", app.browser_search_query, cursor_char, match_info),
            Style::default().fg(app.theme.cursor_bg),
        )
    } else {
        ratatui::text::Line::from("")
    };

    let mut lines = vec![
        ratatui::text::Line::styled(
            format!("  {dir_display}"),
            Style::default().fg(app.theme.inactive_track),
        ),
        search_line,
    ];

    if app.browser_entries.is_empty() {
        lines.push(ratatui::text::Line::styled(
            "  (no files found in current directory)",
            Style::default().fg(app.theme.inactive_track),
        ));
    } else {
        // 2 borders + 2 header lines (dir path + search/blank) = 4 rows consumed
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
            let is_match = search_active
                && !app.browser_search_matches.is_empty()
                && app.browser_search_matches.contains(&abs_idx);
            let (prefix, style) = if abs_idx == app.browser_cursor {
                ("▶ ", Style::default().fg(app.theme.cursor_bg).add_modifier(Modifier::BOLD))
            } else if is_match {
                ("  ", Style::default().fg(app.theme.cursor_bg))
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
                BrowserMode::Sample => "Sample Browser  [Enter: select  -/Backspace: up  /: search  b: bookmarks  B: bookmark here  Esc: cancel]",
                BrowserMode::Project => "Open Project  [Enter: select  -/Backspace: up  /: search  Esc: cancel]",
            })
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.theme.screen_title)),
    )
}

// ── Mixer view render ─────────────────────────────────────────────────────────

pub fn render_mixer_view(app: &App) -> Table<'static> {
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

/// Render the waveform editor screen.
///
/// Returns a `Vec` of ratatui `Line`s suitable for wrapping in a `Paragraph`.
/// The caller passes `width` and `height` (the available content area in terminal
/// columns/rows, excluding any surrounding block border and the one-line status
/// footer that appears below the waveform).
pub fn render_waveform_editor(app: &App, width: usize, height: usize) -> Vec<ratatui::text::Line<'static>> {
    use crate::braille::{render_waveform, ActiveHandle, WaveformHandles};

    if height == 0 || width == 0 {
        return Vec::new();
    }

    let idx = app.active_instrument;
    let instr = match app.song.instruments.get(idx) {
        Some(i) => i,
        None => return Vec::new(),
    };

    let raw_frames = app.waveform_original_frames.max(1);
    let ds_len = app.waveform_samples.len().max(1);
    let to_ds = |f: u32| (f as usize * ds_len / raw_frames).min(ds_len.saturating_sub(1));

    let handles = WaveformHandles {
        sample_start: to_ds(instr.sample_start.unwrap_or(0)),
        sample_end:   to_ds(instr.sample_end.unwrap_or(raw_frames as u32)),
        loop_start:   to_ds(instr.loop_start.unwrap_or(0)),
        loop_end:     to_ds(instr.loop_end.unwrap_or(raw_frames as u32)),
        active: app.waveform_active_handle,
    };

    let handle_label = match app.waveform_active_handle {
        ActiveHandle::SampleStart => "Sample Start",
        ActiveHandle::SampleEnd   => "Sample End",
        ActiveHandle::LoopStart   => "Loop Start",
        ActiveHandle::LoopEnd     => "Loop End",
    };

    let raw_frames_u32 = raw_frames as u32;
    let active_pos = match app.waveform_active_handle {
        ActiveHandle::SampleStart => instr.sample_start.unwrap_or(0),
        ActiveHandle::SampleEnd   => instr.sample_end.unwrap_or(raw_frames_u32),
        ActiveHandle::LoopStart   => instr.loop_start.unwrap_or(0),
        ActiveHandle::LoopEnd     => instr.loop_end.unwrap_or(raw_frames_u32),
    };

    let fmt_time = |frames: u32, rate: u32| -> String {
        let rate = rate.max(1);
        let total_ms = (frames as u64 * 1000) / rate as u64;
        let ms = total_ms % 1000;
        let total_s = total_ms / 1000;
        let s = total_s % 60;
        let m = total_s / 60;
        format!("{m:02}:{s:02}.{ms:03}")
    };

    let sr = app.waveform_sample_rate;
    let pos_time = fmt_time(active_pos, sr);
    let total_time = fmt_time(raw_frames_u32, sr);

    // Reserve the last row for the info line.
    let waveform_height = height.saturating_sub(1);
    let mut lines = render_waveform(&app.waveform_samples, width, waveform_height, &handles);

    // Info line: "Sample Start  |  pos: 4096  (00:00.093)  /  88200 total  (00:02.000)"
    let is_playing = app.preview_playing.load(std::sync::atomic::Ordering::Relaxed);
    let preview_label = if is_playing { "  ▶" } else { "" };
    let info = format!(
        " {handle_label}  |  pos: {active_pos}  ({pos_time})  /  {raw_frames_u32} total  ({total_time}){preview_label}"
    );
    lines.push(ratatui::text::Line::styled(
        info,
        ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
    ));

    lines
}
