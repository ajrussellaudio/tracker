use std::sync::Arc;

use crate::model::{Chain, FxSlot, Instrument, InterpMode, Phrase, FX_SLOTS_PER_STEP, STEPS_PER_PHRASE, TRACKS};

/// Commands sent from the UI thread to the audio thread via a ring buffer.
#[derive(Clone, Debug)]
pub enum Command {
    /// Trigger slot N from the start at the given playback speed ratio.
    /// speed = 1.0 → root pitch, 2.0 → one octave up, 0.5 → one octave down.
    NoteOn { slot: u8, speed: f32 },
    /// Stop slot N immediately.
    NoteOff(u8),
    /// Start the sequencer from the current step.
    Play,
    /// Stop the sequencer (audio continues to drain).
    Stop,
    /// Restart the sequencer from step 0.
    Restart,
    /// Update the sequencer BPM live (no glitch — just changes the threshold).
    SetBpm(f32),
    /// Update the swing factor (0.0 = none, 1.0 = maximum).
    SetSwing(f32),
    /// Push a fresh copy of the phrase to the audio thread.
    UpdatePhrase(Box<Phrase>),
    /// Set the root note of the loaded sample (MIDI 0-127; default 60 = C4).
    SetSampleRoot(u8),
    /// Replace the voice in `slot` with a new buffer and settings.
    LoadVoice {
        slot: u8,
        samples: Arc<Vec<f32>>,
        channels: usize,
        loop_start: u32,
        loop_end: u32,
        interp_mode: InterpMode,
    },
    /// Update loop points for the voice currently in `slot` (takes effect immediately).
    SetLoopPoints { slot: u8, loop_start: u32, loop_end: u32 },
    /// Update interpolation mode for the voice in `slot` (takes effect on next render frame).
    SetInterpMode { slot: u8, interp_mode: InterpMode },
    /// Snapshot the full song hierarchy so the sequencer can traverse
    /// arrangement → chain → phrase during multi-track playback.
    UpdateSongData {
        arrangement: Vec<[Option<u8>; TRACKS]>,
        chains: Vec<Chain>,
        phrases: Vec<Phrase>,
        instruments: Vec<Instrument>,
    },
    /// Set the output volume for track `track` (0.0–2.0).  Takes effect immediately.
    SetTrackVolume { track: u8, volume: f32 },
    /// Set the stereo pan for track `track` (-1.0 to 1.0).  Takes effect immediately.
    SetTrackPan { track: u8, pan: f32 },
    /// Mute or unmute track `track`.  A muted track fires no NoteOn events.
    SetTrackMute { track: u8, mute: bool },
    /// Toggle solo on track `track`.  When any track is soloed, non-soloed tracks are silent.
    SetTrackSolo { track: u8, active: bool },
}

/// 4-point Hermite cubic interpolation for the "Sinc" quality mode.
/// y0..y3 are samples at positions -1, 0, 1, 2; t is the fractional offset in [0, 1).
fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + c0
}

/// A single playing voice backed by an in-memory f32 sample buffer.
///
/// The buffer is interleaved (same channel layout as the source WAV).
/// Output is always summed into a stereo (2-channel) interleaved slice.
/// Pitch shifting is achieved by advancing `frame_pos` by `speed` per output
/// frame, using the selected `interp_mode` between adjacent source frames.
///
/// Loop behaviour: when `loop_end > loop_start` and `loop_end <= total_frames`,
/// the voice wraps back to `loop_start` on reaching `loop_end` (infinite sustain).
/// Setting both to 0 or `loop_end <= loop_start` produces one-shot playback.
pub struct Voice {
    samples: Arc<Vec<f32>>,
    src_channels: usize,
    /// Fractional frame position.
    frame_pos: f64,
    speed: f64,
    active: bool,
    /// Loop start frame (inclusive).  0 = no loop unless loop_end is also set.
    pub loop_start: u32,
    /// Loop end frame (exclusive).  loop_end > loop_start enables looping.
    pub loop_end: u32,
    /// Interpolation quality.
    pub interp_mode: InterpMode,
    /// Per-step volume override (0.0–2.0).  Reset to 1.0 on each trigger.
    volume: f32,
    /// Per-step pan override (-1.0=full left, 0.0=centre, 1.0=full right).  Reset on trigger.
    pan: f32,
}

impl Voice {
    pub fn new(samples: Arc<Vec<f32>>, src_channels: usize) -> Self {
        Self {
            samples,
            src_channels,
            frame_pos: 0.0,
            speed: 1.0,
            active: false,
            loop_start: 0,
            loop_end: 0,
            interp_mode: InterpMode::Linear,
            volume: 1.0,
            pan: 0.0,
        }
    }

    /// Builder: set loop points.
    pub fn with_loop(mut self, loop_start: u32, loop_end: u32) -> Self {
        self.loop_start = loop_start;
        self.loop_end = loop_end;
        self
    }

    /// Builder: set interpolation mode.
    pub fn with_interp_mode(mut self, mode: InterpMode) -> Self {
        self.interp_mode = mode;
        self
    }

    /// Update loop points on an existing voice (takes effect immediately).
    pub fn set_loop_points(&mut self, start: u32, end: u32) {
        self.loop_start = start;
        self.loop_end = end;
    }

    /// Update interpolation mode (takes effect on the next rendered frame).
    pub fn set_interp_mode(&mut self, mode: InterpMode) {
        self.interp_mode = mode;
    }

