//! Local view state for the audio editor (viewport, tool, and selection).

use crate::{
    AmplitudeScale, AudioChannelMode, AudioEditorDrag, AudioEditorDropdown, AudioEditorSnap,
    AudioEditorTool, AudioEditorViewMode, AudioEditorViewport, FrequencyScale, SpectralSelection,
};

#[derive(Debug, Clone)]
pub struct AudioEditorState {
    /// Shared coordinate model used by both rendering and hit-testing.
    pub viewport: AudioEditorViewport,
    pub active_tool: AudioEditorTool,
    pub snap: AudioEditorSnap,
    pub channel_mode: AudioChannelMode,
    pub view_mode: AudioEditorViewMode,
    pub amplitude_scale: AmplitudeScale,
    pub frequency_scale: FrequencyScale,
    /// At most one compact dropdown can be open at a time.
    pub open_dropdown: Option<AudioEditorDropdown>,
    pub zero_crossing_snap: bool,
    /// Beat range relative to clip start (for overlay).
    pub selection_range: Option<(f32, f32)>,
    /// Optional time-frequency selection for spectral repair workflows.
    pub spectral_selection: Option<SpectralSelection>,
    /// Explicit gesture state. Project mutations are committed by the host at
    /// the end of a gesture, not on every pointer sample.
    pub drag: AudioEditorDrag,
    /// Last clip id the editor fitted scroll/zoom for.
    pub fitted_clip_id: Option<String>,
    /// Width used by the last automatic fit. A dock resize should refit the
    /// current clip without resetting zoom on every ordinary repaint.
    last_fit_width: f32,
}

impl Default for AudioEditorState {
    fn default() -> Self {
        Self {
            viewport: AudioEditorViewport::default(),
            // A dedicated clip editor should behave like RX on first open:
            // dragging the waveform selects audio immediately. Pointer remains
            // available for edge trimming and playhead clicks.
            active_tool: AudioEditorTool::Range,
            snap: AudioEditorSnap::Grid,
            channel_mode: AudioChannelMode::Combined,
            view_mode: AudioEditorViewMode::WaveformOverlay,
            amplitude_scale: AmplitudeScale::Decibels,
            frequency_scale: FrequencyScale::Logarithmic,
            open_dropdown: None,
            zero_crossing_snap: false,
            selection_range: None,
            spectral_selection: None,
            drag: AudioEditorDrag::None,
            fitted_clip_id: None,
            last_fit_width: 0.0,
        }
    }
}

impl AudioEditorState {
    pub fn fit_clip(&mut self, clip_id: &str, duration_beats: f32, viewport_width: f32) {
        if self.fitted_clip_id.as_deref() == Some(clip_id)
            && (self.last_fit_width - viewport_width).abs() <= 0.5
        {
            return;
        }
        self.fitted_clip_id = Some(clip_id.to_string());
        self.last_fit_width = viewport_width;
        self.viewport.fit_clip(duration_beats, viewport_width);
    }

    pub fn reset_for_clip_change(&mut self, clip_id: Option<&str>) {
        if clip_id != self.fitted_clip_id.as_deref() {
            self.fitted_clip_id = None;
            self.last_fit_width = 0.0;
            self.selection_range = None;
            self.spectral_selection = None;
            self.drag = AudioEditorDrag::None;
            self.open_dropdown = None;
        }
    }

    pub fn adjust_vertical_zoom(&mut self, factor: f32) {
        self.viewport.vertical_zoom = (self.viewport.vertical_zoom * factor).clamp(0.25, 16.0);
    }

    pub fn reset_vertical_zoom(&mut self) {
        self.viewport.vertical_zoom = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_reacts_to_dock_resize_without_refitting_every_repaint() {
        let mut state = AudioEditorState::default();
        state.fit_clip("clip", 16.0, 800.0);
        let wide_zoom = state.viewport.pixels_per_beat;
        state.fit_clip("clip", 16.0, 800.0);
        assert_eq!(state.viewport.pixels_per_beat, wide_zoom);

        state.fit_clip("clip", 16.0, 400.0);
        assert!(state.viewport.pixels_per_beat < wide_zoom);
    }
}
