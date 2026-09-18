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

/// One impulsive event located by the de-click detector.
///
/// The editor draws these on the processing canvas, so the fields describe the
/// event in source-frame terms rather than in per-channel sample offsets.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClickEvent {
    /// First damaged frame.
    pub frame: usize,
    /// Damaged frame count, already clamped to `max_click_width`.
    pub width: usize,
    /// Channel the event was found on.
    pub channel: usize,
    /// Residual over the detection threshold, 0..=1.
    pub strength: f32,
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

/// Detection threshold for one channel: a multiple of the median sample-to-
/// sample derivative, so the detector adapts to program material level.
fn channel_threshold(samples: &[f32], channels: usize, ch: usize, sensitivity: f32) -> Option<f32> {
    let frames = samples.len() / channels;
    if frames < 2 {
        return None;
    }
    let mut deriv_abs = Vec::with_capacity(frames - 1);
    for i in 1..frames {
        let a = samples[(i - 1) * channels + ch];
        let b = samples[i * channels + ch];
        deriv_abs.push((b - a).abs());
    }
    deriv_abs.sort_by(|a, b| a.total_cmp(b));
    let median = deriv_abs[deriv_abs.len() / 2].max(1.0e-6);
    Some(median * (8.0 + (1.0 - sensitivity) * 24.0))
}

/// Locate impulsive events without modifying the audio.
///
/// [`declick_interleaved`] repairs exactly the events returned here, so the
/// canvas overlay and the committed render cannot disagree.
pub fn detect_clicks(samples: &[f32], channels: usize, params: DeclickParams) -> Vec<ClickEvent> {
    let channels = channels.max(1);
    if samples.len() < channels * 8 {
        return Vec::new();
    }
    let frames = samples.len() / channels;
    let max_width = params.max_click_width.clamp(2, 64);
    let sensitivity = params.sensitivity.clamp(0.05, 1.0);
    let mut events = Vec::new();

    for ch in 0..channels {
        let Some(threshold) = channel_threshold(samples, channels, ch, sensitivity) else {
            continue;
        };
        let mut i = 1usize;
        while i + 1 < frames {
            let idx = i * channels + ch;
            let prev = samples[idx - channels];
            let cur = samples[idx];
            let next = samples[(i + 1) * channels + ch];
            let predicted = median3(prev, (prev + next) * 0.5, next);
            let residual = (cur - predicted).abs();
            let deriv = (cur - prev).abs();
            if residual > threshold && deriv > threshold {
                let mut width = 1usize;
                while i + width + 1 < frames && width < max_width {
                    let nidx = (i + width) * channels + ch;
                    if (samples[nidx] - samples[nidx - channels]).abs() > threshold * 0.5 {
                        width += 1;
                    } else {
                        break;
                    }
                }
                events.push(ClickEvent {
                    frame: i,
                    width,
                    channel: ch,
                    strength: (residual / (threshold * 4.0)).clamp(0.0, 1.0),
                });
                i += width.max(1);
                continue;
            }
            i += 1;
        }
    }
    events.sort_by_key(|event| event.frame);
    events
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
    let events = detect_clicks(samples, channels, params);
    for event in &events {
        let ch = event.channel;
        let i = event.frame;
        if i == 0 || i + 1 >= frames {
            continue;
        }
        let left = out[(i - 1) * channels + ch];
        let right_frame = (i + event.width).min(frames - 1);
        let right = out[right_frame * channels + ch];
        for w in 0..event.width {
            if i + w >= frames {
                break;
            }
            let t = (w + 1) as f32 / (event.width + 1) as f32;
            out[(i + w) * channels + ch] = left + (right - left) * t;
        }
    }
    let repaired = events.len();
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

    #[test]
    fn detection_reports_the_events_that_get_repaired() {
        let sr = 48_000.0;
        let mut samples = Vec::new();
        for i in 0..2048 {
            let s = (std::f32::consts::TAU * 440.0 * i as f32 / sr).sin() * 0.2;
            samples.push(s);
            samples.push(s);
        }
        samples[512] = 0.95;
        samples[513] = 0.95;
        let events = detect_clicks(&samples, 2, DeclickParams::default());
        let (_, repaired) = declick_interleaved(&samples, 2, DeclickParams::default());
        assert_eq!(events.len(), repaired);
        assert!(events.iter().any(|event| event.frame.abs_diff(256) <= 2));
        assert!(events.iter().all(|event| event.strength > 0.0));
    }
}
