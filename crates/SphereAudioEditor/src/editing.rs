//! Reusable, UI-agnostic audio editing vocabulary.
//!
//! The native panel is only one frontend of this module.  Project/engine
//! integrations own the actual clip mutation; these types describe intent and
//! keep the editor's interaction state independent from GPUI entities.

/// The first tools shared by the audio editor and future specialist frontends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioEditorTool {
    #[default]
    Pointer,
    Range,
    /// Time-frequency range selection for spectral repair tools.
    SpectralRange,
    Split,
    Trim,
    Fade,
    Marker,
    /// Non-destructive gain envelope.
    Draw,
    Scrub,
    /// Warp marker editing. Hidden until the warp DSP path is wired.
    Warp,
}

impl AudioEditorTool {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pointer => "Pointer",
            Self::Range => "Range",
            Self::SpectralRange => "Spectral Range",
            Self::Split => "Split",
            Self::Trim => "Trim",
            Self::Fade => "Fade",
            Self::Marker => "Marker",
            Self::Draw => "Gain Envelope",
            Self::Scrub => "Scrub",
            Self::Warp => "Warp",
        }
    }

    pub const fn is_available(self) -> bool {
        !matches!(self, Self::Warp)
    }
}

/// Snap sources.  The host resolves Grid against its existing timeline snap
/// source; the audio editor does not maintain a second musical grid engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioEditorSnap {
    Off,
    #[default]
    Grid,
    ZeroCrossing,
    Markers,
    Transients,
}

impl AudioEditorSnap {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Grid => "Grid",
            Self::ZeroCrossing => "Zero Cross",
            Self::Markers => "Markers",
            Self::Transients => "Transients",
        }
    }

    pub const fn is_available(self) -> bool {
        matches!(self, Self::Off | Self::Grid)
    }
}

/// How channel data is presented.  The renderer may add arbitrary channel
/// layouts later without changing the selection or edit model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioChannelMode {
    #[default]
    Combined,
    SplitStereo,
    Left,
    Right,
    MonoSum,
    Channels,
}

impl AudioChannelMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Combined => "Combined",
            Self::SplitStereo => "Split Stereo",
            Self::Left => "Left",
            Self::Right => "Right",
            Self::MonoSum => "Mono Sum",
            Self::Channels => "Channels",
        }
    }
}

/// Which fade edge a command or gesture addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFadeEdge {
    In,
    Out,
}

/// Supported fade shapes.  The current engine snapshot carries the curve name
/// so adding equal-power/S-curves does not require changing the editor API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioFadeCurve {
    #[default]
    Linear,
    EqualPower,
    SCurve,
}

impl AudioFadeCurve {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::EqualPower => "Equal Power",
            Self::SCurve => "S-Curve",
        }
    }
}

/// Sample-accurate selection in the immutable source media domain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioRangeSelection {
    pub start_frame: i64,
    pub end_frame: i64,
}

/// A two-dimensional time-frequency selection. Frame coordinates refer to the
/// immutable decoded source; the host converts them to/from project beats only
/// at the UI boundary. Editing commands can opt into this later without
/// changing ordinary time-range selection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectralSelection {
    pub start_frame: i64,
    pub end_frame: i64,
    pub min_hz: f32,
    pub max_hz: f32,
}

impl SpectralSelection {
    pub const fn new(start_frame: i64, end_frame: i64, min_hz: f32, max_hz: f32) -> Self {
        Self {
            start_frame,
            end_frame,
            min_hz,
            max_hz,
        }
    }

    pub fn normalized(self) -> Self {
        Self {
            start_frame: self.start_frame.min(self.end_frame),
            end_frame: self.start_frame.max(self.end_frame),
            min_hz: self.min_hz.min(self.max_hz),
            max_hz: self.min_hz.max(self.max_hz),
        }
    }

    pub fn clamp_to(self, total_frames: i64, nyquist_hz: f32) -> Self {
        let mut selection = self.normalized();
        let total_frames = total_frames.max(0);
        let nyquist_hz = nyquist_hz.max(0.0);
        selection.start_frame = selection.start_frame.clamp(0, total_frames);
        selection.end_frame = selection.end_frame.clamp(0, total_frames);
        selection.min_hz = if selection.min_hz.is_finite() {
            selection.min_hz.clamp(0.0, nyquist_hz)
        } else {
            0.0
        };
        selection.max_hz = if selection.max_hz.is_finite() {
            selection.max_hz.clamp(0.0, nyquist_hz)
        } else {
            nyquist_hz
        };
        selection.normalized()
    }

    pub const fn is_empty(self) -> bool {
        self.start_frame == self.end_frame || (self.max_hz - self.min_hz).abs() <= f32::EPSILON
    }
}

/// Envelope interpolation shape. The renderer currently evaluates all
/// variants through the same stable curve API; Linear is the MVP shape and
/// the extra variants keep saved projects forward-compatible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnvelopeCurve {
    #[default]
    Linear,
    Smooth,
    Fast,
    Slow,
    SCurve,
}

