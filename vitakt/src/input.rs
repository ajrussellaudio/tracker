use crate::app::{
    App, InputMode, View, INSTR_FIELD_COUNT, INSTR_FIELD_NAME, INSTR_FIELD_SAMPLE,
};
use crate::commands::instr_editor_increment;
use crate::note_utils::{col_to_fx, pitch_speed, qwerty_to_semitone, COL_COUNT, COL_NOTE};
use crate::render::{
    MIXER_FIELD_COUNT, MIXER_FIELD_MUTE, MIXER_FIELD_PAN, MIXER_FIELD_SEND, MIXER_FIELD_SOLO,
    MIXER_FIELD_VOL,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::sync::atomic::Ordering;
use vitakt_core::{
    audio::Command,
    model::{ChainSlot, FxCommand, STEPS_PER_PHRASE, TRACKS},
};

/// Handle a single key event. Returns `true` if the app should quit.
pub fn handle_input(app: &mut App, key: KeyEvent, terminal_height: u16) -> bool {
    // Global undo/redo: works from any view except insert/command mode.
    let is_insert = matches!((&app.view, &app.mode), (View::PhraseEditor, InputMode::Insert));
    let is_command = matches!((&app.view, &app.mode), (View::PhraseEditor, InputMode::Command));
    if !is_insert && !is_command {
        if key.code == KeyCode::Char('u') && !key.modifiers.contains(KeyModifiers::CONTROL) {
            app.do_undo();
            return false;
        }
        if key.code == KeyCode::Char('r') && key.modifiers.contains(KeyModifiers::CONTROL) {
            app.do_redo();
            return false;
        }
    }

    // Global: ConfirmQuit modal overrides all per-view key handling.
    if matches!(app.mode, InputMode::ConfirmQuit) {
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => return true,
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                app.mode = InputMode::Normal;
            }
            _ => {}
        }
        return false;
    }

    // Global: Keyboard mode overrides all per-view key handling.
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
                    let root = app
                        .song
                        .instruments
                        .get(app.keyboard_instrument)
                        .map(|i| i.root_note)
                        .unwrap_or(app.sample_root);
                    let speed = pitch_speed(midi, root);
                    app.send_cmd(Command::NoteOn { slot, speed });
                }
            }
            _ => {}
        }
        return false;
    }

    match app.view {
        View::Startup => handle_startup(app, key),
        View::SongView => handle_song_view(app, key),
        View::ChainView => handle_chain_view(app, key),
        View::PhraseEditor => handle_phrase_editor(app, key),
        View::InstrumentEditor => handle_instrument_editor(app, key),
        View::SampleBrowser => handle_sample_browser(app, key, terminal_height),
        View::Mixer => handle_mixer(app, key),
        View::WaveformEditor => handle_waveform_editor(app, key),
    }
}

fn handle_startup(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') => {
            if app.is_dirty {
                app.mode = InputMode::ConfirmQuit;
            } else {
                return true;
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
                app.view = View::SongView;
                app.view_stack.clear();
            }
            _ => {
                app.open_project_browser();
            }
        },
        _ => {}
    }
    false
}

