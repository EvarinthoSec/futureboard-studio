//! Realtime biquad notch de-hum.

use super::AudioClipProcessor;

/// De-hum settings. `base_hz == 0` is bypass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DehumParams {
    pub base_hz: f32,
    pub harmonics: u8,
    pub reduction_db: f32,
}

impl Default for DehumParams {
    fn default() -> Self {
        Self {
            base_hz: 50.0,
            harmonics: 4,
            reduction_db: 18.0,
        }
    }
}

impl DehumParams {
    pub fn is_bypass(self) -> bool {
        self.base_hz <= 0.0 || self.harmonics == 0 || self.reduction_db <= 0.0
    }

    pub fn sanitized(self) -> Self {
        Self {
            base_hz: if self.base_hz.is_finite() {
                self.base_hz.clamp(0.0, 400.0)
            } else {
                0.0
            },
            harmonics: self
                .harmonics
                .min(10)
                .max(if self.base_hz > 0.0 { 1 } else { 0 }),
            reduction_db: if self.reduction_db.is_finite() {
                self.reduction_db.clamp(0.0, 48.0)
            } else {
                0.0
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    fn notch(sample_rate: f32, freq: f32, q: f32) -> Self {
        let sr = sample_rate.max(1.0);
        let w = std::f32::consts::TAU * (freq / sr).clamp(0.0001, 0.45);
        let alpha = w.sin() / (2.0 * q.max(0.1));
        let cos_w = w.cos();
        let a0 = 1.0 + alpha;
        Self {
            b0: 1.0 / a0,
            b1: (-2.0 * cos_w) / a0,
            b2: 1.0 / a0,
            a1: (-2.0 * cos_w) / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }

    fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }
}

const MAX_NOTCHES: usize = 10;

/// Stereo notch-filter chain. Filters are preallocated; unused slots are bypass.
#[derive(Debug, Clone)]
pub struct DehumProcessor {
    sample_rate: f32,
    params: DehumParams,
    mix: f32,
    left: [Biquad; MAX_NOTCHES],
    right: [Biquad; MAX_NOTCHES],
    active: usize,
}

impl DehumProcessor {
    pub fn new(sample_rate: u32, params: DehumParams) -> Self {
        let mut processor = Self {
            sample_rate: sample_rate.max(1) as f32,
            params: DehumParams::default(),
            mix: 0.0,
            left: [Biquad::default(); MAX_NOTCHES],
            right: [Biquad::default(); MAX_NOTCHES],
            active: 0,
        };
        processor.set_params(params);
        processor
    }

    pub fn params(&self) -> DehumParams {
        self.params
    }

    pub fn set_params(&mut self, params: DehumParams) {
        let params = params.sanitized();
        self.params = params;
        self.mix = if params.is_bypass() {
            0.0
        } else {
            (1.0 - crate::clip_process::db_to_lin(-params.reduction_db)).clamp(0.0, 1.0)
        };
        self.active = 0;
        if params.is_bypass() {
            return;
        }
        let q = 18.0 + params.reduction_db * 0.4;
        let count = params.harmonics.min(MAX_NOTCHES as u8) as usize;
        for harmonic in 1..=count {
            let freq = params.base_hz * harmonic as f32;
            if freq >= self.sample_rate * 0.45 {
                break;
            }
            self.left[self.active] = Biquad::notch(self.sample_rate, freq, q);
            self.right[self.active] = Biquad::notch(self.sample_rate, freq, q);
            self.active += 1;
        }
    }

    #[inline]
    pub fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        if self.active == 0 || self.mix <= f32::EPSILON {
            return (left, right);
        }
        let mut out_l = left;
        let mut out_r = right;
        for i in 0..self.active {
            out_l = self.left[i].process(out_l);
            out_r = self.right[i].process(out_r);
        }
        (
            left + (out_l - left) * self.mix,
            right + (out_r - right) * self.mix,
        )
    }
}

impl AudioClipProcessor for DehumProcessor {
    fn prepare(&mut self, sample_rate: u32, _channels: usize, _max_block_frames: usize) {
        self.sample_rate = sample_rate.max(1) as f32;
        self.set_params(self.params);
    }

    fn reset(&mut self) {
        for filter in self.left.iter_mut().chain(self.right.iter_mut()) {
            filter.reset();
        }
    }

    fn process(&mut self, input: &[f32], output: &mut [f32]) {
        let n = input.len().min(output.len());
        let mut i = 0;
        while i + 1 < n {
            let (l, r) = self.process_stereo(input[i], input[i + 1]);
            output[i] = l;
            output[i + 1] = r;
            i += 2;
        }
        if i < n {
            output[i] = input[i];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f32, sample_rate: f32, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let s = (std::f32::consts::TAU * freq * i as f32 / sample_rate).sin();
                [s * 0.5, s * 0.5]
            })
            .collect()
    }

    #[test]
    fn notch_attenuates_hum_tone() {
        let sr = 48_000.0;
        let input = sine(50.0, sr, 48_000);
        let mut processor = DehumProcessor::new(
            48_000,
            DehumParams {
                base_hz: 50.0,
                harmonics: 1,
                reduction_db: 24.0,
            },
        );
        processor.reset();
        let mut out = vec![0.0; input.len()];
        processor.process(&input, &mut out);
        let in_peak = input.iter().fold(0.0_f32, |p, s| p.max(s.abs()));
        let out_peak = out.iter().skip(24_000).fold(0.0_f32, |p, s| p.max(s.abs()));
        assert!(out_peak < in_peak * 0.4, "in={in_peak} out={out_peak}");
    }
}