impl EnvelopeCurve {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Linear => "Linear",
            Self::Smooth => "Smooth",
            Self::Fast => "Fast",
            Self::Slow => "Slow",
            Self::SCurve => "S-Curve",
        }
    }

    pub fn shape(self, t: f32) -> f32 {
        let t = t.clamp(0.0, 1.0);
        match self {
            Self::Linear => t,
            Self::Smooth => t * t * (3.0 - 2.0 * t),
            Self::Fast => t.sqrt(),
            Self::Slow => t * t,
            Self::SCurve => t * t * (3.0 - 2.0 * t),
        }
    }

    pub const fn to_tag(self) -> u8 {
        match self {
            Self::Linear => 0,
            Self::Smooth => 1,
            Self::Fast => 2,
            Self::Slow => 3,
            Self::SCurve => 4,
        }
    }

    pub const fn from_tag(tag: u8) -> Self {
        match tag {
            1 => Self::Smooth,
            2 => Self::Fast,
            3 => Self::Slow,
            4 => Self::SCurve,
            _ => Self::Linear,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnvelopePoint {
    pub id: u64,
    /// Normalised clip time in `0..=1`.
    pub time: f32,
    /// Gain relative to the clip in dB.
    pub value_db: f32,
    pub curve: EnvelopeCurve,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClipEnvelope {
    pub points: Vec<EnvelopePoint>,
}

impl Default for ClipEnvelope {
    fn default() -> Self {
        Self { points: Vec::new() }
    }
}

impl ClipEnvelope {
    pub const MAX_POINTS: usize = 2048;

    pub fn sanitize_in_place(&mut self) {
        self.points
            .retain(|point| point.time.is_finite() && point.value_db.is_finite() && point.id != 0);
        for point in &mut self.points {
            point.time = point.time.clamp(0.0, 1.0);
            point.value_db = point.value_db.clamp(-120.0, 24.0);
        }
        self.points
            .sort_by(|a, b| a.time.total_cmp(&b.time).then_with(|| a.id.cmp(&b.id)));
        self.points.dedup_by(|a, b| a.id == b.id);
        self.points.truncate(Self::MAX_POINTS);
    }

    pub fn value_db_at(&self, time: f32) -> f32 {
        let Some(first) = self.points.first() else {
            return 0.0;
        };
        if time <= first.time {
            return first.value_db;
        }
        for pair in self.points.windows(2) {
            let left = pair[0];
            let right = pair[1];
            if time <= right.time {
                let span = (right.time - left.time).max(f32::EPSILON);
                let t = ((time - left.time) / span).clamp(0.0, 1.0);
                return left.value_db + (right.value_db - left.value_db) * right.curve.shape(t);
            }
        }
        self.points
            .last()
            .map(|point| point.value_db)
            .unwrap_or(0.0)
    }
}

impl AudioRangeSelection {
    pub const fn new(start_frame: i64, end_frame: i64) -> Self {
        Self {
            start_frame,
            end_frame,
        }
    }

    pub const fn normalized(self) -> Self {
        if self.start_frame <= self.end_frame {
            self
        } else {
            Self {
                start_frame: self.end_frame,
                end_frame: self.start_frame,
            }
        }
    }

    pub fn clamp_to(self, total_frames: i64) -> Self {
        let total_frames = total_frames.max(0);
        let normalized = self.normalized();
        Self {
            start_frame: normalized.start_frame.clamp(0, total_frames),
            end_frame: normalized.end_frame.clamp(0, total_frames),
        }
        .normalized()
    }

    pub const fn is_empty(self) -> bool {
        self.start_frame == self.end_frame
    }

    pub const fn duration_frames(self) -> i64 {
        let normalized = self.normalized();
        normalized.end_frame - normalized.start_frame
    }
}

/// Explicit interaction state for the editor.  The project host turns the
/// drag intents into one coalesced `UpdateClip` command at pointer-up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AudioEditorDrag {
    None,
    SelectingRange { anchor_beat: f32 },
    SelectingSpectralRange { anchor_frame: i64, anchor_hz: f32 },
    TrimmingLeft { start_beat: f32 },
    TrimmingRight { start_beat: f32 },
    AdjustingFadeIn { original_seconds: f32 },
    AdjustingFadeOut { original_seconds: f32 },
    MovingWarpMarker { marker_id: u64 },
    MovingEnvelopePoint { point_id: u64 },
}

impl Default for AudioEditorDrag {
    fn default() -> Self {
        Self::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_normalizes_and_clamps_to_media() {
        assert_eq!(
            AudioRangeSelection::new(800, -20).clamp_to(600),
            AudioRangeSelection::new(0, 600)
        );
    }

    #[test]
    fn tool_and_snap_labels_are_stable() {
        assert_eq!(AudioEditorTool::Pointer.label(), "Pointer");
        assert_eq!(AudioEditorSnap::ZeroCrossing.label(), "Zero Cross");
        assert_eq!(AudioFadeCurve::EqualPower.label(), "Equal Power");
    }

    #[test]
    fn spectral_selection_normalizes_and_clamps() {
        let selection = SpectralSelection::new(800, -20, 9_000.0, -100.0).clamp_to(600, 8_000.0);
        assert_eq!(selection.start_frame, 0);
        assert_eq!(selection.end_frame, 600);
        assert_eq!(selection.min_hz, 0.0);
        assert_eq!(selection.max_hz, 8_000.0);
    }

    #[test]
    fn envelope_interpolates_and_sanitizes() {
        let mut envelope = ClipEnvelope {
            points: vec![
                EnvelopePoint {
                    id: 2,
                    time: 1.4,
                    value_db: 30.0,
                    curve: EnvelopeCurve::Linear,
                },
                EnvelopePoint {
                    id: 1,
                    time: 0.0,
                    value_db: -6.0,
                    curve: EnvelopeCurve::Linear,
                },
            ],
        };
        envelope.sanitize_in_place();
        assert_eq!(envelope.points[1].time, 1.0);
        assert_eq!(envelope.points[1].value_db, 24.0);
        assert!((envelope.value_db_at(0.5) - 9.0).abs() < 1.0e-5);
    }
}
