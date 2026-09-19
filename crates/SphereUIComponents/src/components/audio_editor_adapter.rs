//! Builds audio editor view models from timeline + peak cache (no PCM).

use sphere_audio_editor::{WaveformColumn, WaveformSample, WaveformViewModel};

use crate::components::timeline::timeline_state::{
    clip_output_local_to_source_sample, AudioImportState, ClipState, ClipType, TimelineState,
};
use crate::components::timeline::waveform_cache::{self, WaveformDisplayStatus, WaveformPeak};
use crate::components::timeline::waveform_canvas::resolve_waveform_source_range;
use crate::components::timeline::waveform_samples;
use crate::theme::Colors;

const MAX_EDITOR_COLUMNS: usize = 2048;

pub fn import_status_label(import: &AudioImportState) -> (String, bool, bool) {
    match import {
        AudioImportState::Pending => ("Queued".to_string(), false, true),
        AudioImportState::Probing => ("Probing metadata…".to_string(), false, true),
        AudioImportState::Decoding { .. } => ("Preparing waveform…".to_string(), false, true),
        AudioImportState::GeneratingPeaks { progress } => {
            let pct = ((*progress * 100.0) as u32).min(100);
            (format!("Building waveform… {pct}%"), false, true)
        }
        AudioImportState::Ready => ("Ready".to_string(), false, false),
        AudioImportState::Failed { message } => (message.clone(), true, false),
    }
}

pub fn build_waveform_view_model(
    clip: &ClipState,
    state: &TimelineState,
    pixels_per_beat: f32,
    scroll_x: f32,
    viewport_width: f32,
    prefer_samples: bool,
) -> WaveformViewModel {
    let (label, is_error, show_progress) = import_status_label(&clip.audio_import);

    let ClipType::Audio {
        source_path: Some(source_path),
        ..
    } = &clip.clip_type
    else {
        return WaveformViewModel::loading("No source file");
    };
    // The source path is mutable (project copy/relink), while file_id is the
    // stable asset identity used by the import worker and disk cache.
    let Some(asset_key) = clip.audio_asset_key() else {
        return WaveformViewModel::loading("No source asset");
    };

    waveform_cache::with_file_entry(asset_key, |entry| {
        let Some(entry) = entry else {
            return WaveformViewModel {
                columns: Vec::new(),
                samples: Vec::new(),
                ready: false,
                status_label: label.clone(),
                is_error,
                show_progress,
            };
        };

        match waveform_cache::display_status_from_entry(entry) {
            WaveformDisplayStatus::Ready { meta } | WaveformDisplayStatus::Partial { meta, .. } => {
                let columns = build_peak_columns(
                    asset_key,
                    source_path,
                    entry,
                    meta.as_ref(),
                    clip,
                    state,
                    pixels_per_beat,
                    scroll_x,
                    viewport_width,
                );
                let samples = build_sample_points(
                    asset_key,
                    source_path,
                    meta.as_ref(),
                    clip,
                    state,
                    pixels_per_beat,
                    scroll_x,
                    viewport_width,
                    prefer_samples,
                );
                let ready = !columns.is_empty() || !samples.is_empty();
                WaveformViewModel {
                    ready,
                    columns,
                    samples,
                    status_label: if ready { String::new() } else { label },
                    is_error: false,
                    show_progress: !ready && show_progress,
                }
            }
            WaveformDisplayStatus::Pending => WaveformViewModel {
                columns: Vec::new(),
                samples: Vec::new(),
                ready: false,
                status_label: label,
                is_error: false,
                show_progress: true,
            },
            WaveformDisplayStatus::Error(message) => WaveformViewModel::error(message),
        }
    })
}

