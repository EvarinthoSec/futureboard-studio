//! Peak / true-peak / loudness normalize gain calculation.

use super::{db_to_lin, lin_to_db, peak_amplitude};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NormalizeMode {
    #[default]
    Peak,
    TruePeak,
    Loudness,
}

impl NormalizeMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Peak => "Peak",
            Self::TruePeak => "True Peak",
            Self::Loudness => "Loudness",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizeParams {
    pub mode: NormalizeMode,
    pub target_peak_dbfs: f32,
    pub target_true_peak_dbtp: f32,
    pub target_lufs: f32,
    pub true_peak_ceiling_dbtp: Option<f32>,
}

impl Default for NormalizeParams {
    fn default() -> Self {
        Self {
            mode: NormalizeMode::Peak,
            target_peak_dbfs: -1.0,
            target_true_peak_dbtp: -1.0,
            target_lufs: -14.0,
            true_peak_ceiling_dbtp: Some(-1.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct NormalizeMeasurement {
    pub peak_dbfs: f32,
    pub true_peak_dbtp: f32,
    pub lufs_i: Option<f32>,
    pub required_gain_db: f32,
}

/// Required gain in dB to reach `target` from `current`. Both in dB.
pub fn required_normalize_gain_db(current_db: f32, target_db: f32) -> f32 {
    if !current_db.is_finite() || current_db <= -119.0 {
        return 0.0;
    }
    (target_db - current_db).clamp(-48.0, 48.0)
}

pub fn measure_normalize(
    samples: &[f32],
    channels: usize,
    sample_rate: u32,
    params: NormalizeParams,
) -> NormalizeMeasurement {
    let peak = peak_amplitude(samples);
    let peak_dbfs = lin_to_db(peak);
    let loudness = crate::analysis::loudness::analyze_loudness(samples, channels, sample_rate);
    let true_peak_dbtp = loudness
        .as_ref()
        .map(|m| m.true_peak_dbtp)
        .unwrap_or(peak_dbfs);
    let lufs_i = loudness.as_ref().map(|m| m.integrated_lufs);
    let mut required = match params.mode {
        NormalizeMode::Peak => required_normalize_gain_db(peak_dbfs, params.target_peak_dbfs),
        NormalizeMode::TruePeak => {
            required_normalize_gain_db(true_peak_dbtp, params.target_true_peak_dbtp)
        }
        NormalizeMode::Loudness => lufs_i
            .map(|lufs| required_normalize_gain_db(lufs, params.target_lufs))
            .unwrap_or(0.0),
    };
    if let Some(ceiling) = params.true_peak_ceiling_dbtp {
        let after = true_peak_dbtp + required;
        if after > ceiling {
            required -= after - ceiling;
        }
    }
    NormalizeMeasurement {
        peak_dbfs,
        true_peak_dbtp,
        lufs_i,
        required_gain_db: required.clamp(-48.0, 48.0),
    }
}

/// Apply a linear gain to interleaved PCM (offline preview helper).
pub fn apply_gain_interleaved(samples: &mut [f32], gain_db: f32) {
    let g = db_to_lin(gain_db);
    for sample in samples {
        *sample *= g;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_normalize_computes_expected_gain() {
        let samples = vec![0.5, -0.25, 0.25, -0.5];
        let m = measure_normalize(&samples, 2, 48_000, NormalizeParams::default());
        // Peak 0.5 = -6.02 dBFS, target -1 → about +5 dB.
        assert!((m.peak_dbfs + 6.02).abs() < 0.05);
        assert!((m.required_gain_db - 5.02).abs() < 0.1);
    }
}
