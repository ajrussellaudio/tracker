use crate::app::{
    App, InputMode, INSTR_FIELD_INTERP, INSTR_FIELD_LOOP_END, INSTR_FIELD_LOOP_START,
    INSTR_FIELD_PAN, INSTR_FIELD_ROOT, INSTR_FIELD_VOLUME,
};
use vitakt_core::{audio::Command, model, model::InterpMode, storage};

impl App {
    pub(crate) fn execute_command(&mut self) {
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
                    self.song = model::migrate(song);
                    if self.song.phrases.is_empty() {
                        self.song.phrases.push(model::Phrase::default());
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
            self.status = "Packing samples…".to_string();
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
            // No-op
        } else {
            self.status = format!("Unknown command: {raw}");
        }
    }
}

/// Increment or decrement the currently-selected numeric/mode instrument field.
/// `delta` is +1 or -1.
pub(crate) fn instr_editor_increment(app: &mut App, delta: i32) {
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
