use anyhow::Result;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc,
};
use vitakt_core::{
    audio::{Command, Mixer, Sequencer, Voice},
    model::{FxCommand, TRACKS},
};

// ── Audio ─────────────────────────────────────────────────────────────────────

pub fn start_audio_stream(
    mut consumer: rtrb::Consumer<Command>,
    seq_playing: Arc<AtomicBool>,
    current_step: Arc<AtomicU8>,
    preview_playing: Arc<AtomicBool>,
    sample_buf: Option<(Arc<Vec<f32>>, usize)>,
    initial_bpm: f32,
) -> Result<cpal::Stream> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device"))?;

    let config = cpal::StreamConfig {
        channels: 2,
        sample_rate: cpal::SampleRate(48000),
        buffer_size: cpal::BufferSize::Default,
    };

    let mut mixer = Mixer::new();
    if let Some((buf, channels)) = sample_buf {
        mixer.load_slot(0, Voice::new(buf, channels));
    }

    let mut sequencer = Sequencer::new(48000.0, initial_bpm);

    // Dedicated preview voice — separate from the 16 instrument slots.
    let mut preview_voice: Option<Voice> = None;

    // Per-track mixer state: updated by SetTrackVolume/Pan/Mute/Solo commands.
    let mut track_volumes = [1.0f32; TRACKS];
    let mut track_pans = [0.0f32; TRACKS];
    let mut track_mute = [false; TRACKS];
    let mut track_solo = [false; TRACKS];

    /// Scheduled retrigger: fires `speed`/`volume`/`pan` on `track` after `samples_until` frames.
    struct Retrigger {
        track: usize,
        speed: f32,
        volume: f32,
        pan: f32,
        samples_until: f64,
    }
    let mut retriggers: Vec<Retrigger> = Vec::new();

    let stream = device.build_output_stream(
        &config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / 2;

            // Fire scheduled retriggers that fall within this buffer.
            retriggers.retain_mut(|rt| {
                rt.samples_until -= frames as f64;
                if rt.samples_until <= 0.0 {
                    mixer.trigger_with_fx(rt.track, rt.speed, rt.volume, rt.pan);
                    false
                } else {
                    true
                }
            });

            // Process commands from the UI thread.
            while let Ok(cmd) = consumer.pop() {
                match cmd {
                    Command::NoteOn { slot, speed } => mixer.trigger(slot as usize, speed),
                    Command::NoteOff(slot) => mixer.stop_slot(slot as usize),
                    Command::Play => {
                        for (track, speed) in sequencer.play() {
                            mixer.trigger(track, speed);
                        }
                        seq_playing.store(true, Ordering::Relaxed);
                    }
                    Command::Stop => {
                        sequencer.stop();
                        retriggers.clear();
                        seq_playing.store(false, Ordering::Relaxed);
                    }
                    Command::Restart => {
                        for (track, speed) in sequencer.restart() {
                            mixer.trigger(track, speed);
                        }
                        current_step.store(0, Ordering::Relaxed);
                        seq_playing.store(true, Ordering::Relaxed);
                    }
                    Command::SetBpm(bpm) => sequencer.bpm = bpm,
                    Command::SetSwing(swing) => sequencer.swing = swing,
                    Command::UpdatePhrase(phrase) => sequencer.set_phrase(phrase),
                    Command::SetSampleRoot(root) => sequencer.sample_root = root,
                    Command::LoadVoice { slot, samples, channels, loop_start, loop_end, interp_mode } => {
                        let voice = Voice::new(samples, channels)
                            .with_loop(loop_start, loop_end)
                            .with_interp_mode(interp_mode);
                        mixer.load_slot(slot as usize, voice);
                    }
                    Command::SetLoopPoints { slot, loop_start, loop_end } => {
                        mixer.set_loop_points(slot as usize, loop_start, loop_end);
                    }
                    Command::SetInterpMode { slot, interp_mode } => {
                        mixer.set_interp_mode(slot as usize, interp_mode);
                    }
                    Command::UpdateSongData { arrangement, chains, phrases, instruments } => {
                        sequencer.update_song_data(arrangement, chains, phrases, instruments);
                    }
                    Command::UpdatePhraseInSong { idx, phrase } => {
                        sequencer.update_phrase_in_song(idx, phrase);
                    }
                    Command::SetTrackVolume { track, volume } => {
                        if (track as usize) < TRACKS {
                            track_volumes[track as usize] = volume;
                        }
                    }
                    Command::SetTrackPan { track, pan } => {
                        if (track as usize) < TRACKS {
                            track_pans[track as usize] = pan;
                        }
                    }
                    Command::SetTrackMute { track, mute } => {
                        if (track as usize) < TRACKS {
                            track_mute[track as usize] = mute;
                        }
                    }
                    Command::SetTrackSolo { track, active } => {
                        if (track as usize) < TRACKS {
                            track_solo[track as usize] = active;
                        }
                    }
                    Command::PreviewSample { samples, channels } => {
                        let mut v = Voice::new(samples, channels);
                        v.trigger(1.0);
                        preview_playing.store(true, Ordering::Relaxed);
                        preview_voice = Some(v);
                    }
                    Command::StopPreview => {
                        preview_playing.store(false, Ordering::Relaxed);
                        preview_voice = None;
                    }
                }
            }

            // Advance the sequencer and trigger notes, applying any FX slots.
            let any_solo = track_solo.iter().any(|&s| s);
            let events = sequencer.advance(frames);
            for event in &events {
                current_step.store(event.step_index, Ordering::Relaxed);
                for (track, speed, fx) in &event.notes {
                    // Skip if muted or if another track is soloed and this one isn't.
                    if track_mute[*track] || (any_solo && !track_solo[*track]) {
                        continue;
                    }

                    let mut final_speed = *speed;
                    // FX slots override vol/pan; track-level values are the base.
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

                    // Schedule retriggers at even sub-step intervals.
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

            mixer.render(data);

            // Mix the preview voice on top of the instrument voices.
            if let Some(pv) = preview_voice.as_mut() {
                pv.render(data);
                if !pv.is_active() {
                    preview_playing.store(false, Ordering::Relaxed);
                    preview_voice = None;
                }
            }
        },
        |err| eprintln!("audio stream error: {err}"),
        None,
    )?;

    stream.play()?;
    Ok(stream)
}