    /// Re-trigger from the beginning with the given speed ratio.
    /// Volume and pan are reset to defaults (1.0 and 0.0).
    pub fn trigger(&mut self, speed: f32) {
        self.trigger_with_fx(speed, 1.0, 0.0);
    }

    /// Re-trigger with explicit FX-overridden volume and pan.
    /// `pan` is in the range −1.0 (full left) to 1.0 (full right), 0.0 = centre.
    pub fn trigger_with_fx(&mut self, speed: f32, volume: f32, pan: f32) {
        self.frame_pos = self.loop_start as f64;
        self.speed = speed as f64;
        self.active = true;
        self.volume = volume.clamp(0.0, 2.0);
        self.pan = pan.clamp(-1.0, 1.0);
    }

    pub fn stop(&mut self) {
        self.active = false;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Mix into `output` (interleaved stereo f32, length = frames * 2).
    /// Applies loop wrapping and the selected interpolation mode.
    /// Marks the voice inactive when samples are exhausted (one-shot only).
    pub fn render(&mut self, output: &mut [f32]) {
        if !self.active {
            return;
        }
        let frames = output.len() / 2;
        let chs = self.src_channels;
        let total_frames = self.samples.len() / chs;
        let loop_active = self.loop_end > self.loop_start
            && (self.loop_end as usize) <= total_frames;
        let loop_len = (self.loop_end - self.loop_start) as f64;
        let loop_end_f = self.loop_end as f64;

        // Snapshot fields that don't change per-frame to avoid repeated self-borrow.
        let interp_mode = self.interp_mode.clone();
        let speed = self.speed;

        // Get a plain slice reference once; Rust allows this alongside field mutations.
        let samples: &[f32] = &self.samples;

        // Returns sample at (frame, channel), clamped to valid range.
        let get = |f: usize, c: usize| -> f32 {
            samples[f.min(total_frames.saturating_sub(1)) * chs + c.min(chs - 1)]
        };

        for i in 0..frames {
            if loop_active {
                while self.frame_pos >= loop_end_f {
                    self.frame_pos -= loop_len;
                }
            }

            let frame0 = self.frame_pos as usize;
            if frame0 >= total_frames {
                self.active = false;
                break;
            }

            let frac = (self.frame_pos - frame0 as f64) as f32;

            let (l, r) = match interp_mode {
                InterpMode::None => {
                    let l = get(frame0, 0);
                    let r = if chs >= 2 { get(frame0, 1) } else { l };
                    (l, r)
                }
                InterpMode::Linear => {
                    let f1 = frame0 + 1;
                    let l0 = get(frame0, 0);
                    let r0 = if chs >= 2 { get(frame0, 1) } else { l0 };
                    let l1 = get(f1, 0);
                    let r1 = if chs >= 2 { get(f1, 1) } else { l1 };
                    (l0 + frac * (l1 - l0), r0 + frac * (r1 - r0))
                }
                InterpMode::Sinc => {
                    let fm1 = frame0.saturating_sub(1);
                    let f1 = frame0 + 1;
                    let f2 = frame0 + 2;
                    let il = hermite(get(fm1, 0), get(frame0, 0), get(f1, 0), get(f2, 0), frac);
                    let ir = if chs >= 2 {
                        hermite(get(fm1, 1), get(frame0, 1), get(f1, 1), get(f2, 1), frac)
                    } else {
                        il
                    };
                    (il, ir)
                }
            };

            output[i * 2] += l * self.volume * (1.0 - self.pan.max(0.0));
            output[i * 2 + 1] += r * self.volume * (1.0 + self.pan.min(0.0));
            self.frame_pos += speed;
        }
    }
}

/// Up to 8 independent voices summed to a stereo output.
pub struct Mixer {
    voices: Vec<Option<Voice>>,
}

impl Mixer {
    pub fn new() -> Self {
        let mut voices = Vec::with_capacity(8);
        for _ in 0..8 {
            voices.push(None);
        }
        Self { voices }
    }

    /// Load a voice into a slot (0..8), replacing whatever was there.
    pub fn load_slot(&mut self, slot: usize, voice: Voice) {
        assert!(slot < 8, "slot must be 0..8");
        self.voices[slot] = Some(voice);
    }

    /// Re-trigger voice in `slot` from the start at `speed`.
    pub fn trigger(&mut self, slot: usize, speed: f32) {
        if let Some(v) = self.voices.get_mut(slot).and_then(|v| v.as_mut()) {
            v.trigger(speed);
        }
    }

    /// Re-trigger voice in `slot` with FX-overridden volume and pan.
    pub fn trigger_with_fx(&mut self, slot: usize, speed: f32, volume: f32, pan: f32) {
        if let Some(v) = self.voices.get_mut(slot).and_then(|v| v.as_mut()) {
            v.trigger_with_fx(speed, volume, pan);
        }
    }

    pub fn stop_slot(&mut self, slot: usize) {
        if let Some(v) = self.voices.get_mut(slot).and_then(|v| v.as_mut()) {
            v.stop();
        }
    }

    /// Update loop points for the voice in `slot` (no-op if slot is empty).
    pub fn set_loop_points(&mut self, slot: usize, loop_start: u32, loop_end: u32) {
        if let Some(v) = self.voices.get_mut(slot).and_then(|v| v.as_mut()) {
            v.set_loop_points(loop_start, loop_end);
        }
    }

