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

/// Time/frequency map of the bins the de-noiser will attenuate.
///
/// `values` is row-major over `columns` STFT frames of `bins` each and holds
/// the applied linear gain, so `1.0` means "passed through" and anything below
/// that is real attenuation the editor can shade onto the canvas.
#[derive(Debug, Clone)]
pub struct NoiseGateMask {
    pub columns: usize,
    pub bins: usize,
    pub hop_size: usize,
    pub fft_size: usize,
    pub values: Vec<f32>,
}

impl NoiseGateMask {
    pub fn gain(&self, column: usize, bin: usize) -> f32 {
        self.values
            .get(column * self.bins + bin)
            .copied()
            .unwrap_or(1.0)
    }

    /// Share of the analyzed cells the gate engages on, 0..=1.
    pub fn coverage(&self) -> f32 {
        if self.values.is_empty() {
            return 0.0;
        }
        let gated = self.values.iter().filter(|gain| **gain < 1.0).count();
        gated as f32 / self.values.len() as f32
    }
}

fn stft_settings_for_profile(noise_profile: &[f32]) -> StftSettings {
    let fft_size = (noise_profile.len() * 2).max(512).next_power_of_two();
    StftSettings {
        fft_size,
        hop_size: fft_size / 4,
    }
}

/// The gate decision shared by the renderer and the canvas overlay.
#[inline]
fn gate_gain(magnitude: f32, noise: f32, threshold: f32, reduction: f32) -> f32 {
    if magnitude <= noise * (1.0 + threshold * 8.0) {
        reduction
    } else {
        1.0
    }
}

/// Compute the attenuation map [`reduce_noise_stft`] would apply, without
/// reconstructing audio.
pub fn noise_gate_mask(
    mono: &[f32],
    noise_profile: &[f32],
    params: SpectralDenoiseParams,
) -> Option<NoiseGateMask> {
    use rustfft::{FftPlanner, num_complex::Complex32};

    let settings = stft_settings_for_profile(noise_profile);
    let fft_size = settings.fft_size;
    let hop = settings.hop_size;
    if mono.len() < fft_size || noise_profile.is_empty() {
        return None;
    }
    let reduction = crate::clip_process::db_to_lin(-params.reduction_db.abs());
    let threshold = crate::clip_process::db_to_lin(params.threshold_db);
    let bins = fft_size / 2 + 1;
    let columns = (mono.len() - fft_size) / hop + 1;
    let window = hann_window(fft_size);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    let mut scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let mut buf = vec![Complex32::new(0.0, 0.0); fft_size];
    let mut values = vec![1.0_f32; columns * bins];

    for column in 0..columns {
        let pos = column * hop;
        for (i, slot) in buf.iter_mut().enumerate() {
            *slot = Complex32::new(mono[pos + i] * window[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);
        for bin in 0..bins {
            let noise = noise_profile.get(bin).copied().unwrap_or(1.0e-6);
            values[column * bins + bin] = gate_gain(buf[bin].norm(), noise, threshold, reduction);
        }
    }

    Some(NoiseGateMask {
        columns,
        bins,
        hop_size: hop,
        fft_size,
        values,
    })
}

/// Reduce bins whose magnitude sits near the learned noise profile.
pub fn reduce_noise_stft(
    mono: &[f32],
    sample_rate: u32,
    noise_profile: &[f32],
    params: SpectralDenoiseParams,
) -> Vec<f32> {
    let settings = stft_settings_for_profile(noise_profile);
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
            let noise = noise_profile.get(bin).copied().unwrap_or(1.0e-6);
            let gain = gate_gain(buf[bin].norm(), noise, threshold, reduction);
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

    #[test]
    fn gate_mask_marks_the_bins_the_renderer_attenuates() {
        let noise: Vec<f32> = (0..16_384)
            .map(|i| ((i * 17) % 97) as f32 / 97.0 * 0.02 - 0.01)
            .collect();
        let profile = learn_noise_profile(&noise, 1024, 256);
        let params = SpectralDenoiseParams::default();
        let mask = noise_gate_mask(&noise, &profile, params).expect("mask");
        assert!(mask.columns > 1);
        assert_eq!(mask.values.len(), mask.columns * mask.bins);
        assert!(mask.coverage() > 0.0, "noise-only input should gate");
        assert!(mask.values.iter().all(|gain| *gain <= 1.0));
    }

    #[test]
    fn gate_mask_leaves_loud_tones_open() {
        let sr = 48_000.0;
        let quiet: Vec<f32> = (0..16_384)
            .map(|i| ((i % 7) as f32 - 3.0) * 1.0e-4)
            .collect();
        let profile = learn_noise_profile(&quiet, 1024, 256);
        let tone: Vec<f32> = (0..16_384)
            .map(|i| (std::f32::consts::TAU * 1_000.0 * i as f32 / sr).sin() * 0.5)
            .collect();
        let mask =
            noise_gate_mask(&tone, &profile, SpectralDenoiseParams::default()).expect("mask");
        assert!(
            mask.coverage() < 0.98,
            "a loud tone must keep some bins open"
        );
    }
}
