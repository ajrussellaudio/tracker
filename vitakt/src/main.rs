use anyhow::Result;
use rtrb::RingBuffer;
use std::sync::{
        atomic::{AtomicBool, AtomicU8},
        Arc,
    };
use vitakt_core::audio::Command;

mod app;
mod braille;
mod app_core;
mod audio_stream;
use audio_stream::start_audio_stream;
mod browser;
mod cli;
use cli::parse_args;
mod commands;
mod config;
mod history;
mod input;
mod note_utils;
mod render;
mod theme;
#[cfg(test)]
use theme::Theme;
mod tui;
mod wav_io;

// Imports used only in tests — gated so they don't generate unused-import warnings
// in release/binary builds.
#[cfg(test)]
use app::*;
#[cfg(test)]
use browser::{list_browser_entries, list_browser_entries_ext};
#[cfg(test)]
use cli::CliAction;
#[cfg(test)]
use commands::instr_editor_increment;
#[cfg(test)]
use note_utils::*;
#[cfg(test)]
use ratatui::{style::Color, Terminal};
#[cfg(test)]
use render::*;
#[cfg(test)]
use std::{path::PathBuf, sync::atomic::Ordering};
#[cfg(test)]
use vitakt_core::model::{ChainSlot, InterpMode, TRACKS};

// ── Shared test utilities ─────────────────────────────────────────────────────

/// Shared mutex for tests that mutate the `HOME` environment variable.
/// All test modules that set/restore HOME must use this single mutex so they
/// don't race with each other (e.g. theme tests vs. config tests).
#[cfg(test)]
pub(crate) static HOME_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

