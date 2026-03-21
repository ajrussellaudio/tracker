use anyhow::Result;
use bincode::Options;
use std::io::{BufReader, BufWriter};

use crate::model::Song;

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
/// Returns an error if the file is missing, unreadable, or corrupt.
pub fn load_trk(path: &str) -> Result<Song> {
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    // Cap allocation to 64 MiB to reject corrupt length-prefix attacks.
    let song: Song = bincode::options()
        .with_limit(64 * 1024 * 1024)
        .deserialize_from(reader)?;
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
