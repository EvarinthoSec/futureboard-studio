//! Small, allocation-free adaptive noise reduction for clip playback.
//!
//! This is intentionally a conservative downward expander rather than a
//! destructive offline filter: it learns the quiet floor while a clip plays,
//! attenuates material below that floor, and leaves transients above the
//! threshold untouched.  The state is owned by one runtime clip, so realtime
//! playback and offline bounce use the same signal path.

/// Preset amount for the adaptive de-noiser, in the normalized `0..=1` range.
pub const MAX_AMOUNT: f32 = 1.0;

#[derive(Debug, Clone, Copy)]
pub struct DenoiseProcessor {
    sample_rate: f32,
    amount: f32,
    noise_floor: f32,
    gain: f32,
}

impl DenoiseProcessor {
    /// Create a processor for one clip. `amount == 0` is a true bypass.
    pub fn new(sample_rate: u32, amount: f32) -> Self {
        Self {
            sample_rate: sample_rate.max(1) as f32,
            amount: amount.clamp(0.0, MAX_AMOUNT),
            // A tiny non-zero seed prevents the first sample from being
            // mistaken for a valid noise profile.
            noise_floor: 1.0e-4,
            gain: 1.0,
        }
    }

    pub fn amount(self) -> f32 {
        self.amount
    }

    pub fn sample_rate(self) -> u32 {
        self.sample_rate as u32
    }

    /// Reset the learned floor at a clip/transport boundary.
    pub fn reset(&mut self) {
        self.noise_floor = 1.0e-4;
        self.gain = 1.0;
    }

    /// Process one stereo sample without allocating or touching shared state.
    #[inline]
    pub fn process_stereo(&mut self, left: f32, right: f32) -> (f32, f32) {
        if self.amount <= f32::EPSILON {
            return (left, right);
        }

        let level = left.abs().max(right.abs()).min(1.0);
        // Learn quiet material quickly enough to become useful in a short
        // clip, but learn louder material very slowly so a transient cannot
        // permanently raise the floor.
        let floor_alpha = if level <= self.noise_floor * 3.0 {
            (40.0 / self.sample_rate).clamp(0.00005, 0.001)
        } else {
            (1.0 / (self.sample_rate * 8.0)).clamp(0.000001, 0.00005)
        };
        self.noise_floor += (level - self.noise_floor) * floor_alpha;
        self.noise_floor = self.noise_floor.clamp(1.0e-5, 1.0);

        let threshold = self.noise_floor * (1.8 + self.amount * 5.2);
        let below_threshold = if threshold > 1.0e-5 {
            ((threshold - level) / threshold).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // At maximum strength, fully quiet samples retain a small residual
        // instead of hard-muting, which avoids the chattering gate sound that
        // a binary noise gate introduces on room tone.
        let target_gain = 1.0 - below_threshold * (0.92 * self.amount);
        let smoothing = if target_gain < self.gain { 0.02 } else { 0.006 };
        self.gain += (target_gain - self.gain) * smoothing;

        (left * self.gain, right * self.gain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypass_is_bitwise_stable_for_zero_amount() {
        let mut processor = DenoiseProcessor::new(48_000, 0.0);
        assert_eq!(processor.process_stereo(0.123, -0.456), (0.123, -0.456));
    }

    #[test]
    fn stronger_amount_attenuates_a_quiet_steady_signal() {
        let mut light = DenoiseProcessor::new(48_000, 0.25);
        let mut strong = DenoiseProcessor::new(48_000, 1.0);
        let mut light_last: f32 = 0.0;
        let mut strong_last: f32 = 0.0;
        for _ in 0..48_000 {
            light_last = light.process_stereo(0.001, 0.001).0;
            strong_last = strong.process_stereo(0.001, 0.001).0;
        }
        assert!(strong_last < light_last);
    }
}
