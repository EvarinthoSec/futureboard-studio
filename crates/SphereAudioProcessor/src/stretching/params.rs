use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StretchMode {
    Off,
    Manual,
    TempoSync,
    Warp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StretchAlgorithm {
    Off,
    RePitch,
    PreservePitch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StretchBackend {
    InternalRePitch,
    Signalsmith,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StretchParams {
    pub mode: StretchMode,
    pub algorithm: StretchAlgorithm,

    /// Timeline/display duration multiplier. `2.0` means the clip is twice as long.
    pub time_ratio: f32,

    /// Pitch multiplier. `1.0` means unchanged.
    pub pitch_ratio: f32,

    pub source_bpm: Option<f32>,
    pub target_bpm: Option<f32>,

    /// `true` = use Signalsmith preserve-pitch stretch.
    pub preserve_pitch: bool,

    /// Reserved for Signalsmith tuning.
    pub quality: f32,
}

impl StretchParams {
    /// Return a runtime-safe copy of parameters that may have come from an
    /// older project file, a bridge, or an untrusted snapshot.
    ///
    /// Stretch processors run close to the realtime boundary. Keeping the
    /// normalization here means every backend receives finite, bounded values
    /// without having to duplicate policy in its audio loop.
    pub fn sanitized(&self) -> Self {
        let mut params = self.clone();
        params.time_ratio = sanitize_time_ratio(self.time_ratio);
        params.pitch_ratio = sanitize_pitch_ratio(self.pitch_ratio);
        params.source_bpm = sanitize_bpm(self.source_bpm);
        params.target_bpm = sanitize_bpm(self.target_bpm);
        params.quality = if self.quality.is_finite() {
            self.quality.clamp(0.0, 1.0)
        } else {
            0.75
        };
        params
    }
}

pub(crate) const MIN_TIME_RATIO: f32 = 0.05;
pub(crate) const MAX_TIME_RATIO: f32 = 20.0;
pub(crate) const MIN_PITCH_RATIO: f32 = 0.0625;
pub(crate) const MAX_PITCH_RATIO: f32 = 16.0;
pub(crate) const MIN_BPM: f32 = 1.0;
pub(crate) const MAX_BPM: f32 = 999.0;

fn sanitize_time_ratio(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value.clamp(MIN_TIME_RATIO, MAX_TIME_RATIO)
    } else {
        1.0
    }
}

fn sanitize_pitch_ratio(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value.clamp(MIN_PITCH_RATIO, MAX_PITCH_RATIO)
    } else {
        1.0
    }
}

fn sanitize_bpm(value: Option<f32>) -> Option<f32> {
    value
        .map(|bpm| {
            if bpm.is_finite() && bpm > 0.0 {
                bpm.clamp(MIN_BPM, MAX_BPM)
            } else {
                0.0
            }
        })
        .filter(|bpm| *bpm > 0.0)
}

impl Default for StretchParams {
    fn default() -> Self {
        Self {
            mode: StretchMode::Off,
            algorithm: StretchAlgorithm::Off,
            time_ratio: 1.0,
            pitch_ratio: 1.0,
            source_bpm: None,
            target_bpm: None,
            preserve_pitch: false,
            quality: 0.75,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_params_are_finite_and_bounded() {
        let params = StretchParams {
            time_ratio: f32::NAN,
            pitch_ratio: f32::INFINITY,
            source_bpm: Some(-10.0),
            target_bpm: Some(f32::INFINITY),
            quality: f32::NAN,
            ..StretchParams::default()
        }
        .sanitized();

        assert_eq!(params.time_ratio, 1.0);
        assert_eq!(params.pitch_ratio, 1.0);
        assert_eq!(params.source_bpm, None);
        assert_eq!(params.target_bpm, None);
        assert_eq!(params.quality, 0.75);
    }

    #[test]
    fn sanitized_params_clamp_extreme_valid_values() {
        let params = StretchParams {
            time_ratio: 1_000.0,
            pitch_ratio: 0.0001,
            source_bpm: Some(10_000.0),
            target_bpm: Some(0.5),
            quality: 2.0,
            ..StretchParams::default()
        }
        .sanitized();

        assert_eq!(params.time_ratio, MAX_TIME_RATIO);
        assert_eq!(params.pitch_ratio, MIN_PITCH_RATIO);
        assert_eq!(params.source_bpm, Some(MAX_BPM));
        assert_eq!(params.target_bpm, Some(MIN_BPM));
        assert_eq!(params.quality, 1.0);
    }
}
