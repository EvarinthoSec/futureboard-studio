//! EBU R128 loudness analysis. Offline / worker thread only.

use serde::{Deserialize, Serialize};

/// Loudness measurement for a decoded buffer.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct LoudnessMeasurement {
    pub momentary_lufs: f32,
    pub shortterm_lufs: f32,
    pub integrated_lufs: f32,
    pub loudness_range: f32,
    pub true_peak_dbtp: f32,
    pub peak_dbfs: f32,
}

fn lin_to_db(lin: f32) -> f32 {
    if !lin.is_finite() || lin <= 1.0e-6 {
        -120.0
    } else {
        20.0 * lin.abs().log10()
    }
}

/// Analyse interleaved PCM with libebur128.
pub fn analyze_loudness(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
) -> Option<LoudnessMeasurement> {
    let channels = channels.max(1);
    if samples.len() < channels || sample_rate == 0 {
        return None;
    }
    let frames = samples.len() / channels;
    if frames < (sample_rate as usize / 10).max(64) {
        return None;
    }
    let mode = ebur128::Mode::I
        | ebur128::Mode::M
        | ebur128::Mode::S
        | ebur128::Mode::LRA
        | ebur128::Mode::TRUE_PEAK;
    let mut meter = ebur128::EbuR128::new(channels as u32, sample_rate, mode).ok()?;
    if meter.add_frames_f32(samples).is_err() {
        return None;
    }
    let integrated = meter.loudness_global().ok()? as f32;
    let momentary = meter.loudness_momentary().unwrap_or(integrated as f64) as f32;
    let shortterm = meter.loudness_shortterm().unwrap_or(integrated as f64) as f32;
    let lra = meter.loudness_range().unwrap_or(0.0) as f32;
    let mut true_peak = 0.0_f32;
    for ch in 0..channels as u32 {
        if let Ok(tp) = meter.true_peak(ch) {
            true_peak = true_peak.max(tp as f32);
        }
    }
    let peak = samples.iter().copied().fold(0.0_f32, |p, s| p.max(s.abs()));
    Some(LoudnessMeasurement {
        momentary_lufs: if momentary.is_finite() {
            momentary
        } else {
            integrated
        },
        shortterm_lufs: if shortterm.is_finite() {
            shortterm
        } else {
            integrated
        },
        integrated_lufs: integrated,
        loudness_range: if lra.is_finite() { lra } else { 0.0 },
        true_peak_dbtp: lin_to_db(true_peak.max(peak)),
        peak_dbfs: lin_to_db(peak),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_has_finite_integrated_loudness() {
        let sr = 48_000u32;
        let mut samples = Vec::new();
        for i in 0..(sr as usize * 3) {
            let s = (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin() * 0.25;
            samples.push(s);
            samples.push(s);
        }
        let m = analyze_loudness(&samples, 2, sr).expect("loudness");
        assert!(m.integrated_lufs.is_finite());
        assert!(m.peak_dbfs < 0.0);
        assert!(m.true_peak_dbtp >= m.peak_dbfs - 0.5);
    }
}
