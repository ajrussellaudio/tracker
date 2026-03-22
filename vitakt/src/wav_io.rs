use anyhow::{Context, Result};
use std::sync::Arc;
use vitakt_core::model::Song;

/// Write a stereo 48 kHz 32-bit float PCM WAV file.
pub fn write_wav(path: &str, samples: &[f32]) -> Result<()> {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 48000,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let file =
        std::fs::File::create(path).with_context(|| format!("cannot create WAV: {path}"))?;
    let mut writer = hound::WavWriter::new(std::io::BufWriter::new(file), spec)
        .with_context(|| format!("cannot open WavWriter for: {path}"))?;
    for &s in samples {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(())
}

/// Decode WAV files for all instruments in `song` into f32 buffers.
/// Instruments without a sample path (or whose file cannot be loaded) get `None`.
pub fn load_all_instrument_samples(
    song: &Song,
) -> Vec<Option<(Arc<Vec<f32>>, usize)>> {
    song.instruments
        .iter()
        .map(|instr| {
            instr.sample.as_ref().and_then(|s| {
                if let Some(bytes) = &s.bytes {
                    load_wav_from_bytes(bytes).ok()
                } else {
                    load_wav(&s.path).ok()
                }
            })
        })
        .collect()
}

pub fn load_wav(path: &str) -> Result<(Arc<Vec<f32>>, usize)> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("failed to open WAV: {path}"))?;
    decode_wav_reader(&mut reader)
}

pub fn load_wav_from_bytes(bytes: &[u8]) -> Result<(Arc<Vec<f32>>, usize)> {
    let cursor = std::io::Cursor::new(bytes);
    let mut reader =
        hound::WavReader::new(cursor).context("failed to parse embedded WAV bytes")?;
    decode_wav_reader(&mut reader)
}

pub fn decode_wav_reader<R: std::io::Read + std::io::Seek>(
    reader: &mut hound::WavReader<R>,
) -> Result<(Arc<Vec<f32>>, usize)> {
    let spec = reader.spec();
    let channels = spec.channels as usize;
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|s| s.map_err(anyhow::Error::from))
            .collect::<Result<_>>()?,
        hound::SampleFormat::Int => {
            let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            match spec.bits_per_sample {
                16 => reader
                    .samples::<i16>()
                    .map(|s| s.map(|v| v as f32 / max).map_err(anyhow::Error::from))
                    .collect::<Result<_>>()?,
                24 | 32 => reader
                    .samples::<i32>()
                    .map(|s| s.map(|v| v as f32 / max).map_err(anyhow::Error::from))
                    .collect::<Result<_>>()?,
                _ => anyhow::bail!("unsupported bit depth: {}", spec.bits_per_sample),
            }
        }
    };
    Ok((Arc::new(samples), channels))
}
