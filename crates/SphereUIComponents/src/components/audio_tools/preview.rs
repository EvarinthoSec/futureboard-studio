//! Temporary clip preview overrides. Never serialized.

use SphereAudioProcessor::{ChannelTransform, DehumParams};

/// Non-destructive preview applied on top of the project graph.
#[derive(Debug, Clone, PartialEq)]
pub struct ClipPreviewOverride {
    pub clip_id: String,
    pub extra_gain: Option<f32>,
    pub channel: Option<ChannelTransform>,
    pub dc_remove: Option<bool>,
    pub dc_left: f32,
    pub dc_right: f32,
    pub dehum: Option<DehumParams>,
    pub stretch_ratio: Option<f64>,
    pub pitch_semitones: Option<f32>,
    pub denoise_amount: Option<f32>,
    pub bypass: bool,
}

impl ClipPreviewOverride {
    pub fn identity(clip_id: String) -> Self {
        Self {
            clip_id,
            extra_gain: None,
            channel: None,
            dc_remove: None,
            dc_left: 0.0,
            dc_right: 0.0,
            dehum: None,
            stretch_ratio: None,
            pitch_semitones: None,
            denoise_amount: None,
            bypass: false,
        }
    }
}

pub fn apply_preview_to_clip(
    process: &mut DirectAudio::types::EngineClipAudioProcess,
    preview: &ClipPreviewOverride,
) {
    if preview.bypass {
        process.preview_bypass = true;
        process.extra_gain = 1.0;
        return;
    }
    process.preview_bypass = false;
    if let Some(gain) = preview.extra_gain {
        process.extra_gain = gain.max(0.0);
    }
    if let Some(channel) = preview.channel {
        process.channel_transform = channel.to_tag();
    }
    if let Some(remove) = preview.dc_remove {
        process.dc_remove = remove;
        process.dc_left = preview.dc_left;
        process.dc_right = preview.dc_right;
    }
    if let Some(dehum) = preview.dehum {
        process.dehum_hz = dehum.base_hz;
        process.dehum_harmonics = dehum.harmonics;
        process.dehum_reduction_db = dehum.reduction_db;
    }
    if let Some(amount) = preview.denoise_amount {
        process.denoise_amount = amount.clamp(0.0, 1.0);
    }
    if let Some(ratio) = preview.stretch_ratio {
        process.effective_time_ratio = ratio;
        process.preserve_pitch = true;
    }
    if let Some(semitones) = preview.pitch_semitones {
        process.pitch_semitones = semitones as f64;
        process.pitch_ratio = SphereAudioProcessor::semitone_to_pitch_ratio(semitones, 0.0) as f64;
        process.preserve_pitch = true;
    }
}

pub fn apply_previews_to_snapshot(
    snapshot: &mut DirectAudio::types::EngineProjectSnapshot,
    previews: &std::collections::HashMap<String, ClipPreviewOverride>,
) {
    if previews.is_empty() {
        return;
    }
    for clip in &mut snapshot.clips {
        let Some(preview) = previews.get(&clip.id) else {
            continue;
        };
        if let Some(process) = clip.audio_process.as_mut() {
            apply_preview_to_clip(process, preview);
        }
    }
}
