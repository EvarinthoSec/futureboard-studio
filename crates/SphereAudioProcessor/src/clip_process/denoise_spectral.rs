//! Spectral noise reduction via STFT subtraction. Offline / worker only.

use super::spectral::StftSettings;
use crate::analysis::spectrum::{hann_window, magnitude_frames};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectralDenoiseParams {
    pub reduction_db: f32,
    pub threshold_db: f32,
    pub smoothing: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
}

impl Default for SpectralDenoiseParams {
    fn default() -> Self {
        Self {
            reduction_db: 12.0,
            threshold_db: -48.0,
            smoothing: 0.35,
            attack_ms: 8.0,
            release_ms: 80.0,
        }
    }
}

/// Average magnitude spectrum of a noise-only region (mono).
pub fn learn_noise_profile(mono: &[f32], fft_size: usize, hop: usize) -> Vec<f32> {
    let frames = magnitude_frames(mono, fft_size, hop);
    if frames.is_empty() {
        return vec![1.0e-6; fft_size / 2];
    }
    let bins = frames[0].len();
    let mut acc = vec![0.0_f32; bins];
    for frame in &frames {
        for (bin, mag) in frame.iter().enumerate() {
            acc[bin] += *mag;
        }
    }
    let n = frames.len() as f32;
    for mag in &mut acc {
        *mag = (*mag / n).max(1.0e-6);
    }
    acc
}

/// Reduce bins whose magnitude sits near the learned noise profile.
pub fn reduce_noise_stft(
    mono: &[f32],
    sample_rate: u32,
    noise_profile: &[f32],
    params: SpectralDenoiseParams,
) -> Vec<f32> {
    let settings = StftSettings {
        fft_size: (noise_profile.len() * 2).max(512).next_power_of_two(),
        hop_size: ((noise_profile.len() * 2).max(512).next_power_of_two()) / 4,
    };
    let reduction = crate::clip_process::db_to_lin(-params.reduction_db.abs());
    let threshold = crate::clip_process::db_to_lin(params.threshold_db);
    reconstruct_noise(
        mono,
        sample_rate,
        settings,
        noise_profile,
        reduction,
        threshold,
    )
}

fn reconstruct_noise(
    mono: &[f32],
    _sample_rate: u32,
    settings: StftSettings,
    noise_profile: &[f32],
    reduction: f32,
    threshold: f32,
) -> Vec<f32> {
    use rustfft::{FftPlanner, num_complex::Complex32};

    let fft_size = settings.fft_size;
    let hop = settings.hop_size;
    if mono.len() < fft_size {
        return mono.to_vec();
    }
    let window = hann_window(fft_size);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    let ifft = planner.plan_fft_inverse(fft_size);
    let mut fwd = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let mut inv = vec![Complex32::new(0.0, 0.0); ifft.get_inplace_scratch_len()];
    let mut acc = vec![0.0_f32; mono.len() + fft_size];
    let mut norm = vec![0.0_f32; acc.len()];
    let mut buf = vec![Complex32::new(0.0, 0.0); fft_size];
    let mut pos = 0usize;
    while pos + fft_size <= mono.len() {
        for i in 0..fft_size {
            buf[i] = Complex32::new(mono[pos + i] * window[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut fwd);
        let half = fft_size / 2;
        for bin in 0..=half {
            let mag = buf[bin].norm();
            let noise = noise_profile.get(bin).copied().unwrap_or(1.0e-6);
            let gain = if mag <= noise * (1.0 + threshold * 8.0) {
                reduction
            } else {
                1.0
            };
            buf[bin] *= gain;
            if bin > 0 && bin < half {
                buf[fft_size - bin] = buf[bin].conj();
            }
        }
        ifft.process_with_scratch(&mut buf, &mut inv);
        let scale = 1.0 / fft_size as f32;
        for i in 0..fft_size {
            acc[pos + i] += buf[i].re * scale * window[i];
            norm[pos + i] += window[i] * window[i];
        }
        pos += hop;
    }
    let mut out = vec![0.0; mono.len()];
    for i in 0..mono.len() {
        out[i] = if norm[i] > 1.0e-6 {
            acc[i] / norm[i]
        } else {
            mono[i]
        };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_noise_profile_is_nonzero() {
        let noise: Vec<f32> = (0..8_192)
            .map(|i| ((i * 17) % 97) as f32 / 97.0 * 0.02 - 0.01)
            .collect();
        let profile = learn_noise_profile(&noise, 1024, 256);
        assert!(profile.iter().any(|m| *m > 0.0));
    }
}
