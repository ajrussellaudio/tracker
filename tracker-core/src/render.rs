//! Offline renderer: runs the sequencer without real-time constraints and
//! accumulates a stereo f32 PCM buffer suitable for WAV export.

use std::sync::Arc;

use crate::{
    audio::{Mixer, Sequencer, StepEvent, Voice},
    model::{FxCommand, Song, STEPS_PER_PHRASE, TRACKS},
};

const SAMPLE_RATE: f32 = 48000.0;
const CHUNK_FRAMES: usize = 512;
/// Silence appended after the sequencer finishes, to let voices fully decay.
const TAIL_SECONDS: f32 = 2.0;

// ── Retrigger ─────────────────────────────────────────────────────────────────

struct Retrigger {
    track: usize,
    speed: f32,
    volume: f32,
    pan: f32,
    /// Samples remaining until this retrigger fires.
    samples_until: f64,
}

// ── Step count ────────────────────────────────────────────────────────────────

/// Number of step-advance events that constitute "one full pass" of the song.
///
/// If the arrangement is empty, one phrase (16 steps) is rendered.
/// Otherwise, we render `arrangement.len() × max_chain_slots × STEPS_PER_PHRASE`
/// steps, where `max_chain_slots` is the longest chain referenced anywhere in
/// the arrangement.  This ensures every track has completed at least one full
/// traversal.
pub fn total_song_steps(song: &Song) -> usize {
    if song.arrangement.is_empty() {
        return STEPS_PER_PHRASE;
    }
    let max_chain_slots = song
        .arrangement
        .iter()
        .flat_map(|row| row.iter())
        .filter_map(|ci| *ci)
        .filter_map(|ci| song.chains.get(ci as usize))
        .map(|c| c.slots.len().max(1))
        .max()
        .unwrap_or(1);
    song.arrangement.len() * max_chain_slots * STEPS_PER_PHRASE
}

// ── Core render ───────────────────────────────────────────────────────────────