    /// Update interpolation mode for the voice in `slot` (no-op if slot is empty).
    pub fn set_interp_mode(&mut self, slot: usize, mode: InterpMode) {
        if let Some(v) = self.voices.get_mut(slot).and_then(|v| v.as_mut()) {
            v.set_interp_mode(mode);
        }
    }

    /// Returns true if any voice is currently active.
    pub fn any_active(&self) -> bool {
        self.voices.iter().flatten().any(|v| v.is_active())
    }

    /// Render `output.len()/2` frames of interleaved stereo into `output`.
    pub fn render(&mut self, output: &mut [f32]) {
        output.fill(0.0);
        for voice in self.voices.iter_mut().flatten() {
            voice.render(output);
        }
    }
}

impl Default for Mixer {
    fn default() -> Self {
        Self::new()
    }
}

// ── StepEvent ─────────────────────────────────────────────────────────────────

/// One step boundary event returned by `Sequencer::advance`.
///
/// `notes` contains `(track_index, playback_speed, fx_slots)` tuples for every
/// track that has a non-empty step at this boundary.  `fx_slots` is the raw FX
/// array from the step so the caller can apply VOL/PAN/PIT/RET without
/// needing a separate lookup.  Empty if all tracks are silent.
#[derive(Debug, Clone)]
pub struct StepEvent {
    pub step_index: u8,
    pub notes: Vec<(usize, f32, [FxSlot; FX_SLOTS_PER_STEP])>,
}

// ── TrackState ────────────────────────────────────────────────────────────────

/// Per-track position within the arrangement hierarchy.
#[derive(Debug, Clone, Default)]
struct TrackState {
    /// Current row in `Song::arrangement`.
    song_row: usize,
    /// Current slot index within the track's chain at `song_row`.
    chain_slot: usize,
}

// ── Sequencer ─────────────────────────────────────────────────────────────────

/// Sample-accurate step sequencer.
///
/// Timing is driven entirely by the audio callback's sample counter (`tick_counter`),
/// never by the OS clock, guaranteeing zero drift relative to the audio output.
///
/// Each callback invocation adds `frames_rendered` to `tick_counter`.  When
/// `tick_counter >= step_duration(current_step)` the sequencer advances to the
/// next step and subtracts the threshold from the counter.
///
/// Swing: odd steps (1, 3, 5, …) have an extended duration of
///   `samples_per_step + swing * samples_per_step`.
pub struct Sequencer {
    /// Sub-sample accumulator driven by the audio callback.
    pub tick_counter: f64,
    /// Current step index (0–15), shared across all 8 tracks.
    pub step_index: u8,
    /// Beats per minute.
    pub bpm: f32,
    /// Audio sample rate in Hz.
    pub sample_rate: f32,
    /// Swing factor (0.0 = straight, 1.0 = maximum late-shift on odd steps).
    pub swing: f32,
    /// Steps per beat (default 4 for 16th-note steps in 4/4).
    pub steps_per_beat: f32,
    /// Whether the sequencer is currently running.
    pub playing: bool,
    /// Legacy single-track phrase (used when arrangement is empty).
    phrase: Option<Box<Phrase>>,
    /// Root note fallback for legacy single-track mode.
    pub sample_root: u8,
    /// Per-track position state (8 tracks, all advance in step-lock).
    track_states: Vec<TrackState>,
    /// Snapshot of the song arrangement (from `UpdateSongData`).
    arrangement: Vec<[Option<u8>; TRACKS]>,
    /// Snapshot of all chains.
    chains: Vec<Chain>,
    /// Snapshot of all phrases (indexed by ChainSlot::phrase).
    phrases_data: Vec<Phrase>,
    /// Snapshot of all instruments (for root-note lookup).
    instruments: Vec<Instrument>,
}

impl Sequencer {
    pub fn new(sample_rate: f32, bpm: f32) -> Self {
        Self {
            tick_counter: 0.0,
            step_index: 0,
            bpm,
            sample_rate,
            swing: 0.0,
            steps_per_beat: 4.0,
            playing: false,
            phrase: None,
            sample_root: 60,
            track_states: vec![TrackState::default(); TRACKS],
            arrangement: Vec::new(),
            chains: Vec::new(),
            phrases_data: Vec::new(),
            instruments: Vec::new(),
        }
    }

    /// Duration of one step in samples (fractional, no swing applied).
    pub fn samples_per_step(&self) -> f64 {
        self.sample_rate as f64 * 60.0 / (self.bpm as f64 * self.steps_per_beat as f64)
    }

    /// Effective duration of `step_idx` in samples (swing applied to odd steps).
    pub fn step_duration_for(&self, step_idx: u8) -> f64 {
        let sps = self.samples_per_step();
        if step_idx % 2 == 1 {
            sps + sps * self.swing as f64
        } else {
            sps
        }
    }

    fn current_step_duration(&self) -> f64 {
        self.step_duration_for(self.step_index)
    }

    pub fn set_phrase(&mut self, phrase: Box<Phrase>) {
        self.phrase = Some(phrase);
    }

