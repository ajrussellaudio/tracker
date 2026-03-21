use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION: u32 = 3;
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Sample {
    pub path: String,
    pub embedded: bool,
}

impl Sample {
    pub fn from_path(path: impl Into<String>) -> Self {
        Self { path: path.into(), embedded: false }
    }
}

// ── Instrument ────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Instrument {
    pub name: String,
    pub sample: Option<Sample>,
    pub root_note: u8,
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
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
}
