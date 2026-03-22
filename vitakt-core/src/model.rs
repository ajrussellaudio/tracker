use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION: u32 = 4;
pub const STEPS_PER_PHRASE: usize = 16;
pub const FX_SLOTS_PER_STEP: usize = 4;
/// Number of simultaneous tracks in the sequencer.
pub const TRACKS: usize = 8;

// ── Interpolation mode ────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub enum InterpMode {
    #[default]
    None,
    Linear,
    Sinc,
}

// ── Sample reference ──────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Sample {
    pub path: String,
    pub embedded: bool,
    /// Raw WAV bytes when loaded from a packed `.trk` file. Not serialized.
    #[serde(skip)]
    pub bytes: Option<Vec<u8>>,
}

impl PartialEq for Sample {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path && self.embedded == other.embedded
    }
}

impl Sample {
    pub fn from_path(path: impl Into<String>) -> Self {
        Self { path: path.into(), embedded: false, bytes: None }
    }
}

// ── Packed song (self-contained bundle with embedded sample bytes) ────────────

/// A packed project file: a [`Song`] plus raw WAV bytes for every referenced
/// sample, keyed by the instrument's original sample path.
#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct PackedSong {
    pub song: Song,
    /// `(original_path_hint, raw_wav_bytes)` — one entry per embedded sample.
    pub samples: Vec<(String, Vec<u8>)>,
}

// ── Instrument ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Instrument {
    pub name: String,
    pub sample: Option<Sample>,
    pub root_note: u8,
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
    #[serde(default)]
    pub sample_start: Option<u32>,
    #[serde(default)]
    pub sample_end: Option<u32>,
    pub interp_mode: InterpMode,
    pub volume: f32,
    pub pan: f32,
}

impl Default for Instrument {
    fn default() -> Self {
        Self {
            name: String::new(),
            sample: None,
            root_note: 60,
            loop_start: None,
            loop_end: None,
            sample_start: None,
            sample_end: None,
            interp_mode: InterpMode::None,
            volume: 1.0,
            pan: 0.0,
        }
    }
}

// ── FX slot ───────────────────────────────────────────────────────────────────

/// Registered FX command identifiers stored as u8 in `FxSlot::command`.
///
/// Adding a new command requires only: add a variant here + a handler branch in
/// `audio.rs` — the `Step` struct and serialisation format stay unchanged.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum FxCommand {
    /// Override track volume for this step (value 0–255 → 0.0–1.0).
    Vol = 1,
    /// Override track pan for this step (0=full left, 128=centre, 255=full right).
    Pan = 2,
    /// Pitch offset in semitones (value treated as signed i8, applied on top of note).
    Pit = 3,
    /// Retrigger: repeat the note `value` times within the step duration.
    Ret = 4,
}

impl FxCommand {
    /// Look up a command by its numeric ID (as stored in `FxSlot::command`).
    pub fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Self::Vol),
            2 => Some(Self::Pan),
            3 => Some(Self::Pit),
            4 => Some(Self::Ret),
            _ => None,
        }
    }

    /// Return the numeric ID for this command.
    pub fn id(&self) -> u8 {
        match self {
            Self::Vol => 1,
            Self::Pan => 2,
            Self::Pit => 3,
            Self::Ret => 4,
        }
    }

    /// Parse a three-letter command code (case-insensitive).
    pub fn from_code(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "VOL" => Some(Self::Vol),
            "PAN" => Some(Self::Pan),
            "PIT" => Some(Self::Pit),
            "RET" => Some(Self::Ret),
            _ => None,
        }
    }

    /// Return the canonical three-letter display code for this command.
    pub fn to_code(&self) -> &'static str {
        match self {
            Self::Vol => "VOL",
            Self::Pan => "PAN",
            Self::Pit => "PIT",
            Self::Ret => "RET",
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct FxSlot {
    pub command: u8,
    pub value: u8,
}

// ── Step ──────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Step {
    pub note: Option<u8>,
    pub instrument: Option<u8>,
    pub velocity: u8,
    pub fx: [FxSlot; FX_SLOTS_PER_STEP],
}

impl Default for Step {
    fn default() -> Self {
        Self {
            note: None,
            instrument: None,
            velocity: 127,
            fx: Default::default(),
        }
    }
}

// ── Phrase ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Phrase {
    pub steps: Vec<Step>,
}

impl Default for Phrase {
    fn default() -> Self {
        Self { steps: vec![Step::default(); STEPS_PER_PHRASE] }
    }
}

// ── ChainSlot ─────────────────────────────────────────────────────────────────

/// One slot in a Chain: a phrase index plus a semitone transpose offset.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ChainSlot {
    /// Index into `Song::phrases`.
    pub phrase: u8,
    /// Semitone transpose applied to every note in this phrase slot.
    pub transpose: i8,
}

// ── Chain ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Chain {
    /// Ordered list of phrase slots (phrase index + semitone transpose).
    pub slots: Vec<ChainSlot>,
}

// ── MixerTrack ────────────────────────────────────────────────────────────────

/// Per-track mixer state persisted in the Song.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MixerTrack {
    /// Output volume multiplier (0.0 = silent, 1.0 = unity gain, 2.0 = double).
    pub volume: f32,
    /// Stereo pan position (-1.0 = full left, 0.0 = centre, 1.0 = full right).
    pub pan: f32,
    /// When true the track produces no audio output.
    pub mute: bool,
    /// When true (and at least one track is soloed) all non-soloed tracks are silent.
    pub solo: bool,
    /// FX send level (0.0–1.0).  Stored and displayed but not yet routed to any bus.
    pub fx_send: f32,
}