    /// Receive a song-data snapshot from the UI thread for arrangement playback.
    pub fn update_song_data(
        &mut self,
        arrangement: Vec<[Option<u8>; TRACKS]>,
        chains: Vec<Chain>,
        phrases: Vec<Phrase>,
        instruments: Vec<Instrument>,
    ) {
        self.arrangement = arrangement;
        self.chains = chains;
        self.phrases_data = phrases;
        self.instruments = instruments;
    }

    /// Resolve the playback speed for `track` at `step_idx`, using the arrangement
    /// if available, otherwise falling back to the legacy single-phrase mode.
    fn resolve_track_speed(&self, track: usize, step_idx: u8) -> Option<f32> {
        if self.arrangement.is_empty() {
            // Legacy mode: only track 0 uses the single phrase.
            if track != 0 {
                return None;
            }
            return self.phrase.as_ref().and_then(|p| {
                let step = &p.steps[step_idx as usize];
                step.note.map(|note| {
                    let delta = note as i32 - self.sample_root as i32;
                    2.0_f64.powf(delta as f64 / 12.0) as f32
                })
            });
        }

        let ts = &self.track_states[track];
        let chain_idx = self.arrangement.get(ts.song_row)?.get(track)?.as_ref()?;
        let chain = self.chains.get(*chain_idx as usize)?;
        let slot = chain.slots.get(ts.chain_slot)?;
        let phrase = self.phrases_data.get(slot.phrase as usize)?;
        let step = &phrase.steps[step_idx as usize];
        step.note.map(|note| {
            let root = step
                .instrument
                .and_then(|i| self.instruments.get(i as usize))
                .map(|instr| instr.root_note)
                .unwrap_or(self.sample_root);
            let delta = note as i32 + slot.transpose as i32 - root as i32;
            2.0_f64.powf(delta as f64 / 12.0) as f32
        })
    }

    /// Return the FX slots for `track` at `step_idx`, or a default (all-zero) array
    /// if the track has no note at this step.
    fn resolve_track_fx(&self, track: usize, step_idx: u8) -> [FxSlot; FX_SLOTS_PER_STEP] {
        if self.arrangement.is_empty() {
            if track != 0 {
                return Default::default();
            }
            return self
                .phrase
                .as_ref()
                .map(|p| p.steps[step_idx as usize].fx.clone())
                .unwrap_or_default();
        }
        let ts = &self.track_states[track];
        let chain_idx = match self.arrangement.get(ts.song_row).and_then(|r| r[track]) {
            Some(ci) => ci,
            None => return Default::default(),
        };
        let chain = match self.chains.get(chain_idx as usize) {
            Some(c) => c,
            None => return Default::default(),
        };
        let slot = match chain.slots.get(ts.chain_slot) {
            Some(s) => s,
            None => return Default::default(),
        };
        match self.phrases_data.get(slot.phrase as usize) {
            Some(p) => p.steps[step_idx as usize].fx.clone(),
            None => Default::default(),
        }
    }

    /// Advance all track states when a phrase boundary is crossed (step 15 → 0).
    fn advance_track_states(&mut self) {
        if self.arrangement.is_empty() {
            return;
        }
        for track in 0..TRACKS {
            let ts = &mut self.track_states[track];
            ts.chain_slot += 1;

            // Check if the chain's slots are exhausted for this track.
            let chain_exhausted = self
                .arrangement
                .get(ts.song_row)
                .and_then(|row| row[track])
                .and_then(|ci| self.chains.get(ci as usize))
                .map(|c| ts.chain_slot >= c.slots.len())
                .unwrap_or(true);

            if chain_exhausted {
                ts.chain_slot = 0;
                // Advance (and wrap) the song row.
                if !self.arrangement.is_empty() {
                    ts.song_row = (ts.song_row + 1) % self.arrangement.len();
                }
            }
        }
    }

    /// Reset all track positions to the beginning of the arrangement.
    fn reset_track_states(&mut self) {
        for ts in &mut self.track_states {
            ts.song_row = 0;
            ts.chain_slot = 0;
        }
    }

    /// Start playing from the current step.
    /// Returns `(track, speed)` pairs for every non-empty track at the current step.
    pub fn play(&mut self) -> Vec<(usize, f32)> {
        self.playing = true;
        self.tick_counter = 0.0;
        let si = self.step_index;
        (0..TRACKS).filter_map(|t| self.resolve_track_speed(t, si).map(|s| (t, s))).collect()
    }

    /// Restart from step 0, resetting all track positions.
    /// Returns `(track, speed)` pairs for every non-empty track at step 0.
    pub fn restart(&mut self) -> Vec<(usize, f32)> {
        self.playing = true;
        self.step_index = 0;
        self.tick_counter = 0.0;
        self.reset_track_states();
        (0..TRACKS).filter_map(|t| self.resolve_track_speed(t, 0).map(|s| (t, s))).collect()
    }

    pub fn stop(&mut self) {
        self.playing = false;
    }

    /// Returns a [`StepEvent`] for the current `step_index` without advancing.
    ///
    /// Used by the offline renderer to trigger the initial notes at step 0
    /// before the first call to [`advance`].
    pub fn current_step_notes(&self) -> StepEvent {
        let si = self.step_index;
        let notes = (0..TRACKS)
            .filter_map(|t| {
                self.resolve_track_speed(t, si)
                    .map(|s| (t, s, self.resolve_track_fx(t, si)))
            })
            .collect();
        StepEvent { step_index: si, notes }
    }

