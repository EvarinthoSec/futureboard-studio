//! GPUI host for the audio clip editor.
//!
//! This is the adapter boundary: `sphere-audio-editor` renders semantic UI and
//! this host translates gestures into the existing Timeline command/history,
//! media, and transport paths.

use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui::{
    div, Context, Entity, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollWheelEvent, Styled, Subscription, Window,
};
use sphere_audio_editor::{
    audio_editor_panel, default_wheel_handler_at, empty_audio_editor, nearest_zero_crossing,
    AudioEditorCallbacks, AudioEditorDrag, AudioEditorEvent, AudioEditorSnap, AudioEditorState,
    AudioEditorTool, AudioEditorViewModel, AudioRangeSelection, AudioToolKind, AudioToolTarget,
    ClipChannelMode, EnvelopeCurve, EnvelopePoint, FrequencyScale, SpectralSelection,
    WarpMarkerView, AUDIO_EDITOR_INSPECTOR_WIDTH, AUDIO_EDITOR_TOOLS_WIDTH,
};

use crate::components::audio_editor_adapter::{
    audio_editor_theme, build_waveform_view_model, selected_audio_clip,
};
use crate::components::audio_editor_spectrogram::{
    cached_or_analyze, error_view_model, loading_view_model, to_view_model, RenderedSpectrogram,
    SpectrogramJobParams,
};
use crate::components::timeline::timeline::Timeline;
use crate::components::timeline::timeline_state::{
    clip_output_local_to_source_sample, AudioClipStretchState, ClipEdge, ClipState, ClipType,
    StretchAlgorithm, StretchMode, WarpMarker,
};
use crate::components::timeline::{waveform_cache, waveform_samples};
use crate::theme::Colors;

const INSPECTOR_W: f32 = AUDIO_EDITOR_INSPECTOR_WIDTH;
const TOOLS_W: f32 = AUDIO_EDITOR_TOOLS_WIDTH;

pub struct AudioEditorHost {
    timeline: Entity<Timeline>,
    state: AudioEditorState,
    viewport_width: Rc<Cell<f32>>,
    viewport_height: Rc<Cell<f32>>,
    viewport_origin_x: Rc<Cell<f32>>,
    viewport_origin_y: Rc<Cell<f32>>,
    focus: FocusHandle,
    last_editing_clip: Option<String>,
    active_clip_id: Option<String>,
    pending_pointer_x: f32,
    spectrogram_key: Option<String>,
    spectrogram_result: Option<Result<Arc<RenderedSpectrogram>, String>>,
    spectrogram_generation: u64,
    pending_open_tool: Option<(AudioToolKind, AudioToolTarget)>,
    pending_audition: Option<(f32, bool)>,
    _timeline_observer: Subscription,
}

impl AudioEditorHost {
    fn ensure_spectrogram(&mut self, vm: &AudioEditorViewModel, cx: &mut Context<Self>) {
        let Some(path) = vm.source_path.clone() else {
            self.spectrogram_key = None;
            self.spectrogram_result = None;
            return;
        };
        let params = SpectrogramJobParams {
            frequency_scale: self.state.frequency_scale,
            pitch_semitones: vm.pitch_semitones + vm.fine_cents / 100.0,
            reverse: vm.reverse,
        };
        let key = format!(
            "{path}|{:?}|p{:.3}|r{}",
            params.frequency_scale,
            params.pitch_semitones,
            u8::from(params.reverse)
        );
        if self.spectrogram_key.as_deref() == Some(key.as_str()) {
            return;
        }

        self.spectrogram_key = Some(key);
        self.spectrogram_result = None;
        self.spectrogram_generation = self.spectrogram_generation.wrapping_add(1);
        let generation = self.spectrogram_generation;
        let host = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { cached_or_analyze(&path, params) })
                .await;
            let _ = host.update(cx, |this, cx| {
                if this.spectrogram_generation != generation {
                    return;
                }
                this.spectrogram_result = Some(result);
                cx.notify();
            });
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.platform || modifiers.alt {
            return;
        }

        let tool = match key {
            "q" | "p" => Some(AudioEditorTool::Pointer),
            "r" => Some(AudioEditorTool::Range),
            "x" => Some(AudioEditorTool::SpectralRange),
            "s" => Some(AudioEditorTool::Split),
            "f" => Some(AudioEditorTool::Fade),
            "m" => Some(AudioEditorTool::Marker),
            "y" => Some(AudioEditorTool::Trim),
            "z" => Some(AudioEditorTool::Scrub),
            "d" | "e" => Some(AudioEditorTool::Draw),
            "w" => Some(AudioEditorTool::Warp),
            "a" => Some(AudioEditorTool::Audition),
            _ => None,
        };

