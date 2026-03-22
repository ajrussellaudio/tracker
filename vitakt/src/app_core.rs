use crate::app::{App, BrowserMode, InputMode, View, MAX_INSTRUMENTS};
use crate::config::Config;
use crate::history::History;
use crate::note_utils::{note_name, pitch_speed};
use crate::theme;
use crate::wav_io::{load_all_instrument_samples, load_wav, load_wav_from_bytes, write_wav};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering},
    Arc,
};
use vitakt_core::{
    audio::Command,
    model::{Chain, ChainSlot, Song, STEPS_PER_PHRASE, TRACKS},
    storage,
};

impl App {
    pub(crate) fn new(
        producer: Option<rtrb::Producer<Command>>,
        sample_root: u8,
        seq_playing: Arc<AtomicBool>,
        current_seq_step: Arc<AtomicU8>,
        preview_playing: Arc<AtomicBool>,
    ) -> Self {
        let song = Song::default();
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
            browser_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            browser_mode: BrowserMode::Sample,
            browser_scroll: 0,
            startup_cursor: 0,
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
            mixer_cursor_track: 0,
            mixer_cursor_field: 0,
            keyboard_instrument: 0,
            render_receiver: None,
            render_progress: Arc::new(AtomicU32::new(0)),
            history: History::new(),
            is_dirty: false,
            is_previewing: false,
            preview_playing,
            status_timer: None,
            theme: theme::load(),
            config: Config::load(),
            browser_show_bookmarks: false,
            browser_bookmark_cursor: 0,
            needs_terminal_clear: false,
            waveform_samples: Vec::new(),
            waveform_active_handle: crate::braille::ActiveHandle::SampleStart,
        }
    }

    pub(crate) fn phrase_mut(&mut self) -> &mut vitakt_core::model::Phrase {
        let idx = self.active_phrase_idx.min(self.song.phrases.len().saturating_sub(1));
        &mut self.song.phrases[idx]
    }

    pub(crate) fn phrase(&self) -> &vitakt_core::model::Phrase {
        let idx = self.active_phrase_idx.min(self.song.phrases.len().saturating_sub(1));
        &self.song.phrases[idx]
    }

    /// Send a command to the audio thread (fire-and-forget).
    pub(crate) fn send_cmd(&mut self, cmd: Command) {
        if let Some(prod) = &mut self.producer {
            let _ = prod.push(cmd);
        }
    }

    /// Push a fresh phrase snapshot to the sequencer.
    pub(crate) fn sync_phrase_to_sequencer(&mut self) {
        let phrase = Box::new(self.phrase().clone());
        self.send_cmd(Command::UpdatePhrase(phrase.clone()));
        self.send_cmd(Command::UpdatePhraseInSong {
            idx: self.active_phrase_idx,
            phrase,
        });
        self.send_cmd(Command::SetSampleRoot(self.sample_root));
    }

    /// Send the full song data snapshot to the audio thread for arrangement playback.
    pub(crate) fn sync_song_to_sequencer(&mut self) {
        let arrangement = self.song.arrangement.clone();
        let chains = self.song.chains.clone();
        let phrases = self.song.phrases.clone();
        let instruments = self.song.instruments.clone();
        self.send_cmd(Command::UpdateSongData { arrangement, chains, phrases, instruments });
        self.send_cmd(Command::SetSampleRoot(self.sample_root));
    }

    /// Push all mixer state to the audio thread (called after load and incremental changes).
    pub(crate) fn sync_mixer_to_audio(&mut self) {
        for t in 0..TRACKS {
            let (vol, pan, mute, solo) = {
                let m = &self.song.mixer[t];
                (m.volume, m.pan, m.mute, m.solo)
            };
            self.send_cmd(Command::SetTrackVolume { track: t as u8, volume: vol });
            self.send_cmd(Command::SetTrackPan { track: t as u8, pan });
            self.send_cmd(Command::SetTrackMute { track: t as u8, mute });
            self.send_cmd(Command::SetTrackSolo { track: t as u8, active: solo });
        }
    }

    /// Toggle play / stop.
    pub(crate) fn toggle_play(&mut self) {
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
    pub(crate) fn restart_play(&mut self) {
        self.sync_song_to_sequencer();
        self.send_cmd(Command::Restart);
        self.seq_playing.store(true, Ordering::Relaxed);
    }

    /// Ensure instrument slots 0..=idx exist (creates defaults up to MAX_INSTRUMENTS).
    pub(crate) fn ensure_instrument(&mut self, idx: usize) {
        while self.song.instruments.len() <= idx && self.song.instruments.len() < MAX_INSTRUMENTS {
            self.song.instruments.push(vitakt_core::model::Instrument::default());
        }
    }

    /// Open the instrument editor for the active instrument, creating it if needed.
    pub(crate) fn open_instrument_editor(&mut self) {
        if self.song.instruments.len() < MAX_INSTRUMENTS {
            self.ensure_instrument(self.active_instrument);
        }
        self.push_view(View::InstrumentEditor);
        self.instr_cursor = 0;
        self.instr_editing = false;
        self.instr_edit_buf.clear();
    }

    /// Open the waveform editor for the active instrument.
    /// Refuses (with a status message) if the instrument has no sample assigned.
    pub(crate) fn open_waveform_editor(&mut self) {
        let idx = self.active_instrument;
        let instr = match self.song.instruments.get(idx) {
            Some(i) => i,
            None => {
                self.set_timed_status("No instrument selected".to_string());
                return;
            }
        };

        if instr.sample.is_none() {
            self.set_timed_status("No sample assigned to this instrument".to_string());
            return;
        }

        let sample = instr.sample.as_ref().unwrap();
        let load_result = if let Some(bytes) = &sample.bytes {
            load_wav_from_bytes(bytes)
        } else {
            load_wav(&sample.path)
        };

        match load_result {
            Ok((buf, _channels)) => {
                // Downsample to at most 4096 points so the renderer stays fast.
                const MAX_WAVEFORM_SAMPLES: usize = 4096;
                let samples: Vec<f32> = if buf.len() <= MAX_WAVEFORM_SAMPLES {
                    buf.as_ref().to_vec()
                } else {
                    let step = buf.len() as f64 / MAX_WAVEFORM_SAMPLES as f64;
                    (0..MAX_WAVEFORM_SAMPLES)
                        .map(|i| buf[(i as f64 * step) as usize])
                        .collect()
                };
                self.waveform_samples = samples;
                self.waveform_active_handle = crate::braille::ActiveHandle::SampleStart;
                self.push_view(View::WaveformEditor);
            }
            Err(e) => {
                self.set_timed_status(format!("Error loading sample: {e}"));
            }
        }
    }

    /// Reload sample from disk for the active instrument and send a LoadVoice command.
    pub(crate) fn reload_instrument_sample(&mut self) {
        let idx = self.active_instrument;
        if let Some(instr) = self.song.instruments.get(idx) {
            if let Some(sample) = &instr.sample {
                let path = sample.path.clone();
                let embedded_bytes = sample.bytes.clone();
                let loop_start = instr.loop_start.unwrap_or(0);
                let loop_end = instr.loop_end.unwrap_or(0);
                let sample_start = instr.sample_start;
                let sample_end = instr.sample_end;
                let interp_mode = instr.interp_mode.clone();
                let load_result = if let Some(bytes) = embedded_bytes {
                    load_wav_from_bytes(&bytes)
                } else {
                    load_wav(&path)
                };
                match load_result {
                    Ok((buf, channels)) => {
                        let (buf, loop_start, loop_end) =
                            apply_sample_bounds(buf, channels, sample_start, sample_end, loop_start, loop_end);
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
    pub(crate) fn reload_instruments(&mut self) {
        if self.song.instruments.is_empty() {
            return;
        }
        let instr = &self.song.instruments[0];
        if let Some(sample) = &instr.sample {
            let path = sample.path.clone();
            let embedded_bytes = sample.bytes.clone();
            let loop_start = instr.loop_start.unwrap_or(0);
            let loop_end = instr.loop_end.unwrap_or(0);
            let sample_start = instr.sample_start;
            let sample_end = instr.sample_end;
            let interp_mode = instr.interp_mode.clone();
            let load_result = if let Some(bytes) = embedded_bytes {
                load_wav_from_bytes(&bytes)
            } else {
                load_wav(&path)
            };
            match load_result {
                Ok((buf, channels)) => {
                    let (buf, loop_start, loop_end) =
                        apply_sample_bounds(buf, channels, sample_start, sample_end, loop_start, loop_end);
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

    /// Adjust BPM by `delta` and send the new value to the audio thread.
    pub(crate) fn adjust_bpm(&mut self, delta: f32) {
        self.record("set BPM");
        self.song.bpm = (self.song.bpm + delta).clamp(20.0, 999.0);
        self.send_cmd(Command::SetBpm(self.song.bpm));
    }

    /// Push the current view onto the navigation stack and switch to `next`.
    pub(crate) fn push_view(&mut self, next: View) {
        let current = std::mem::replace(&mut self.view, next);
        self.view_stack.push(current);
    }

    /// Pop the navigation stack and return to the previous view.
    pub(crate) fn pop_view(&mut self) {
        if let Some(prev) = self.view_stack.pop() {
            self.view = prev;
        }
        if self.is_previewing {
            self.send_cmd(Command::StopPreview);
            self.is_previewing = false;
            self.preview_playing.store(false, Ordering::Relaxed);
        }
        if matches!(self.view, View::PhraseEditor) {
            self.mode = InputMode::Normal;
        }
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
    pub(crate) fn enter_note(&mut self, midi: u8) {
        self.record(&format!("set note {} at step {}", note_name(midi), self.cursor_step));
        let cursor = self.cursor_step;
        let instr = self.active_instrument as u8;
        let step = &mut self.phrase_mut().steps[cursor];
        step.note = Some(midi);
        step.instrument = Some(instr);
        step.velocity = 100;

        if let Some(prod) = &mut self.producer {
            let speed = pitch_speed(midi, self.sample_root);
            let _ = prod.push(Command::NoteOn { slot: 0, speed });
        }

        self.sync_phrase_to_sequencer();

        self.cursor_step = (self.cursor_step + 1) % STEPS_PER_PHRASE;
    }

    /// Spawn a background thread to render the full mix and write to `path`.
    pub(crate) fn start_render_mix(&mut self, path: String) {
        if self.render_receiver.is_some() {
            self.status = "Error: render already in progress".to_string();
            return;
        }
        let song = self.song.clone();
        let progress = Arc::clone(&self.render_progress);
        progress.store(0, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.render_receiver = Some(rx);
        self.status = "Rendering mix... 0%".to_string();

        std::thread::spawn(move || {
            let buffers = load_all_instrument_samples(&song);
            let result: anyhow::Result<String> = (|| {
                let audio = vitakt_core::render::render_to_buffer(
                    &song,
                    &buffers,
                    None,
                    &mut |p| {
                        progress.store((p * 100.0) as u32, Ordering::Relaxed);
                    },
                );
                write_wav(&path, &audio)?;
                Ok(format!("Mix exported: {path}"))
            })();
            let _ = tx.send(result);
        });
    }

    /// Spawn a background thread to render per-track stems into `dir`.
    pub(crate) fn start_render_stems(&mut self, dir: String) {
        if self.render_receiver.is_some() {
            self.status = "Error: render already in progress".to_string();
            return;
        }
        let song = self.song.clone();
        let progress = Arc::clone(&self.render_progress);
        progress.store(0, Ordering::Relaxed);
        let (tx, rx) = std::sync::mpsc::channel();
        self.render_receiver = Some(rx);
        self.status = "Rendering stems... 0%".to_string();

        std::thread::spawn(move || {
            let result: anyhow::Result<String> = (|| {
                std::fs::create_dir_all(&dir)?;
                let buffers = load_all_instrument_samples(&song);
                for track in 0..TRACKS {
                    let audio = vitakt_core::render::render_to_buffer(
                        &song,
                        &buffers,
                        Some(track),
                        &mut |p| {
                            let overall = (track as f32 + p) / TRACKS as f32;
                            progress.store((overall * 100.0) as u32, Ordering::Relaxed);
                        },
                    );
                    let filename = format!("{dir}/track-{:02}.wav", track + 1);
                    write_wav(&filename, &audio)?;
                }
                Ok(format!("Stems exported to: {dir}"))
            })();
            let _ = tx.send(result);
        });
    }

    /// Poll the render thread receiver; update status bar on completion or progress.
    pub(crate) fn poll_render(&mut self) {
        let result = match &self.render_receiver {
            None => return,
            Some(rx) => match rx.try_recv() {
                Ok(r) => r,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    let pct = self.render_progress.load(Ordering::Relaxed);
                    self.status = format!("Rendering... {pct}%");
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err(anyhow::anyhow!("render thread disconnected unexpectedly"))
                }
            },
        };
        self.render_receiver = None;
        match result {
            Ok(msg) => self.status = msg,
            Err(e) => self.status = format!("Export error: {e}"),
        }
    }

    /// Save a snapshot before a mutation. Call BEFORE any mutation.
    pub(crate) fn record(&mut self, description: &str) {
        let snapshot = self.song.clone();
        self.history.push(description.to_string(), snapshot);
        self.is_dirty = true;
    }

    /// Set a status message that clears after 2 seconds.
    pub(crate) fn set_timed_status(&mut self, msg: String) {
        self.status = msg;
        self.status_timer = Some(std::time::Instant::now());
    }

    /// Undo the most recent mutation.
    pub(crate) fn do_undo(&mut self) {
        let current = self.song.clone();
        if let Some((snapshot, desc)) = self.history.undo(current) {
            self.song = snapshot;
            self.is_dirty = true;
            self.set_timed_status(format!("Undid: {desc}"));
            self.sync_phrase_to_sequencer();
            self.sync_song_to_sequencer();
            self.sync_mixer_to_audio();
        } else {
            self.set_timed_status("Nothing to undo".to_string());
        }
    }

    /// Redo the most recently undone mutation.
    pub(crate) fn do_redo(&mut self) {
        let current = self.song.clone();
        if let Some((snapshot, desc)) = self.history.redo(current) {
            self.song = snapshot;
            self.is_dirty = true;
            self.set_timed_status(format!("Redid: {desc}"));
            self.sync_phrase_to_sequencer();
            self.sync_song_to_sequencer();
            self.sync_mixer_to_audio();
        } else {
            self.set_timed_status("Nothing to redo".to_string());
        }
    }

    pub(crate) fn enter_keyboard_mode(&mut self) {
        self.mode = InputMode::Keyboard;
    }

    pub(crate) fn exit_keyboard_mode(&mut self) {
        self.mode = InputMode::Normal;
    }

    pub(crate) fn keyboard_instrument_prev(&mut self) {
        if self.keyboard_instrument > 0 {
            self.keyboard_instrument -= 1;
        }
    }

    pub(crate) fn keyboard_instrument_next(&mut self) {
        if self.keyboard_instrument < 255 {
            self.keyboard_instrument += 1;
        }
    }
}

/// Slice a decoded sample buffer to the region [sample_start, sample_end] and adjust
/// loop points to be relative to the new start, clamped to the new buffer length.
pub(crate) fn apply_sample_bounds(
    buf: Arc<Vec<f32>>,
    channels: usize,
    sample_start: Option<u32>,
    sample_end: Option<u32>,
    loop_start: u32,
    loop_end: u32,
) -> (Arc<Vec<f32>>, u32, u32) {
    let total_frames = buf.len() / channels.max(1);
    let start_frame = sample_start.unwrap_or(0) as usize;
    let end_frame = sample_end.map(|e| e as usize).unwrap_or(total_frames);
    let start_frame = start_frame.min(total_frames);
    let end_frame = end_frame.clamp(start_frame, total_frames);
    if start_frame == 0 && end_frame == total_frames {
        return (buf, loop_start, loop_end);
    }
    let new_length = end_frame - start_frame;
    let sliced = Arc::new(buf[start_frame * channels..end_frame * channels].to_vec());
    let adj_loop_start =
        ((loop_start as usize).saturating_sub(start_frame)).min(new_length) as u32;
    let adj_loop_end =
        ((loop_end as usize).saturating_sub(start_frame)).min(new_length) as u32;
    (sliced, adj_loop_start, adj_loop_end)
}

#[cfg(test)]
mod sample_bounds_tests {
    use super::*;

    fn make_buf(frames: usize) -> Arc<Vec<f32>> {
        Arc::new(vec![0.5f32; frames])
    }

    #[test]
    fn no_bounds_returns_original_buffer() {
        let buf = make_buf(100);
        let (out, ls, le) = apply_sample_bounds(Arc::clone(&buf), 1, None, None, 10, 50);
        assert_eq!(out.len(), 100);
        assert_eq!(ls, 10);
        assert_eq!(le, 50);
    }

    #[test]
    fn sample_start_shifts_loop_points() {
        let buf = make_buf(200);
        let (out, ls, le) =
            apply_sample_bounds(Arc::clone(&buf), 1, Some(50), None, 60, 80);
        assert_eq!(out.len(), 150);
        assert_eq!(ls, 10);
        assert_eq!(le, 30);
    }

    #[test]
    fn loop_end_clamped_when_exceeds_sample_end() {
        let buf = make_buf(500);
        let (out, ls, le) =
            apply_sample_bounds(Arc::clone(&buf), 1, Some(0), Some(200), 10, 500);
        assert_eq!(out.len(), 200);
        assert_eq!(ls, 10);
        assert_eq!(le, 200, "loop_end should be clamped to new buffer length");
    }

    #[test]
    fn loop_points_before_start_frame_clamped_to_zero() {
        let buf = make_buf(100);
        let (out, ls, le) =
            apply_sample_bounds(Arc::clone(&buf), 1, Some(50), None, 20, 80);
        assert_eq!(out.len(), 50);
        assert_eq!(ls, 0);
        assert_eq!(le, 30);
    }

    #[test]
    fn multichannel_buffer_sliced_correctly() {
        let buf = Arc::new(vec![0.5f32; 200]);
        let (out, _, _) =
            apply_sample_bounds(Arc::clone(&buf), 2, Some(10), Some(40), 0, 0);
        assert_eq!(out.len(), 60);
    }
}