fn handle_song_view(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('q') => {
            if app.is_dirty {
                app.mode = InputMode::ConfirmQuit;
            } else {
                return true;
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
        KeyCode::Char('j') | KeyCode::Down => {
            let rows = app.song.arrangement.len();
            if rows == 0 || app.song_cursor_row + 1 >= rows {
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
        KeyCode::Char(c)
            if c.is_ascii_hexdigit() && c != 'j' && c != 'k' && c != 'h' && c != 'l' =>
        {
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
        KeyCode::Delete | KeyCode::Char('x') => {
            let row = app.song_cursor_row;
            let track = app.song_cursor_track;
            app.record(&format!("clear arrangement row {} track {}", row, track));
            if let Some(arr_row) = app.song.arrangement.get_mut(row) {
                arr_row[track] = None;
                app.sync_song_to_sequencer();
            }
        }
        KeyCode::Char('o') => {
            app.record("add song row below");
            let insert_at = app.song_cursor_row + 1;
            app.song.arrangement.insert(insert_at, [None; TRACKS]);
        }
        KeyCode::Char('O') => {
            app.record("add song row above");
            let insert_at = app.song_cursor_row;
            app.song.arrangement.insert(insert_at, [None; TRACKS]);
            app.song_cursor_row += 1;
        }
        KeyCode::Enter => {
            let row = app.song_cursor_row;
            let track = app.song_cursor_track;
            app.chain_view_track = track;
            app.chain_view_row = row;
            app.chain_cursor = 0;
            app.chain_insert_mode = false;
            app.push_view(View::ChainView);
        }
        KeyCode::Tab => {
            app.push_view(View::PhraseEditor);
            app.mode = InputMode::Normal;
        }
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
    }
    false
}

fn handle_chain_view(app: &mut App, key: KeyEvent) -> bool {
    let track = app.chain_view_track;
    let row = app.chain_view_row;
    let chain_idx_opt = app
        .song
        .arrangement
        .get(row)
        .and_then(|r| r[track])
        .map(|c| c as usize);

    if app.chain_insert_mode {
        match key.code {
            KeyCode::Esc => {
                app.chain_insert_mode = false;
            }
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
            KeyCode::Char('i') => {
                if chain_idx_opt.is_some() {
                    app.chain_insert_mode = true;
                }
            }
            KeyCode::Char('o') => {
                if let Some(ci) = chain_idx_opt {
                    app.record("add chain slot below");
                    let insert_at =
                        (app.chain_cursor + 1).min(app.song.chains[ci].slots.len());
                    app.song.chains[ci].slots.insert(
                        insert_at,
                        ChainSlot { phrase: 0, transpose: 0 },
                    );
                    app.chain_cursor = insert_at;
                    app.sync_song_to_sequencer();
                }
            }
            KeyCode::Char('O') => {
                if let Some(ci) = chain_idx_opt {
                    app.record("add chain slot above");
                    let insert_at = app.chain_cursor;
                    app.song.chains[ci].slots.insert(
                        insert_at,
                        ChainSlot { phrase: 0, transpose: 0 },
                    );
                    app.chain_cursor += 1;
                    app.sync_song_to_sequencer();
                }
            }
            KeyCode::Char('d') | KeyCode::Delete => {
                if let Some(ci) = chain_idx_opt {
                    let len = app.song.chains[ci].slots.len();
                    if len > 0 {
                        app.record("delete chain slot");
                        app.song.chains[ci].slots.remove(app.chain_cursor);
                        if app.chain_cursor >= app.song.chains[ci].slots.len()
                            && app.chain_cursor > 0
                        {
                            app.chain_cursor -= 1;
                        }
                        app.sync_song_to_sequencer();
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(ci) = chain_idx_opt {
                    if let Some(slot) = app.song.chains[ci].slots.get(app.chain_cursor) {
                        let phrase_idx = slot.phrase as usize;
                        while app.song.phrases.len() <= phrase_idx {
                            app.song
                                .phrases
                                .push(vitakt_core::model::Phrase::default());
                        }
                        app.active_phrase_idx = phrase_idx;
                        app.push_view(View::PhraseEditor);
                        app.mode = InputMode::Normal;
                    }
                }
            }
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
    false
}

fn handle_phrase_editor(app: &mut App, key: KeyEvent) -> bool {
    match app.mode {
        InputMode::Normal => {
            app.yy_pending = false;
            match key.code {
                KeyCode::Esc => app.pop_view(),
                KeyCode::Char('q') => {
                    if app.is_dirty {
                        app.mode = InputMode::ConfirmQuit;
                    } else {
                        return true;
                    }
                }
                KeyCode::F(1) => {
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
                KeyCode::Tab => app.open_instrument_editor(),
                KeyCode::Char(' ') => app.toggle_play(),
                KeyCode::F(5) => app.restart_play(),
                KeyCode::Left => app.adjust_bpm(-1.0),
                KeyCode::Right => app.adjust_bpm(1.0),
                KeyCode::Char('j') | KeyCode::Down => {
                    app.cursor_step = (app.cursor_step + 1) % STEPS_PER_PHRASE;
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    app.cursor_step =
                        (app.cursor_step + STEPS_PER_PHRASE - 1) % STEPS_PER_PHRASE;
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
                KeyCode::Char('d') | KeyCode::Delete => {
                    let idx = app.cursor_step;
                    if let Some((slot_idx, _)) = col_to_fx(app.cursor_col) {
                        app.record(&format!(
                            "clear FX slot {} at step {}",
                            slot_idx + 1,
                            app.cursor_step
                        ));
                        app.phrase_mut().steps[idx].fx[slot_idx] =
                            vitakt_core::model::FxSlot::default();
                    } else {
                        app.record(&format!("clear step {}", app.cursor_step));
                        let step = &mut app.phrase_mut().steps[idx];
                        step.note = None;
                        step.instrument = None;
                        step.velocity = 0;
                        step.fx = Default::default();
                    }
                    app.fx_edit_buf.clear();
                    app.sync_phrase_to_sequencer();
                }
                KeyCode::Char('y') => {
                    if app.yy_pending {
                        app.yanked_step =
                            Some(app.phrase().steps[app.cursor_step].clone());
                        app.set_timed_status(format!("Yanked step {}", app.cursor_step));
                        app.yy_pending = false;
                    } else {
                        app.yy_pending = true;
                        return false; // don't reset yy_pending below
                    }
                }
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

        InputMode::Insert => match key.code {
            KeyCode::Esc => {
                app.mode = InputMode::Normal;
                app.fx_edit_buf.clear();
            }
            KeyCode::Up => {
                app.cursor_step =
                    (app.cursor_step + STEPS_PER_PHRASE - 1) % STEPS_PER_PHRASE;
                app.fx_edit_buf.clear();
            }
            KeyCode::Down => {
                app.cursor_step = (app.cursor_step + 1) % STEPS_PER_PHRASE;
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
            KeyCode::Delete | KeyCode::Backspace
                if col_to_fx(app.cursor_col).is_some() =>
            {
                if !app.fx_edit_buf.is_empty() {
                    app.fx_edit_buf.pop();
                } else if let Some((slot_idx, _)) = col_to_fx(app.cursor_col) {
                    let step_idx = app.cursor_step;
                    app.record(&format!(
                        "clear FX slot {} at step {}",
                        slot_idx + 1,
                        app.cursor_step
                    ));
                    app.phrase_mut().steps[step_idx].fx[slot_idx] =
                        vitakt_core::model::FxSlot::default();
                    app.sync_phrase_to_sequencer();
                }
            }
            KeyCode::Char(c) => {
                match col_to_fx(app.cursor_col) {
                    Some((slot_idx, true)) => {
                        if c.is_alphabetic() {
                            app.fx_edit_buf.push(c.to_ascii_uppercase());
                            if app.fx_edit_buf.len() == 3 {
                                let buf = app.fx_edit_buf.clone();
                                app.fx_edit_buf.clear();
                                let step_idx = app.cursor_step;
                                if let Some(cmd) = FxCommand::from_code(&buf) {
                                    app.record(&format!(
                                        "set FX{} command at step {}",
                                        slot_idx + 1,
                                        app.cursor_step
                                    ));
                                    app.phrase_mut().steps[step_idx].fx[slot_idx].command =
                                        cmd.id();
                                    app.sync_phrase_to_sequencer();
                                    if app.cursor_col + 1 < COL_COUNT {
                                        app.cursor_col += 1;
                                    }
                                } else {
                                    app.status = format!("Unknown FX command: {buf}");
                                }
                            }
                        }
                    }
                    Some((slot_idx, false)) => {
                        if c.is_ascii_digit() {
                            app.fx_edit_buf.push(c);
                            if app.fx_edit_buf.len() == 3 {
                                let buf = app.fx_edit_buf.clone();
                                app.fx_edit_buf.clear();
                                if let Ok(v) = buf.parse::<u16>() {
                                    let step_idx = app.cursor_step;
                                    app.record(&format!(
                                        "set FX{} value at step {}",
                                        slot_idx + 1,
                                        app.cursor_step
                                    ));
                                    app.phrase_mut().steps[step_idx].fx[slot_idx].value =
                                        v.min(255) as u8;
                                    app.sync_phrase_to_sequencer();
                                    app.cursor_step =
                                        (app.cursor_step + 1) % STEPS_PER_PHRASE;
                                }
                            }
                        }
                    }
                    None => {
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
            KeyCode::Enter if col_to_fx(app.cursor_col).is_some() => {
                if let Some((slot_idx, false)) = col_to_fx(app.cursor_col) {
                    let buf = app.fx_edit_buf.clone();
                    app.fx_edit_buf.clear();
                    if !buf.is_empty() {
                        if let Ok(v) = buf.parse::<u16>() {
                            let step_idx = app.cursor_step;
                            app.record(&format!(
                                "set FX value at step {}",
                                app.cursor_step
                            ));
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
        InputMode::Keyboard => {}
        InputMode::ConfirmQuit => {}
    }
    false
}

fn handle_instrument_editor(app: &mut App, key: KeyEvent) -> bool {
    if app.instr_editing {
        match key.code {
            KeyCode::Enter => {
                let buf = app.instr_edit_buf.clone();
                let cursor = app.instr_cursor;
                app.record(&format!(
                    "edit instrument {} name/sample",
                    app.active_instrument
                ));
                if let Some(instr) = app.song.instruments.get_mut(app.active_instrument) {
                    match cursor {
                        INSTR_FIELD_NAME => instr.name = buf,
                        INSTR_FIELD_SAMPLE => {
                            instr.sample =
                                Some(vitakt_core::model::Sample::from_path(&buf));
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
        match key.code {
            KeyCode::Esc => {
                app.pop_view();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                app.instr_cursor = (app.instr_cursor + 1) % INSTR_FIELD_COUNT;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                app.instr_cursor =
                    (app.instr_cursor + INSTR_FIELD_COUNT - 1) % INSTR_FIELD_COUNT;
            }
            KeyCode::Char('i') => {
                let cursor = app.instr_cursor;
                if cursor == INSTR_FIELD_SAMPLE {
                    app.open_sample_browser();
                } else if cursor == INSTR_FIELD_NAME {
                    let cur_name = app
                        .song
                        .instruments
                        .get(app.active_instrument)
                        .map(|i| i.name.clone())
                        .unwrap_or_default();
                    app.instr_edit_buf = cur_name;
                    app.instr_editing = true;
                } else {
                    app.record(&format!(
                        "edit instrument {} field {}",
                        app.active_instrument, app.instr_cursor
                    ));
                    instr_editor_increment(app, 1);
                }
            }
            KeyCode::Char('w') => {
                app.open_waveform_editor();
            }
            KeyCode::Enter => {
                if app.instr_cursor == INSTR_FIELD_SAMPLE {
                    app.open_sample_browser();
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                app.record(&format!(
                    "edit instrument {} field {}",
                    app.active_instrument, app.instr_cursor
                ));
                instr_editor_increment(app, -1);
            }
            KeyCode::Char('l') | KeyCode::Right => {
                app.record(&format!(
                    "edit instrument {} field {}",
                    app.active_instrument, app.instr_cursor
                ));
                instr_editor_increment(app, 1);
            }
            _ => {}
        }
    }
    false
}

fn handle_sample_browser(app: &mut App, key: KeyEvent, terminal_height: u16) -> bool {
    if app.browser_show_bookmarks {
        let valid_len = app
            .config
            .bookmarks
            .iter()
            .filter(|p| std::path::Path::new(p.as_str()).is_dir())
            .count();
        match key.code {
            KeyCode::Esc => {
                app.browser_overlay_esc();
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if valid_len > 0 {
                    app.browser_bookmark_cursor =
                        (app.browser_bookmark_cursor + 1) % valid_len;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if valid_len > 0 {
                    app.browser_bookmark_cursor =
                        (app.browser_bookmark_cursor + valid_len - 1) % valid_len;
                }
            }
            KeyCode::Enter => {
                app.browser_overlay_enter();
            }
            _ => {}
        }
    } else {
        match key.code {
            KeyCode::Esc => {
                app.pop_view();
            }
            KeyCode::Char(' ') => app.browser_preview_toggle(),
            KeyCode::Char('j') | KeyCode::Down => {
                if !app.browser_entries.is_empty() {
                    app.browser_cursor =
                        (app.browser_cursor + 1) % app.browser_entries.len();
                    let available = (terminal_height as usize).saturating_sub(5);
                    app.browser_clamp_scroll(available);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if !app.browser_entries.is_empty() {
                    app.browser_cursor = (app.browser_cursor
                        + app.browser_entries.len()
                        - 1)
                        % app.browser_entries.len();
                    let available = (terminal_height as usize).saturating_sub(5);
                    app.browser_clamp_scroll(available);
                }
            }
            KeyCode::Enter => app.browser_enter(),
            KeyCode::Backspace | KeyCode::Char('-') => app.browser_go_up(),
            KeyCode::Char('b') => {
                app.browser_try_open_bookmarks();
            }
            KeyCode::Char('B') => {
                app.browser_add_bookmark();
            }
            KeyCode::Char('e') => {
                app.browser_launch_external();
            }
            _ => {}
        }
    }
    false
}

fn handle_mixer(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::F(2) => app.pop_view(),

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

        KeyCode::Char('j') | KeyCode::Down => {
            app.mixer_cursor_field =
                (app.mixer_cursor_field + 1) % MIXER_FIELD_COUNT;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            app.mixer_cursor_field =
                (app.mixer_cursor_field + MIXER_FIELD_COUNT - 1) % MIXER_FIELD_COUNT;
        }

        KeyCode::Char('+') | KeyCode::Char('=') => {
            app.record(&format!(
                "set mixer track {} field",
                app.mixer_cursor_track
            ));
            let t = app.mixer_cursor_track;
            let m = &mut app.song.mixer[t];
            match app.mixer_cursor_field {
                MIXER_FIELD_VOL => {
                    m.volume = ((m.volume + 0.05) * 100.0).round() / 100.0;
                    m.volume = m.volume.clamp(0.0, 2.0);
                    let v = m.volume;
                    app.send_cmd(Command::SetTrackVolume { track: t as u8, volume: v });
                }
                MIXER_FIELD_PAN => {
                    m.pan = ((m.pan + 0.05) * 100.0).round() / 100.0;
                    m.pan = m.pan.clamp(-1.0, 1.0);
                    let p = m.pan;
                    app.send_cmd(Command::SetTrackPan { track: t as u8, pan: p });
                }
                MIXER_FIELD_SEND => {
                    m.fx_send = ((m.fx_send + 0.05) * 100.0).round() / 100.0;
                    m.fx_send = m.fx_send.clamp(0.0, 1.0);
                }
                _ => {}
            }
        }
        KeyCode::Char('-') => {
            app.record(&format!(
                "set mixer track {} field",
                app.mixer_cursor_track
            ));
            let t = app.mixer_cursor_track;
            let m = &mut app.song.mixer[t];
            match app.mixer_cursor_field {
                MIXER_FIELD_VOL => {
                    m.volume = ((m.volume - 0.05) * 100.0).round() / 100.0;
                    m.volume = m.volume.clamp(0.0, 2.0);
                    let v = m.volume;
                    app.send_cmd(Command::SetTrackVolume { track: t as u8, volume: v });
                }
                MIXER_FIELD_PAN => {
                    m.pan = ((m.pan - 0.05) * 100.0).round() / 100.0;
                    m.pan = m.pan.clamp(-1.0, 1.0);
                    let p = m.pan;
                    app.send_cmd(Command::SetTrackPan { track: t as u8, pan: p });
                }
                MIXER_FIELD_SEND => {
                    m.fx_send = ((m.fx_send - 0.05) * 100.0).round() / 100.0;
                    m.fx_send = m.fx_send.clamp(0.0, 1.0);
                }
                _ => {}
            }
        }

        KeyCode::Char('m') => {
            let t = app.mixer_cursor_track;
            app.record(&format!("toggle mute track {}", t));
            let new_mute = !app.song.mixer[t].mute;
            app.song.mixer[t].mute = new_mute;
            app.send_cmd(Command::SetTrackMute { track: t as u8, mute: new_mute });
        }

        KeyCode::Char('s') => {
            let t = app.mixer_cursor_track;
            app.record(&format!("toggle solo track {}", t));
            let new_solo = !app.song.mixer[t].solo;
            app.song.mixer[t].solo = new_solo;
            app.send_cmd(Command::SetTrackSolo { track: t as u8, active: new_solo });
        }

        KeyCode::Enter => {
            let t = app.mixer_cursor_track;
            match app.mixer_cursor_field {
                MIXER_FIELD_MUTE => {
                    app.record(&format!("toggle mute track {}", t));
                    let new_mute = !app.song.mixer[t].mute;
                    app.song.mixer[t].mute = new_mute;
                    app.send_cmd(Command::SetTrackMute { track: t as u8, mute: new_mute });
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

        KeyCode::Char(' ') => app.toggle_play(),
        KeyCode::F(5) => app.restart_play(),

        _ => {}
    }
    false
}

fn handle_waveform_editor(app: &mut App, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.pop_view();
        }
        KeyCode::Char(' ') => app.waveform_preview_toggle(),
        _ => {}
    }
    false
}
