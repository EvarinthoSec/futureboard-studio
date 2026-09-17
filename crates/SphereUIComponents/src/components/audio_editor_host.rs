//! GPUI host for the audio clip editor.
//!
//! This is the adapter boundary: `sphere-audio-editor` renders semantic UI and
//! this host translates gestures into the existing Timeline command/history,
//! media, and transport paths.

use std::{cell::Cell, rc::Rc, sync::Arc};

use gpui::{
    div, Context, Entity, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Render, ScrollWheelEvent, Styled, Window,
};
use sphere_audio_editor::{
    audio_editor_panel, default_wheel_handler_at, empty_audio_editor, AudioEditorCallbacks,
    AudioEditorDrag, AudioEditorEvent, AudioEditorSnap, AudioEditorState, AudioEditorTool,
    AudioEditorViewModel, EnvelopeCurve, EnvelopePoint, FrequencyScale, SpectralSelection,
};

use crate::components::audio_editor_adapter::{
    audio_editor_theme, build_waveform_view_model, selected_audio_clip,
};
use crate::components::audio_editor_spectrogram::{
    cached_or_analyze, error_view_model, loading_view_model, to_view_model, RenderedSpectrogram,
};
use crate::components::timeline::timeline::Timeline;
use crate::components::timeline::timeline_state::{
    AudioClipStretchState, ClipEdge, ClipState, ClipType, StretchAlgorithm, StretchMode,
};
use crate::components::timeline::waveform_cache;
use crate::theme::Colors;

const INSPECTOR_W: f32 = 174.0;

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
}

impl AudioEditorHost {
    fn ensure_spectrogram(&mut self, vm: &AudioEditorViewModel, cx: &mut Context<Self>) {
        let Some(path) = vm.source_path.clone() else {
            self.spectrogram_key = None;
            self.spectrogram_result = None;
            return;
        };
        let key = format!("{path}|{:?}", self.state.frequency_scale);
        if self.spectrogram_key.as_deref() == Some(key.as_str()) {
            return;
        }

        self.spectrogram_key = Some(key);
        self.spectrogram_result = None;
        self.spectrogram_generation = self.spectrogram_generation.wrapping_add(1);
        let generation = self.spectrogram_generation;
        let frequency_scale = self.state.frequency_scale;
        let host = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { cached_or_analyze(&path, frequency_scale) })
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
            "w" => Some(AudioEditorTool::Warp),
            "t" => Some(AudioEditorTool::Transient),
            "d" | "e" => Some(AudioEditorTool::Draw),
            _ => None,
        };

        match (key, tool) {
            (_, Some(tool)) => {
                self.state.active_tool = tool;
                self.state.open_dropdown = None;
            }
            ("escape", None) => {
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
        }
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
            start_beat: clip.start_beat,
            duration_beats: clip.duration_beats,
            offset_beats: clip.offset_beats,
            beats_per_bar: tl.state.beats_per_bar(),
            bpm: tl.state.bpm,
            track_color: track.color,
            waveform: build_waveform_view_model(clip, &tl.state, ppb, scroll_x, viewport_w),
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
        if self.state.snap == AudioEditorSnap::Grid {
            self.timeline
                .read(cx)
                .state
                .snap_beats_with_bypass(beat, bypass)
        } else {
            beat
        }
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
            AudioEditorTool::Pointer => {
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
                } else {
                    self.seek_relative(vm.start_beat + rel_beat, cx);
                }
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
            AudioEditorTool::Warp | AudioEditorTool::Transient => {
                // The toolbar exposes these future tools so the state machine
                // and command boundary are ready, but no misleading edit is
                // emitted until their analysis/command backends exist.
                self.seek_relative(vm.start_beat + rel_beat, cx);
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
                self.state.selection_range = Some((anchor_beat, rel_beat));
                cx.notify();
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
            AudioEditorDrag::MovingWarpMarker { .. } => {}
        }
    }

    fn finish_pointer(&mut self, _x: f32, _y: f32, _shift: bool, cx: &mut Context<Self>) {
        let drag = std::mem::take(&mut self.state.drag);
        match drag {
            AudioEditorDrag::SelectingRange { .. } => {
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
                let clip_id = self.active_clip_id.clone();
                if let Some(clip_id) = clip_id {
                    let _ = self.timeline.update(cx, |timeline, cx| {
                        if timeline.commit_inspector_clip_gesture(&clip_id, cx) {
                            timeline.mark_media_changed(cx);
                        }
                        cx.notify();
                    });
                }
            }
            AudioEditorDrag::MovingEnvelopePoint { .. } => {
                let clip_id = self.active_clip_id.clone();
                if let Some(clip_id) = clip_id {
                    let _ = self.timeline.update(cx, |timeline, cx| {
                        if timeline.commit_inspector_clip_gesture(&clip_id, cx) {
                            timeline.mark_media_changed(cx);
                        }
                        cx.notify();
                    });
                }
            }
            AudioEditorDrag::None | AudioEditorDrag::MovingWarpMarker { .. } => {}
        }
    }

    fn begin_clip_gesture(&mut self, clip_id: &str, cx: &mut Context<Self>) {
        self.active_clip_id = Some(clip_id.to_string());
        let _ = self.timeline.update(cx, |timeline, _cx| {
            timeline.begin_inspector_clip_gesture(clip_id);
        });
    }

    fn seconds_per_beat(&self, cx: &Context<Self>) -> f32 {
        self.timeline.read(cx).state.seconds_per_beat().max(0.0001)
    }

    fn total_source_frames(&self, vm: &AudioEditorViewModel) -> i64 {
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
        let total_frames = self.total_source_frames(vm);
        if total_frames <= 0 || vm.duration_beats <= f32::EPSILON {
            return 0;
        }
        ((rel_beat / vm.duration_beats).clamp(0.0, 1.0) * total_frames as f32).round() as i64
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
        let selected_id = self
            .timeline
            .read(cx)
            .state
            .selection
            .selected_clip_ids
            .first()
            .cloned()
            .filter(|id| {
                self.timeline
                    .read(cx)
                    .state
                    .find_clip(id)
                    .is_some_and(|(_, c)| matches!(c.clip_type, ClipType::Audio { .. }))
            });

        if selected_id.is_some() {
            self.active_clip_id = selected_id.clone();
        }
        if selected_id != self.last_editing_clip {
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
                        let viewport = (width - INSPECTOR_W).max(320.0);
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
