pub mod audio;
pub mod model;
pub mod storage;

pub use model::{
    Chain, ChainSlot, FxCommand, FxSlot, Instrument, InterpMode, MixerTrack, Phrase, Sample,
    Song, Step, CURRENT_VERSION, FX_SLOTS_PER_STEP, STEPS_PER_PHRASE, TRACKS,
};

pub use audio::{Sequencer, StepEvent};

#[cfg(test)]
mod tests {
    use super::*;
    use model::{Instrument, InterpMode, Phrase, Sample, Song, Step};

    fn make_test_song() -> Song {
        let mut song = Song::default();
        song.name = "Test Song".to_string();
        song.bpm = 140.0;
        song.instruments.push(Instrument {
            name: "Kick".to_string(),
            sample: Some(Sample::from_path("/samples/kick.wav")),
            root_note: 36,
            loop_start: Some(0),
            loop_end: Some(512),
            interp_mode: InterpMode::Linear,
            volume: 0.9,
            pan: -0.1,
        });
        let mut phrase = Phrase::default();
        phrase.steps[0] = Step {
            note: Some(60),
            instrument: Some(0),
            velocity: 100,
            fx: Default::default(),
        };
        song.phrases.push(phrase);
        let mut chain = model::Chain::default();
        chain.slots.push(model::ChainSlot { phrase: 0, transpose: 0 });
        song.chains.push(chain);
        song
    }

    #[test]
    fn roundtrip_bincode() {
        let song = make_test_song();
        let path = std::env::temp_dir().join("tracker_test_roundtrip.trk");
        let path_str = path.to_str().unwrap();

        storage::save_trk(&song, path_str).expect("save failed");
        let loaded = storage::load_trk(path_str).expect("load failed");
        std::fs::remove_file(&path).ok();

        assert_eq!(song, loaded);
    }

    #[test]
    fn roundtrip_json() {
        let song = make_test_song();
        let path = std::env::temp_dir().join("tracker_test_roundtrip.json");
        let path_str = path.to_str().unwrap();

        storage::export_json(&song, path_str).expect("export failed");
        let loaded = storage::import_json(path_str).expect("import failed");
        std::fs::remove_file(&path).ok();

        assert_eq!(song, loaded);
    }

    #[test]
    fn load_file_not_found_returns_error() {
        let result = storage::load_trk("/nonexistent/path/file.trk");
        assert!(result.is_err());
    }

    #[test]
    fn load_corrupt_file_returns_error() {
        let path = std::env::temp_dir().join("tracker_test_corrupt.trk");
        std::fs::write(&path, b"this is not valid bincode data!!!").unwrap();
        let result = storage::load_trk(path.to_str().unwrap());
        std::fs::remove_file(&path).ok();
        assert!(result.is_err());
    }

    #[test]
    fn song_has_version_field() {
        let song = Song::default();
        assert_eq!(song.version, CURRENT_VERSION);
    }

    #[test]
    fn migrate_returns_current_version() {
        let mut song = Song::default();
        song.version = 0;
        let migrated = model::migrate(song);
        assert_eq!(migrated.version, CURRENT_VERSION);
    }

    #[test]
    fn song_default_mixer_is_unity_gain() {
        let song = Song::default();
        for t in 0..TRACKS {
            let m = &song.mixer[t];
            assert!((m.volume - 1.0).abs() < 1e-4, "track {t} default volume should be 1.0");
            assert!(m.pan.abs() < 1e-4, "track {t} default pan should be 0.0");
            assert!(!m.mute, "track {t} should not be muted by default");
            assert!(!m.solo, "track {t} should not be soloed by default");
        }
    }

    #[test]
    fn mixer_state_roundtrips_bincode() {
        let mut song = make_test_song();
        song.mixer[0].volume = 0.5;
        song.mixer[1].mute = true;
        song.mixer[2].solo = true;
        song.mixer[3].pan = 0.75;

        let path = std::env::temp_dir().join("tracker_mixer_state_roundtrip.trk");
        let path_str = path.to_str().unwrap();

        storage::save_trk(&song, path_str).expect("save failed");
        let loaded = storage::load_trk(path_str).expect("load failed");
        std::fs::remove_file(&path).ok();

        assert!((loaded.mixer[0].volume - 0.5).abs() < 1e-4, "volume should persist");
        assert!(loaded.mixer[1].mute, "mute should persist");
        assert!(loaded.mixer[2].solo, "solo should persist");
        assert!((loaded.mixer[3].pan - 0.75).abs() < 1e-4, "pan should persist");
    }
}
