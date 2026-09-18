//! Transient detection via spectral flux + adaptive threshold + peak picking.
//!
//! Offline / worker thread only.

use serde::{Deserialize, Serialize};

use super::spectrum::magnitude_frames;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TransientMarker {
    pub id: u64,
    pub source_frame: u64,
    pub strength: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransientDetectParams {
    /// 0..=1
    pub sensitivity: f32,
    pub min_gap_ms: f32,
    pub freq_low_hz: f32,
    pub freq_high_hz: f32,
}

impl Default for TransientDetectParams {
    fn default() -> Self {
        Self {
            sensitivity: 0.65,
            min_gap_ms: 20.0,
            freq_low_hz: 0.0,
            freq_high_hz: f32::MAX,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrequencyFocus {
    FullBand,
    Low,
    Mid,
    High,
}

impl FrequencyFocus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::FullBand => "Full Band",
            Self::Low => "Low",
            Self::Mid => "Mid",
            Self::High => "High",
        }
    }

    pub fn band(self, nyquist: f32) -> (f32, f32) {
        match self {
            Self::FullBand => (0.0, nyquist),
            Self::Low => (0.0, 250.0_f32.min(nyquist)),
            Self::Mid => (250.0, 4_000.0_f32.min(nyquist)),
            Self::High => (4_000.0_f32.min(nyquist), nyquist),
        }
    }
}

const FRAME: usize = 1024;
const HOP: usize = 256;

pub fn detect_transients(
    mono: &[f32],
    sample_rate: u32,
    params: TransientDetectParams,
) -> Vec<TransientMarker> {
    if sample_rate == 0 || mono.len() < FRAME * 2 {
        return Vec::new();
    }
    let frames = magnitude_frames(mono, FRAME, HOP);
    if frames.len() < 8 {
        return Vec::new();
    }
    let nyquist = sample_rate as f32 * 0.5;
    let low_bin = ((params.freq_low_hz.max(0.0) * FRAME as f32 / sample_rate as f32).floor()
        as usize)
        .min(FRAME / 2);
    let high_bin = ((params.freq_high_hz.min(nyquist) * FRAME as f32 / sample_rate as f32).ceil()
        as usize)
        .clamp(low_bin + 1, FRAME / 2);

    let mut flux = Vec::with_capacity(frames.len().saturating_sub(1));
    for pair in frames.windows(2) {
        let mut sum = 0.0_f32;
        let start = low_bin.max(1);
        let end = high_bin.min(pair[0].len()).min(pair[1].len());
        for bin in start..end {
            let d = pair[1][bin] - pair[0][bin];
            if d > 0.0 {
                sum += d;
            }
        }
        flux.push(sum);
    }
    // Time-domain energy onsets catch short clicks that a long STFT window
    // otherwise smears into the noise floor.
    let energy_hop = HOP;
    if mono.len() >= energy_hop * 2 {
        let mut energy = Vec::with_capacity(mono.len() / energy_hop);
        let mut i = 0usize;
        while i + energy_hop <= mono.len() {
            let mut e = 0.0_f32;
            for sample in &mono[i..i + energy_hop] {
                e += sample * sample;
            }
            energy.push(e);
            i += energy_hop;
        }
        if energy.len() + 1 >= flux.len() && flux.len() > 1 {
            for i in 0..flux.len() {
                let prev = energy.get(i).copied().unwrap_or(0.0);
                let next = energy.get(i + 1).copied().unwrap_or(prev);
                flux[i] += (next - prev).max(0.0);
            }
        }
    }
    if flux.is_empty() {
        return Vec::new();
    }
    let mean = flux.iter().sum::<f32>() / flux.len() as f32;
    let mut var = 0.0_f32;
    for v in &flux {
        let d = *v - mean;
        var += d * d;
    }
    let std = (var / flux.len() as f32).sqrt().max(1.0e-6);
    let sensitivity = params.sensitivity.clamp(0.05, 1.0);
    let threshold = mean + std * (1.8 - sensitivity * 1.4);
    let min_gap_frames =
        ((params.min_gap_ms.max(1.0) * 0.001 * sample_rate as f32) / HOP as f32).ceil() as usize;
    let min_gap_frames = min_gap_frames.max(1);

    let mut peaks = Vec::new();
    let mut last: Option<usize> = None;
    for (i, value) in flux.iter().copied().enumerate() {
        if i == 0 || i + 1 >= flux.len() {
            continue;
        }
        if value > threshold && value >= flux[i - 1] && value >= flux[i + 1] {
            if last.is_none_or(|prev| i.saturating_sub(prev) >= min_gap_frames) {
                let strength = ((value - threshold) / (std * 4.0)).clamp(0.0, 1.0);
                peaks.push(TransientMarker {
                    id: (peaks.len() as u64).saturating_add(1),
                    source_frame: (i as u64) * HOP as u64,
                    strength,
                });
                last = Some(i);
            }
        }
    }
    peaks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clicks_produce_transients() {
        let sr = 48_000u32;
        let mut mono = vec![0.0_f32; sr as usize];
        for frame in [2_000usize, 12_000, 24_000] {
            mono[frame] = 1.0;
            if frame + 1 < mono.len() {
                mono[frame + 1] = -0.5;
            }
            for i in 2..64 {
                if frame + i < mono.len() {
                    mono[frame + i] =
                        0.35 * (-(i as f32) / 12.0).exp() * if i % 2 == 0 { 1.0 } else { -1.0 };
                }
            }
        }
        let markers = detect_transients(&mono, sr, TransientDetectParams::default());
        assert!(markers.len() >= 2, "got {}", markers.len());
    }
}
