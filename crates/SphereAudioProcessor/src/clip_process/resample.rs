//! Offline sample-rate conversion via rubato. Worker thread only.

use rubato::{
    Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};
use std::fs::File;
use std::io::Write;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ResampleError {
    #[error("invalid sample rate")]
    InvalidRate,
    #[error("resampler: {0}")]
    Rubato(String),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
}

/// Resample interleaved PCM from `source_rate` to `target_rate`.
pub fn resample_interleaved(
    samples: &[f32],
    channels: usize,
    source_rate: u32,
    target_rate: u32,
) -> Result<Vec<f32>, ResampleError> {
    let channels = channels.max(1);
    if source_rate == 0 || target_rate == 0 {
        return Err(ResampleError::InvalidRate);
    }
    if source_rate == target_rate {
        return Ok(samples.to_vec());
    }
    let frames = samples.len() / channels;
    if frames == 0 {
        return Ok(Vec::new());
    }
    let mut planar_in = vec![vec![0.0_f32; frames]; channels];
    for (frame, chunk) in samples.chunks(channels).enumerate() {
        for ch in 0..channels {
            planar_in[ch][frame] = chunk.get(ch).copied().unwrap_or(0.0);
        }
    }
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Linear,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let chunk = 1024.min(frames).max(64);
    let mut resampler = SincFixedIn::<f32>::new(
        target_rate as f64 / source_rate as f64,
        2.0,
        params,
        chunk,
        channels,
    )
    .map_err(|e| ResampleError::Rubato(e.to_string()))?;

    let mut planar_out = vec![Vec::new(); channels];
    let mut offset = 0usize;
    while offset < frames {
        let end = (offset + chunk).min(frames);
        let mut block: Vec<Vec<f32>> = (0..channels)
            .map(|ch| {
                let mut row = planar_in[ch][offset..end].to_vec();
                if row.len() < chunk {
                    row.resize(chunk, 0.0);
                }
                row
            })
            .collect();
        if end == frames && end - offset < chunk {
            let waves = resampler
                .process_partial(Some(&block), None)
                .map_err(|e| ResampleError::Rubato(e.to_string()))?;
            for ch in 0..channels {
                planar_out[ch].extend_from_slice(&waves[ch]);
            }
            break;
        }
        let waves = resampler
            .process(&block, None)
            .map_err(|e| ResampleError::Rubato(e.to_string()))?;
        for ch in 0..channels {
            planar_out[ch].extend_from_slice(&waves[ch]);
        }
        let _ = &mut block;
        offset = end;
    }

    let out_frames = planar_out.first().map(|c| c.len()).unwrap_or(0);
    let mut interleaved = Vec::with_capacity(out_frames * channels);
    for frame in 0..out_frames {
        for ch in 0..channels {
            interleaved.push(planar_out[ch][frame]);
        }
    }
    Ok(interleaved)
}

/// Write interleaved f32 PCM as a 16-bit WAV. Used to create a derived asset.
pub fn write_wav_f32(
    path: &Path,
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
) -> Result<(), ResampleError> {
    let channels = channels.max(1);
    let frames = samples.len() / channels as usize;
    let data_bytes = frames * channels as usize * 2;
    let mut header = Vec::with_capacity(44 + data_bytes);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_bytes as u32).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * channels as u32 * 2;
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&(channels * 2).to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for sample in samples.iter().take(frames * channels as usize) {
        let clamped = sample.clamp(-1.0, 1.0);
        let int = (clamped * 32767.0).round() as i16;
        header.extend_from_slice(&int.to_le_bytes());
    }
    let mut file = File::create(path)?;
    file.write_all(&header)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_rate_copies() {
        let samples = vec![0.1, -0.2, 0.3, -0.4];
        let out = resample_interleaved(&samples, 2, 48_000, 48_000).unwrap();
        assert_eq!(out, samples);
    }
}