        match (key, tool) {
            ("t", None) => {
                if let Some(target) = self.build_tool_target(cx) {
                    self.pending_open_tool = Some((AudioToolKind::TransientDetector, target));
                }
            }
            ("n", None) => {
                if let Some(target) = self.build_tool_target(cx) {
                    self.pending_open_tool = Some((AudioToolKind::Normalize, target));
                }
            }
            (_, Some(tool)) => {
                self.commit_active_clip_gesture(cx);
                self.state.active_tool = tool;
                self.state.open_dropdown = None;
            }
            ("escape", None) => {
                self.cancel_active_clip_gesture(cx);
                if self.pending_audition.is_some() {
                    self.pending_audition = Some((0.0, false));
                }
                self.state.selection_range = None;
                self.state.spectral_selection = None;
                self.state.drag = AudioEditorDrag::None;
                self.state.open_dropdown = None;
            }
            ("0", None) => {
                if let Some(vm) = self.build_view_model(cx) {
                    self.state
                        .viewport
                        .fit_clip(vm.duration_beats, self.viewport_width.get());
                }
            }
            ("+" | "=", None) => self
                .state
                .viewport
                .zoom_around_x(1.25, self.viewport_width.get() * 0.5),
            ("-" | "_", None) => self
                .state
                .viewport
                .zoom_around_x(0.8, self.viewport_width.get() * 0.5),
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    pub fn new(timeline: Entity<Timeline>, cx: &mut Context<Self>) -> Self {
        let _timeline_observer = cx.observe(&timeline, |_, _, cx| cx.notify());
        Self {
            timeline,
            state: AudioEditorState::default(),
            viewport_width: Rc::new(Cell::new(800.0)),
            viewport_height: Rc::new(Cell::new(360.0)),
            viewport_origin_x: Rc::new(Cell::new(174.0)),
            viewport_origin_y: Rc::new(Cell::new(56.0)),
            focus: cx.focus_handle(),
            last_editing_clip: None,
            active_clip_id: None,
            pending_pointer_x: 0.0,
            spectrogram_key: None,
            spectrogram_result: None,
            spectrogram_generation: 0,
            pending_open_tool: None,
            pending_audition: None,
            _timeline_observer,
        }
    }

    /// Drop cached spectrogram tiles so the next render re-analyzes the clip.
    pub(crate) fn refresh_clip_visuals(
        &mut self,
        source_path: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = source_path {
            crate::components::audio_editor_spectrogram::invalidate_path(path);
        }
        self.spectrogram_key = None;
        self.spectrogram_result = None;
        self.spectrogram_generation = self.spectrogram_generation.wrapping_add(1);
        cx.notify();
    }

    pub fn take_pending_open_tool(&mut self) -> Option<(AudioToolKind, AudioToolTarget)> {
        self.pending_open_tool.take()
    }

    pub fn take_pending_audition(&mut self) -> Option<(f32, bool)> {
        self.pending_audition.take()
    }

    pub fn current_tool_target(&self, cx: &Context<Self>) -> Option<AudioToolTarget> {
        self.build_tool_target(cx)
    }

    fn build_tool_target(&self, cx: &Context<Self>) -> Option<AudioToolTarget> {
        let vm = self.build_view_model(cx)?;
        let tl = self.timeline.read(cx);
        let (_, clip) = self.active_clip(&tl.state)?;
        let meta = clip
            .audio_asset_key()
            .and_then(waveform_cache::get_file_meta);
        let sample_rate = meta
            .as_ref()
            .map(|m| m.sample_rate)
            .or_else(|| (vm.spectrogram.sample_rate > 0).then_some(vm.spectrogram.sample_rate))
            .unwrap_or(clip.stretch.original_sample_rate.max(44_100));
        let channels = meta.as_ref().map(|m| m.channels).unwrap_or(2);
        let source_frames = meta
            .as_ref()
            .map(|m| m.total_frames as i64)
            .unwrap_or_else(|| self.total_source_frames(&vm));
        let time_selection = self.state.selection_range.map(|(a, b)| {
            let start = self.frame_for_rel_beat(&vm, a.min(b));
            let end = self.frame_for_rel_beat(&vm, a.max(b));
            AudioRangeSelection::new(start, end)
        });
        Some(AudioToolTarget {
            clip_id: vm.clip_id.clone(),
            source_id: clip
                .audio_asset_key()
                .unwrap_or(vm.clip_id.as_str())
                .to_string(),
            clip_name: vm.clip_name,
            file_label: vm.file_label.unwrap_or_else(|| "Audio Clip".to_string()),
            source_path: vm.source_path,
            sample_rate,
            channels,
            source_frames,
            time_selection,
            spectral_selection: self.state.spectral_selection,
        })
    }

    fn active_clip<'a>(
        &'a self,
        state: &'a crate::components::timeline::timeline_state::TimelineState,
    ) -> Option<(
        &'a crate::components::timeline::timeline_state::TrackState,
        &'a ClipState,
    )> {
        selected_audio_clip(state).or_else(|| {
            let id = self.active_clip_id.as_deref()?;
            let (track, clip) = state.find_clip(id)?;
            matches!(clip.clip_type, ClipType::Audio { .. }).then_some((track, clip))
        })
    }

    fn build_view_model(&self, cx: &Context<Self>) -> Option<AudioEditorViewModel> {
        let tl = self.timeline.read(cx);
        let (track, clip) = self.active_clip(&tl.state)?;
        let theme = audio_editor_theme();
        let viewport_w = self.viewport_width.get().max(320.0);
        let ppb = self.state.viewport.pixels_per_beat;
        let scroll_x = self.state.viewport.scroll_x;
        let seconds_per_beat = tl.state.seconds_per_beat().max(0.0001);

        let playhead_in_clip = {
            let rel = tl.state.transport.playhead_beats - clip.start_beat;
            if tl.state.transport.playing && rel >= 0.0 && rel <= clip.duration_beats {
                Some(rel)
            } else {
                None
            }
        };

        let selection_range = self.state.selection_range.or_else(|| {
            tl.state.arrangement_range.as_ref().map(|range| {
                let (a, b) = range.as_f32_range();
                let lo = (a - clip.start_beat).max(0.0);
                let hi = (b - clip.start_beat).min(clip.duration_beats);
                (lo.min(hi), lo.max(hi))
            })
        });

        let file_label = match &clip.clip_type {
            ClipType::Audio {
                source_path: Some(path),
                ..
            } => Some(
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path)
                    .to_string(),
            ),
            _ => None,
        };
        let source_path = match &clip.clip_type {
            ClipType::Audio { source_path, .. } => source_path.clone(),
            _ => None,
        };

        let meta = clip
            .audio_asset_key()
            .and_then(waveform_cache::get_file_meta);
        let gain_db = gain_to_db(clip.gain);
        let (pitch_semitones, fine_cents) = clip.stretch.pitch_semi_and_cents();
        let selection_labels = selection_labels(selection_range, seconds_per_beat);
        let peak_label = clip
            .audio_asset_key()
            .and_then(|key| peak_label_for_asset(key, clip))
            .unwrap_or_else(|| "—".to_string());
        let source_summary = meta
            .as_ref()
            .map(|m| format!("{} kHz · {} ch", m.sample_rate / 1000, m.channels))
            .unwrap_or_else(|| match &clip.audio_import {
                crate::components::timeline::timeline_state::AudioImportState::Failed {
                    ..
                } => "Source unavailable".to_string(),
                _ => "Preparing source…".to_string(),
            });
        let stretch_mode = match clip.stretch.mode {
            StretchMode::Off => "Off",
            StretchMode::Resample => "Resample",
            StretchMode::TempoSync => "Tempo Sync",
            StretchMode::Manual => "Manual",
            StretchMode::Warp => "Warp",
        };

        let spectrogram = self
            .spectrogram_result
            .as_ref()
            .and_then(|result| result.as_ref().ok())
            .map(|rendered| to_view_model(rendered, self.state.view_mode))
            .unwrap_or_else(|| {
                self.spectrogram_result
                    .as_ref()
                    .and_then(|result| result.as_ref().err())
                    .map_or_else(
                        || loading_view_model("Preparing spectrogram…"),
                        error_view_model,
                    )
            });

