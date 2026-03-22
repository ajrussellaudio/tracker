use anyhow::Context;

/// Decode a WAV file and downsample to `target_width` peak-amplitude values.
///
/// Returns a `Vec<f32>` of exactly `target_width` elements, each in `[-1.0, 1.0]`,
/// representing the peak absolute amplitude within each evenly-divided window.
pub fn decode_and_downsample(path: &str, target_width: usize) -> anyhow::Result<Vec<f32>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("failed to open WAV: {path}"))?;

    let spec = reader.spec();
    let channels = spec.channels as usize;

    // Decode all samples to f32, normalised to [-1.0, 1.0].
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .context("failed to read float samples")?,
        hound::SampleFormat::Int => {
            let max = (1_i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .samples::<i32>()
                .collect::<Result<Vec<_>, _>>()
                .context("failed to read int samples")?
                .into_iter()
                .map(|s| s as f32 / max)
                .collect()
        }
    };

    // Mix down to mono by averaging channels.
    let mono: Vec<f32> = raw
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();

    if target_width == 0 {
        return Ok(Vec::new());
    }

    let total = mono.len();

    // Chunk into target_width windows; take peak absolute amplitude per window.
    let result: Vec<f32> = (0..target_width)
        .map(|i| {
            let start = i * total / target_width;
            let end = ((i + 1) * total / target_width).max(start + 1).min(total);
            if start >= total {
                return 0.0_f32;
            }
            mono[start..end]
                .iter()
                .map(|s| s.abs())
                .fold(0.0_f32, f32::max)
        })
        .collect();

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &str, channels: u16, samples_per_channel: usize, value: i16) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: 44100,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(path, spec).unwrap();
        for _ in 0..samples_per_channel {
            for _ in 0..channels {
                writer.write_sample(value).unwrap();
            }
        }
        writer.finalize().unwrap();
    }

    #[test]
    fn synthetic_wav_produces_exact_target_width() {
        let path = std::env::temp_dir()
            .join("waveform_test_exact_width.wav")
            .to_str()
            .unwrap()
            .to_owned();
        write_wav(&path, 1, 1000, 16384);
        let result = decode_and_downsample(&path, 100).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(result.len(), 100);
    }

    #[test]
    fn downsample_smaller_than_input_produces_exact_target_width() {
        let path = std::env::temp_dir()
            .join("waveform_test_smaller.wav")
            .to_str()
            .unwrap()
            .to_owned();
        write_wav(&path, 1, 500, 8192);
        let result = decode_and_downsample(&path, 50).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(result.len(), 50);
    }

    #[test]
    fn nonexistent_file_returns_err() {
        let result = decode_and_downsample("/nonexistent/no_such_file.wav", 100);
        assert!(result.is_err());
    }

    #[test]
    fn silent_wav_produces_all_zero_output() {
        let path = std::env::temp_dir()
            .join("waveform_test_silent.wav")
            .to_str()
            .unwrap()
            .to_owned();
        write_wav(&path, 1, 200, 0);
        let result = decode_and_downsample(&path, 50).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(result.len(), 50);
        assert!(result.iter().all(|&v| v == 0.0), "expected all-zero output");
    }
}
