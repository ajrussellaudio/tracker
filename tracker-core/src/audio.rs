use std::sync::Arc;

/// Commands sent from the UI thread to the audio thread via a ring buffer.
#[derive(Clone, Copy, Debug)]
pub enum Command {
    /// Trigger slot N from the start at the given playback speed ratio.
    /// speed = 1.0 → root pitch, 2.0 → one octave up, 0.5 → one octave down.
    NoteOn { slot: u8, speed: f32 },
    /// Stop slot N immediately.
    NoteOff(u8),
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
}
