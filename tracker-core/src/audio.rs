use std::sync::Arc;

use crate::model::{Phrase, STEPS_PER_PHRASE};

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
}

/// A single playing voice backed by an in-memory f32 sample buffer.
///
/// The buffer is interleaved (same channel layout as the source WAV).
/// Output is always summed into a stereo (2-channel) interleaved slice.
/// Pitch shifting is achieved by advancing `frame_pos` by `speed` per output
/// frame, with linear interpolation between adjacent source frames.
pub struct Voice {
    samples: Arc<Vec<f32>>,
    src_channels: usize,
    /// Fractional frame position for pitch shifting via linear interpolation.
    frame_pos: f64,
    speed: f64,
    active: bool,
}

impl Voice {
    pub fn new(samples: Arc<Vec<f32>>, src_channels: usize) -> Self {
        Self {
            samples,
            src_channels,
            frame_pos: 0.0,
            speed: 1.0,
            active: false,
        }
    }

    /// Re-trigger from the beginning with the given speed ratio.
    pub fn trigger(&mut self, speed: f32) {
        self.frame_pos = 0.0;
        self.speed = speed as f64;
        self.active = true;
    }

    pub fn stop(&mut self) {
        self.active = false;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Mix into `output` (interleaved stereo f32, length = frames * 2).
    /// Uses linear interpolation when speed != 1.0.
    /// Marks the voice inactive when all source samples are consumed.
    pub fn render(&mut self, output: &mut [f32]) {
        if !self.active {
            return;
        }
        let frames = output.len() / 2;
        let total_frames = self.samples.len() / self.src_channels;

        for i in 0..frames {
            let frame0 = self.frame_pos as usize;
            if frame0 >= total_frames {
                self.active = false;
                break;
            }

            let frac = (self.frame_pos - frame0 as f64) as f32;
            let frame1 = (frame0 + 1).min(total_frames.saturating_sub(1));

            let src0 = frame0 * self.src_channels;
            let src1 = frame1 * self.src_channels;

            let l0 = self.samples[src0];
            let r0 = if self.src_channels >= 2 { self.samples[src0 + 1] } else { l0 };
            let l1 = self.samples[src1];
            let r1 = if self.src_channels >= 2 { self.samples[src1 + 1] } else { l1 };

            output[i * 2] += l0 + frac * (l1 - l0);
            output[i * 2 + 1] += r0 + frac * (r1 - r0);

            self.frame_pos += self.speed;
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

    pub fn stop_slot(&mut self, slot: usize) {
        if let Some(v) = self.voices.get_mut(slot).and_then(|v| v.as_mut()) {
            v.stop();
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
    /// Current step index (0–15).
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
    phrase: Option<Box<Phrase>>,
    /// Root note of the loaded sample for pitch-speed calculation.
    pub sample_root: u8,
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

    /// Speed ratio for the given step, or None if the step is empty.
    fn speed_for_step(&self, step_idx: u8) -> Option<f32> {
        self.phrase.as_ref().and_then(|p| {
            let step = &p.steps[step_idx as usize];
            step.note.map(|note| {
                let delta = note as i32 - self.sample_root as i32;
                2.0_f64.powf(delta as f64 / 12.0) as f32
            })
        })
    }

    /// Start playing from the current step.
    /// Returns the playback speed if step 0 is non-empty, so the caller can
    /// immediately trigger the mixer.
    pub fn play(&mut self) -> Option<f32> {
        self.playing = true;
        self.tick_counter = 0.0;
        self.speed_for_step(self.step_index)
    }

    /// Restart from step 0.
    pub fn restart(&mut self) -> Option<f32> {
        self.playing = true;
        self.step_index = 0;
        self.tick_counter = 0.0;
        self.speed_for_step(0)
    }

    pub fn stop(&mut self) {
        self.playing = false;
    }

    /// Advance the sequencer by `frames` audio samples.
    ///
    /// Returns a list of `(step_index, Option<speed>)` events that fired.
    /// `speed = Some(s)` means a non-empty step fired with playback speed `s`.
    /// `speed = None`  means the step was empty (silence).
    pub fn advance(&mut self, frames: usize) -> Vec<(u8, Option<f32>)> {
        if !self.playing {
            return vec![];
        }

        let mut events = Vec::new();
        self.tick_counter += frames as f64;

        loop {
            let threshold = self.current_step_duration();
            if self.tick_counter >= threshold {
                self.tick_counter -= threshold;
                self.step_index = (self.step_index + 1) % STEPS_PER_PHRASE as u8;
                let speed = self.speed_for_step(self.step_index);
                events.push((self.step_index, speed));
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
        seq.restart(); // fires step 0
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
        let sps = seq.samples_per_step(); // 6000.0 exactly
        let total_steps: usize = 100 * STEPS_PER_PHRASE; // 1600
        let total_samples = (sps * total_steps as f64).round() as usize; // 9,600,000

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

        let sps = seq.samples_per_step() as usize; // 6000
        let swing_offset = (seq.samples_per_step() * 0.5) as usize; // 3000

        seq.restart(); // fires step 0; step_index = 0, tick_counter = 0

        // Step 0 is even → its duration is `sps`.  Step 1 fires after exactly sps frames.
        let e1 = seq.advance(sps);
        assert_eq!(e1.len(), 1, "step 1 should fire after sps frames");
        assert_eq!(e1[0].0, 1, "first fired step should be 1");

        // Step 1 is odd → its duration is sps + swing_offset = 9000.
        // One frame before the threshold: nothing fires.
        let e_not_yet = seq.advance(sps + swing_offset - 1);
        assert_eq!(
            e_not_yet.len(),
            0,
            "step 2 should not fire before the odd-step threshold"
        );
        // The final frame crosses the threshold.
        let e2 = seq.advance(1);
        assert_eq!(e2.len(), 1, "step 2 should fire on the threshold frame");
        assert_eq!(e2[0].0, 2, "fired step should be 2");
    }

    #[test]
    fn sequencer_empty_steps_produce_no_note_events() {
        let mut seq = Sequencer::new(48000.0, 120.0);
        // phrase with all empty steps
        let phrase = crate::model::Phrase::default();
        seq.set_phrase(Box::new(phrase));
        seq.restart();

        let events = seq.advance(96000);
        assert_eq!(events.len(), 16, "step-advance count should still be 16");
        for (_, speed) in &events {
            assert!(speed.is_none(), "empty steps must not produce NoteOn events");
        }
    }

    #[test]
    fn sequencer_bpm_change_while_playing_takes_effect_immediately() {
        let mut seq = make_seq_with_all_notes(120.0, 48000.0);
        seq.restart();

        // Advance one step at 120 BPM (sps = 6000).
        let _ = seq.advance(6000);

        // Change to 240 BPM mid-playback (sps becomes 3000).
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
}
