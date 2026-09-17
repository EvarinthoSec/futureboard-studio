use super::params::{
    MAX_PITCH_RATIO, MAX_TIME_RATIO, MIN_PITCH_RATIO, MIN_TIME_RATIO, StretchMode, StretchParams,
};

const MIN_READ_RATE: f32 = MIN_PITCH_RATIO;
const MAX_READ_RATE: f32 = MAX_PITCH_RATIO;

/// Pitch multiplier from semitones and fine cents: `2^((semi + cents/100) / 12)`.
pub fn semitone_to_pitch_ratio(semitones: f32, cents: f32) -> f32 {
    let semitones = if semitones.is_finite() {
        semitones
    } else {
        0.0
    };
    let cents = if cents.is_finite() { cents } else { 0.0 };
    let total = (semitones + cents / 100.0).clamp(-48.0, 48.0);
    2.0_f32.powf(total / 12.0)
}

/// Split a pitch multiplier into whole semitones and residual cents.
pub fn pitch_ratio_to_semitone_cents(pitch_ratio: f32) -> (f32, f32) {
    let total_semitones = 12.0 * pitch_ratio.max(0.001).log2();
    let semi = total_semitones.trunc();
    let cents = (total_semitones - semi) * 100.0;
    (semi, cents)
}

pub fn effective_time_ratio(params: &StretchParams, project_bpm: Option<f32>) -> f32 {
    if params.mode == StretchMode::Off {
        return 1.0;
    }

    match params.mode {
        StretchMode::Off => 1.0,
        StretchMode::Manual | StretchMode::Warp => sanitize_time_ratio(params.time_ratio),
        StretchMode::TempoSync => {
            let Some(source_bpm) = params.source_bpm.filter(|v| valid_positive(*v)) else {
                return 1.0;
            };
            let target_bpm = params
                .target_bpm
                .or(project_bpm)
                .filter(|v| valid_positive(*v))
                .unwrap_or(source_bpm);
            sanitize_time_ratio(source_bpm / target_bpm)
        }
    }
}

pub fn effective_pitch_ratio(params: &StretchParams) -> f32 {
    sanitize_pitch_ratio(params.pitch_ratio)
}

pub fn source_read_rate_for_repitch(params: &StretchParams, project_bpm: Option<f32>) -> f32 {
    if params.mode == StretchMode::Off {
        return 1.0;
    }
    let time_ratio = effective_time_ratio(params, project_bpm);
    sanitize_read_rate(effective_pitch_ratio(params) / time_ratio)
}

pub fn stretched_duration_samples(
    source_len: u64,
    params: &StretchParams,
    project_bpm: Option<f32>,
) -> u64 {
    if source_len == 0 {
        return 0;
    }

    let duration = source_len as f64 * effective_time_ratio(params, project_bpm) as f64;
    duration.round().clamp(0.0, u64::MAX as f64) as u64
}

fn valid_positive(value: f32) -> bool {
    value.is_finite() && value > 0.0
}

fn sanitize_time_ratio(value: f32) -> f32 {
    if valid_positive(value) {
        value.clamp(MIN_TIME_RATIO, MAX_TIME_RATIO)
    } else {
        1.0
    }
}

fn sanitize_pitch_ratio(value: f32) -> f32 {
    if valid_positive(value) {
        value.clamp(MIN_PITCH_RATIO, MAX_PITCH_RATIO)
    } else {
        1.0
    }
}

fn sanitize_read_rate(value: f32) -> f32 {
    if valid_positive(value) {
        value.clamp(MIN_READ_RATE, MAX_READ_RATE)
    } else {
        1.0
    }
}
