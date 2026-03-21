use anyhow::Result;
use bincode::Options;
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Read, Write};

use crate::model::{PackedSong, Song};

/// Magic prefix for packed `.trk` files (TRKP).
const PACKED_MAGIC: &[u8; 4] = b"TRKP";

/// Save a [`Song`] to a binary `.trk` file using bincode encoding.
pub fn save_trk(song: &Song, path: &str) -> Result<()> {
    let file = std::fs::File::create(path)?;
    let writer = BufWriter::new(file);
    bincode::options()
        .with_limit(64 * 1024 * 1024)
        .serialize_into(writer, song)?;
    Ok(())
}

/// Load a [`Song`] from a binary `.trk` file.
///
/// Automatically detects packed files (written by [`save_packed_trk`]) and
/// populates embedded sample bytes into `instrument.sample.bytes` so the
/// audio system can use them without touching the filesystem.
///
/// Returns an error if the file is missing, unreadable, or corrupt.
pub fn load_trk(path: &str) -> Result<Song> {
    if is_packed_file(path)? {
        return load_packed_trk_inner(path);
    }
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    // Cap allocation to 64 MiB to reject corrupt length-prefix attacks.
    let song: Song = bincode::options()
        .with_limit(64 * 1024 * 1024)
        .deserialize_from(reader)?;
    Ok(song)
}

/// Write a self-contained packed `.trk` file with all sample WAV bytes embedded.
///
/// For each instrument whose `sample` field points to a readable WAV file, the
/// raw file bytes are embedded and `sample.embedded` is set to `true`. Instruments
/// whose sample file cannot be read are left unchanged and their paths are
/// reported in the returned list of failures.
///
/// Returns a list of `"path: error"` strings for samples that could **not** be
/// embedded (empty on full success).
pub fn save_packed_trk(song: &Song, path: &str) -> Result<Vec<String>> {
    let mut packed = PackedSong { song: song.clone(), samples: Vec::new() };
    let mut failed: Vec<String> = Vec::new();

    for instr in &mut packed.song.instruments {
        if let Some(sample) = &mut instr.sample {
            match std::fs::read(&sample.path) {
                Ok(bytes) => {
                    packed.samples.push((sample.path.clone(), bytes));
                    sample.embedded = true;
                }
                Err(e) => {
                    failed.push(format!("{}: {e}", sample.path));
                }
            }
        }
    }

    let file = std::fs::File::create(path)?;
    let mut writer = BufWriter::new(file);
    writer.write_all(PACKED_MAGIC)?;
    // Allow up to 256 MiB for packed files that may contain many samples.
    bincode::options()
        .with_limit(256 * 1024 * 1024)
        .serialize_into(&mut writer, &packed)?;

    Ok(failed)
}

/// Returns `true` if the file at `path` begins with the packed-file magic bytes.
fn is_packed_file(path: &str) -> Result<bool> {
    let mut file = std::fs::File::open(path)?;
    let mut magic = [0u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return Ok(false);
    }
    Ok(&magic == PACKED_MAGIC)
}

/// Deserialise a packed `.trk` file and repopulate `sample.bytes` for every
/// embedded instrument so the caller can play audio without reading from disk.
fn load_packed_trk_inner(path: &str) -> Result<Song> {
    let file = std::fs::File::open(path)?;
    let mut reader = BufReader::new(file);
    // Skip the 4-byte magic we already verified.
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;

    let packed: PackedSong = bincode::options()
        .with_limit(256 * 1024 * 1024)
        .deserialize_from(reader)?;

    let mut song = packed.song;
    let sample_map: HashMap<String, Vec<u8>> = packed.samples.into_iter().collect();

    for instr in &mut song.instruments {
        if let Some(sample) = &mut instr.sample {
            if sample.embedded {
                if let Some(bytes) = sample_map.get(&sample.path) {
                    sample.bytes = Some(bytes.clone());
                }
            }
        }
    }

    Ok(song)
}

/// Write a human-readable JSON representation of a [`Song`].
pub fn export_json(song: &Song, path: &str) -> Result<()> {
    let json = serde_json::to_string_pretty(song)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Load a [`Song`] from a JSON file (useful for debugging / round-trip tests).
pub fn import_json(path: &str) -> Result<Song> {
    let text = std::fs::read_to_string(path)?;
    let song: Song = serde_json::from_str(&text)?;
    Ok(song)
}