/// Render the song to a stereo interleaved f32 buffer at 48 kHz.
///
/// * `sample_buffers` — decoded WAV data per instrument slot (index matches
///   `song.instruments`).  Slots beyond the slice length are left silent.
/// * `solo_track` — `None` mixes all tracks (honouring `song.mixer` mute/solo);
///   `Some(n)` renders only track `n` with all others silenced.
/// * `progress_cb` — called periodically with a value in `[0.0, 1.0]`.
pub fn render_to_buffer(
    song: &Song,
    sample_buffers: &[Option<(Arc<Vec<f32>>, usize)>],
    solo_track: Option<usize>,
    progress_cb: &mut dyn FnMut(f32),
) -> Vec<f32> {
    // ── Mixer ────────────────────────────────────────────────────────────────
    let mut mixer = Mixer::new();
    for (i, buf_opt) in sample_buffers.iter().enumerate().take(TRACKS) {
        if let Some((samples, channels)) = buf_opt {
            let instr = song.instruments.get(i);
            let loop_start = instr.and_then(|x| x.loop_start).unwrap_or(0);
            let loop_end = instr.and_then(|x| x.loop_end).unwrap_or(0);
            let interp_mode = instr.map(|x| x.interp_mode.clone()).unwrap_or_default();
            let voice = Voice::new(Arc::clone(samples), *channels)
                .with_loop(loop_start, loop_end)
                .with_interp_mode(interp_mode);
            mixer.load_slot(i, voice);
        }
    }

    // ── Sequencer ────────────────────────────────────────────────────────────
    let mut sequencer = Sequencer::new(SAMPLE_RATE, song.bpm);
    sequencer.update_song_data(
        song.arrangement.clone(),
        song.chains.clone(),
        song.phrases.clone(),
        song.instruments.clone(),
    );

    // ── Per-track mix state (derived from song.mixer or solo override) ───────
    let mut track_volumes = [1.0f32; TRACKS];
    let mut track_pans = [0.0f32; TRACKS];
    let mut track_mute = [false; TRACKS];

    match solo_track {
        Some(n) => {
            // Render only track n; silence all others.
            for t in 0..TRACKS {
                track_mute[t] = t != n;
            }
            if n < TRACKS {
                track_volumes[n] = song.mixer[n].volume;
                track_pans[n] = song.mixer[n].pan;
            }
        }
        None => {
            let any_solo = song.mixer.iter().any(|m| m.solo);
            for t in 0..TRACKS {
                let m = &song.mixer[t];
                track_volumes[t] = m.volume;
                track_pans[t] = m.pan;
                track_mute[t] = m.mute || (any_solo && !m.solo);
            }
        }
    }

    // ── Timing ───────────────────────────────────────────────────────────────
    let total_steps = total_song_steps(song);
    let total_tail_frames = (SAMPLE_RATE * TAIL_SECONDS) as usize;
    // Approx total frames for progress reporting (slightly over-estimates with swing).
    let approx_total_frames =
        (total_steps as f64 * sequencer.samples_per_step()) as usize + total_tail_frames;

    // ── Initial trigger: fire step 0 ─────────────────────────────────────────
    sequencer.playing = true;
    let init_event = sequencer.current_step_notes();
    let mut retriggers: Vec<Retrigger> = Vec::new();
    process_event(
        &init_event,
        &track_mute,
        &track_volumes,
        &track_pans,
        &mut mixer,
        &mut retriggers,
        &sequencer,
    );

    // ── Main render loop ─────────────────────────────────────────────────────
    let mut output: Vec<f32> = Vec::with_capacity(approx_total_frames * 2);
    let mut steps_seen = 0usize;
    let mut in_tail = false;
    let mut tail_remaining = total_tail_frames;
    let mut chunk = vec![0.0f32; CHUNK_FRAMES * 2];

    loop {
        if in_tail {
            if tail_remaining == 0 || !mixer.any_active() {
                break;
            }
            let frames = CHUNK_FRAMES.min(tail_remaining);
            chunk[..frames * 2].fill(0.0);
            mixer.render(&mut chunk[..frames * 2]);
            output.extend_from_slice(&chunk[..frames * 2]);
            tail_remaining = tail_remaining.saturating_sub(frames);
            progress_cb(1.0);
            continue;
        }

        // Fire any retriggers that expire within this chunk.
        let mut to_fire: Vec<(usize, f32, f32, f32)> = Vec::new();
        retriggers.retain_mut(|rt| {
            rt.samples_until -= CHUNK_FRAMES as f64;
            if rt.samples_until <= 0.0 {
                to_fire.push((rt.track, rt.speed, rt.volume, rt.pan));
                false
            } else {
                true
            }
        });
        for (track, speed, vol, pan) in to_fire {
            mixer.trigger_with_fx(track, speed, vol, pan);
        }

        // Advance sequencer and process events.
        let events = sequencer.advance(CHUNK_FRAMES);
        steps_seen += events.len();
        for event in &events {
            process_event(
                event,
                &track_mute,
                &track_volumes,
                &track_pans,
                &mut mixer,
                &mut retriggers,
                &sequencer,
            );
        }

        chunk.fill(0.0);
        mixer.render(&mut chunk);
        output.extend_from_slice(&chunk);

        if steps_seen >= total_steps {
            in_tail = true;
            sequencer.playing = false;
        }

        let frames_so_far = output.len() / 2;
        progress_cb((frames_so_far as f32 / approx_total_frames as f32).min(0.99));
    }

    progress_cb(1.0);
    output
}

// ── Event processing (mirrors real-time audio callback) ───────────────────────

