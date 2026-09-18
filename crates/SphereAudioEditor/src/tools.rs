//! Audio editor tool kinds, targeting, and session state.
//!
//! Windowed tools live in the host (`AudioToolWindowManager`). This module is
//! the UI-agnostic contract: what a tool is, which selection it accepts, and
//! the session a window binds to a clip.

use crate::editing::{AudioRangeSelection, SpectralSelection};

/// Tools that open an independent GPUI window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioToolKind {
    SpectrumAnalyzer,
    Loudness,
    Normalize,
    TransientDetector,
    TimePitch,
    Resample,
    ChannelTools,
    PhaseAnalyzer,
    DcOffset,
    BpmAnalysis,
    KeyAnalysis,
    AudioRepair,
    SpectralProcessor,
    SpectrogramSettings,
}

impl AudioToolKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::SpectrumAnalyzer => "Spectrum Analyzer",
            Self::Loudness => "Loudness Analyzer",
            Self::Normalize => "Normalize",
            Self::TransientDetector => "Transient Detector",
            Self::TimePitch => "Time & Pitch",
            Self::Resample => "Resample",
            Self::ChannelTools => "Channel Tools",
            Self::PhaseAnalyzer => "Phase Analyzer",
            Self::DcOffset => "DC Offset",
            Self::BpmAnalysis => "BPM Analyzer",
            Self::KeyAnalysis => "Key Analyzer",
            Self::AudioRepair => "Audio Repair",
            Self::SpectralProcessor => "Spectral Processing",
            Self::SpectrogramSettings => "Spectrogram Settings",
        }
    }

    pub const fn is_analysis_only(self) -> bool {
        matches!(
            self,
            Self::SpectrumAnalyzer
                | Self::Loudness
                | Self::PhaseAnalyzer
                | Self::BpmAnalysis
                | Self::KeyAnalysis
                | Self::SpectrogramSettings
        )
    }

    pub const fn default_follow_selection(self) -> bool {
        self.is_analysis_only()
    }

    pub const fn supports_clip(self) -> bool {
        !matches!(self, Self::SpectrogramSettings)
    }

    pub const fn supports_time_selection(self) -> bool {
        matches!(
            self,
            Self::SpectrumAnalyzer
                | Self::Loudness
                | Self::Normalize
                | Self::TransientDetector
                | Self::DcOffset
                | Self::BpmAnalysis
                | Self::KeyAnalysis
                | Self::AudioRepair
                | Self::SpectralProcessor
                | Self::PhaseAnalyzer
        )
    }

    pub const fn supports_spectral_selection(self) -> bool {
        matches!(self, Self::AudioRepair | Self::SpectralProcessor)
    }

    pub const fn default_size(self) -> (f32, f32) {
        match self {
            Self::SpectrumAnalyzer => (640.0, 420.0),
            Self::Loudness => (420.0, 360.0),
            Self::Normalize => (440.0, 420.0),
            Self::TransientDetector => (460.0, 420.0),
            Self::TimePitch => (480.0, 520.0),
            Self::Resample => (400.0, 320.0),
            Self::ChannelTools => (400.0, 380.0),
            Self::PhaseAnalyzer => (420.0, 380.0),
            Self::DcOffset => (400.0, 320.0),
            Self::BpmAnalysis => (400.0, 360.0),
            Self::KeyAnalysis => (400.0, 340.0),
            Self::AudioRepair => (520.0, 480.0),
            Self::SpectralProcessor => (480.0, 400.0),
            Self::SpectrogramSettings => (380.0, 280.0),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioToolTarget {
    pub clip_id: String,
    pub source_id: String,
    pub clip_name: String,
    pub file_label: String,
    pub source_path: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub source_frames: i64,
    pub time_selection: Option<AudioRangeSelection>,
    pub spectral_selection: Option<SpectralSelection>,
}

impl AudioToolTarget {
    pub fn summary_line(&self) -> String {
        let ch = match self.channels {
            1 => "Mono",
            2 => "Stereo",
            n => return format!("{} · {} Hz · {} ch", self.file_label, self.sample_rate, n),
        };
        format!("{} · {} Hz · {}", self.file_label, self.sample_rate, ch)
    }

    pub fn target_label(&self, kind: AudioToolKind) -> &'static str {
        if kind.supports_spectral_selection() && self.spectral_selection.is_some() {
            "Spectral Selection"
        } else if kind.supports_time_selection() && self.time_selection.is_some() {
            "Time Selection"
        } else {
            "Clip"
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioToolSession {
    pub tool_kind: AudioToolKind,
    pub target: AudioToolTarget,
    pub source_revision: u64,
    pub preview_enabled: bool,
    pub preview_bypassed: bool,
    pub follow_selection: bool,
    pub pin_target: bool,
    pub dirty: bool,
    pub analyzing: bool,
    pub analyze_progress: f32,
}

impl AudioToolSession {
    pub fn new(tool_kind: AudioToolKind, target: AudioToolTarget) -> Self {
        Self {
            follow_selection: tool_kind.default_follow_selection(),
            pin_target: !tool_kind.default_follow_selection(),
            tool_kind,
            target,
            source_revision: 0,
            preview_enabled: false,
            preview_bypassed: false,
            dirty: false,
            analyzing: false,
            analyze_progress: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRepairModule {
    Denoise,
    DeClick,
    DeHum,
    DeReverb,
    DeBleed,
    DeFeedback,
    DrumSilencer,
    SpectralRepair,
}

impl AudioRepairModule {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Denoise => "De-Noise",
            Self::DeClick => "De-Click",
            Self::DeHum => "De-Hum",
            Self::DeReverb => "De-Reverb",
            Self::DeBleed => "De-Bleed",
            Self::DeFeedback => "De-Feedback",
            Self::DrumSilencer => "Drum Silencer",
            Self::SpectralRepair => "Spectral Repair",
        }
    }

    pub const fn is_available(self) -> bool {
        matches!(
            self,
            Self::Denoise | Self::DeClick | Self::DeHum | Self::SpectralRepair
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyzers_follow_selection_by_default() {
        assert!(AudioToolKind::SpectrumAnalyzer.default_follow_selection());
        assert!(!AudioToolKind::Normalize.default_follow_selection());
    }

    #[test]
    fn unavailable_repair_modules_are_marked() {
        assert!(AudioRepairModule::Denoise.is_available());
        assert!(!AudioRepairModule::DeReverb.is_available());
    }
}
