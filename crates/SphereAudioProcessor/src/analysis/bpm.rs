//! Tempo (BPM) estimation via spectral-flux onset envelope + autocorrelation.
//!
//! Offline / control-thread only.

use serde::{Deserialize, Serialize};

use super::spectrum::magnitude_frames;

/// Estimated tempo of an audio buffer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoEstimate {
    /// Beats per minute.
    pub bpm: f32,
    /// Rough confidence in `[0, 1]` (peak autocorrelation vs. average).
    pub confidence: f32,
}

const FRAME_SIZE: usize = 1024;
const HOP: usize = 512;

/// Estimate tempo of a mono buffer. Returns `None` for signals too short to
/// hold a meaningful onset envelope.
pub fn estimate_bpm(
    samples: &[f32],
    sample_rate: f32,
    min_bpm: f32,
    max_bpm: f32,
) -> Option<TempoEstimate> {
    if sample_rate <= 0.0 || !sample_rate.is_finite() {
        return None;
    }
    let (min_bpm, max_bpm) = sanitize_range(min_bpm, max_bpm);

    let frames = magnitude_frames(samples, FRAME_SIZE, HOP);
    if frames.len() < 8 {
        return None;
    }

    // Spectral flux: sum of positive magnitude differences between frames.
    let mut flux = Vec::with_capacity(frames.len() - 1);
    for pair in frames.windows(2) {
        let (prev, cur) = (&pair[0], &pair[1]);
        let mut sum = 0.0_f32;
        for (a, b) in cur.iter().zip(prev.iter()) {
            let d = a - b;
            if d > 0.0 {
                sum += d;
            }
        }
        flux.push(sum);
    }

    // Half-wave rectify around the mean to emphasise onsets.
    let mean = flux.iter().sum::<f32>() / flux.len() as f32;
    let env: Vec<f32> = flux.iter().map(|v| (v - mean).max(0.0)).collect();
    let energy: f32 = env.iter().map(|v| v * v).sum();
    if energy <= f32::EPSILON {
        return None;
    }

    let fps = sample_rate / HOP as f32;

    // Autocorrelate the onset envelope at each candidate BPM lag.
    let mut best_bpm = 0.0_f32;
    let mut best_score = 0.0_f32;
    let mut sum_score = 0.0_f32;
    let mut count = 0.0_f32;

    let mut bpm = min_bpm;
    while bpm <= max_bpm {
        let lag = (fps * 60.0 / bpm).round() as usize;
        if lag >= 1 && lag < env.len() {
            let mut acc = 0.0_f32;
            for n in 0..env.len() - lag {
                acc += env[n] * env[n + lag];
            }
            acc /= (env.len() - lag) as f32;
            sum_score += acc;
            count += 1.0;
            if acc > best_score {
                best_score = acc;
                best_bpm = bpm;
            }
        }
        bpm += 0.5;
    }

    if best_bpm <= 0.0 || count <= 0.0 {
        return None;
    }

    let avg_score = sum_score / count;
    let confidence = if avg_score > 0.0 {
        (1.0 - avg_score / best_score).clamp(0.0, 1.0)
    } else {
        0.0
    };

    Some(TempoEstimate {
        bpm: best_bpm,
        confidence,
    })
}

/// Ranked tempo candidates: the winner plus musically related doubles/halves.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TempoCandidate {
    pub bpm: f32,
    pub confidence: f32,
}

pub fn estimate_bpm_candidates(
    samples: &[f32],
    sample_rate: f32,
    min_bpm: f32,
    max_bpm: f32,
) -> Vec<TempoCandidate> {
    let Some(best) = estimate_bpm(samples, sample_rate, min_bpm, max_bpm) else {
        return Vec::new();
    };
    let (min_bpm, max_bpm) = sanitize_range(min_bpm, max_bpm);
    let mut candidates = vec![TempoCandidate {
        bpm: best.bpm,
        confidence: best.confidence,
    }];
    for (ratio, conf_scale) in [(0.5, 0.72), (2.0, 0.68)] {
        let bpm = (best.bpm * ratio as f32 * 100.0).round() / 100.0;
        if bpm >= min_bpm && bpm <= max_bpm {
            candidates.push(TempoCandidate {
                bpm,
                confidence: (best.confidence * conf_scale).clamp(0.0, 1.0),
            });
        }
    }
    candidates.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    candidates
}

fn sanitize_range(min_bpm: f32, max_bpm: f32) -> (f32, f32) {
    let mut lo = if min_bpm.is_finite() && min_bpm > 0.0 {
        min_bpm
    } else {
        60.0
    };
    let mut hi = if max_bpm.is_finite() && max_bpm > lo {
        max_bpm
    } else {
        200.0
    };
    lo = lo.clamp(20.0, 400.0);
    hi = hi.clamp(lo + 1.0, 400.0);
    (lo, hi)
}