impl Default for MixerTrack {
    fn default() -> Self {
        Self { volume: 1.0, pan: 0.0, mute: false, solo: false, fx_send: 0.0 }
    }
}

// ── Song (top-level document) ─────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Song {
    pub version: u32,
    pub name: String,
    pub bpm: f32,
    pub instruments: Vec<Instrument>,
    pub samples: Vec<Sample>,
    pub phrases: Vec<Phrase>,
    pub chains: Vec<Chain>,
    /// Song arrangement grid: each row has one optional chain index per track.
    /// `None` means the track is silent for that row.
    ///
    /// Playback strategy: loops back to row 0 when the last row is exhausted,
    /// so a single-row arrangement produces an infinite phrase loop.
    pub arrangement: Vec<[Option<u8>; TRACKS]>,
    /// Per-track mixer state (volume, pan, mute, solo, FX send).
    #[serde(default)]
    pub mixer: [MixerTrack; TRACKS],
}

impl Default for Song {
    fn default() -> Self {
        // One empty phrase, one chain pointing to it (slot 0, no transpose),
        // and one arrangement row assigning that chain to track 0.
        // This means a fresh project loops phrase 0 on track 0 indefinitely.
        Self {
            version: CURRENT_VERSION,
            name: String::new(),
            bpm: 120.0,
            instruments: Vec::new(),
            samples: Vec::new(),
            phrases: vec![Phrase::default()],
            chains: vec![Chain {
                slots: vec![ChainSlot { phrase: 0, transpose: 0 }],
            }],
            arrangement: vec![[Some(0), None, None, None, None, None, None, None]],
            mixer: Default::default(),
        }
    }
}

// ── Migration ─────────────────────────────────────────────────────────────────

/// Apply all necessary version migrations and return the updated Song.
///
/// Migration table:
///   v0 → v1: initial version, no-op
///   v1 → v2: Chain `phrases: Vec<u8>` replaced by `slots: Vec<ChainSlot>`;
///            `Song::arrangement` added. Binary (.trk) v1 files are not
///            layout-compatible (will return a deserialization error on load).
///   v2 → v3: `Song::mixer` field added (per-track vol/pan/mute/solo/FX send).
///            Missing field defaults to all-unity (volume=1, pan=0, rest false/zero).
///   v3 → v4: `Instrument::sample_start` and `sample_end` fields added.
///            Missing fields default to `None` (full file range, unchanged behaviour).
pub fn migrate(mut song: Song) -> Song {
    song.version = CURRENT_VERSION;
    song
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn song_deserializes_without_mixer_field() {
        // Simulates loading a v2 JSON file that pre-dates the mixer field.
        // #[serde(default)] on Song::mixer must prevent a "missing field" error.
        let json = r#"{
            "version": 2,
            "name": "",
            "bpm": 120.0,
            "instruments": [],
            "samples": [],
            "phrases": [{"steps": []}],
            "chains": [{"slots": [{"phrase": 0, "transpose": 0}]}],
            "arrangement": [[0, null, null, null, null, null, null, null]]
        }"#;
        let song: Song = serde_json::from_str(json).expect("v2 JSON should deserialise without mixer field");
        // All tracks should default to unity gain
        for track in &song.mixer {
            assert!((track.volume - 1.0).abs() < f32::EPSILON);
            assert!((track.pan).abs() < f32::EPSILON);
            assert!(!track.mute);
            assert!(!track.solo);
        }
    }

    #[test]
    fn v3_instrument_deserializes_without_sample_start_end() {
        // Simulates loading a v3 JSON file that pre-dates sample_start/sample_end.
        let json = r#"{
            "version": 3,
            "name": "",
            "bpm": 120.0,
            "instruments": [{
                "name": "kick",
                "sample": null,
                "root_note": 60,
                "loop_start": null,
                "loop_end": null,
                "interp_mode": "None",
                "volume": 1.0,
                "pan": 0.0
            }],
            "samples": [],
            "phrases": [{"steps": []}],
            "chains": [{"slots": [{"phrase": 0, "transpose": 0}]}],
            "arrangement": [[0, null, null, null, null, null, null, null]]
        }"#;
        let song: Song = serde_json::from_str(json)
            .expect("v3 JSON should deserialise without sample_start/sample_end");
        let instr = &song.instruments[0];
        assert!(instr.sample_start.is_none(), "sample_start should default to None");
        assert!(instr.sample_end.is_none(), "sample_end should default to None");
    }

    #[test]
    fn instrument_sample_start_end_round_trips() {
        let mut song = Song::default();
        let mut instr = Instrument::default();
        instr.sample_start = Some(100);
        instr.sample_end = Some(8000);
        song.instruments.push(instr);
        let json = serde_json::to_string(&song).unwrap();
        let loaded: Song = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.instruments[0].sample_start, Some(100));
        assert_eq!(loaded.instruments[0].sample_end, Some(8000));
    }

    #[test]
    fn migrate_sets_version_to_current() {
        let mut old_song = Song::default();
        old_song.version = 3;
        let migrated = migrate(old_song);
        assert_eq!(migrated.version, CURRENT_VERSION);
    }
}