        Some(AudioEditorViewModel {
            clip_id: clip.id.clone(),
            clip_name: clip.name.clone(),
            file_label,
            source_path,
            source_start_frame: clip.stretch.source_start_samples as i64,
            source_end_frame: {
                let start = clip.stretch.source_start_samples;
                if clip.stretch.source_end_samples > start {
                    clip.stretch.source_end_samples as i64
                } else {
                    clip.stretch
                        .original_duration_samples
                        .max(meta.as_ref().map(|m| m.total_frames).unwrap_or(0))
                        as i64
                }
            },
            start_beat: clip.start_beat,
            duration_beats: clip.duration_beats,
            offset_beats: clip.offset_beats,
            beats_per_bar: tl.state.beats_per_bar(),
            bpm: tl.state.bpm,
            track_color: track.color,
            waveform: build_waveform_view_model(
                clip,
                &tl.state,
                ppb,
                scroll_x,
                viewport_w,
                self.state.view_mode.prefers_sample_view(),
            ),
            spectrogram,
            view_mode: self.state.view_mode,
            amplitude_scale: self.state.amplitude_scale,
            frequency_scale: self.state.frequency_scale,
            playhead_in_clip,
            selection_range,
            spectral_selection: self.state.spectral_selection,
            gain_envelope: clip.stretch.gain_envelope.clone(),
            gain_db,
            pitch_semitones,
            fine_cents,
            original_bpm: clip.stretch.bpm_source,
            follow_tempo: clip.stretch.follows_project_tempo(),
            stretch_mode,
            stretch_percent: clip.stretch.stretch_percent(),
            reverse: clip.stretch.reverse,
            denoise_amount: clip.stretch.denoise_amount,
            channel_mode: self.state.channel_mode,
            fade_in_beats: (clip.stretch.fade_in_ms.max(0.0) / 1000.0) / seconds_per_beat,
            fade_out_beats: (clip.stretch.fade_out_ms.max(0.0) / 1000.0) / seconds_per_beat,
            warp_markers: clip
                .stretch
                .warp_markers
                .iter()
                .map(|marker| WarpMarkerView {
                    id: marker.id,
                    rel_beat: (marker.timeline_beat as f32 - clip.start_beat)
                        .clamp(0.0, clip.duration_beats.max(0.0)),
                    source_sample: marker.source_sample,
                    locked: marker.locked,
                })
                .collect(),
            clip_channel: ClipChannelMode::from_tag(clip.stretch.channel_transform),
            display_smoothing: self.state.display_smoothing,
            sample_rate: meta.as_ref().map(|m| m.sample_rate).unwrap_or(0),
            selection_start_label: selection_labels.0,
            selection_end_label: selection_labels.1,
            selection_duration_label: selection_labels.2,
            peak_label,
            source_summary,
            theme,
        })
    }

    fn local_x(&self, window_x: f32) -> f32 {
        (window_x - self.viewport_origin_x.get()).max(0.0)
    }

    fn local_y(&self, window_y: f32) -> f32 {
        (window_y - self.viewport_origin_y.get()).max(0.0)
    }

    fn clip_at_event(&self, cx: &Context<Self>) -> Option<(String, f32, f32, f32)> {
        let tl = self.timeline.read(cx);
        let (_, clip) = self.active_clip(&tl.state)?;
        let local_x = self.local_x(self.pending_pointer_x);
        let rel_beat = self
            .state
            .viewport
            .x_to_beat(local_x)
            .clamp(0.0, clip.duration_beats.max(0.0));
        Some((
            clip.id.clone(),
            clip.start_beat,
            clip.duration_beats,
            rel_beat,
        ))
    }

    fn snap_beat(&self, beat: f32, bypass: bool, cx: &Context<Self>) -> f32 {
        if bypass {
            return beat;
        }
        match self.state.snap {
            AudioEditorSnap::Off => beat,
            AudioEditorSnap::Grid => self
                .timeline
                .read(cx)
                .state
                .snap_beats_with_bypass(beat, false),
            AudioEditorSnap::ZeroCrossing => self.snap_zero_crossing(beat, cx).unwrap_or(beat),
            AudioEditorSnap::Markers | AudioEditorSnap::Transients => beat,
        }
    }

    fn snap_zero_crossing(&self, beat: f32, cx: &Context<Self>) -> Option<f32> {
        let vm = self.build_view_model(cx)?;
        let tl = self.timeline.read(cx);
        let (_, clip) = self.active_clip(&tl.state)?;
        let asset_key = clip.audio_asset_key()?;
        let rel = (beat - vm.start_beat).clamp(0.0, vm.duration_beats.max(0.0));
        let frame = self.frame_for_rel_beat(&vm, rel).max(0) as u64;
        let radius = 2_048_u64;
        let window = waveform_samples::get_window(
            asset_key,
            frame.saturating_sub(radius),
            frame.saturating_add(radius),
        );
        let window = match window {
            Some(window) => window,
            None => {
                if let Some(path) = vm.source_path.as_deref() {
                    waveform_samples::note_needed(
                        asset_key,
                        path,
                        frame.saturating_sub(radius),
                        frame.saturating_add(radius),
                        self.total_source_frames(&vm).max(0) as u64,
                    );
                }
                return None;
            }
        };
        let origin = frame.saturating_sub(window.start_frame) as usize;
        let snapped = nearest_zero_crossing(&window.mono, origin, radius as usize)?;
        let snapped_frame = window.start_frame as i64 + snapped as i64;
        Some(vm.start_beat + self.rel_beat_for_frame(&vm, snapped_frame))
    }

    fn rel_beat_for_frame(&self, vm: &AudioEditorViewModel, frame: i64) -> f32 {
        let start = vm.source_start_frame.max(0);
        let end = vm.source_end_frame.max(start + 1);
        let t = (frame - start) as f32 / (end - start) as f32;
        (t.clamp(0.0, 1.0) * vm.duration_beats).clamp(0.0, vm.duration_beats.max(0.0))
    }

    fn handle_event(
        &mut self,
        event: AudioEditorEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            AudioEditorEvent::ToggleDropdown(dropdown) => {
                self.state.open_dropdown =
                    (self.state.open_dropdown != Some(dropdown)).then_some(dropdown);
                cx.notify();
            }
            AudioEditorEvent::SetTool(tool) => {
                self.commit_active_clip_gesture(cx);
                self.state.drag = AudioEditorDrag::None;
                self.state.active_tool = tool;
                self.state.open_dropdown = None;
                cx.notify();
            }
            AudioEditorEvent::SetSnap(snap) => {
                self.state.snap = snap;
                self.state.zero_crossing_snap = snap == AudioEditorSnap::ZeroCrossing;
                self.state.open_dropdown = None;
                cx.notify();
            }
            AudioEditorEvent::SetChannelMode(mode) => {
                self.state.channel_mode = mode;
                self.state.open_dropdown = None;
                cx.notify();
            }
            AudioEditorEvent::SetViewMode(mode) => {
                self.state.view_mode = mode;
                self.state.open_dropdown = None;
                cx.notify();
            }
            AudioEditorEvent::SetAmplitudeScale(scale) => {
                self.state.amplitude_scale = scale;
                self.state.open_dropdown = None;
                cx.notify();
            }
            AudioEditorEvent::SetFrequencyScale(scale) => {
                self.state.frequency_scale = scale;
                self.state.open_dropdown = None;
                cx.notify();
            }
            AudioEditorEvent::SetDisplaySmoothing(mode) => {
                self.state.display_smoothing = mode;
                self.state.open_dropdown = None;
                cx.notify();
            }
            AudioEditorEvent::SetClipChannel(mode) => {
                self.set_clip_channel(mode, cx);
            }
            AudioEditorEvent::AdjustVerticalZoom { factor } => {
                self.state.adjust_vertical_zoom(factor);
                cx.notify();
            }
            AudioEditorEvent::ResetVerticalZoom => {
                self.state.reset_vertical_zoom();
                cx.notify();
            }
            AudioEditorEvent::ZoomBy { factor, anchor_x } => {
                let anchor = anchor_x.unwrap_or(self.viewport_width.get() * 0.5);
                self.state.viewport.zoom_around_x(factor, anchor);
                cx.notify();
            }
            AudioEditorEvent::FitClip => {
                if let Some(vm) = self.build_view_model(cx) {
                    self.state
                        .viewport
                        .fit_clip(vm.duration_beats, self.viewport_width.get());
                    cx.notify();
                }
            }
            AudioEditorEvent::Seek { x } => {
                self.pending_pointer_x = x;
                let Some((_, start_beat, _, rel_beat)) = self.clip_at_event(cx) else {
                    return;
                };
                let beat = self.snap_beat(start_beat + rel_beat, false, cx);
                let _ = self.timeline.update(cx, |timeline, cx| {
                    timeline.seek_to_exact_beat(beat, crate::layout::SeekReason::TimelineClick, cx);
                });
            }
            AudioEditorEvent::PointerDown { x, y, shift } => {
                self.pending_pointer_x = x;
                self.begin_pointer(x, y, shift, cx);
            }
            AudioEditorEvent::PointerMove { x, y, shift } => {
                self.pending_pointer_x = x;
                self.update_pointer(x, y, shift, cx);
            }
            AudioEditorEvent::PointerUp { x, y, shift } => {
                self.pending_pointer_x = x;
                // A few native backends deliver the press/release pair but
                // omit intermediate move notifications for a short drag.
                // Apply the release position once before committing so range,
                // spectral, trim, fade, and envelope edits still land at the
                // point where the user released the pointer.
                self.update_pointer(x, y, shift, cx);
                self.finish_pointer(x, y, shift, cx);
            }
            AudioEditorEvent::AdjustGain { delta_db } => {
                self.adjust_gain(delta_db, cx);
            }
            AudioEditorEvent::AdjustPitch { delta_semitones } => {
                self.adjust_pitch(delta_semitones, 0.0, cx);
            }
            AudioEditorEvent::AdjustFinePitch { delta_cents } => {
                self.adjust_pitch(0.0, delta_cents, cx);
            }
            AudioEditorEvent::ToggleFollowTempo => self.toggle_follow_tempo(cx),
            AudioEditorEvent::ToggleReverse => self.toggle_reverse(cx),
            AudioEditorEvent::SetFollowTempo(enabled) => {
                self.set_follow_tempo(enabled, cx);
            }
            AudioEditorEvent::SetReverse(enabled) => {
                self.set_reverse(enabled, cx);
            }
            AudioEditorEvent::SetDenoiseAmount(amount) => {
                self.set_denoise_amount(amount, cx);
            }
            AudioEditorEvent::OpenTool(kind) => {
                self.state.open_dropdown = None;
                if let Some(target) = self.build_tool_target(cx) {
                    self.pending_open_tool = Some((kind, target));
                }
                cx.notify();
            }
        }
        let _ = window;
    }

    fn begin_pointer(&mut self, x: f32, _y: f32, shift: bool, cx: &mut Context<Self>) {
        let Some(vm) = self.build_view_model(cx) else {
            return;
        };
        let local_x = self.local_x(x);
        let raw_rel_beat = self
            .state
            .viewport
            .x_to_beat(local_x)
            .clamp(0.0, vm.duration_beats.max(0.0));
        let abs_beat = self.snap_beat(vm.start_beat + raw_rel_beat, shift, cx);
        let rel_beat = (abs_beat - vm.start_beat).clamp(0.0, vm.duration_beats.max(0.0));

        match self.state.active_tool {
            AudioEditorTool::Split => {
                let _ = self.timeline.update(cx, |timeline, cx| {
                    if timeline.split_audio_clip_at_beat(&vm.clip_id, abs_beat, cx) {
                        timeline.mark_media_changed(cx);
                    }
                });
            }
            AudioEditorTool::Range => {
                self.state.selection_range = Some((rel_beat, rel_beat));
                self.state.drag = AudioEditorDrag::SelectingRange {
                    anchor_beat: rel_beat,
                };
                cx.notify();
            }
            AudioEditorTool::SpectralRange => {
                let local_y = self.local_y(_y);
                let view_h = self.viewport_height.get().max(180.0);
                let frame = self.frame_for_rel_beat(&vm, rel_beat);
                let hz = self.hz_for_local_y(&vm, local_y, view_h);
                let total_frames = self.total_source_frames(&vm);
                let nyquist_hz = self.nyquist_hz(&vm);
                self.state.spectral_selection = Some(
                    SpectralSelection::new(frame, frame, hz, hz).clamp_to(total_frames, nyquist_hz),
                );
                self.state.drag = AudioEditorDrag::SelectingSpectralRange {
                    anchor_frame: frame,
                    anchor_hz: hz,
                };
                cx.notify();
            }
            AudioEditorTool::Fade => {
                let fade_in_width = vm.fade_in_beats * self.state.viewport.pixels_per_beat;
                let fade_out_width = vm.fade_out_beats * self.state.viewport.pixels_per_beat;
                let clip_start_x = -self.state.viewport.scroll_x;
                let clip_end_x = vm.duration_beats * self.state.viewport.pixels_per_beat
                    - self.state.viewport.scroll_x;
                if fade_in_width > 1.0
                    && local_x >= clip_start_x - 8.0
                    && local_x <= clip_start_x + fade_in_width + 8.0
                {
                    self.begin_clip_gesture(&vm.clip_id, cx);
                    self.state.drag = AudioEditorDrag::AdjustingFadeIn {
                        original_seconds: vm.fade_in_beats * self.seconds_per_beat(cx),
                    };
                } else if fade_out_width > 1.0
                    && local_x >= clip_end_x - fade_out_width - 8.0
                    && local_x <= clip_end_x + 8.0
                {
                    self.begin_clip_gesture(&vm.clip_id, cx);
                    self.state.drag = AudioEditorDrag::AdjustingFadeOut {
                        original_seconds: vm.fade_out_beats * self.seconds_per_beat(cx),
                    };
                } else {
                    self.seek_relative(vm.start_beat + rel_beat, cx);
                }
            }
            AudioEditorTool::Pointer | AudioEditorTool::Trim => {
                let clip_start_x = -self.state.viewport.scroll_x;
                let clip_end_x = vm.duration_beats * self.state.viewport.pixels_per_beat
                    - self.state.viewport.scroll_x;
                let edge_tolerance = 8.0;
                if (local_x - clip_start_x).abs() <= edge_tolerance {
                    self.begin_clip_gesture(&vm.clip_id, cx);
                    self.state.drag = AudioEditorDrag::TrimmingLeft {
                        start_beat: vm.start_beat,
                    };
                } else if (local_x - clip_end_x).abs() <= edge_tolerance {
                    self.begin_clip_gesture(&vm.clip_id, cx);
                    self.state.drag = AudioEditorDrag::TrimmingRight {
                        start_beat: vm.start_beat,
                    };
                } else if matches!(self.state.active_tool, AudioEditorTool::Pointer) {
                    self.seek_relative(vm.start_beat + rel_beat, cx);
                }
            }
            AudioEditorTool::Marker => {
                let abs_beat = vm.start_beat + rel_beat;
                let _ = self.timeline.update(cx, |timeline, cx| {
                    let prev = timeline.state.markers.clone();
                    timeline.state.add_marker_at_beat(abs_beat as f64);
                    if timeline.record_marker_edit("Add Marker", prev, cx) {
                        timeline.mark_media_changed(cx);
                    }
                });
            }
            AudioEditorTool::Scrub => {
                self.seek_relative(vm.start_beat + rel_beat, cx);
                self.state.drag = AudioEditorDrag::SelectingRange {
                    anchor_beat: rel_beat,
                };
            }
            AudioEditorTool::Audition => {
                let beat = if let Some((a, b)) = self.state.selection_range {
                    vm.start_beat + a.min(b)
                } else {
                    vm.start_beat + rel_beat
                };
                self.seek_relative(beat, cx);
                self.pending_audition = Some((beat, true));
                self.state.drag = AudioEditorDrag::SelectingRange {
                    anchor_beat: rel_beat,
                };
                cx.notify();
            }
            AudioEditorTool::Warp => {
                let hit_id = vm.warp_markers.iter().find_map(|marker| {
                    let x = marker.rel_beat * self.state.viewport.pixels_per_beat
                        - self.state.viewport.scroll_x;
                    ((x - local_x).abs() <= 8.0).then_some(marker.id)
                });
                if let Some(marker_id) = hit_id {
                    self.begin_clip_gesture(&vm.clip_id, cx);
                    self.state.drag = AudioEditorDrag::MovingWarpMarker { marker_id };
                } else {
                    self.add_warp_marker(&vm.clip_id, vm.start_beat + rel_beat, cx);
                }
                cx.notify();
            }
            AudioEditorTool::Draw => {
                let local_y = self.local_y(_y);
                let view_h = self.viewport_height.get().max(180.0);
                let time = (rel_beat / vm.duration_beats.max(f32::EPSILON)).clamp(0.0, 1.0);
                let value_db = gain_db_for_local_y(local_y, view_h);
                let point_id = vm
                    .gain_envelope
                    .points
                    .iter()
                    .filter_map(|point| {
                        let point_x =
                            point.time * vm.duration_beats * self.state.viewport.pixels_per_beat
                                - self.state.viewport.scroll_x;
                        let point_y = envelope_y_for_host(point.value_db, view_h);
                        ((point_x - local_x).abs() <= 10.0 && (point_y - local_y).abs() <= 10.0)
                            .then_some(point.id)
                    })
                    .next()
                    .unwrap_or_else(|| {
                        vm.gain_envelope
                            .points
                            .iter()
                            .map(|point| point.id)
                            .max()
                            .unwrap_or(0)
                            .saturating_add(1)
                    });
                self.begin_clip_gesture(&vm.clip_id, cx);
                let clip_id = vm.clip_id.clone();
                let _ = self.timeline.update(cx, |timeline, cx| {
                    let Some(mut stretch) = timeline.state.clip_stretch(&clip_id).cloned() else {
                        return;
                    };
                    if let Some(point) = stretch
                        .gain_envelope
                        .points
                        .iter_mut()
                        .find(|point| point.id == point_id)
                    {
                        point.time = time;
                        point.value_db = value_db;
                    } else {
                        stretch.gain_envelope.points.push(EnvelopePoint {
                            id: point_id,
                            time,
                            value_db,
                            curve: EnvelopeCurve::Linear,
                        });
                    }
                    stretch.gain_envelope.sanitize_in_place();
                    if timeline.state.set_clip_stretch(&clip_id, stretch) {
                        cx.notify();
                    }
                });
                self.state.drag = AudioEditorDrag::MovingEnvelopePoint { point_id };
            }
        }
    }

    fn update_pointer(&mut self, x: f32, _y: f32, shift: bool, cx: &mut Context<Self>) {
        let drag = self.state.drag;
        let Some(vm) = self.build_view_model(cx) else {
            return;
        };
        let raw_rel_beat = self
            .state
            .viewport
            .x_to_beat(self.local_x(x))
            .clamp(0.0, vm.duration_beats.max(0.0));
        let rel_beat = (self.snap_beat(vm.start_beat + raw_rel_beat, shift, cx) - vm.start_beat)
            .clamp(0.0, vm.duration_beats.max(0.0));
        match drag {
            AudioEditorDrag::None => {}
            AudioEditorDrag::SelectingRange { anchor_beat } => {
                if self.state.active_tool == AudioEditorTool::Scrub
                    || self.state.active_tool == AudioEditorTool::Audition
                {
                    self.seek_relative(vm.start_beat + rel_beat, cx);
                } else {
                    self.state.selection_range = Some((anchor_beat, rel_beat));
                    cx.notify();
                }
            }
            AudioEditorDrag::SelectingSpectralRange {
                anchor_frame,
                anchor_hz,
            } => {
                let local_y = self.local_y(_y);
                let frame = self.frame_for_rel_beat(&vm, rel_beat);
                let hz = self.hz_for_local_y(&vm, local_y, self.viewport_height.get().max(180.0));
                self.state.spectral_selection = Some(
                    SpectralSelection::new(anchor_frame, frame, anchor_hz, hz)
                        .clamp_to(self.total_source_frames(&vm), self.nyquist_hz(&vm)),
                );
                cx.notify();
            }
            AudioEditorDrag::TrimmingLeft { .. } => {
                let beat = vm.start_beat + rel_beat;
                let clip_id = vm.clip_id.clone();
                let _ = self.timeline.update(cx, |timeline, cx| {
                    timeline
                        .state
                        .resize_clip_with_bypass(&clip_id, ClipEdge::Left, beat, true);
                    cx.notify();
                });
            }
            AudioEditorDrag::TrimmingRight { .. } => {
                let beat = vm.start_beat + rel_beat;
                let clip_id = vm.clip_id.clone();
                let _ = self.timeline.update(cx, |timeline, cx| {
                    timeline
                        .state
                        .resize_clip_with_bypass(&clip_id, ClipEdge::Right, beat, true);
                    cx.notify();
                });
            }
            AudioEditorDrag::AdjustingFadeIn { .. } => {
                self.set_fade(&vm.clip_id, rel_beat * self.seconds_per_beat(cx), None, cx);
            }
            AudioEditorDrag::AdjustingFadeOut { .. } => {
                let fade_seconds = (vm.duration_beats - rel_beat) * self.seconds_per_beat(cx);
                self.set_fade(&vm.clip_id, fade_seconds, Some(AudioFadeSide::Out), cx);
            }
            AudioEditorDrag::MovingEnvelopePoint { point_id } => {
                let local_y = self.local_y(_y);
                let time = (rel_beat / vm.duration_beats.max(f32::EPSILON)).clamp(0.0, 1.0);
                let value_db = gain_db_for_local_y(local_y, self.viewport_height.get().max(180.0));
                self.update_envelope_point(&vm.clip_id, point_id, time, value_db, cx);
            }
            AudioEditorDrag::MovingWarpMarker { marker_id } => {
                self.move_warp_marker(
                    &vm.clip_id,
                    marker_id,
                    vm.start_beat + rel_beat,
                    vm.start_beat,
                    vm.duration_beats,
                    cx,
                );
            }
        }
    }

    fn finish_pointer(&mut self, _x: f32, _y: f32, _shift: bool, cx: &mut Context<Self>) {
        let drag = std::mem::take(&mut self.state.drag);
        match drag {
            AudioEditorDrag::SelectingRange { .. } => {
                if self.state.active_tool == AudioEditorTool::Audition {
                    self.pending_audition = Some((0.0, false));
                }
                if let Some((start, end)) = self.state.selection_range {
                    if (end - start).abs() < 0.0001 {
                        self.state.selection_range = None;
                    }
                }
                cx.notify();
            }
            AudioEditorDrag::SelectingSpectralRange { .. } => {
                if self
                    .state
                    .spectral_selection
                    .is_some_and(SpectralSelection::is_empty)
                {
                    self.state.spectral_selection = None;
                }
                cx.notify();
            }
            AudioEditorDrag::TrimmingLeft { .. }
            | AudioEditorDrag::TrimmingRight { .. }
            | AudioEditorDrag::AdjustingFadeIn { .. }
            | AudioEditorDrag::AdjustingFadeOut { .. } => {
                self.commit_active_clip_gesture(cx);
            }
            AudioEditorDrag::MovingEnvelopePoint { .. }
            | AudioEditorDrag::MovingWarpMarker { .. } => {
                self.commit_active_clip_gesture(cx);
            }
            AudioEditorDrag::None => {}
        }
    }

    fn begin_clip_gesture(&mut self, clip_id: &str, cx: &mut Context<Self>) {
        if self.active_clip_id.as_deref() != Some(clip_id) {
            self.commit_active_clip_gesture(cx);
        }
        self.active_clip_id = Some(clip_id.to_string());
        let _ = self.timeline.update(cx, |timeline, _cx| {
            timeline.begin_inspector_clip_gesture(clip_id);
        });
    }

    fn commit_active_clip_gesture(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(clip_id) = self.active_clip_id.clone() else {
            return false;
        };
        let mut changed = false;
        let _ = self.timeline.update(cx, |timeline, cx| {
            if timeline.commit_inspector_clip_gesture(&clip_id, cx) {
                timeline.mark_media_changed(cx);
                changed = true;
            }
            cx.notify();
        });
        changed
    }

    fn cancel_active_clip_gesture(&mut self, cx: &mut Context<Self>) -> bool {
        let mut restored = false;
        let _ = self.timeline.update(cx, |timeline, cx| {
            restored = timeline.cancel_inspector_clip_gesture(cx);
        });
        if restored {
            cx.notify();
        }
        restored
    }

    fn seconds_per_beat(&self, cx: &Context<Self>) -> f32 {
        self.timeline.read(cx).state.seconds_per_beat().max(0.0001)
    }

    fn total_source_frames(&self, vm: &AudioEditorViewModel) -> i64 {
        let window = (vm.source_end_frame - vm.source_start_frame).max(0);
        if window > 0 {
            return vm.source_end_frame.max(0);
        }
        (vm.spectrogram.duration_seconds.max(0.0) * vm.spectrogram.sample_rate as f32)
            .round()
            .max(0.0) as i64
    }

    fn nyquist_hz(&self, vm: &AudioEditorViewModel) -> f32 {
        vm.spectrogram
            .max_frequency_hz
            .max(vm.spectrogram.sample_rate as f32 * 0.5)
            .max(1.0)
    }

    fn frame_for_rel_beat(&self, vm: &AudioEditorViewModel, rel_beat: f32) -> i64 {
        let start = vm.source_start_frame.max(0);
        let end = vm.source_end_frame.max(start);
        if end <= start || vm.duration_beats <= f32::EPSILON {
            return start;
        }
        let t = (rel_beat / vm.duration_beats).clamp(0.0, 1.0);
        start + ((t * (end - start) as f32).round() as i64)
    }

    fn hz_for_local_y(&self, vm: &AudioEditorViewModel, local_y: f32, view_h: f32) -> f32 {
        let max_hz = self.nyquist_hz(vm);
        let position = (1.0 - local_y / view_h.max(1.0)).clamp(0.0, 1.0);
        match self.state.frequency_scale {
            FrequencyScale::Linear => position * max_hz,
            FrequencyScale::Logarithmic => {
                let min_hz = 20.0_f32.min(max_hz);
                min_hz * (max_hz / min_hz).powf(position)
            }
        }
    }

    fn seek_relative(&self, beat: f32, cx: &mut Context<Self>) {
        let _ = self.timeline.update(cx, |timeline, cx| {
            timeline.seek_to_exact_beat(beat, crate::layout::SeekReason::TimelineClick, cx);
        });
    }

    fn update_envelope_point(
        &self,
        clip_id: &str,
        point_id: u64,
        time: f32,
        value_db: f32,
        cx: &mut Context<Self>,
    ) {
        let clip_id = clip_id.to_string();
        let _ = self.timeline.update(cx, |timeline, cx| {
            let Some(mut stretch) = timeline.state.clip_stretch(&clip_id).cloned() else {
                return;
            };
            let Some(point) = stretch
                .gain_envelope
                .points
                .iter_mut()
                .find(|point| point.id == point_id)
            else {
                return;
            };
            point.time = time.clamp(0.0, 1.0);
            point.value_db = value_db.clamp(-120.0, 24.0);
            stretch.gain_envelope.sanitize_in_place();
            stretch.dirty = true;
            if timeline.state.set_clip_stretch(&clip_id, stretch) {
                cx.notify();
            }
        });
    }

    fn set_fade(
        &self,
        clip_id: &str,
        seconds: f32,
        side: Option<AudioFadeSide>,
        cx: &mut Context<Self>,
    ) {
        let side = side.unwrap_or(AudioFadeSide::In);
        let _ = self.timeline.update(cx, |timeline, cx| {
            let Some(mut stretch) = timeline.state.clip_stretch(clip_id).cloned() else {
                return;
            };
            match side {
                AudioFadeSide::In => stretch.fade_in_ms = seconds.max(0.0) * 1000.0,
                AudioFadeSide::Out => stretch.fade_out_ms = seconds.max(0.0) * 1000.0,
            }
            stretch.dirty = true;
            if timeline.state.set_clip_stretch(clip_id, stretch) {
                cx.notify();
            }
        });
    }

    fn adjust_gain(&mut self, delta_db: f32, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        let clip_id = clip.id.clone();
        let next_db = (gain_to_db(clip.gain) + delta_db).clamp(-60.0, 12.0);
        let next_gain = db_to_gain(next_db);
        let gesture_clip_id = clip_id.clone();
        self.apply_clip_gesture(&clip_id, cx, move |timeline| {
            timeline.state.set_clip_gain(&gesture_clip_id, next_gain)
        });
    }

    fn adjust_pitch(&mut self, delta_semitones: f32, delta_cents: f32, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        let clip_id = clip.id.clone();
        let mut next = clip.stretch.clone();
        let (semi, cents) = next.pitch_semi_and_cents();
        next.set_pitch_semi_and_cents(semi + delta_semitones, cents + delta_cents);
        self.apply_clip_stretch_gesture(&clip_id, next, cx);
    }

    fn toggle_follow_tempo(&mut self, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        self.set_follow_tempo(!clip.stretch.follows_project_tempo(), cx);
    }

    fn set_follow_tempo(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        let clip_id = clip.id.clone();
        let mut next = clip.stretch.clone();
        let project_bpm = self.timeline.read(cx).state.bpm as f64;
        if enabled {
            next.mode = StretchMode::TempoSync;
            next.algorithm = StretchAlgorithm::PhaseVocoder;
            next.preserve_pitch = true;
            next.apply_tempo_sync(project_bpm);
        } else {
            next.mode = StretchMode::Manual;
            next.bpm_target = None;
        }
        next.dirty = true;
        self.state.open_dropdown = None;
        self.apply_clip_stretch_gesture(&clip_id, next, cx);
    }

    fn toggle_reverse(&mut self, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        self.set_reverse(!clip.stretch.reverse, cx);
    }

    fn set_reverse(&mut self, enabled: bool, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        let clip_id = clip.id.clone();
        let mut next = clip.stretch.clone();
        next.reverse = enabled;
        next.dirty = true;
        self.state.open_dropdown = None;
        self.apply_clip_stretch_gesture(&clip_id, next, cx);
    }

    fn set_denoise_amount(&mut self, amount: f32, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        let clip_id = clip.id.clone();
        let mut next = clip.stretch.clone();
        next.denoise_amount = amount.clamp(0.0, 1.0);
        next.dirty = true;
        self.state.open_dropdown = None;
        self.apply_clip_stretch_gesture(&clip_id, next, cx);
    }

    fn set_clip_channel(&mut self, mode: ClipChannelMode, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        let clip_id = clip.id.clone();
        let mut next = clip.stretch.clone();
        next.channel_transform = mode.to_tag();
        next.dirty = true;
        self.state.open_dropdown = None;
        self.apply_clip_stretch_gesture(&clip_id, next, cx);
    }

    fn add_warp_marker(&mut self, clip_id: &str, timeline_beat: f32, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.active_clip(&self.timeline.read(cx).state) else {
            return;
        };
        if clip.id != clip_id {
            return;
        }
        let prev = clip.stretch.clone();
        let start = clip.start_beat as f64;
        let dur = clip.duration_beats.max(0.0) as f64;
        if dur <= 0.0 {
            return;
        }
        let bpm = self.timeline.read(cx).state.bpm as f64;
        let playhead = timeline_beat as f64;
        let local_frac = ((playhead - start) / dur).clamp(0.0, 1.0);
        let output_len = (prev.source_len_samples() as f64) * prev.effective_time_ratio(bpm);
        let source_sample = clip_output_local_to_source_sample(
            local_frac * output_len,
            prev.source_start_samples,
            prev.source_end_samples,
            prev.effective_time_ratio(bpm),
            prev.reverse,
        )
        .round() as u64;
        let id = prev.warp_markers.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        let mut next = prev;
        next.warp_markers.push(WarpMarker {
            id,
            source_sample,
            timeline_beat: playhead,
            locked: false,
        });
        next.warp_markers
            .sort_by(|a, b| a.timeline_beat.total_cmp(&b.timeline_beat));
        next.mode = StretchMode::Warp;
        next.dirty = true;
        self.apply_clip_stretch_gesture(clip_id, next, cx);
    }

    fn move_warp_marker(
        &mut self,
        clip_id: &str,
        marker_id: u64,
        timeline_beat: f32,
        clip_start: f32,
        duration_beats: f32,
        cx: &mut Context<Self>,
    ) {
        let clip_id = clip_id.to_string();
        let _ = self.timeline.update(cx, |timeline, cx| {
            let Some(mut stretch) = timeline.state.clip_stretch(&clip_id).cloned() else {
                return;
            };
            let Some(index) = stretch
                .warp_markers
                .iter()
                .position(|marker| marker.id == marker_id)
            else {
                return;
            };
            if stretch.warp_markers[index].locked {
                return;
            }
            let min_beat = if index == 0 {
                clip_start as f64
            } else {
                stretch.warp_markers[index - 1].timeline_beat + 1.0e-4
            };
            let max_beat = if index + 1 >= stretch.warp_markers.len() {
                (clip_start + duration_beats) as f64
            } else {
                stretch.warp_markers[index + 1].timeline_beat - 1.0e-4
            };
            if max_beat <= min_beat {
                return;
            }
            stretch.warp_markers[index].timeline_beat =
                (timeline_beat as f64).clamp(min_beat, max_beat);
            stretch.dirty = true;
            if timeline.state.set_clip_stretch(&clip_id, stretch) {
                cx.notify();
            }
        });
    }

    fn apply_clip_gesture(
        &mut self,
        clip_id: &str,
        cx: &mut Context<Self>,
        mutate: impl FnOnce(&mut Timeline) -> bool,
    ) {
        let clip_id = clip_id.to_string();
        let _ = self.timeline.update(cx, |timeline, cx| {
            timeline.begin_inspector_clip_gesture(&clip_id);
            if mutate(timeline) && timeline.commit_inspector_clip_gesture(&clip_id, cx) {
                timeline.mark_media_changed(cx);
            }
            cx.notify();
        });
    }

    fn apply_clip_stretch_gesture(
        &mut self,
        clip_id: &str,
        stretch: AudioClipStretchState,
        cx: &mut Context<Self>,
    ) {
        let clip_id = clip_id.to_string();
        let _ = self.timeline.update(cx, |timeline, cx| {
            timeline.begin_inspector_clip_gesture(&clip_id);
            if timeline.state.set_clip_stretch(&clip_id, stretch)
                && timeline.commit_inspector_clip_gesture(&clip_id, cx)
            {
                timeline.mark_media_changed(cx);
            }
            cx.notify();
        });
    }
}

