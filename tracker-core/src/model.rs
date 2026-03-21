use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION: u32 = 2;
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
pub fn migrate(mut song: Song) -> Song {
    song.version = CURRENT_VERSION;
    song
}