fn process_event(
    event: &StepEvent,
    track_mute: &[bool; TRACKS],
    track_volumes: &[f32; TRACKS],
    track_pans: &[f32; TRACKS],
    mixer: &mut Mixer,
    retriggers: &mut Vec<Retrigger>,
    sequencer: &Sequencer,
) {
    for (track, speed, fx) in &event.notes {
        if track_mute[*track] {
            continue;
        }

        let mut final_speed = *speed;
        let mut vol = track_volumes[*track];
        let mut pan = track_pans[*track];
        let mut ret_count: u8 = 0;

        for slot in fx.iter() {
            if slot.command == 0 {
                continue;
            }
            match FxCommand::from_id(slot.command) {
                Some(FxCommand::Vol) => vol = slot.value as f32 / 255.0,
                Some(FxCommand::Pan) => {
                    pan = (slot.value as f32 - 128.0) / 127.0;
                }
                Some(FxCommand::Pit) => {
                    let semitones = slot.value as i8;
                    final_speed *= 2.0f32.powf(semitones as f32 / 12.0);
                }
                Some(FxCommand::Ret) if slot.value >= 2 => {
                    ret_count = slot.value;
                }
                _ => {}
            }
        }

        mixer.trigger_with_fx(*track, final_speed, vol, pan);

        if ret_count >= 2 {
            let step_samples = sequencer.samples_per_step();
            let interval = step_samples / ret_count as f64;
            for i in 1..ret_count {
                retriggers.push(Retrigger {
                    track: *track,
                    speed: final_speed,
                    volume: vol,
                    pan,
                    samples_until: interval * i as f64,
                });
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChainSlot, Instrument, Sample, Song, Step};
    use std::time::Instant;

    /// Build a minimal song using `Song::default()` (phrase 0, chain 0, row 0) and put
    /// a single note at step 0 of phrase 0 on track 0.  This is the simplest possible
    /// arrangement that will actually produce audio on track 0.
    fn make_test_song(root_note: u8) -> Song {
        let mut song = Song::default(); // phrases[0] empty, chains[0]→phrase 0, arrangement[0]=[Some(0),…]
        song.bpm = 120.0;

        song.instruments.push(Instrument {
            name: "test".to_string(),
            sample: Some(Sample::from_path("ignored.wav")),
            root_note,
            loop_start: None,
            loop_end: None,
            interp_mode: crate::model::InterpMode::Linear,
            volume: 1.0,
            pan: 0.0,
        });

        // Place a note at step 0 of phrase 0 (already in the default arrangement).
        song.phrases[0].steps[0] = Step {
            note: Some(root_note),
            instrument: Some(0),
            velocity: 100,
            fx: Default::default(),
        };

        song
    }

    #[test]
    fn render_produces_non_silent_audio_for_note_at_step_0() {
        let song = make_test_song(60);

        // One sample of value 1.0 (mono).
        let buf = Arc::new(vec![1.0f32]);
        let sample_buffers: Vec<Option<(Arc<Vec<f32>>, usize)>> = vec![Some((buf, 1))];

        let mut progress_values = Vec::new();
        let audio = render_to_buffer(&song, &sample_buffers, None, &mut |p| {
            progress_values.push(p);
        });

        // The very first two samples (L and R) should be 1.0 (voice triggers at step 0).
        assert!(audio.len() >= 2, "output must have at least one stereo frame");
        assert!(
            (audio[0] - 1.0).abs() < 1e-5,
            "L sample should be 1.0, got {}",
            audio[0]
        );
        assert!(
            (audio[1] - 1.0).abs() < 1e-5,
            "R sample should be 1.0, got {}",
            audio[1]
        );

        // Progress should reach 1.0.
        assert!(
            progress_values.iter().any(|&p| p >= 1.0),
            "progress callback must reach 1.0"
        );
    }

    #[test]
    fn render_respects_solo_track_isolation() {
        let song = make_test_song(60);
        let buf = Arc::new(vec![1.0f32]);
        let buffers: Vec<Option<(Arc<Vec<f32>>, usize)>> = vec![Some((buf.clone(), 1))];

        // Render with track 0 solo — should produce audio.
        let mix = render_to_buffer(&song, &buffers, Some(0), &mut |_| {});
        let has_audio = mix.iter().any(|&s| s.abs() > 1e-6);
        assert!(has_audio, "solo render of track 0 should produce audio");

        // Render with track 1 solo — should be silent (no instrument or notes on track 1).
        let silent = render_to_buffer(&song, &buffers, Some(1), &mut |_| {});
        let all_silent = silent.iter().all(|&s| s.abs() < 1e-6);
        assert!(all_silent, "solo render of empty track 1 should be silent");
    }

    #[test]
    fn render_is_faster_than_real_time() {
        let song = make_test_song(60);
        let buf = Arc::new(vec![0.5f32; 4800]); // ~0.1s at 48 kHz
        let buffers: Vec<Option<(Arc<Vec<f32>>, usize)>> = vec![Some((buf, 1))];

        let start = Instant::now();
        let audio = render_to_buffer(&song, &buffers, None, &mut |_| {});
        let render_duration = start.elapsed();

        let song_duration_secs = audio.len() as f64 / (2.0 * SAMPLE_RATE as f64);
        let render_secs = render_duration.as_secs_f64();

        assert!(
            render_secs < song_duration_secs,
            "offline render ({render_secs:.3}s) must be faster than song duration ({song_duration_secs:.3}s)"
        );
    }

    #[test]
    fn total_song_steps_empty_arrangement() {
        let mut song = Song::default();
        song.arrangement.clear();
        assert_eq!(total_song_steps(&song), STEPS_PER_PHRASE);
    }

    #[test]
    fn total_song_steps_with_arrangement() {
        let mut song = Song::default();
        // Replace the default chain (1 slot) with one that has 3 slots.
        song.chains[0].slots.push(ChainSlot { phrase: 0, transpose: 0 });
        song.chains[0].slots.push(ChainSlot { phrase: 0, transpose: 0 });
        // Now chains[0] has 3 slots.
        // Add a second arrangement row (Song::default already has row 0).
        song.arrangement.push([Some(0); TRACKS]);
        // 2 rows × 3 slots × 16 steps = 96
        assert_eq!(total_song_steps(&song), 2 * 3 * STEPS_PER_PHRASE);
    }
}