#[derive(Clone, Copy)]
enum AudioFadeSide {
    In,
    Out,
}

impl Render for AudioEditorHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_id =
            selected_audio_clip(&self.timeline.read(cx).state).map(|(_, clip)| clip.id.clone());

        let selection_changed = selected_id != self.last_editing_clip;
        if selection_changed {
            // The selection observer can repaint before the old clip emits a
            // PointerUp. Treat that switch as a gesture boundary and roll back
            // the live preview instead of leaving it outside Project State.
            self.cancel_active_clip_gesture(cx);
            self.state.drag = AudioEditorDrag::None;
        }
        if selected_id.is_some() {
            self.active_clip_id = selected_id.clone();
        }
        if selection_changed {
            self.last_editing_clip = selected_id;
            if self.last_editing_clip.is_none() {
                self.active_clip_id = None;
                self.state.fitted_clip_id = None;
                self.state.drag = AudioEditorDrag::None;
            }
        }

        let theme = audio_editor_theme();
        let body: gpui::AnyElement = match self.build_view_model(cx) {
            Some(vm) => {
                self.ensure_spectrogram(&vm, cx);
                self.state.reset_for_clip_change(Some(&vm.clip_id));
                self.state
                    .fit_clip(&vm.clip_id, vm.duration_beats, self.viewport_width.get());
                let host = cx.entity().clone();
                let on_event: Arc<dyn Fn(AudioEditorEvent, &mut Window, &mut gpui::App)> =
                    Arc::new(move |event, window, app| {
                        let _ = host.update(app, |this, cx| this.handle_event(event, window, cx));
                    });
                let callbacks = AudioEditorCallbacks {
                    on_event: Some(on_event),
                };
                audio_editor_panel(
                    &vm,
                    &self.state,
                    self.viewport_width.get(),
                    self.viewport_height.get(),
                    &callbacks,
                )
                .into_any_element()
            }
            None => empty_audio_editor(&theme).into_any_element(),
        };

        let viewport_width = self.viewport_width.clone();
        let viewport_height = self.viewport_height.clone();
        let viewport_origin_x = self.viewport_origin_x.clone();
        let viewport_origin_y = self.viewport_origin_y.clone();
        div()
            .key_context("AudioEditor")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_col()
            .size_full()
            .bg(Colors::surface_base())
            .on_scroll_wheel(cx.listener(Self::on_wheel))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .size_full()
                    .on_children_prepainted(move |bounds, _window, cx| {
                        let Some(bounds) = bounds.first() else {
                            return;
                        };
                        let origin_x: f32 = bounds.origin.x.into();
                        let origin_y: f32 = bounds.origin.y.into();
                        let width: f32 = bounds.size.width.into();
                        let height: f32 = bounds.size.height.into();
                        let viewport = (width - INSPECTOR_W - TOOLS_W).max(320.0);
                        let visual_height = (height - 56.0).max(180.0);
                        if (viewport_width.get() - viewport).abs() > 0.5
                            || (viewport_height.get() - visual_height).abs() > 0.5
                            || (viewport_origin_x.get() - origin_x - INSPECTOR_W).abs() > 0.5
                            || (viewport_origin_y.get() - origin_y - 56.0).abs() > 0.5
                        {
                            viewport_width.set(viewport);
                            viewport_height.set(visual_height);
                            viewport_origin_x.set(origin_x + INSPECTOR_W);
                            viewport_origin_y.set(origin_y + 56.0);
                            cx.refresh_windows();
                        }
                    })
                    .child(body),
            )
    }
}