    /// Advance the sequencer by `frames` audio samples.
    ///
    /// Returns one `StepEvent` for each step boundary crossed.  Each event
    /// contains the step index and the `(track, speed)` pairs for non-empty tracks.
    pub fn advance(&mut self, frames: usize) -> Vec<StepEvent> {
        if !self.playing {
            return vec![];
        }

        let mut events = Vec::new();
        self.tick_counter += frames as f64;

        loop {
            let threshold = self.current_step_duration();
            if self.tick_counter >= threshold {
                self.tick_counter -= threshold;
                let prev_step = self.step_index;
                self.step_index = (self.step_index + 1) % STEPS_PER_PHRASE as u8;

                // When we wrap back to step 0 we've completed a phrase cycle —
                // advance all track states to the next chain slot / song row.
                if self.step_index == 0 && prev_step == STEPS_PER_PHRASE as u8 - 1 {
                    self.advance_track_states();
                }

                let si = self.step_index;
                let notes: Vec<(usize, f32, [FxSlot; FX_SLOTS_PER_STEP])> = (0..TRACKS)
                    .filter_map(|t| {
                        self.resolve_track_speed(t, si)
                            .map(|s| (t, s, self.resolve_track_fx(t, si)))
                    })
                    .collect();
                events.push(StepEvent { step_index: si, notes });
            } else {
                break;
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::TAU;

    fn sine_buffer(num_frames: usize) -> Arc<Vec<f32>> {
        let samples: Vec<f32> = (0..num_frames)
            .map(|i| (TAU * 440.0 * i as f32 / 44100.0).sin())
            .collect();
        Arc::new(samples)
    }

    #[test]
    fn mixer_renders_non_silent_sine() {
        const FRAMES: usize = 512;
        let buf = sine_buffer(FRAMES);
        let voice = Voice::new(buf, 1);
        let mut mixer = Mixer::new();
        mixer.load_slot(0, voice);
        mixer.trigger(0, 1.0);

        let mut output = vec![0.0f32; FRAMES * 2];
        mixer.render(&mut output);

        let rms: f32 = output.iter().map(|s| s * s).sum::<f32>() / output.len() as f32;
        assert!(rms > 0.0, "output should be non-silent, got rms={rms}");
    }

    #[test]
    fn voice_retrigger_resets_position() {
        const FRAMES: usize = 4;
        let buf = Arc::new(vec![1.0f32, 2.0, 3.0, 4.0]);
        let mut voice = Voice::new(buf, 1);

        voice.trigger(1.0);
        let mut out = vec![0.0f32; FRAMES * 2];
        voice.render(&mut out);
        let first_run: Vec<f32> = out.iter().step_by(2).copied().collect();

        voice.trigger(1.0);
        let mut out2 = vec![0.0f32; FRAMES * 2];
        voice.render(&mut out2);
        let second_run: Vec<f32> = out2.iter().step_by(2).copied().collect();

        assert_eq!(first_run, second_run);
    }

    #[test]
    fn voice_goes_inactive_after_consuming_all_samples() {
        let buf = Arc::new(vec![0.5f32; 8]);
        let mut voice = Voice::new(buf, 1);
        voice.trigger(1.0);
        let mut out = vec![0.0f32; 512];
        voice.render(&mut out);
        assert!(!voice.is_active());
    }

    #[test]
    fn voice_double_speed_consumes_samples_twice_as_fast() {
        // 64 mono frames of silence. At 2× speed we should exhaust it in ~32 output frames.
        let buf = Arc::new(vec![0.0f32; 64]);
        let mut voice = Voice::new(buf, 1);
        voice.trigger(2.0);
        let mut out = vec![0.0f32; 40 * 2]; // 40 output frames
        voice.render(&mut out);
        assert!(!voice.is_active(), "2× speed should exhaust 64-frame buffer in ~32 output frames");
    }

    #[test]
    fn pitch_speed_octave_up_is_double() {
        let root: u8 = 60;
        let note: u8 = 72; // one octave up
        let delta = note as i32 - root as i32;
        let speed = 2.0_f64.powf(delta as f64 / 12.0) as f32;
        assert!((speed - 2.0).abs() < 1e-4, "octave up should be 2×, got {speed}");
    }

    #[test]
    fn pitch_speed_octave_down_is_half() {
        let root: u8 = 60;
        let note: u8 = 48; // one octave down
        let delta = note as i32 - root as i32;
        let speed = 2.0_f64.powf(delta as f64 / 12.0) as f32;
        assert!((speed - 0.5).abs() < 1e-4, "octave down should be 0.5×, got {speed}");
    }

    // ── Sequencer tests ───────────────────────────────────────────────────────

    fn make_seq_with_all_notes(bpm: f32, sample_rate: f32) -> Sequencer {
        let mut seq = Sequencer::new(sample_rate, bpm);
        let mut phrase = crate::model::Phrase::default();
        for step in &mut phrase.steps {
            step.note = Some(60);
        }
        seq.set_phrase(Box::new(phrase));
        seq.sample_root = 60;
        seq
    }

    /// At 120 BPM, 4 steps/beat, 48 kHz:
    ///   samples_per_step = 48000 * 60 / (120 * 4) = 6000
    ///   16 steps × 6000 = 96000 samples = exactly 2 seconds.
    #[test]
    fn sequencer_16_steps_complete_in_2_seconds_at_120_bpm() {
        let mut seq = make_seq_with_all_notes(120.0, 48000.0);
        seq.restart();
        let events = seq.advance(96000);
        assert_eq!(events.len(), 16, "expected 16 step advances in 96000 samples");
        assert_eq!(seq.step_index, 0, "should have wrapped back to step 0");
        assert!(
            seq.tick_counter.abs() < 1e-6,
            "tick_counter should be ~0 after exact multiple of sps, got {}",
            seq.tick_counter
        );
    }

    /// Sample-counter driven timing accumulates zero drift over 100 bars
    /// (1600 steps at 120 BPM = 9,600,000 samples).
    #[test]
    fn sequencer_no_drift_over_100_bars() {
        let mut seq = make_seq_with_all_notes(120.0, 48000.0);
        let sps = seq.samples_per_step();
        let total_steps: usize = 100 * STEPS_PER_PHRASE;
        let total_samples = (sps * total_steps as f64).round() as usize;

        seq.restart();
        let events = seq.advance(total_samples);

        assert_eq!(
            events.len(),
            total_steps,
            "expected {total_steps} step events over {total_samples} samples"
        );
        assert_eq!(seq.step_index, 0, "should be back at step 0 with no drift");
        assert!(
            seq.tick_counter < 1.0,
            "sub-sample drift only: tick_counter = {}",
            seq.tick_counter
        );
    }

    /// With 50% swing, odd steps fire `swing_samples = 0.5 * sps` late.
    /// At 120 BPM / 48 kHz: sps = 6000, swing_offset = 3000.
    #[test]
    fn sequencer_swing_50_percent_delays_odd_steps() {
        let mut seq = make_seq_with_all_notes(120.0, 48000.0);
        seq.swing = 0.5;

        let sps = seq.samples_per_step() as usize;
        let swing_offset = (seq.samples_per_step() * 0.5) as usize;

        seq.restart();

        let e1 = seq.advance(sps);
        assert_eq!(e1.len(), 1, "step 1 should fire after sps frames");
        assert_eq!(e1[0].step_index, 1, "first fired step should be 1");

        let e_not_yet = seq.advance(sps + swing_offset - 1);
        assert_eq!(e_not_yet.len(), 0, "step 2 should not fire before the odd-step threshold");

        let e2 = seq.advance(1);
        assert_eq!(e2.len(), 1, "step 2 should fire on the threshold frame");
        assert_eq!(e2[0].step_index, 2, "fired step should be 2");
    }

    #[test]
    fn sequencer_empty_steps_produce_no_note_events() {
        let mut seq = Sequencer::new(48000.0, 120.0);
        let phrase = crate::model::Phrase::default();
        seq.set_phrase(Box::new(phrase));
        seq.restart();

        let events = seq.advance(96000);
        assert_eq!(events.len(), 16, "step-advance count should still be 16");
        for event in &events {
            assert!(event.notes.is_empty(), "empty steps must not produce NoteOn events");
        }
    }

    #[test]
    fn sequencer_bpm_change_while_playing_takes_effect_immediately() {
        let mut seq = make_seq_with_all_notes(120.0, 48000.0);
        seq.restart();

        let _ = seq.advance(6000);

        seq.bpm = 240.0;
        let events = seq.advance(3000);
        assert_eq!(events.len(), 1, "at 240 BPM one step should fire in 3000 frames");
    }

    #[test]
    fn sequencer_stop_produces_no_events() {
        let mut seq = make_seq_with_all_notes(120.0, 48000.0);
        seq.restart();
        seq.stop();

        let events = seq.advance(96000);
        assert!(events.is_empty(), "stopped sequencer should not fire events");
    }

    #[test]
    fn sequencer_multi_track_arrangement_fires_on_correct_tracks() {
        use crate::model::{Chain, ChainSlot, Phrase, TRACKS};

        let mut seq = Sequencer::new(48000.0, 120.0);

        // Two phrases: phrase 0 has note 60 on step 0; phrase 1 has note 72 on step 0.
        let mut p0 = Phrase::default();
        p0.steps[0].note = Some(60);
        let mut p1 = Phrase::default();
        p1.steps[0].note = Some(72);
        let phrases = vec![p0, p1];

        // Chain 0 → phrase 0; Chain 1 → phrase 1.
        let chains = vec![
            Chain { slots: vec![ChainSlot { phrase: 0, transpose: 0 }] },
            Chain { slots: vec![ChainSlot { phrase: 1, transpose: 0 }] },
        ];

        // Arrangement: track 0 → chain 0, track 1 → chain 1, others silent.
        let mut row = [None; TRACKS];
        row[0] = Some(0);
        row[1] = Some(1);
        let arrangement = vec![row];

        seq.update_song_data(arrangement, chains, phrases, vec![]);
        seq.sample_root = 60;
        let initial = seq.restart();

        // Step 0: track 0 fires at speed 1.0, track 1 fires at speed 2.0.
        let t0_speed = initial.iter().find(|&&(t, _)| t == 0).map(|&(_, s)| s);
        let t1_speed = initial.iter().find(|&&(t, _)| t == 1).map(|&(_, s)| s);
        assert!(t0_speed.is_some(), "track 0 should fire on step 0");
        assert!((t0_speed.unwrap() - 1.0).abs() < 1e-4, "track 0 speed should be 1.0");
        assert!(t1_speed.is_some(), "track 1 should fire on step 0");
        assert!((t1_speed.unwrap() - 2.0).abs() < 1e-4, "track 1 speed should be 2.0 (note 72, root 60)");
    }

    #[test]
    fn sequencer_transpose_shifts_pitch() {
        use crate::model::{Chain, ChainSlot, Phrase, TRACKS};

        let mut seq = Sequencer::new(48000.0, 120.0);

        let mut p = Phrase::default();
        p.steps[0].note = Some(60); // C4, speed 1.0 at root 60

        let chains = vec![Chain {
            slots: vec![ChainSlot { phrase: 0, transpose: 12 }], // +1 octave
        }];
        let mut row = [None; TRACKS];
        row[0] = Some(0);
        let arrangement = vec![row];

        seq.update_song_data(arrangement, chains, vec![p], vec![]);
        seq.sample_root = 60;
        let initial = seq.restart();

        let t0_speed = initial.iter().find(|&&(t, _)| t == 0).map(|&(_, s)| s);
        assert!(t0_speed.is_some());
        // note=60, transpose=12, root=60 → delta=12 → speed=2.0
        assert!((t0_speed.unwrap() - 2.0).abs() < 1e-4, "transpose +12 should double speed");
    }

    // ── Loop point tests ──────────────────────────────────────────────────────

    #[test]
    fn voice_loops_when_loop_end_greater_than_loop_start() {
        // 8 mono frames; loop region = frames 2..6 (length 4).
        let buf = Arc::new(vec![0.0f32, 0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0]);
        let mut voice = Voice::new(buf, 1)
            .with_loop(2, 6)
            .with_interp_mode(crate::model::InterpMode::None);

        voice.trigger(1.0);
        let mut out = vec![0.0f32; 20 * 2]; // render 20 frames
        voice.render(&mut out);

        // Voice should still be active (looping indefinitely).
        assert!(voice.is_active(), "looping voice should still be active after 20 frames");
    }

    #[test]
    fn voice_one_shot_when_loop_end_equals_loop_start() {
        // 8 mono frames; loop_end == loop_start → one-shot.
        let buf = Arc::new(vec![0.5f32; 8]);
        let mut voice = Voice::new(buf, 1).with_loop(4, 4); // equal → one-shot

        voice.trigger(1.0);
        let mut out = vec![0.0f32; 16 * 2];
        voice.render(&mut out);

        assert!(!voice.is_active(), "one-shot voice (loop_end == loop_start) should go inactive");
    }

    #[test]
    fn voice_one_shot_when_both_loop_points_zero() {
        let buf = Arc::new(vec![0.5f32; 8]);
        let mut voice = Voice::new(buf, 1).with_loop(0, 0); // both 0 → one-shot

        voice.trigger(1.0);
        let mut out = vec![0.0f32; 16 * 2];
        voice.render(&mut out);

        assert!(!voice.is_active(), "one-shot voice (both loop points 0) should go inactive");
    }

    #[test]
    fn voice_interp_none_produces_output() {
        let buf = sine_buffer(512);
        let mut voice = Voice::new(buf, 1)
            .with_interp_mode(crate::model::InterpMode::None);
        voice.trigger(1.0);

        let mut out = vec![0.0f32; 256 * 2];
        voice.render(&mut out);

        let rms: f32 = out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32;
        assert!(rms > 0.0, "Nearest interp should produce non-silent output");
    }

    #[test]
    fn voice_interp_sinc_produces_output() {
        let buf = sine_buffer(512);
        let mut voice = Voice::new(buf, 1)
            .with_interp_mode(crate::model::InterpMode::Sinc);
        voice.trigger(1.0);

        let mut out = vec![0.0f32; 256 * 2];
        voice.render(&mut out);

        let rms: f32 = out.iter().map(|s| s * s).sum::<f32>() / out.len() as f32;
        assert!(rms > 0.0, "Sinc (Hermite) interp should produce non-silent output");
    }

    #[test]
    fn voice_set_interp_mode_takes_effect_immediately() {
        // Render with Linear then Sinc — both should produce non-silent output
        // (just verifying the method call doesn't panic and voice stays active).
        let buf = sine_buffer(1024);
        let mut voice = Voice::new(buf, 1);
        voice.trigger(1.0);

        let mut out1 = vec![0.0f32; 128 * 2];
        voice.render(&mut out1);

        voice.set_interp_mode(crate::model::InterpMode::Sinc);

        let mut out2 = vec![0.0f32; 128 * 2];
        voice.render(&mut out2);

        let rms: f32 = out2.iter().map(|s| s * s).sum::<f32>() / out2.len() as f32;
        assert!(rms > 0.0, "Voice after interp mode change should still produce output");
    }

    // ── FX slot tests ─────────────────────────────────────────────────────────

    #[test]
    fn fx_command_round_trip_id() {
        use crate::model::FxCommand;
        for (cmd, expected_id) in [
            (FxCommand::Vol, 1u8),
            (FxCommand::Pan, 2),
            (FxCommand::Pit, 3),
            (FxCommand::Ret, 4),
        ] {
            assert_eq!(cmd.id(), expected_id);
            assert_eq!(FxCommand::from_id(expected_id), Some(cmd));
        }
        assert_eq!(FxCommand::from_id(0), None);
        assert_eq!(FxCommand::from_id(255), None);
    }

    #[test]
    fn fx_command_round_trip_code() {
        use crate::model::FxCommand;
        for code in ["VOL", "PAN", "PIT", "RET"] {
            let cmd = FxCommand::from_code(code).expect("known code must parse");
            assert_eq!(cmd.to_code(), code);
        }
        assert_eq!(FxCommand::from_code("XYZ"), None);
        assert!(FxCommand::from_code("vol").is_some(), "should be case-insensitive");
    }

    #[test]
    fn voice_vol_fx_scales_output() {
        let buf = sine_buffer(1024);
        let mut voice_full = Voice::new(buf.clone(), 1);
        let mut voice_half = Voice::new(buf, 1);

        voice_full.trigger_with_fx(1.0, 1.0, 0.0);
        voice_half.trigger_with_fx(1.0, 0.5, 0.0);

        let mut out_full = vec![0.0f32; 512 * 2];
        let mut out_half = vec![0.0f32; 512 * 2];
        voice_full.render(&mut out_full);
        voice_half.render(&mut out_half);

        let rms_full: f32 =
            (out_full.iter().map(|s| s * s).sum::<f32>() / out_full.len() as f32).sqrt();
        let rms_half: f32 =
            (out_half.iter().map(|s| s * s).sum::<f32>() / out_half.len() as f32).sqrt();
        let ratio = rms_half / rms_full;
        assert!(
            (ratio - 0.5).abs() < 0.01,
            "half-volume should give ~0.5× RMS, got ratio={ratio}"
        );
    }

    #[test]
    fn voice_vol_fx_above_unity_amplifies_output() {
        let buf = sine_buffer(1024);
        let mut voice_full = Voice::new(buf.clone(), 1);
        let mut voice_loud = Voice::new(buf, 1);

        voice_full.trigger_with_fx(1.0, 1.0, 0.0);
        voice_loud.trigger_with_fx(1.0, 1.5, 0.0);

        let mut out_full = vec![0.0f32; 512 * 2];
        let mut out_loud = vec![0.0f32; 512 * 2];
        voice_full.render(&mut out_full);
        voice_loud.render(&mut out_loud);

        let rms_full: f32 =
            (out_full.iter().map(|s| s * s).sum::<f32>() / out_full.len() as f32).sqrt();
        let rms_loud: f32 =
            (out_loud.iter().map(|s| s * s).sum::<f32>() / out_loud.len() as f32).sqrt();
        let ratio = rms_loud / rms_full;
        assert!(
            (ratio - 1.5).abs() < 0.01,
            "volume=1.5 should give ~1.5× RMS, got ratio={ratio}"
        );
    }

    #[test]
    fn voice_pan_left_silences_right_channel() {
        let buf = sine_buffer(1024);
        let mut voice = Voice::new(buf, 1);
        voice.trigger_with_fx(1.0, 1.0, -1.0); // full left

        let mut out = vec![0.0f32; 512 * 2];
        voice.render(&mut out);

        let right_rms: f32 = out.iter().skip(1).step_by(2).map(|s| s * s).sum::<f32>()
            / (out.len() / 2) as f32;
        assert!(
            right_rms < 1e-6,
            "full-left pan should silence right channel, got rms={right_rms}"
        );
    }

    #[test]
    fn voice_pan_right_silences_left_channel() {
        let buf = sine_buffer(1024);
        let mut voice = Voice::new(buf, 1);
        voice.trigger_with_fx(1.0, 1.0, 1.0); // full right

        let mut out = vec![0.0f32; 512 * 2];
        voice.render(&mut out);

        let left_rms: f32 = out.iter().step_by(2).map(|s| s * s).sum::<f32>()
            / (out.len() / 2) as f32;
        assert!(
            left_rms < 1e-6,
            "full-right pan should silence left channel, got rms={left_rms}"
        );
    }

    #[test]
    fn sequencer_fx_slots_included_in_step_event() {
        use crate::model::{FxSlot, Phrase};

        let mut seq = Sequencer::new(48000.0, 120.0);
        let mut phrase = Phrase::default();
        phrase.steps[1].note = Some(60);
        phrase.steps[1].fx[0] = FxSlot { command: 1, value: 128 }; // VOL=128
        seq.set_phrase(Box::new(phrase));
        seq.sample_root = 60;
        seq.restart();

        // Advance to step 1 (6000 frames at 120 BPM / 48 kHz).
        let events = seq.advance(6000);
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.step_index, 1);
        assert_eq!(ev.notes.len(), 1);
        let (track, _speed, fx) = &ev.notes[0];
        assert_eq!(*track, 0);
        assert_eq!(fx[0].command, 1, "FX command should be preserved");
        assert_eq!(fx[0].value, 128, "FX value should be preserved");
    }

    #[test]
    fn sequencer_empty_fx_slots_are_zeroed() {
        use crate::model::Phrase;

        let mut seq = Sequencer::new(48000.0, 120.0);
        let mut phrase = Phrase::default();
        phrase.steps[1].note = Some(60); // no FX
        seq.set_phrase(Box::new(phrase));
        seq.sample_root = 60;
        seq.restart();

        let events = seq.advance(6000);
        let (_, _, fx) = &events[0].notes[0];
        for slot in fx.iter() {
            assert_eq!(slot.command, 0, "empty FX slot command should be 0");
            assert_eq!(slot.value, 0, "empty FX slot value should be 0");
        }
    }
}