// ── Entry point ───────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();
    let action = parse_args(&raw_args).unwrap_or_else(|e| {
        eprintln!("error: {e}");
        std::process::exit(1);
    });

    // Lock-free SPSC channel: UI → audio thread.
    let (producer, consumer) = RingBuffer::<Command>::new(64);

    // Shared state: UI reads, audio writes.
    let seq_playing = Arc::new(AtomicBool::new(false));
    let current_seq_step = Arc::new(AtomicU8::new(0));
    let preview_playing = Arc::new(AtomicBool::new(false));

    let initial_bpm = 120.0f32;

    let _stream = start_audio_stream(
        consumer,
        Arc::clone(&seq_playing),
        Arc::clone(&current_seq_step),
        Arc::clone(&preview_playing),
        None,
        initial_bpm,
    )
    .unwrap_or_else(|e| {
        eprintln!("Warning: could not open audio device: {e}");
        panic!("audio unavailable: {e}")
    });

    tui::run_tui(Some(producer), 60, seq_playing, current_seq_step, preview_playing, action)?;
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use vitakt_core::model::Song;

    fn make_app() -> App {
        App::new(
            None,
            60,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
        )
    }

    #[test]
    fn app_default_song_has_current_version() {
        let app = make_app();
        assert_eq!(app.song.version, vitakt_core::CURRENT_VERSION);
    }

    #[test]
    fn app_has_one_phrase_on_init() {
        let app = make_app();
        assert_eq!(app.song.phrases.len(), 1);
        assert_eq!(app.song.phrases[0].steps.len(), 16);
    }

    #[test]
    fn note_name_middle_c() {
        assert_eq!(note_name(60), "C-4");
    }

    #[test]
    fn note_name_a4() {
        assert_eq!(note_name(69), "A-4");
    }

    #[test]
    fn note_name_c_sharp() {
        assert_eq!(note_name(61), "C#4");
    }

    #[test]
    fn qwerty_z_is_c() {
        assert_eq!(qwerty_to_semitone('z'), Some(0));
    }

    #[test]
    fn qwerty_s_is_csharp() {
        assert_eq!(qwerty_to_semitone('s'), Some(1));
    }

    #[test]
    fn qwerty_unknown_is_none() {
        assert_eq!(qwerty_to_semitone('a'), None);
    }

    #[test]
    fn enter_note_sets_step_and_advances_cursor() {
        let mut app = make_app();
        app.enter_note(60);
        assert_eq!(app.song.phrases[0].steps[0].note, Some(60));
        assert_eq!(app.cursor_step, 1);
    }

    #[test]
    fn enter_note_wraps_cursor_at_end() {
        let mut app = make_app();
        app.cursor_step = 15;
        app.enter_note(60);
        assert_eq!(app.cursor_step, 0);
    }

    #[test]
    fn pitch_speed_root_is_one() {
        let speed = pitch_speed(60, 60);
        assert!((speed - 1.0).abs() < 1e-4);
    }

    #[test]
    fn pitch_speed_octave_up_is_two() {
        let speed = pitch_speed(72, 60);
        assert!((speed - 2.0).abs() < 1e-4);
    }

    #[test]
    fn pitch_speed_octave_down_is_half() {
        let speed = pitch_speed(48, 60);
        assert!((speed - 0.5).abs() < 1e-4);
    }

    #[test]
    fn pit_fx_value_12_doubles_speed() {
        // PIT value 12 → i8 = 12 → +1 octave → speed ×2
        let semitones = 12u8 as i8;
        let factor = 2.0f32.powf(semitones as f32 / 12.0);
        assert!((factor - 2.0).abs() < 1e-4, "PIT+12 should double speed, got {factor}");
    }

    #[test]
    fn pit_fx_value_244_halves_speed() {
        // PIT value 244 reinterpreted as i8 = -12 → -1 octave → speed ×0.5
        let semitones = 244u8 as i8;
        assert_eq!(semitones, -12, "244u8 as i8 must equal -12");
        let factor = 2.0f32.powf(semitones as f32 / 12.0);
        assert!((factor - 0.5).abs() < 1e-4, "PIT-12 should halve speed, got {factor}");
    }

    #[test]
    fn execute_command_w_error_on_bad_path() {
        let mut app = make_app();
        app.mode = InputMode::Command;
        app.cmd_buf = "w /nonexistent_dir/out.trk".to_string();
        app.execute_command();
        assert!(app.status.starts_with("Error:"), "got: {}", app.status);
    }

    #[test]
    fn execute_command_e_error_on_missing_file() {
        let mut app = make_app();
        app.mode = InputMode::Command;
        app.cmd_buf = "e /nonexistent/file.trk".to_string();
        app.execute_command();
        assert!(app.status.starts_with("Error:"), "got: {}", app.status);
    }

    #[test]
    fn execute_bpm_sets_song_bpm() {
        let mut app = make_app();
        app.cmd_buf = "bpm 140".to_string();
        app.execute_command();
        assert!((app.song.bpm - 140.0).abs() < 1e-4, "BPM should be 140.0, got {}", app.song.bpm);
    }

    #[test]
    fn execute_bpm_clamps_below_minimum() {
        let mut app = make_app();
        app.cmd_buf = "bpm 10".to_string();
        app.execute_command();
        assert!((app.song.bpm - 20.0).abs() < 1e-4, "BPM should clamp to 20.0, got {}", app.song.bpm);
    }

    #[test]
    fn execute_bpm_clamps_above_maximum() {
        let mut app = make_app();
        app.cmd_buf = "bpm 1000".to_string();
        app.execute_command();
        assert!((app.song.bpm - 999.0).abs() < 1e-4, "BPM should clamp to 999.0, got {}", app.song.bpm);
    }

    #[test]
    fn execute_bpm_invalid_shows_error() {
        let mut app = make_app();
        app.cmd_buf = "bpm foo".to_string();
        app.execute_command();
        assert!(app.status.contains("Invalid BPM"), "expected error, got: {}", app.status);
    }

    #[test]
    fn execute_bpm_no_value_shows_usage() {
        let mut app = make_app();
        app.cmd_buf = "bpm".to_string();
        app.execute_command();
        assert!(app.status.contains("Usage"), "expected usage hint, got: {}", app.status);
    }

    #[test]
    fn execute_bpm_is_undoable() {
        let mut app = make_app();
        let original_bpm = app.song.bpm;
        app.cmd_buf = "bpm 180".to_string();
        app.execute_command();
        assert!((app.song.bpm - 180.0).abs() < 1e-4);
        app.do_undo();
        assert!((app.song.bpm - original_bpm).abs() < 1e-4, "undo should restore original BPM");
    }

    #[test]
    fn execute_bpm_nan_shows_error_and_does_not_corrupt_bpm() {
        let mut app = make_app();
        let original_bpm = app.song.bpm;
        app.cmd_buf = "bpm nan".to_string();
        app.execute_command();
        assert!(app.status.contains("Invalid BPM"), "expected error, got: {}", app.status);
        assert_eq!(app.song.bpm, original_bpm, "NaN must not corrupt song.bpm");
    }

    #[test]
    fn step_roundtrip_bincode() {
        let mut app = make_app();
        app.enter_note(60);
        app.enter_note(64);
        app.enter_note(67);

        let path = std::env::temp_dir().join("vitakt_phrase_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "got: {}", app.status);

        let original_steps = app.song.phrases[0].steps.clone();
        app.song = Song::default();
        app.song.phrases.push(vitakt_core::model::Phrase::default());

        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "got: {}", app.status);

        assert_eq!(app.song.phrases[0].steps, original_steps);
        std::fs::remove_file(&path).ok();
    }

    // ── Instrument editor tests ───────────────────────────────────────────────

    #[test]
    fn ensure_instrument_creates_default() {
        let mut app = make_app();
        assert!(app.song.instruments.is_empty());
        app.ensure_instrument(0);
        assert_eq!(app.song.instruments.len(), 1);
        assert_eq!(app.song.instruments[0].root_note, 60);
    }

    #[test]
    fn ensure_instrument_does_not_exceed_max() {
        let mut app = make_app();
        // Request instrument at index MAX_INSTRUMENTS (should not create >MAX)
        app.ensure_instrument(MAX_INSTRUMENTS - 1);
        assert_eq!(app.song.instruments.len(), MAX_INSTRUMENTS);

        // Requesting one beyond the max should not grow further
        app.ensure_instrument(MAX_INSTRUMENTS);
        assert_eq!(app.song.instruments.len(), MAX_INSTRUMENTS);
    }

    #[test]
    fn open_instrument_editor_creates_instrument_if_missing() {
        let mut app = make_app();
        app.open_instrument_editor();
        assert_eq!(app.song.instruments.len(), 1);
        assert!(matches!(app.view, View::InstrumentEditor));
    }

    #[test]
    fn instr_editor_increment_root_note() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].root_note = 60;
        app.instr_cursor = INSTR_FIELD_ROOT;
        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].root_note, 61);
        instr_editor_increment(&mut app, -1);
        assert_eq!(app.song.instruments[0].root_note, 60);
    }

    #[test]
    fn instr_editor_increment_loop_start() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.instr_cursor = INSTR_FIELD_LOOP_START;
        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].loop_start, Some(1));
    }

    #[test]
    fn instr_editor_increment_loop_end() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.instr_cursor = INSTR_FIELD_LOOP_END;
        instr_editor_increment(&mut app, 5);
        // Note: delta is applied once per call, so we need 5 calls or a different approach
        // Actually delta=5 means +5 in one call
        assert_eq!(app.song.instruments[0].loop_end, Some(5));
    }

    #[test]
    fn instr_editor_increment_interp_mode_cycles() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].interp_mode = InterpMode::None;
        app.instr_cursor = INSTR_FIELD_INTERP;

        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].interp_mode, InterpMode::Linear);

        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].interp_mode, InterpMode::Sinc);

        instr_editor_increment(&mut app, 1);
        assert_eq!(app.song.instruments[0].interp_mode, InterpMode::None);
    }

    #[test]
    fn instr_editor_increment_interp_mode_reverse() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].interp_mode = InterpMode::None;
        app.instr_cursor = INSTR_FIELD_INTERP;

        instr_editor_increment(&mut app, -1);
        assert_eq!(app.song.instruments[0].interp_mode, InterpMode::Sinc);
    }

    #[test]
    fn instrument_changes_persist_through_save_load() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].name = "TestKit".to_string();
        app.song.instruments[0].root_note = 48;
        app.song.instruments[0].loop_start = Some(100);
        app.song.instruments[0].loop_end = Some(200);
        app.song.instruments[0].interp_mode = InterpMode::Sinc;
        app.song.instruments[0].volume = 0.75;
        app.song.instruments[0].pan = -0.5;

        let path = std::env::temp_dir().join("vitakt_instrument_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "save failed: {}", app.status);

        let orig = app.song.instruments[0].clone();

        // Reset song and reload
        app.song = Song::default();
        app.song.phrases.push(vitakt_core::model::Phrase::default());

        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "load failed: {}", app.status);

        assert!(!app.song.instruments.is_empty(), "instruments should have been loaded");
        let loaded = &app.song.instruments[0];
        assert_eq!(loaded.name, orig.name);
        assert_eq!(loaded.root_note, orig.root_note);
        assert_eq!(loaded.loop_start, orig.loop_start);
        assert_eq!(loaded.loop_end, orig.loop_end);
        assert_eq!(loaded.interp_mode, orig.interp_mode);
        assert!((loaded.volume - orig.volume).abs() < 0.01);
        assert!((loaded.pan - orig.pan).abs() < 0.01);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn list_browser_entries_tags_and_filters_correctly() {
        let dir = std::env::temp_dir().join("vitakt_browser_test");
        std::fs::create_dir_all(&dir).ok();

        // Create wav files (case-insensitive extension), a non-wav file, and a subdir
        for name in &["b.wav", "a.WAV", "c.wav", "ignored.txt"] {
            std::fs::write(dir.join(name), b"RIFF").ok();
        }
        let subdir = dir.join("samples_dir");
        std::fs::create_dir_all(&subdir).ok();

        let entries = list_browser_entries(&dir);

        // Clean up
        for name in &["b.wav", "a.WAV", "c.wav", "ignored.txt"] {
            std::fs::remove_file(dir.join(name)).ok();
        }
        std::fs::remove_dir(&subdir).ok();

        // Should contain the directory and the 3 wav files, not the txt file
        assert!(!entries.is_empty(), "should find entries");
        let dirs: Vec<_> = entries.iter().filter(|e| matches!(e, BrowserEntry::Dir(_))).collect();
        let wavs: Vec<_> = entries.iter().filter(|e| matches!(e, BrowserEntry::Wav(_))).collect();
        assert_eq!(dirs.len(), 1, "should have 1 directory");
        assert_eq!(wavs.len(), 3, "should have 3 wav files");

        // Check sorted (case-insensitive) — dirs and wavs interleaved alphabetically
        let names: Vec<String> = entries.iter().map(|e| e.sort_key()).collect();
        let is_sorted = names.windows(2).all(|w| w[0] <= w[1]);
        assert!(is_sorted, "entries should be sorted: {names:?}");
    }

    #[test]
    fn list_browser_entries_empty_dir() {
        let dir = std::env::temp_dir().join("vitakt_browser_empty_test");
        std::fs::create_dir_all(&dir).ok();
        // Remove any files that might exist from a previous run
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for entry in rd.flatten() {
                std::fs::remove_file(entry.path()).ok();
                std::fs::remove_dir(entry.path()).ok();
            }
        }
        let entries = list_browser_entries(&dir);
        // An empty non-root directory should contain only the synthetic `..` entry.
        assert_eq!(entries.len(), 1, "empty non-root dir should yield exactly the '..' entry");
        assert!(matches!(entries[0], BrowserEntry::ParentDir), "first entry should be ParentDir");
    }

    #[test]
    fn sample_browser_opens_and_cancel_returns_to_instrument_editor() {
        let mut app = make_app();
        app.open_instrument_editor();
        app.open_sample_browser();
        assert!(matches!(app.view, View::SampleBrowser));
        // Simulate Esc — cancel
        app.view = View::InstrumentEditor;
        assert!(matches!(app.view, View::InstrumentEditor));
    }

    #[test]
    fn browser_enter_on_dir_updates_browser_dir_and_entries() {
        let parent = std::env::temp_dir().join("vitakt_browser_enter_test");
        let subdir = parent.join("subdir");
        std::fs::create_dir_all(&subdir).ok();
        std::fs::write(subdir.join("kick.wav"), b"RIFF").ok();

        let mut app = make_app();
        app.browser_dir = parent.clone();
        app.browser_entries = list_browser_entries(&parent);
        app.browser_cursor = 0;

        // Find the index of the Dir entry
        let dir_idx = app
            .browser_entries
            .iter()
            .position(|e| matches!(e, BrowserEntry::Dir(_)))
            .expect("should have a Dir entry");
        app.browser_cursor = dir_idx;

        app.browser_enter();

        assert_eq!(app.browser_dir, subdir, "browser_dir should update to subdir");
        assert_eq!(app.browser_cursor, 0, "cursor should reset to 0");
        let has_wav = app
            .browser_entries
            .iter()
            .any(|e| matches!(e, BrowserEntry::Wav(n) if n == "kick.wav"));
        assert!(has_wav, "browser_entries should contain kick.wav after entering subdir");

        // Clean up
        std::fs::remove_file(subdir.join("kick.wav")).ok();
        std::fs::remove_dir(&subdir).ok();
        std::fs::remove_dir(&parent).ok();
    }

    #[test]
    fn browser_go_up_navigates_to_parent() {
        let parent = std::env::temp_dir().join("vitakt_go_up_test");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).ok();

        let mut app = make_app();
        app.browser_dir = child.clone();
        app.browser_entries = list_browser_entries(&child);

        app.browser_go_up();

        assert_eq!(app.browser_dir, parent, "browser_dir should be the parent after go_up");
        assert_eq!(app.browser_cursor, 0, "cursor should reset to 0");

        // Clean up
        std::fs::remove_dir(&child).ok();
        std::fs::remove_dir(&parent).ok();
    }

    #[test]
    fn browser_go_up_at_root_does_nothing() {
        let root = PathBuf::from("/");
        let mut app = make_app();
        app.browser_dir = root.clone();
        app.browser_entries = Vec::new();

        app.browser_go_up();

        assert_eq!(app.browser_dir, root, "browser_dir should not change when already at root");
    }

    #[test]
    fn song_view_assign_chain_persists() {
        let mut app = make_app();
        app.song_cursor_row = 0;
        app.song_cursor_track = 1;
        // Simulate assigning chain 0 to track 1
        app.ensure_chain(0);
        app.song.arrangement[0][1] = Some(0);
        assert_eq!(app.song.arrangement[0][1], Some(0));
    }

    #[test]
    fn song_view_clear_chain() {
        let mut app = make_app();
        app.song.arrangement[0][0] = Some(5);
        app.song.arrangement[0][0] = None;
        assert_eq!(app.song.arrangement[0][0], None);
    }

    #[test]
    fn chain_view_transpose_adjusts() {
        let mut app = make_app();
        // Chain 0 exists from default
        app.chain_view_track = 0;
        app.chain_view_row = 0;
        app.chain_cursor = 0;
        let ci = 0usize;
        app.song.chains[ci].slots[0].transpose = 5;
        assert_eq!(app.song.chains[ci].slots[0].transpose, 5);
        app.song.chains[ci].slots[0].transpose += 1;
        assert_eq!(app.song.chains[ci].slots[0].transpose, 6);
    }

    #[test]
    fn arrangement_roundtrip_bincode() {
        let mut app = make_app();
        app.song.arrangement[0][0] = Some(0);
        app.song.arrangement[0][3] = Some(2);
        app.ensure_chain(2);

        let path = std::env::temp_dir().join("vitakt_arrangement_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "save failed: {}", app.status);

        let saved_arr = app.song.arrangement.clone();

        app.song = Song::default();
        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "load failed: {}", app.status);

        assert_eq!(app.song.arrangement, saved_arr, "arrangement should survive round-trip");
        std::fs::remove_file(&path).ok();
    }

    // ── Mixer tests ──────────────────────────────────────────────────────────

    #[test]
    fn mixer_defaults_to_unity_gain_center_pan_no_mute_solo() {
        let app = make_app();
        for t in 0..TRACKS {
            let m = &app.song.mixer[t];
            assert!((m.volume - 1.0).abs() < 1e-4, "track {t} volume should default to 1.0");
            assert!(m.pan.abs() < 1e-4, "track {t} pan should default to 0.0");
            assert!(!m.mute, "track {t} should not be muted by default");
            assert!(!m.solo, "track {t} should not be soloed by default");
            assert!(m.fx_send.abs() < 1e-4, "track {t} fx_send should default to 0.0");
        }
    }

    #[test]
    fn mixer_mute_toggle_updates_model() {
        let mut app = make_app();
        assert!(!app.song.mixer[0].mute);
        app.song.mixer[0].mute = true;
        assert!(app.song.mixer[0].mute);
        app.song.mixer[0].mute = false;
        assert!(!app.song.mixer[0].mute);
    }

    #[test]
    fn mixer_solo_toggle_updates_model() {
        let mut app = make_app();
        assert!(!app.song.mixer[2].solo);
        app.song.mixer[2].solo = true;
        assert!(app.song.mixer[2].solo);
    }

    #[test]
    fn mixer_volume_clamped_to_range() {
        let mut app = make_app();
        app.song.mixer[0].volume = 3.0;
        app.song.mixer[0].volume = app.song.mixer[0].volume.clamp(0.0, 2.0);
        assert!((app.song.mixer[0].volume - 2.0).abs() < 1e-4);

        app.song.mixer[0].volume = -0.5;
        app.song.mixer[0].volume = app.song.mixer[0].volume.clamp(0.0, 2.0);
        assert!(app.song.mixer[0].volume.abs() < 1e-4);
    }

    #[test]
    fn mixer_state_persists_through_save_load() {
        let mut app = make_app();
        app.song.mixer[0].volume = 0.75;
        app.song.mixer[1].pan = -0.5;
        app.song.mixer[2].mute = true;
        app.song.mixer[3].solo = true;
        app.song.mixer[4].fx_send = 0.3;

        let path = std::env::temp_dir().join("vitakt_mixer_roundtrip.trk");
        let path_str = path.to_str().unwrap().to_string();

        app.mode = InputMode::Command;
        app.cmd_buf = format!("w {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Saved:"), "save failed: {}", app.status);

        app.song = Song::default();
        app.mode = InputMode::Command;
        app.cmd_buf = format!("e {path_str}");
        app.execute_command();
        assert!(app.status.starts_with("Loaded:"), "load failed: {}", app.status);

        assert!((app.song.mixer[0].volume - 0.75).abs() < 1e-3, "volume should persist");
        assert!((app.song.mixer[1].pan - (-0.5)).abs() < 1e-3, "pan should persist");
        assert!(app.song.mixer[2].mute, "mute should persist");
        assert!(app.song.mixer[3].solo, "solo should persist");
        assert!((app.song.mixer[4].fx_send - 0.3).abs() < 1e-3, "fx_send should persist");

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn mixer_multiple_solos_allowed() {
        let mut app = make_app();
        app.song.mixer[0].solo = true;
        app.song.mixer[3].solo = true;
        assert!(app.song.mixer[0].solo);
        assert!(app.song.mixer[3].solo);
        assert!(!app.song.mixer[1].solo);
    }

    // ── Undo/redo tests ──────────────────────────────────────────────────────

    #[test]
    fn undo_redo_note_entry() {
        let mut app = make_app();
        let original = app.song.clone();

        // Enter 5 notes
        for i in 0..5u8 {
            app.cursor_step = i as usize;
            app.enter_note(60 + i);
        }
        let mutated = app.song.clone();
        // Verify mutations happened
        for i in 0..5usize {
            assert_eq!(app.song.phrases[0].steps[i].note, Some(60 + i as u8));
        }

        // Undo all 5
        for _ in 0..5 {
            app.do_undo();
        }
        assert_eq!(app.song.phrases[0], original.phrases[0], "after 5 undos should match original");

        // Redo all 5
        for _ in 0..5 {
            app.do_redo();
        }
        assert_eq!(app.song.phrases[0], mutated.phrases[0], "after 5 redos should match mutated state");
    }

    #[test]
    fn new_mutation_clears_redo_stack() {
        let mut app = make_app();
        app.enter_note(60);
        app.do_undo();
        // Redo stack has one entry now
        // Enter a new note — should clear redo
        app.cursor_step = 1;
        app.enter_note(62);
        app.do_redo(); // should do nothing
        // Step 0 should still be empty (redo was cleared)
        assert_eq!(app.song.phrases[0].steps[0].note, None);
        // Step 1 should have the new note
        assert_eq!(app.song.phrases[0].steps[1].note, Some(62));
    }

    #[test]
    fn undo_stack_capped_at_1000() {
        let mut app = make_app();
        for i in 0..1001u16 {
            app.song.bpm = 100.0 + i as f32 * 0.001;
            app.history.push(format!("change {i}"), app.song.clone());
        }
        // Should have exactly 1000 entries (oldest dropped)
        assert_eq!(app.history.undo_stack.len(), 1000);
    }

    #[test]
    fn song_view_o_inserts_row_below_cursor() {
        let mut app = make_app();
        // Start with 1 row; cursor at row 0
        assert_eq!(app.song.arrangement.len(), 1);
        app.song_cursor_row = 0;
        // Mark row 0 so we can check order after insert
        app.song.arrangement[0][0] = Some(7);
        // Simulate 'o': insert below cursor
        let insert_at = app.song_cursor_row + 1;
        app.song.arrangement.insert(insert_at, [None; TRACKS]);
        assert_eq!(app.song.arrangement.len(), 2);
        // Original row stays at 0, new blank row is at 1
        assert_eq!(app.song.arrangement[0][0], Some(7));
        assert_eq!(app.song.arrangement[1][0], None);
        // Cursor unchanged
        assert_eq!(app.song_cursor_row, 0);
    }

    #[test]
    fn song_view_capital_o_inserts_row_above_cursor_and_cursor_follows() {
        let mut app = make_app();
        app.song.arrangement[0][0] = Some(3);
        app.song_cursor_row = 0;
        // Simulate 'O': insert above cursor, cursor increments to stay on original row
        let insert_at = app.song_cursor_row;
        app.song.arrangement.insert(insert_at, [None; TRACKS]);
        app.song_cursor_row += 1;
        assert_eq!(app.song.arrangement.len(), 2);
        // New blank row at 0, original row shifted to 1
        assert_eq!(app.song.arrangement[0][0], None);
        assert_eq!(app.song.arrangement[1][0], Some(3));
        // Cursor follows original row
        assert_eq!(app.song_cursor_row, 1);
    }

    #[test]
    fn song_view_o_inserts_row_below_cursor_at_middle_position() {
        let mut app = make_app();
        // Set up 3 rows: [Some(1), Some(2), Some(3)]
        app.song.arrangement[0][0] = Some(1);
        app.song.arrangement.push([None; TRACKS]);
        app.song.arrangement[1][0] = Some(2);
        app.song.arrangement.push([None; TRACKS]);
        app.song.arrangement[2][0] = Some(3);
        assert_eq!(app.song.arrangement.len(), 3);
        // Cursor at row 1 (middle)
        app.song_cursor_row = 1;
        // Simulate 'o': insert below cursor (at index 2)
        let insert_at = app.song_cursor_row + 1;
        app.song.arrangement.insert(insert_at, [None; TRACKS]);
        assert_eq!(app.song.arrangement.len(), 4);
        // Original rows at 0 and 1 unchanged; new blank row at 2; row 3 is shifted Some(3)
        assert_eq!(app.song.arrangement[0][0], Some(1));
        assert_eq!(app.song.arrangement[1][0], Some(2));
        assert_eq!(app.song.arrangement[2][0], None);
        assert_eq!(app.song.arrangement[3][0], Some(3));
        // Cursor stays on middle row
        assert_eq!(app.song_cursor_row, 1);
    }

    #[test]
    fn song_view_capital_o_inserts_row_above_middle_cursor_and_cursor_follows() {
        let mut app = make_app();
        // Set up 3 rows: [Some(1), Some(2), Some(3)]
        app.song.arrangement[0][0] = Some(1);
        app.song.arrangement.push([None; TRACKS]);
        app.song.arrangement[1][0] = Some(2);
        app.song.arrangement.push([None; TRACKS]);
        app.song.arrangement[2][0] = Some(3);
        assert_eq!(app.song.arrangement.len(), 3);
        // Cursor at row 1 (middle)
        app.song_cursor_row = 1;
        // Simulate 'O': insert above cursor (at index 1), cursor increments
        let insert_at = app.song_cursor_row;
        app.song.arrangement.insert(insert_at, [None; TRACKS]);
        app.song_cursor_row += 1;
        assert_eq!(app.song.arrangement.len(), 4);
        // Blank row inserted at index 1; original row 1 shifted to 2
        assert_eq!(app.song.arrangement[0][0], Some(1));
        assert_eq!(app.song.arrangement[1][0], None);
        assert_eq!(app.song.arrangement[2][0], Some(2));
        assert_eq!(app.song.arrangement[3][0], Some(3));
        // Cursor follows original row to index 2
        assert_eq!(app.song_cursor_row, 2);
    }

    #[test]
    fn chain_view_o_inserts_slot_below_cursor() {
        let mut app = make_app();
        let ci = 0usize;
        // Start with 1 slot at cursor 0
        assert_eq!(app.song.chains[ci].slots.len(), 1);
        app.chain_cursor = 0;
        app.song.chains[ci].slots[0].phrase = 5;
        // Simulate 'o': insert below cursor
        let insert_at = (app.chain_cursor + 1).min(app.song.chains[ci].slots.len());
        app.song.chains[ci].slots.insert(insert_at, ChainSlot { phrase: 0, transpose: 0 });
        app.chain_cursor = insert_at;
        assert_eq!(app.song.chains[ci].slots.len(), 2);
        // Original slot stays at index 0
        assert_eq!(app.song.chains[ci].slots[0].phrase, 5);
        // New slot at index 1
        assert_eq!(app.song.chains[ci].slots[1].phrase, 0);
        // Cursor moved to new slot
        assert_eq!(app.chain_cursor, 1);
    }

    #[test]
    fn chain_view_capital_o_inserts_slot_above_cursor_and_cursor_follows() {
        let mut app = make_app();
        let ci = 0usize;
        app.chain_cursor = 0;
        app.song.chains[ci].slots[0].phrase = 5;
        // Simulate 'O': insert above cursor, cursor increments
        let insert_at = app.chain_cursor;
        app.song.chains[ci].slots.insert(insert_at, ChainSlot { phrase: 0, transpose: 0 });
        app.chain_cursor += 1;
        assert_eq!(app.song.chains[ci].slots.len(), 2);
        // New blank slot at index 0
        assert_eq!(app.song.chains[ci].slots[0].phrase, 0);
        // Original slot shifted to index 1
        assert_eq!(app.song.chains[ci].slots[1].phrase, 5);
        // Cursor follows original slot
        assert_eq!(app.chain_cursor, 1);
    }

    #[test]
    fn chain_view_o_inserts_slot_below_middle_cursor() {
        let mut app = make_app();
        let ci = 0usize;
        // Set up 3 slots: phrases [1, 2, 3]
        app.song.chains[ci].slots[0].phrase = 1;
        app.song.chains[ci].slots.push(ChainSlot { phrase: 2, transpose: 0 });
        app.song.chains[ci].slots.push(ChainSlot { phrase: 3, transpose: 0 });
        assert_eq!(app.song.chains[ci].slots.len(), 3);
        // Cursor at slot 1 (middle)
        app.chain_cursor = 1;
        // Simulate 'o': insert below cursor (at index 2)
        let insert_at = (app.chain_cursor + 1).min(app.song.chains[ci].slots.len());
        app.song.chains[ci].slots.insert(insert_at, ChainSlot { phrase: 0, transpose: 0 });
        app.chain_cursor = insert_at;
        assert_eq!(app.song.chains[ci].slots.len(), 4);
        assert_eq!(app.song.chains[ci].slots[0].phrase, 1);
        assert_eq!(app.song.chains[ci].slots[1].phrase, 2);
        assert_eq!(app.song.chains[ci].slots[2].phrase, 0); // new blank slot
        assert_eq!(app.song.chains[ci].slots[3].phrase, 3);
        assert_eq!(app.chain_cursor, 2);
    }

    #[test]
    fn chain_view_capital_o_inserts_slot_above_middle_cursor_and_cursor_follows() {
        let mut app = make_app();
        let ci = 0usize;
        // Set up 3 slots: phrases [1, 2, 3]
        app.song.chains[ci].slots[0].phrase = 1;
        app.song.chains[ci].slots.push(ChainSlot { phrase: 2, transpose: 0 });
        app.song.chains[ci].slots.push(ChainSlot { phrase: 3, transpose: 0 });
        assert_eq!(app.song.chains[ci].slots.len(), 3);
        // Cursor at slot 1 (middle)
        app.chain_cursor = 1;
        // Simulate 'O': insert above cursor (at index 1), cursor increments
        let insert_at = app.chain_cursor;
        app.song.chains[ci].slots.insert(insert_at, ChainSlot { phrase: 0, transpose: 0 });
        app.chain_cursor += 1;
        assert_eq!(app.song.chains[ci].slots.len(), 4);
        assert_eq!(app.song.chains[ci].slots[0].phrase, 1);
        assert_eq!(app.song.chains[ci].slots[1].phrase, 0); // new blank slot
        assert_eq!(app.song.chains[ci].slots[2].phrase, 2);
        assert_eq!(app.song.chains[ci].slots[3].phrase, 3);
        // Cursor follows original slot to index 2
        assert_eq!(app.chain_cursor, 2);
    }

    #[test]
    fn load_clears_history() {
        let mut app = make_app();
        app.enter_note(60);
        assert!(!app.history.undo_stack.is_empty());
        // Simulate :e by calling history.clear() (as done in execute_command)
        app.history.clear();
        assert!(app.history.undo_stack.is_empty());
        assert!(app.history.redo_stack.is_empty());
    }

    #[test]
    fn phrase_grid_highlights_playback_row_when_playing() {
        use ratatui::backend::TestBackend;
        let phrase = vitakt_core::model::Phrase::default();
        let theme = Theme::default();
        let table = render_phrase_grid(&phrase, 0, 0, 0, &theme, true, 5);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(table, f.area())).unwrap();
        let buf = terminal.backend().buffer();
        // Row 5 is at y = 1 (top border) + 1 (header) + 5 = 7.
        // The step# cell (x=1) always uses row_style, which for a playback row is playback_head_bg.
        assert_eq!(
            buf[(1u16, 7u16)].bg,
            Color::Rgb(0, 95, 135),
            "playback row should carry playback_head_bg"
        );
    }

    #[test]
    fn phrase_grid_cursor_takes_priority_over_playback_head() {
        use ratatui::backend::TestBackend;
        let phrase = vitakt_core::model::Phrase::default();
        let theme = Theme::default();
        // cursor and playback head both on row 3
        let table = render_phrase_grid(&phrase, 3, 0, 0, &theme, true, 3);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(table, f.area())).unwrap();
        let buf = terminal.backend().buffer();
        // Row 3 is at y = 1 (top border) + 1 (header) + 3 = 5.
        // The step# cell (x=1) uses row_style; for a cursor row row_style is DarkGray,
        // regardless of playback position.
        assert_eq!(
            buf[(1u16, 5u16)].bg,
            Color::DarkGray,
            "cursor style should take priority over playback head on coincident row"
        );
        assert_ne!(
            buf[(1u16, 5u16)].bg,
            Color::Rgb(0, 95, 135),
            "playback_head_bg must not appear on cursor row when they coincide"
        );
    }

    #[test]
    fn parse_args_no_args_shows_startup() {
        let args: Vec<String> = vec!["vitakt".to_string()];
        assert_eq!(parse_args(&args).unwrap(), CliAction::ShowStartup);
    }

    #[test]
    fn parse_args_empty_slice_shows_startup() {
        let args: Vec<String> = vec![];
        assert_eq!(parse_args(&args).unwrap(), CliAction::ShowStartup);
    }

    #[test]
    fn parse_args_positional_path_gives_open_file() {
        let args: Vec<String> = vec!["vitakt".to_string(), "my-song.trk".to_string()];
        assert_eq!(
            parse_args(&args).unwrap(),
            CliAction::OpenFile(std::path::PathBuf::from("my-song.trk"))
        );
    }

    #[test]
    fn parse_args_removed_sample_flag_gives_error() {
        let args: Vec<String> =
            vec!["vitakt".to_string(), "--sample".to_string(), "kick.wav".to_string()];
        assert!(parse_args(&args).is_err(), "--sample should return an error");
        let err = parse_args(&args).unwrap_err().to_string();
        assert!(err.contains("--sample"), "error should mention --sample");
    }

    #[test]
    fn parse_args_unknown_flag_gives_error() {
        let args: Vec<String> = vec!["vitakt".to_string(), "--unknown".to_string()];
        assert!(parse_args(&args).is_err(), "unknown flag should return an error");
    }

    // ── Keyboard mode tests ──────────────────────────────────────────────────

    #[test]
    fn keyboard_mode_enter_sets_mode() {
        let mut app = make_app();
        assert_eq!(app.mode, InputMode::Normal);
        app.enter_keyboard_mode();
        assert_eq!(app.mode, InputMode::Keyboard);
    }

    #[test]
    fn keyboard_mode_esc_returns_to_normal() {
        let mut app = make_app();
        app.enter_keyboard_mode();
        app.exit_keyboard_mode();
        assert_eq!(app.mode, InputMode::Normal);
    }

    #[test]
    fn keyboard_instrument_prev_clamps_at_zero() {
        let mut app = make_app();
        app.keyboard_instrument = 0;
        app.keyboard_instrument_prev();
        assert_eq!(app.keyboard_instrument, 0, "should not underflow below 0");
    }

    #[test]
    fn keyboard_instrument_next_clamps_at_255() {
        let mut app = make_app();
        app.keyboard_instrument = 255;
        app.keyboard_instrument_next();
        assert_eq!(app.keyboard_instrument, 255, "should not overflow above 255");
    }

    #[test]
    fn keyboard_instrument_prev_decrements() {
        let mut app = make_app();
        app.keyboard_instrument = 5;
        app.keyboard_instrument_prev();
        assert_eq!(app.keyboard_instrument, 4);
    }

    #[test]
    fn keyboard_instrument_next_increments() {
        let mut app = make_app();
        app.keyboard_instrument = 5;
        app.keyboard_instrument_next();
        assert_eq!(app.keyboard_instrument, 6);
    }

    #[test]
    fn keyboard_instrument_persists_across_mode_entries() {
        let mut app = make_app();
        app.enter_keyboard_mode();
        app.keyboard_instrument = 7;
        app.exit_keyboard_mode();
        app.enter_keyboard_mode();
        assert_eq!(app.keyboard_instrument, 7);
    }

    #[test]
    fn is_dirty_false_on_startup() {
        let app = make_app();
        assert!(!app.is_dirty);
    }

    #[test]
    fn is_dirty_set_after_record() {
        let mut app = make_app();
        assert!(!app.is_dirty);
        app.record("test mutation");
        assert!(app.is_dirty);
    }

    #[test]
    fn is_dirty_cleared_after_enter_note() {
        // enter_note calls record(), so dirty should be true afterwards.
        let mut app = make_app();
        app.enter_note(60);
        assert!(app.is_dirty);
    }

    #[test]
    fn browser_preview_toggle_sets_is_previewing_on_wav_entry() {
        let mut app = make_app();
        // Simulate a .wav entry in the browser (no real file needed — send_cmd is a no-op
        // when producer is None, and load_wav will fail gracefully; use a known path).
        // Instead, directly verify the flag is false initially.
        assert!(!app.is_previewing);
        // Manually set a wav entry and invoke toggle; load_wav will fail on a fake path
        // so is_previewing stays false — but we verify no panic.
        app.browser_entries = vec![BrowserEntry::Wav("nonexistent.wav".to_string())];
        app.browser_cursor = 0;
        app.browser_preview_toggle(); // load_wav fails → sets timed error, is_previewing stays false
        assert!(!app.is_previewing);
    }

    #[test]
    fn browser_preview_toggle_ignores_directory_entry() {
        let mut app = make_app();
        app.browser_entries = vec![BrowserEntry::Dir("samples".to_string())];
        app.browser_cursor = 0;
        app.browser_preview_toggle();
        assert!(!app.is_previewing, "Space on a directory should not set is_previewing");
    }

    #[test]
    fn pop_view_clears_is_previewing() {
        let mut app = make_app();
        // Simulate an active preview.
        app.is_previewing = true;
        app.preview_playing.store(true, Ordering::Relaxed);
        app.push_view(View::SampleBrowser);
        app.pop_view();
        assert!(!app.is_previewing, "pop_view should clear is_previewing");
        assert!(
            !app.preview_playing.load(Ordering::Relaxed),
            "pop_view should clear the preview_playing atomic"
        );
    }

    #[test]
    fn browser_clamp_scroll_scrolls_down_when_cursor_below_window() {
        let mut app = make_app();
        // 10 entries, viewport shows 5 at a time
        app.browser_entries = (0..10).map(|i| BrowserEntry::Wav(format!("{i}.wav"))).collect();
        app.browser_scroll = 0;
        app.browser_cursor = 7; // past end of window [0..5)
        app.browser_clamp_scroll(5);
        assert_eq!(app.browser_scroll, 3, "scroll should move cursor to last visible row");
    }

    #[test]
    fn browser_clamp_scroll_scrolls_up_when_cursor_above_window() {
        let mut app = make_app();
        app.browser_entries = (0..10).map(|i| BrowserEntry::Wav(format!("{i}.wav"))).collect();
        app.browser_scroll = 5;
        app.browser_cursor = 2; // above scroll offset
        app.browser_clamp_scroll(5);
        assert_eq!(app.browser_scroll, 2, "scroll should move to show cursor at top");
    }

    #[test]
    fn browser_clamp_scroll_noop_when_cursor_in_window() {
        let mut app = make_app();
        app.browser_entries = (0..10).map(|i| BrowserEntry::Wav(format!("{i}.wav"))).collect();
        app.browser_scroll = 2;
        app.browser_cursor = 4; // inside window [2..7)
        app.browser_clamp_scroll(5);
        assert_eq!(app.browser_scroll, 2, "scroll should not change when cursor is visible");
    }

    #[test]
    fn browser_clamp_scroll_noop_when_available_is_zero() {
        let mut app = make_app();
        app.browser_entries = (0..10).map(|i| BrowserEntry::Wav(format!("{i}.wav"))).collect();
        app.browser_scroll = 3;
        app.browser_cursor = 0;
        app.browser_clamp_scroll(0); // zero available — should be a no-op
        assert_eq!(app.browser_scroll, 3, "scroll should not change when available is 0");
    }

    #[test]
    fn browser_enter_resets_scroll_on_dir_navigation() {
        let parent = std::env::temp_dir().join("vitakt_scroll_reset_test");
        let subdir = parent.join("subdir");
        std::fs::create_dir_all(&subdir).ok();
        std::fs::write(subdir.join("kick.wav"), b"RIFF").ok();

        let mut app = make_app();
        app.browser_dir = parent.clone();
        app.browser_entries = list_browser_entries(&parent);
        app.browser_scroll = 5; // simulate scrolled state
        let dir_idx = app
            .browser_entries
            .iter()
            .position(|e| matches!(e, BrowserEntry::Dir(_)))
            .expect("should have a Dir entry");
        app.browser_cursor = dir_idx;
        app.browser_enter();

        assert_eq!(app.browser_scroll, 0, "browser_scroll should reset to 0 after navigating into a dir");

        std::fs::remove_file(subdir.join("kick.wav")).ok();
        std::fs::remove_dir(&subdir).ok();
        std::fs::remove_dir(&parent).ok();
    }

    #[test]
    fn list_browser_entries_has_parent_dir_for_non_root() {
        let dir = std::env::temp_dir().join("vitakt_parent_dir_test");
        std::fs::create_dir_all(&dir).ok();
        let entries = list_browser_entries(&dir);
        assert!(
            matches!(entries.first(), Some(BrowserEntry::ParentDir)),
            "first entry should be ParentDir for a non-root directory"
        );
    }

    #[test]
    fn list_browser_entries_no_parent_dir_at_root() {
        let root = std::path::Path::new("/");
        let entries = list_browser_entries(root);
        assert!(
            !entries.iter().any(|e| matches!(e, BrowserEntry::ParentDir)),
            "ParentDir should not appear when browsing /"
        );
    }

    #[test]
    fn browser_enter_on_parent_dir_goes_up() {
        let parent = std::env::temp_dir().join("vitakt_enter_parent_test");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).ok();

        let mut app = make_app();
        app.browser_dir = child.clone();
        app.browser_entries = list_browser_entries(&child);
        // ParentDir is the first entry
        app.browser_cursor = 0;
        assert!(matches!(app.browser_entries[0], BrowserEntry::ParentDir));

        app.browser_enter();

        assert_eq!(app.browser_dir, parent, "Enter on .. should navigate to parent");
        assert_eq!(app.browser_cursor, 0, "cursor should reset after navigating up");

        std::fs::remove_dir(&child).ok();
        std::fs::remove_dir(&parent).ok();
    }

    #[test]
    fn parent_dir_is_never_included_in_wav_entries() {
        let dir = std::env::temp_dir().join("vitakt_parent_not_wav_test");
        std::fs::create_dir_all(&dir).ok();
        std::fs::write(dir.join("test.wav"), b"RIFF").ok();

        let entries = list_browser_entries(&dir);
        let wavs: Vec<_> = entries.iter().filter(|e| matches!(e, BrowserEntry::Wav(_))).collect();
        let parent_dirs: Vec<_> = entries.iter().filter(|e| matches!(e, BrowserEntry::ParentDir)).collect();

        assert_eq!(parent_dirs.len(), 1, "should have exactly one ParentDir");
        assert_eq!(wavs.len(), 1, "should have exactly one Wav entry");
        assert!(
            matches!(entries.first(), Some(BrowserEntry::ParentDir)),
            "ParentDir should be the first entry"
        );

        std::fs::remove_file(dir.join("test.wav")).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn browser_b_key_with_no_bookmarks_shows_status() {
        let mut app = make_app();
        app.config.bookmarks.clear();
        app.browser_try_open_bookmarks();
        assert!(!app.browser_show_bookmarks, "overlay must stay closed");
        assert!(
            app.status.contains("No bookmarks"),
            "status should hint about missing bookmarks, got: {}",
            app.status
        );
    }

    #[test]
    fn browser_b_key_with_valid_bookmarks_opens_overlay() {
        let dir = std::env::temp_dir().join("vitakt_bookmark_open_test");
        std::fs::create_dir_all(&dir).ok();

        let mut app = make_app();
        app.config.bookmarks = vec![dir.to_string_lossy().to_string()];
        app.browser_bookmark_cursor = 5; // pre-set to non-zero
        app.browser_try_open_bookmarks();

        assert!(app.browser_show_bookmarks, "overlay must open with valid bookmark");
        assert_eq!(app.browser_bookmark_cursor, 0, "cursor must reset to 0");

        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn browser_capital_b_adds_and_deduplicates_bookmark() {
        let dir = std::env::temp_dir().join("vitakt_add_bookmark_test");
        std::fs::create_dir_all(&dir).ok();

        let mut app = make_app();
        app.config.bookmarks.clear();
        app.browser_dir = dir.clone();

        // First press — should add the bookmark.
        app.browser_add_bookmark();
        assert_eq!(app.config.bookmarks.len(), 1, "bookmark should be added");
        assert_eq!(app.config.bookmarks[0], dir.to_string_lossy().as_ref());
        assert!(
            app.status.starts_with("Bookmarked:") || app.status.starts_with("Error"),
            "status should confirm bookmark, got: {}",
            app.status
        );

        // Second press — should deduplicate.
        app.browser_add_bookmark();
        assert_eq!(app.config.bookmarks.len(), 1, "duplicate must not be added");
        assert!(
            app.status.contains("Already bookmarked"),
            "status should say already bookmarked, got: {}",
            app.status
        );

        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn browser_overlay_enter_navigates_to_bookmark_dir() {
        let dest = std::env::temp_dir().join("vitakt_overlay_enter_test");
        std::fs::create_dir_all(&dest).ok();
        std::fs::write(dest.join("snare.wav"), b"RIFF").ok();

        let mut app = make_app();
        app.config.bookmarks = vec![dest.to_string_lossy().to_string()];
        app.browser_show_bookmarks = true;
        app.browser_bookmark_cursor = 0;
        let original_dir = app.browser_dir.clone();

        app.browser_overlay_enter();

        assert_eq!(app.browser_dir, dest, "browser_dir must update to selected bookmark");
        assert_ne!(app.browser_dir, original_dir);
        assert_eq!(app.browser_cursor, 0, "cursor must reset to 0");
        assert_eq!(app.browser_scroll, 0, "scroll must reset to 0");
        assert!(!app.browser_show_bookmarks, "overlay must close after Enter");
        assert_eq!(app.browser_bookmark_cursor, 0, "bookmark cursor must reset");
        let has_wav = app
            .browser_entries
            .iter()
            .any(|e| matches!(e, BrowserEntry::Wav(n) if n == "snare.wav"));
        assert!(has_wav, "browser entries should reflect the new directory");

        std::fs::remove_file(dest.join("snare.wav")).ok();
        std::fs::remove_dir(&dest).ok();
    }

    #[test]
    fn browser_overlay_esc_closes_without_navigating() {
        let dest = std::env::temp_dir().join("vitakt_overlay_esc_test");
        std::fs::create_dir_all(&dest).ok();

        let mut app = make_app();
        app.config.bookmarks = vec![dest.to_string_lossy().to_string()];
        app.browser_show_bookmarks = true;
        let original_dir = app.browser_dir.clone();

        app.browser_overlay_esc();

        assert!(!app.browser_show_bookmarks, "overlay must close on Esc");
        assert_eq!(app.browser_dir, original_dir, "browser_dir must not change on Esc");

        std::fs::remove_dir(&dest).ok();
    }

    #[test]
    fn browser_launch_external_loads_wav_from_chooser_file() {
        let dir = std::env::temp_dir().join("vitakt_ext_browser_wav_test");
        std::fs::create_dir_all(&dir).ok();
        let wav_path = dir.join("kick.wav");
        std::fs::write(&wav_path, b"RIFF").ok();
        let wav_str = wav_path.to_string_lossy().to_string();

        let mut app = make_app();
        app.open_instrument_editor();
        app.open_sample_browser();

        // Call the loading branch directly with the wav path so the test does not
        // depend on subprocess execution (which may fail in CI environments).
        app.browser_apply_chooser_result(&wav_str);

        let instr = &app.song.instruments[app.active_instrument];
        assert!(instr.sample.is_some(), "sample should be set after selecting a .wav");
        assert_eq!(
            instr.sample.as_ref().unwrap().path,
            wav_str,
            "sample path should match the chosen wav"
        );
        assert!(
            matches!(app.view, View::InstrumentEditor),
            "view should pop back to InstrumentEditor"
        );

        std::fs::remove_file(&wav_path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    #[test]
    fn browser_launch_external_ignores_non_wav_selection() {
        let dir = std::env::temp_dir().join("vitakt_ext_browser_nonwav_test");
        std::fs::create_dir_all(&dir).ok();
        let txt_path = dir.join("not_a_sample.txt");
        std::fs::write(&txt_path, b"hello").ok();
        let txt_str = txt_path.to_string_lossy().to_string();

        let mut app = make_app();
        app.open_instrument_editor();
        app.open_sample_browser();
        app.ensure_instrument(app.active_instrument);
        let initial_sample = app.song.instruments[app.active_instrument].sample.clone();

        // Call the loading branch directly with a non-.wav path.
        app.browser_apply_chooser_result(&txt_str);

        let instr = &app.song.instruments[app.active_instrument];
        assert_eq!(
            instr.sample, initial_sample,
            "instrument sample must be unchanged when a non-.wav is selected"
        );
        assert!(
            matches!(app.view, View::SampleBrowser),
            "view should remain SampleBrowser when selection is ignored"
        );

        std::fs::remove_file(&txt_path).ok();
        std::fs::remove_dir(&dir).ok();
    }

    fn minimal_wav_bytes(num_frames: usize) -> Vec<u8> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 44100,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        };
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for _ in 0..num_frames {
                writer.write_sample(0.0f32).unwrap();
            }
            writer.finalize().unwrap();
        }
        cursor.into_inner()
    }

    #[test]
    fn open_waveform_editor_without_sample_sets_timed_status() {
        let mut app = make_app();
        app.open_instrument_editor();
        // Instrument 0 has no sample assigned by default.
        app.ensure_instrument(0);
        app.song.instruments[0].sample = None;

        app.open_waveform_editor();

        assert!(
            !matches!(app.view, View::WaveformEditor),
            "view must NOT be WaveformEditor when there is no sample"
        );
        assert!(
            app.status_timer.is_some(),
            "a timed status message should be set"
        );
    }

    #[test]
    fn open_waveform_editor_with_valid_sample_pushes_view_and_populates_samples() {
        let mut app = make_app();
        app.open_instrument_editor();
        app.ensure_instrument(0);

        let mut sample = vitakt_core::model::Sample::from_path("test.wav");
        sample.bytes = Some(minimal_wav_bytes(200));
        app.song.instruments[0].sample = Some(sample);

        app.open_waveform_editor();

        assert!(
            matches!(app.view, View::WaveformEditor),
            "view must be WaveformEditor after opening with a valid sample"
        );
        assert!(
            !app.waveform_samples.is_empty(),
            "waveform_samples must be populated"
        );
    }

    #[test]
    fn esc_in_waveform_editor_pops_back_to_instrument_editor() {
        let mut app = make_app();
        app.open_instrument_editor();
        app.ensure_instrument(0);

        let mut sample = vitakt_core::model::Sample::from_path("test.wav");
        sample.bytes = Some(minimal_wav_bytes(100));
        app.song.instruments[0].sample = Some(sample);

        app.open_waveform_editor();
        assert!(matches!(app.view, View::WaveformEditor));

        // Simulate Esc — pop back.
        app.pop_view();

        assert!(
            matches!(app.view, View::InstrumentEditor),
            "view must be InstrumentEditor after Esc from waveform editor"
        );
    }

    #[test]
    fn waveform_preview_toggle_with_valid_sample_sets_is_previewing() {
        let mut app = make_app();
        app.ensure_instrument(0);

        let mut sample = vitakt_core::model::Sample::from_path("test.wav");
        sample.bytes = Some(minimal_wav_bytes(100));
        app.song.instruments[0].sample = Some(sample);
        app.active_instrument = 0;

        assert!(!app.is_previewing);
        app.waveform_preview_toggle();
        assert!(app.is_previewing, "toggle-on with a valid sample should set is_previewing = true");
    }

    #[test]
    fn waveform_preview_toggle_with_no_sample_is_noop() {
        let mut app = make_app();
        app.ensure_instrument(0);
        app.song.instruments[0].sample = None;
        app.active_instrument = 0;

        app.waveform_preview_toggle();

        assert!(!app.is_previewing, "toggle with no sample assigned should be a no-op");
    }

    #[test]
    fn waveform_preview_toggle_off_clears_is_previewing() {
        let mut app = make_app();
        // Simulate an active preview state.
        app.is_previewing = true;
        app.preview_playing.store(true, Ordering::Relaxed);

        app.waveform_preview_toggle();

        assert!(!app.is_previewing, "second Space press should clear is_previewing");
        assert!(
            !app.preview_playing.load(Ordering::Relaxed),
            "second Space press should clear preview_playing atomic"
        );
    }

    #[test]
    fn waveform_preview_toggle_with_out_of_bounds_sample_start_does_not_panic() {
        let mut app = make_app();
        app.ensure_instrument(0);

        let mut sample = vitakt_core::model::Sample::from_path("test.wav");
        sample.bytes = Some(minimal_wav_bytes(10)); // only 10 frames
        app.song.instruments[0].sample = Some(sample);
        // sample_start beyond the decoded length — must not panic.
        app.song.instruments[0].sample_start = Some(9999);
        app.active_instrument = 0;

        app.waveform_preview_toggle(); // should not panic
    }
}

#[cfg(test)]
mod browser_search_tests {
    use super::*;

    fn make_app() -> App {
        App::new(
            None,
            60,
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicU8::new(0)),
            Arc::new(AtomicBool::new(false)),
        )
    }

    /// Build a browser with entries: ParentDir, then WAVs matching names from `names`.
    fn make_browser_entries(names: &[&str]) -> Vec<BrowserEntry> {
        let mut entries = vec![BrowserEntry::ParentDir];
        for n in names {
            entries.push(BrowserEntry::Wav(n.to_string()));
        }
        entries
    }

    // ── browser_search_update ────────────────────────────────────────────────

    #[test]
    fn search_update_finds_matching_entries_and_jumps_cursor() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&["kick.wav", "snare.wav", "kick_hard.wav"]);
        app.browser_search_query = "kick".to_string();
        app.browser_search_update(20);
        // ParentDir is at index 0 (excluded), kick at 1, snare at 2, kick_hard at 3
        assert_eq!(app.browser_search_matches, vec![1, 3]);
        assert_eq!(app.browser_cursor, 1, "cursor should jump to first match");
    }

    #[test]
    fn search_update_excludes_parent_dir() {
        let mut app = make_app();
        // ParentDir display name contains ".." — make sure it is never included.
        app.browser_entries = make_browser_entries(&["file.wav"]);
        app.browser_search_query = ".".to_string(); // would match ".." if ParentDir were included
        app.browser_search_update(20);
        // Only index 1 (file.wav doesn't contain ".") — but ".." would match ".".
        // If ParentDir were included, index 0 would appear.
        assert!(
            !app.browser_search_matches.contains(&0),
            "ParentDir (index 0) must never appear in matches"
        );
    }

    #[test]
    fn search_update_empty_query_clears_matches() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&["kick.wav"]);
        // Populate some stale matches first.
        app.browser_search_matches = vec![1];
        app.browser_search_query = String::new();
        app.browser_search_update(20);
        // Empty query matches everything that isn't ParentDir.
        // The important contract is that it doesn't panic and idx is reset to 0.
        assert_eq!(app.browser_search_idx, 0);
    }

    #[test]
    fn search_update_no_matches_leaves_cursor_unchanged() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&["kick.wav"]);
        app.browser_cursor = 1;
        app.browser_search_query = "zzz".to_string();
        app.browser_search_update(20);
        assert!(app.browser_search_matches.is_empty());
        assert_eq!(app.browser_cursor, 1, "cursor should not move when there are no matches");
    }

    // ── browser_search_next ──────────────────────────────────────────────────

    #[test]
    fn search_next_advances_to_next_match() {
        let mut app = make_app();
        // entries: ParentDir(0), a(1), b(2), c(3)
        app.browser_entries = make_browser_entries(&["a.wav", "b.wav", "c.wav"]);
        app.browser_search_matches = vec![1, 2, 3];
        app.browser_cursor = 1; // on first match
        app.browser_search_next(20);
        assert_eq!(app.browser_cursor, 2);
    }

    #[test]
    fn search_next_wraps_from_last_to_first() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&["a.wav", "b.wav", "c.wav"]);
        app.browser_search_matches = vec![1, 2, 3];
        app.browser_cursor = 3; // on last match
        app.browser_search_next(20);
        assert_eq!(app.browser_cursor, 1, "next from last match should wrap to first");
    }

    #[test]
    fn search_next_noop_when_no_matches() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&["a.wav"]);
        app.browser_search_matches = vec![];
        app.browser_cursor = 1;
        app.browser_search_next(20);
        assert_eq!(app.browser_cursor, 1, "cursor must not move when matches is empty");
    }

    #[test]
    fn search_next_re_anchors_after_manual_j_k_navigation() {
        // Reproduce the scenario from the review: 5 matches, confirm on match[0],
        // navigate via j/k to match[3], press n → should jump to match[4].
        let mut app = make_app();
        // entries: ParentDir(0), m0(1), gap(2), m1(3), gap(4), m2(5), gap(6), m3(7), gap(8), m4(9)
        app.browser_entries = make_browser_entries(&[
            "m0.wav", "gap.wav", "m1.wav", "gap2.wav", "m2.wav",
            "gap3.wav", "m3.wav", "gap4.wav", "m4.wav",
        ]);
        app.browser_search_matches = vec![1, 3, 5, 7, 9];
        // Simulate: searched, landed on match[0]=1, then user pressed j to land on match[3]=7.
        app.browser_cursor = 7;
        app.browser_search_idx = 0; // stale — left over from when search was confirmed
        app.browser_search_next(20);
        assert_eq!(
            app.browser_cursor, 9,
            "n from cursor=match[3] should advance to match[4], not match[1]"
        );
    }

    // ── browser_search_prev ──────────────────────────────────────────────────

    #[test]
    fn search_prev_retreats_to_previous_match() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&["a.wav", "b.wav", "c.wav"]);
        app.browser_search_matches = vec![1, 2, 3];
        app.browser_cursor = 3; // on last match
        app.browser_search_prev(20);
        assert_eq!(app.browser_cursor, 2);
    }

    #[test]
    fn search_prev_wraps_from_first_to_last() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&["a.wav", "b.wav", "c.wav"]);
        app.browser_search_matches = vec![1, 2, 3];
        app.browser_cursor = 1; // on first match
        app.browser_search_prev(20);
        assert_eq!(app.browser_cursor, 3, "prev from first match should wrap to last");
    }

    #[test]
    fn search_prev_noop_when_no_matches() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&["a.wav"]);
        app.browser_search_matches = vec![];
        app.browser_cursor = 1;
        app.browser_search_prev(20);
        assert_eq!(app.browser_cursor, 1, "cursor must not move when matches is empty");
    }

    #[test]
    fn search_prev_re_anchors_after_manual_navigation() {
        let mut app = make_app();
        app.browser_entries = make_browser_entries(&[
            "m0.wav", "gap.wav", "m1.wav", "gap2.wav", "m2.wav",
            "gap3.wav", "m3.wav", "gap4.wav", "m4.wav",
        ]);
        app.browser_search_matches = vec![1, 3, 5, 7, 9];
        // cursor is at match[3]=7 but idx is stale at 0
        app.browser_cursor = 7;
        app.browser_search_idx = 0;
        app.browser_search_prev(20);
        assert_eq!(
            app.browser_cursor, 5,
            "N from cursor=match[3] should retreat to match[2], not wrap to match[4]"
        );
    }

    // ── browser_search_clear ─────────────────────────────────────────────────

    #[test]
    fn search_clear_resets_all_search_state() {
        let mut app = make_app();
        app.browser_searching = true;
        app.browser_search_query = "kick".to_string();
        app.browser_search_matches = vec![1, 3];
        app.browser_search_idx = 1;
        app.browser_search_clear();
        assert!(!app.browser_searching);
        assert!(app.browser_search_query.is_empty());
        assert!(app.browser_search_matches.is_empty());
        assert_eq!(app.browser_search_idx, 0);
    }
}


