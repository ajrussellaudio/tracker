use std::sync::Arc;

/// Commands sent from the UI thread to the audio thread via a ring buffer.
#[derive(Clone, Copy, Debug)]
pub enum Command {
    /// Trigger slot N from the start (re-triggers if already playing).
    NoteOn(u8),
    /// Stop slot N immediately.
    NoteOff(u8),
}

/// A single playing voice backed by an in-memory f32 sample buffer.
///
/// The buffer is interleaved (same channel layout as the source WAV).
/// Output is always summed into a stereo (2-channel) interleaved slice.
pub struct Voice {
    samples: Arc<Vec<f32>>,
    src_channels: usize,
    /// Current frame index (not sample index).
    frame_pos: usize,
    active: bool,
}

impl Voice {
    pub fn new(samples: Arc<Vec<f32>>, src_channels: usize) -> Self {
        Self {
            samples,
            src_channels,
            frame_pos: 0,
            active: false,
        }
    }

    /// Re-trigger from the beginning (monophonic).
    pub fn trigger(&mut self) {
        self.frame_pos = 0;
        self.active = true;
    }

    pub fn stop(&mut self) {
        self.active = false;
    }

    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Mix into `output` (interleaved stereo f32, length = frames * 2).
    /// Marks the voice inactive when all samples are consumed.
    pub fn render(&mut self, output: &mut [f32]) {
        if !self.active {
            return;
        }
        let frames = output.len() / 2;
        let total_frames = self.samples.len() / self.src_channels;

        for i in 0..frames {
            if self.frame_pos >= total_frames {
                self.active = false;
                break;
            }
            let src_base = self.frame_pos * self.src_channels;
            let l = self.samples[src_base];
            let r = if self.src_channels >= 2 {
                self.samples[src_base + 1]
            } else {
                l // mono → duplicate to both channels
            };
            output[i * 2] += l;
            output[i * 2 + 1] += r;
            self.frame_pos += 1;
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

    /// Re-trigger voice in `slot` from the start.
    pub fn trigger(&mut self, slot: usize) {
        if let Some(v) = self.voices.get_mut(slot).and_then(|v| v.as_mut()) {
            v.trigger();
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

    /// Build a 440 Hz mono sine-wave buffer at 44 100 Hz for `num_frames` frames.
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
        mixer.trigger(0);

        let mut output = vec![0.0f32; FRAMES * 2];
        mixer.render(&mut output);

        let rms: f32 = output.iter().map(|s| s * s).sum::<f32>() / output.len() as f32;
        assert!(rms > 0.0, "output should be non-silent, got rms={rms}");
    }

    #[test]
    fn voice_retrigger_resets_position() {
        const FRAMES: usize = 4;
        // 4 mono samples: [1, 2, 3, 4]
        let buf = Arc::new(vec![1.0f32, 2.0, 3.0, 4.0]);
        let mut voice = Voice::new(buf, 1);

        voice.trigger();
        let mut out = vec![0.0f32; FRAMES * 2];
        voice.render(&mut out);
        // L channel: [1, 2, 3, 4]
        let first_run: Vec<f32> = out.iter().step_by(2).copied().collect();

        // Re-trigger should restart from the beginning.
        voice.trigger();
        let mut out2 = vec![0.0f32; FRAMES * 2];
        voice.render(&mut out2);
        let second_run: Vec<f32> = out2.iter().step_by(2).copied().collect();

        assert_eq!(first_run, second_run);
    }

    #[test]
    fn voice_goes_inactive_after_consuming_all_samples() {
        let buf = Arc::new(vec![0.5f32; 8]); // 8 mono frames
        let mut voice = Voice::new(buf, 1);
        voice.trigger();
        let mut out = vec![0.0f32; 512]; // far more than 8 frames worth
        voice.render(&mut out);
        assert!(!voice.is_active());
    }
}
