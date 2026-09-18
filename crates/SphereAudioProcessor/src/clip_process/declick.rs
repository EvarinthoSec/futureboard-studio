//! Click detection and local interpolation. Offline / worker thread only.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeclickParams {
    /// 0..=1. Higher values detect more clicks.
    pub sensitivity: f32,
    /// Maximum click width in samples.
    pub max_click_width: usize,
}

impl Default for DeclickParams {
    fn default() -> Self {
        Self {
            sensitivity: 0.65,
            max_click_width: 12,
        }
    }
}

fn median3(a: f32, b: f32, c: f32) -> f32 {
    if (a <= b && b <= c) || (c <= b && b <= a) {
        b
    } else if (b <= a && a <= c) || (c <= a && a <= b) {
        a
    } else {
        c
    }
}

/// Repair impulsive clicks in interleaved PCM. Operates per channel.
pub fn declick_interleaved(
    samples: &[f32],
    channels: usize,
    params: DeclickParams,
) -> (Vec<f32>, usize) {
    let channels = channels.max(1);
    let mut out = samples.to_vec();
    if out.len() < channels * 8 {
        return (out, 0);
    }
    let frames = out.len() / channels;
    let max_width = params.max_click_width.clamp(2, 64);
    let sensitivity = params.sensitivity.clamp(0.05, 1.0);
    let mut repaired = 0usize;

    for ch in 0..channels {
        let mut deriv_abs = Vec::with_capacity(frames.saturating_sub(1));
        for i in 1..frames {
            let a = out[(i - 1) * channels + ch];
            let b = out[i * channels + ch];
            deriv_abs.push((b - a).abs());
        }
        if deriv_abs.is_empty() {
            continue;
        }
        let mut sorted = deriv_abs.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let median = sorted[sorted.len() / 2].max(1.0e-6);
        let threshold = median * (8.0 + (1.0 - sensitivity) * 24.0);

        let mut i = 1usize;
        while i + 1 < frames {
            let idx = i * channels + ch;
            let prev = out[idx - channels];
            let cur = out[idx];
            let next = out[(i + 1) * channels + ch];
            let predicted = median3(prev, (prev + next) * 0.5, next);
            let residual = (cur - predicted).abs();
            let deriv = (cur - prev).abs();
            if residual > threshold && deriv > threshold {
                let mut width = 1usize;
                while i + width + 1 < frames && width < max_width {
                    let nidx = (i + width) * channels + ch;
                    let nprev = out[nidx - channels];
                    let ncur = out[nidx];
                    if (ncur - nprev).abs() > threshold * 0.5 {
                        width += 1;
                    } else {
                        break;
                    }
                }
                let left = out[(i - 1) * channels + ch];
                let right_frame = (i + width).min(frames - 1);
                let right = out[right_frame * channels + ch];
                for w in 0..width {
                    let t = (w + 1) as f32 / (width + 1) as f32;
                    out[(i + w) * channels + ch] = left + (right - left) * t;
                }
                repaired += 1;
                i += width.max(1);
                continue;
            }
            i += 1;
        }
    }
    (out, repaired)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates_an_impulse_on_a_sine() {
        let sr = 48_000.0;
        let mut samples = Vec::new();
        for i in 0..2048 {
            let s = (std::f32::consts::TAU * 440.0 * i as f32 / sr).sin() * 0.2;
            samples.push(s);
            samples.push(s);
        }
        samples[512] = 0.95;
        samples[513] = 0.95;
        let (out, repaired) = declick_interleaved(&samples, 2, DeclickParams::default());
        assert!(repaired >= 1);
        assert!(out[512].abs() < 0.4);
    }
}
