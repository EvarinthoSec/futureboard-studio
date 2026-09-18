//! Non-destructive clip processors and offline reconstruction helpers.
//!
//! Realtime processors must not allocate, lock, or log from `process_stereo`.
//! Offline helpers (STFT reconstruction, resample, loudness scan) belong on a
//! worker thread.

mod channel;
mod dc;
mod declick;
mod dehum;
mod denoise_spectral;
mod normalize;
mod resample;
mod spectral;

pub use channel::{ChannelTransform, apply_channel_transform, apply_channel_transform_interleaved};
pub use dc::{DcOffset, DcOffsetProcessor, measure_dc_offset};
pub use declick::{ClickEvent, DeclickParams, declick_interleaved, detect_clicks};
pub use dehum::{DehumParams, DehumProcessor};
pub use denoise_spectral::{
    NoiseGateMask, SpectralDenoiseParams, learn_noise_profile, noise_gate_mask, reduce_noise_stft,
};
pub use normalize::{
    NormalizeMeasurement, NormalizeMode, NormalizeParams, apply_gain_interleaved,
    measure_normalize, required_normalize_gain_db,
};
pub use resample::{ResampleError, resample_interleaved, write_wav_f32};
pub use spectral::{
    SpectralGainParams, StftSettings, apply_spectral_gain, interpolate_spectral_region,
};

/// Common processor contract for clip-level DSP.
///
/// Realtime implementations must keep `process` allocation-free after
/// [`AudioClipProcessor::prepare`]. Offline processors may allocate.
pub trait AudioClipProcessor {
    fn prepare(&mut self, sample_rate: u32, channels: usize, max_block_frames: usize);
    fn reset(&mut self);
    fn process(&mut self, input: &[f32], output: &mut [f32]);
    fn latency_samples(&self) -> usize {
        0
    }
}

/// Convert dB to a linear amplitude multiplier.
#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    if !db.is_finite() || db <= -120.0 {
        0.0
    } else {
        10.0_f32.powf(db / 20.0)
    }
}

/// Convert a linear amplitude to dB. Silence maps to `-120`.
#[inline]
pub fn lin_to_db(lin: f32) -> f32 {
    if !lin.is_finite() || lin <= 1.0e-6 {
        -120.0
    } else {
        20.0 * lin.abs().log10()
    }
}

/// Peak of interleaved PCM.
pub fn peak_amplitude(samples: &[f32]) -> f32 {
    samples
        .iter()
        .copied()
        .filter(|s| s.is_finite())
        .fold(0.0_f32, |peak, s| peak.max(s.abs()))
}

/// Mix interleaved PCM to mono.
pub fn downmix_interleaved(samples: &[f32], channels: usize) -> Vec<f32> {
    if channels == 0 {
        return Vec::new();
    }
    if channels == 1 {
        return samples.to_vec();
    }
    samples
        .chunks(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect()
}

/// Extract a time range from interleaved PCM. `start`/`end` are frames.
pub fn slice_frames(samples: &[f32], channels: usize, start: i64, end: i64) -> Vec<f32> {
    let channels = channels.max(1);
    let frames = samples.len() / channels;
    let start = start.max(0) as usize;
    let end = (end.max(0) as usize).min(frames);
    if start >= end {
        return Vec::new();
    }
    samples[start * channels..end * channels].to_vec()
}

/// Copy `replacement` into `dest` starting at `start` frames. Extra replacement
/// samples past the destination end are dropped so a length-preserving process
/// cannot grow the clip buffer.
pub fn replace_frame_range(dest: &mut [f32], channels: usize, start: i64, replacement: &[f32]) {
    let channels = channels.max(1);
    let start = start.max(0) as usize * channels;
    if start >= dest.len() || replacement.is_empty() {
        return;
    }
    let n = replacement.len().min(dest.len() - start);
    dest[start..start + n].copy_from_slice(&replacement[..n]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_roundtrip_is_stable_around_unity() {
        let lin = db_to_lin(6.0);
        assert!((lin_to_db(lin) - 6.0).abs() < 0.02);
        assert_eq!(db_to_lin(-200.0), 0.0);
    }

    #[test]
    fn replace_frame_range_writes_only_the_requested_region() {
        let mut samples = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        replace_frame_range(&mut samples, 2, 1, &[9.0, 8.0]);
        assert_eq!(samples, vec![1.0, 2.0, 9.0, 8.0, 5.0, 6.0]);
    }
}