fn build_peak_columns(
    asset_key: &str,
    source_path: &str,
    entry: &waveform_cache::FileEntry,
    meta: &waveform_cache::WaveformFileMeta,
    clip: &ClipState,
    state: &TimelineState,
    pixels_per_beat: f32,
    scroll_x: f32,
    viewport_width: f32,
) -> Vec<WaveformColumn> {
    let clip_width_px = (clip.duration_beats * pixels_per_beat).max(1.0);
    let visible_start = scroll_x.max(0.0);
    let visible_end = (scroll_x + viewport_width).min(clip_width_px);
    let visible_w = (visible_end - visible_start).max(1.0);
    let num_cols = (visible_w.ceil() as usize).clamp(16, MAX_EDITOR_COLUMNS);

    let seconds_per_beat = state.seconds_per_beat();
    let (source_start, source_end, effective_time_ratio) =
        resolve_waveform_source_range(clip, meta.total_frames, meta.sample_rate, state.bpm as f64);
    let output_len = SphereAudioProcessor::stretched_duration_samples(
        source_end.saturating_sub(source_start),
        &clip.stretch.to_sphere_stretch_params(state.bpm as f64),
        Some(state.bpm),
    )
    .max(1) as f64;

    let pixels_per_second = pixels_per_beat / seconds_per_beat.max(1e-6);
    let desired_spp = match crate::components::timeline::waveform_detail::detail_level_for_zoom(
        pixels_per_second * effective_time_ratio.max(1.0e-6) as f32,
        meta.sample_rate,
    ) {
        Some(detail_spp) => {
            // The Audio Editor is a second consumer of the same bounded,
            // on-demand detail cache as the arrangement. Without this request
            // path the editor stayed on the 256-frame LOD even when the user
            // zoomed in, which made transients look like soft stair-steps.
            crate::components::timeline::waveform_detail::note_needed(
                asset_key,
                source_path,
                detail_spp,
                source_start,
                source_end,
                meta.total_frames,
            );
            detail_spp
        }
        None => waveform_cache::pick_best_samples_per_peak(pixels_per_second, meta.sample_rate),
    };
    let spp = waveform_cache::best_available_samples_per_peak_in_entry(entry, desired_spp);

    (0..num_cols)
        .filter_map(|col| {
            let x0 = visible_start + (col as f32 / num_cols as f32) * visible_w;
            let x1 = visible_start + ((col + 1) as f32 / num_cols as f32) * visible_w;
            let frac0 = (x0 / clip_width_px).clamp(0.0, 1.0) as f64;
            let frac1 = (x1 / clip_width_px).clamp(0.0, 1.0) as f64;
            let out0 = frac0 * output_len;
            let out1 = frac1 * output_len;
            let s0 = clip_output_local_to_source_sample(
                out0,
                source_start,
                source_end,
                effective_time_ratio,
                clip.stretch.reverse,
            );
            let s1 = clip_output_local_to_source_sample(
                out1,
                source_start,
                source_end,
                effective_time_ratio,
                clip.stretch.reverse,
            );
            let p0 = sample_to_peak_index(s0.min(s1), spp);
            let p1 = sample_to_peak_index(s0.max(s1), spp).max(p0);
            let WaveformPeak { min, max } =
                waveform_cache::aggregate_peak_range_in_entry(entry, spp, p0, p1 + 1);
            if min == 0.0 && max == 0.0 {
                return None;
            }
            let gain = clip.gain.max(0.0);
            Some(WaveformColumn {
                x: x0 - scroll_x,
                min: min * gain,
                max: max * gain,
            })
        })
        .collect()
}

fn sample_to_peak_index(sample: f64, samples_per_peak: usize) -> usize {
    sample.max(0.0) as usize / samples_per_peak.max(1)
}