impl AudioEditorHost {
    fn on_wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let anchor_x = self.local_x(event.position.x.into());
        default_wheel_handler_at(event, &mut self.state, anchor_x);
        if let Some(vm) = self.build_view_model(cx) {
            self.state
                .viewport
                .clamp_scroll(vm.duration_beats, self.viewport_width.get());
        }
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }
}

fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.000001 {
        -60.0
    } else {
        (20.0 * gain.log10()).clamp(-60.0, 12.0)
    }
}

fn db_to_gain(db: f32) -> f32 {
    10.0_f32.powf(db.clamp(-60.0, 12.0) / 20.0)
}

fn selection_labels(
    selection: Option<(f32, f32)>,
    seconds_per_beat: f32,
) -> (String, String, String) {
    let Some((start, end)) = selection else {
        return ("—".into(), "—".into(), "—".into());
    };
    let start_seconds = start.min(end) * seconds_per_beat;
    let end_seconds = start.max(end) * seconds_per_beat;
    (
        format_time(start_seconds),
        format_time(end_seconds),
        format_time(end_seconds - start_seconds),
    )
}

fn format_time(seconds: f32) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "—".into();
    }
    let minutes = (seconds / 60.0).floor() as u32;
    let secs = seconds - minutes as f32 * 60.0;
    format!("{minutes:02}:{secs:05.2}")
}

fn gain_db_for_local_y(local_y: f32, view_h: f32) -> f32 {
    let t = (1.0 - (local_y - 10.0) / (view_h - 20.0).max(1.0)).clamp(0.0, 1.0);
    -60.0 + t * 72.0
}

fn envelope_y_for_host(value_db: f32, view_h: f32) -> f32 {
    let t = ((value_db.clamp(-60.0, 12.0) + 60.0) / 72.0).clamp(0.0, 1.0);
    view_h - 10.0 - t * (view_h - 20.0).max(1.0)
}

fn peak_label_for_asset(asset_key: &str, clip: &ClipState) -> Option<String> {
    let meta = waveform_cache::get_file_meta(asset_key)?;
    let peak =
        waveform_cache::aggregate_peak_range(asset_key, meta.primary_spp, 0, meta.peak_count);
    let peak = peak.max.abs().max(0.000001);
    Some(format!(
        "{:+.1} dB",
        20.0 * peak.log10() + gain_to_db(clip.gain)
    ))
}
