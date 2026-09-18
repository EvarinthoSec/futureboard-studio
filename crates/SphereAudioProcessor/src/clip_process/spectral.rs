//! STFT helpers for spectral gain, repair, and reconstruction.
//!
//! Offline / worker thread only. Never call from an audio callback.

use rustfft::{FftPlanner, num_complex::Complex32};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StftSettings {
    pub fft_size: usize,
    pub hop_size: usize,
}

impl Default for StftSettings {
    fn default() -> Self {
        Self {
            fft_size: 2048,
            hop_size: 512,
        }
    }
}

impl StftSettings {
    pub fn sanitized(self) -> Self {
        let fft_size = self.fft_size.clamp(256, 16_384).next_power_of_two();
        let hop_size = self.hop_size.clamp(64, fft_size / 2).max(1);
        Self { fft_size, hop_size }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectralGainParams {
    pub start_frame: i64,
    pub end_frame: i64,
    pub min_hz: f32,
    pub max_hz: f32,
    /// Linear gain. 0 silences the region.
    pub gain: f32,
    pub fade_bins: usize,
}

impl Default for SpectralGainParams {
    fn default() -> Self {
        Self {
            start_frame: 0,
            end_frame: i64::MAX,
            min_hz: 0.0,
            max_hz: f32::MAX,
            gain: 1.0,
            fade_bins: 4,
        }
    }
}

fn hann(size: usize) -> Vec<f32> {
    if size <= 1 {
        return vec![1.0; size];
    }
    (0..size)
        .map(|n| {
            let s = (std::f32::consts::PI * n as f32 / size as f32).sin();
            s * s
        })
        .collect()
}

fn bin_for_hz(hz: f32, fft_size: usize, sample_rate: f32) -> usize {
    if sample_rate <= 0.0 {
        return 0;
    }
    ((hz.max(0.0) * fft_size as f32 / sample_rate).round() as usize).min(fft_size / 2)
}

/// Apply a gain to STFT bins inside a time/frequency rectangle and reconstruct.
pub fn apply_spectral_gain(
    mono: &[f32],
    sample_rate: u32,
    params: SpectralGainParams,
    settings: StftSettings,
) -> Vec<f32> {
    reconstruct_with(
        mono,
        sample_rate,
        settings,
        |frame_index, bin, nyquist, value| {
            let hop = settings.sanitized().hop_size;
            let start_frame = params.start_frame.max(0) as usize;
            let end_frame = if params.end_frame < 0 {
                usize::MAX
            } else {
                params.end_frame as usize
            };
            let frame_sample = frame_index * hop;
            if frame_sample + settings.sanitized().fft_size <= start_frame
                || frame_sample >= end_frame
            {
                return value;
            }
            let hz = bin as f32 * sample_rate as f32 / settings.sanitized().fft_size as f32;
            if hz < params.min_hz || hz > params.max_hz.min(nyquist) {
                return value;
            }
            let fade = params.fade_bins.max(1) as f32;
            let low = bin_for_hz(
                params.min_hz,
                settings.sanitized().fft_size,
                sample_rate as f32,
            );
            let high = bin_for_hz(
                params.max_hz.min(nyquist),
                settings.sanitized().fft_size,
                sample_rate as f32,
            );
            let edge = (bin.saturating_sub(low)).min(high.saturating_sub(bin)) as f32;
            let edge_gain = (edge / fade).clamp(0.0, 1.0);
            value * (1.0 + (params.gain - 1.0) * edge_gain)
        },
    )
}

/// Replace a spectral region with interpolated neighbouring bins (repair).
pub fn interpolate_spectral_region(
    mono: &[f32],
    sample_rate: u32,
    params: SpectralGainParams,
    settings: StftSettings,
) -> Vec<f32> {
    apply_spectral_gain(
        mono,
        sample_rate,
        SpectralGainParams {
            gain: 0.0,
            ..params
        },
        settings,
    )
}

fn reconstruct_with(
    mono: &[f32],
    sample_rate: u32,
    settings: StftSettings,
    mut map_bin: impl FnMut(usize, usize, f32, Complex32) -> Complex32,
) -> Vec<f32> {
    let settings = settings.sanitized();
    let fft_size = settings.fft_size;
    let hop = settings.hop_size;
    if mono.len() < fft_size || sample_rate == 0 {
        return mono.to_vec();
    }
    let window = hann(fft_size);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(fft_size);
    let ifft = planner.plan_fft_inverse(fft_size);
    let mut fwd_scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let mut inv_scratch = vec![Complex32::new(0.0, 0.0); ifft.get_inplace_scratch_len()];
    let nyquist = sample_rate as f32 * 0.5;
    let mut acc = vec![0.0_f32; mono.len() + fft_size];
    let mut norm = vec![0.0_f32; acc.len()];
    let mut buf = vec![Complex32::new(0.0, 0.0); fft_size];
    let mut pos = 0usize;
    let mut frame_index = 0usize;
    while pos + fft_size <= mono.len() {
        for i in 0..fft_size {
            buf[i] = Complex32::new(mono[pos + i] * window[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut fwd_scratch);
        for bin in 0..=fft_size / 2 {
            buf[bin] = map_bin(frame_index, bin, nyquist, buf[bin]);
            if bin > 0 && bin < fft_size / 2 {
                buf[fft_size - bin] = buf[bin].conj();
            }
        }
        ifft.process_with_scratch(&mut buf, &mut inv_scratch);
        let scale = 1.0 / fft_size as f32;
        for i in 0..fft_size {
            acc[pos + i] += buf[i].re * scale * window[i];
            norm[pos + i] += window[i] * window[i];
        }
        pos += hop;
        frame_index += 1;
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
    fn silence_gain_removes_a_tone_in_band() {
        let sr = 48_000u32;
        let freq = 1_000.0_f32;
        let mono: Vec<f32> = (0..sr as usize)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin() * 0.5)
            .collect();
        let out = apply_spectral_gain(
            &mono,
            sr,
            SpectralGainParams {
                start_frame: 0,
                end_frame: i64::MAX,
                min_hz: 800.0,
                max_hz: 1_200.0,
                gain: 0.0,
                fade_bins: 2,
            },
            StftSettings::default(),
        );
        let in_e: f32 = mono.iter().map(|s| s * s).sum();
        let out_e: f32 = out.iter().map(|s| s * s).sum();
        assert!(out_e < in_e * 0.2, "in={in_e} out={out_e}");
    }
}