fn build_sample_points(
    asset_key: &str,
    source_path: &str,
    meta: &waveform_cache::WaveformFileMeta,
    clip: &ClipState,
    state: &TimelineState,
    pixels_per_beat: f32,
    scroll_x: f32,
    viewport_width: f32,
    prefer_samples: bool,
) -> Vec<WaveformSample> {
    let seconds_per_beat = state.seconds_per_beat().max(1e-6);
    let pixels_per_second = pixels_per_beat / seconds_per_beat;
    let effective_time_ratio = clip.stretch.effective_time_ratio(state.bpm as f64);
    let zoom_pps = pixels_per_second * effective_time_ratio.max(1.0e-6) as f32;
    if !prefer_samples && !waveform_samples::sample_view_needed(zoom_pps, meta.sample_rate) {
        return Vec::new();
    }

    let clip_width_px = (clip.duration_beats * pixels_per_beat).max(1.0);
    let visible_start = scroll_x.max(0.0);
    let visible_end = (scroll_x + viewport_width).min(clip_width_px);
    if visible_end <= visible_start {
        return Vec::new();
    }

    let (source_start, source_end, effective_time_ratio) =
        resolve_waveform_source_range(clip, meta.total_frames, meta.sample_rate, state.bpm as f64);
    let output_len = SphereAudioProcessor::stretched_duration_samples(
        source_end.saturating_sub(source_start),
        &clip.stretch.to_sphere_stretch_params(state.bpm as f64),
        Some(state.bpm),
    )
    .max(1) as f64;

    let frac0 = (visible_start / clip_width_px).clamp(0.0, 1.0) as f64;
    let frac1 = (visible_end / clip_width_px).clamp(0.0, 1.0) as f64;
    let s0 = clip_output_local_to_source_sample(
        frac0 * output_len,
        source_start,
        source_end,
        effective_time_ratio,
        clip.stretch.reverse,
    )
    .floor()
    .max(0.0) as u64;
    let s1 = clip_output_local_to_source_sample(
        frac1 * output_len,
        source_start,
        source_end,
        effective_time_ratio,
        clip.stretch.reverse,
    )
    .ceil()
    .max(0.0) as u64;
    let first = s0.min(s1);
    let last = s0.max(s1).max(first + 1);
    if last - first > waveform_samples::SAMPLE_VIEW_MAX_FRAMES_PER_PIXEL as u64 * 4096 {
        return Vec::new();
    }

    waveform_samples::note_needed(asset_key, source_path, first, last, meta.total_frames);
    let Some(window) = waveform_samples::get_window(asset_key, first, last) else {
        return Vec::new();
    };

    let gain = clip.gain.max(0.0);
    let mut points = Vec::new();
    for (offset, value) in window.mono.iter().copied().enumerate() {
        let frame = window.start_frame + offset as u64;
        if frame < first || frame >= last {
            continue;
        }
        let source_from_start = if clip.stretch.reverse {
            source_end.saturating_sub(frame).saturating_sub(1)
        } else {
            frame.saturating_sub(source_start)
        } as f64;
        let out = source_from_start * effective_time_ratio.max(1e-6);
        let frac = (out / output_len).clamp(0.0, 1.0) as f32;
        let x = frac * clip_width_px - scroll_x;
        if x < -2.0 || x > viewport_width + 2.0 {
            continue;
        }
        points.push(WaveformSample {
            x,
            value: value * gain,
            frame: frame as i64,
        });
        if points.len() >= 4096 {
            break;
        }
    }
    points
}

pub fn audio_editor_theme() -> sphere_audio_editor::AudioEditorTheme {
    sphere_audio_editor::AudioEditorTheme {
        surface_base: Colors::surface_base(),
        surface_panel: Colors::surface_panel(),
        text_primary: Colors::text_primary(),
        text_secondary: Colors::text_secondary(),
        text_muted: Colors::text_muted(),
        border_subtle: Colors::border_subtle(),
        accent: Colors::accent_primary(),
        playhead: Colors::timeline_playhead(),
        error: Colors::status_error(),
        selection: Colors::accent_primary(),
    }
}

pub fn selected_audio_clip<'a>(
    state: &'a TimelineState,
) -> Option<(
    &'a crate::components::timeline::timeline_state::TrackState,
    &'a ClipState,
)> {
    // Multi-selection is ordered by the timeline. The editor should resolve
    // the first *audio* target, not blindly inspect only index 0: selecting a
    // MIDI clip together with an audio clip must still leave Audio Editor on a
    // real editable audio target.
    state
        .selection
        .selected_clip_ids
        .iter()
        .filter_map(|clip_id| state.find_clip(clip_id))
        .find(|(_, clip)| matches!(clip.clip_type, ClipType::Audio { .. }))
}

pub fn clip_type_hint_for_selection(
    state: &TimelineState,
) -> Option<sphere_audio_editor::ClipTypeHint> {
    let clip_id = state.selection.selected_clip_ids.first()?;
    let (_, clip) = state.find_clip(clip_id)?;
    match clip.clip_type {
        ClipType::Audio { .. } => Some(sphere_audio_editor::ClipTypeHint::Audio),
        ClipType::Midi { .. } => Some(sphere_audio_editor::ClipTypeHint::Midi),
        // The audio editor has nothing to show for a reference video clip.
        ClipType::Video { .. } => None,
    }
}
