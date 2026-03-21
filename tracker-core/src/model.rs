use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION: u32 = 1;
pub const STEPS_PER_PHRASE: usize = 16;
pub const FX_SLOTS_PER_STEP: usize = 4;

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
    /// Filesystem path to the WAV file.
    pub path: String,
    /// When true the bytes are embedded in the project file rather than
    /// referenced by path.
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
    /// MIDI root note (0–127; 60 = middle C).
    pub root_note: u8,
    pub loop_start: Option<u32>,
    pub loop_end: Option<u32>,
    pub interp_mode: InterpMode,
    /// Linear volume scalar (0.0 – 1.0).
    pub volume: f32,
    /// Pan position (−1.0 = full left, 0.0 = centre, 1.0 = full right).
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

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct FxSlot {
    /// FX command byte (0 = no effect).
    pub command: u8,
    /// FX parameter value.
    pub value: u8,
}

// ── Step ──────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Step {
    /// MIDI note number (0–127), or None for an empty step.
    pub note: Option<u8>,
    /// Index into `Song::instruments`, or None.
    pub instrument: Option<u8>,
    /// Note velocity (0–127).
    pub velocity: u8,
    /// Four per-step FX slots.
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

// ── Chain ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Chain {
    /// Ordered list of phrase indices into `Song::phrases`.
    pub phrases: Vec<u8>,
}

// ── Song (top-level document) ─────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Song {
    /// Format version used for migration.
    pub version: u32,
    pub name: String,
    pub bpm: f32,
    pub instruments: Vec<Instrument>,
    pub samples: Vec<Sample>,
    pub phrases: Vec<Phrase>,
    pub chains: Vec<Chain>,
}

impl Default for Song {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            name: String::new(),
            bpm: 120.0,
            instruments: Vec::new(),
            samples: Vec::new(),
            phrases: Vec::new(),
            chains: Vec::new(),
        }
    }
}

// ── Migration ─────────────────────────────────────────────────────────────────

/// Apply all necessary version migrations and return the updated Song.
///
/// Migration table:
///   - v0 → v1: initial version, no-op
pub fn migrate(mut song: Song) -> Song {
    // Future migrations are inserted here, each incrementing song.version.
    // e.g. if song.version == 0 { /* upgrade fields */ song.version = 1; }
    song.version = CURRENT_VERSION;
    song
}
