//! On-demand decoded PCM windows for sample-accurate editor views.
//!
//! Peak LODs stop being useful once a pixel covers fewer than about eight
//! source frames. This module decodes only the visible window on a background
//! thread, then hands the host a bounded mono/stereo slice for drawing and
//! zero-crossing snap. Nothing here runs on the UI paint path except a mutex
//! lookup, and nothing runs on the audio thread.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};

use DirectAudio::{open_clip_audio_source, read_frame_stereo};

/// Frames-per-pixel at which min/max peaks stop being the useful drawing.
pub const SAMPLE_VIEW_MAX_FRAMES_PER_PIXEL: f32 = 8.0;

const ALIGN_FRAMES: u64 = 256;
const MAX_WINDOW_FRAMES: usize = 8192;
const MAX_RESIDENT: usize = 24;
const REQUEST_QUEUE_DEPTH: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct WindowKey {
    cache_key: String,
    start_frame: u64,
    frame_count: u32,
}

struct SampleWindow {
    start_frame: u64,
    mono: Arc<[f32]>,
    left: Arc<[f32]>,
    right: Arc<[f32]>,
}

#[derive(Clone)]
pub struct SampleWindowView {
    pub start_frame: u64,
    pub mono: Arc<[f32]>,
    pub left: Arc<[f32]>,
    pub right: Arc<[f32]>,
}

struct Registry {
    known: HashMap<WindowKey, Option<SampleWindow>>,
    order: VecDeque<WindowKey>,
}

fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        Mutex::new(Registry {
            known: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

struct SampleRequest {
    key: WindowKey,
    path: String,
}

fn sender() -> &'static SyncSender<SampleRequest> {
    static SENDER: OnceLock<SyncSender<SampleRequest>> = OnceLock::new();
    SENDER.get_or_init(|| {
        let (tx, rx) = sync_channel::<SampleRequest>(REQUEST_QUEUE_DEPTH);
        std::thread::Builder::new()
            .name("waveform-samples".to_string())
            .spawn(move || {
                let mut open: Option<(String, DirectAudio::ClipAudioSource)> = None;
                while let Ok(request) = rx.recv() {
                    if open
                        .as_ref()
                        .map(|(p, _)| p != &request.path)
                        .unwrap_or(true)
                    {
                        open = open_clip_audio_source(&request.path)
                            .ok()
                            .map(|source| (request.path.clone(), source));
                    }
                    let Some((_, source)) = open.as_ref() else {
                        forget(&request.key);
                        continue;
                    };
                    match decode_window(source, request.key.start_frame, request.key.frame_count) {
                        Some(window) => install(request.key, window),
                        None => forget(&request.key),
                    }
                }
            })
            .ok();
        tx
    })
}

fn decode_window(
    source: &DirectAudio::ClipAudioSource,
    start_frame: u64,
    frame_count: u32,
) -> Option<SampleWindow> {
    let total = source.frames() as u64;
    if start_frame >= total || frame_count == 0 {
        return None;
    }
    let count = (frame_count as u64)
        .min(MAX_WINDOW_FRAMES as u64)
        .min(total - start_frame) as usize;
    let mut mono = Vec::with_capacity(count);
    let mut left = Vec::with_capacity(count);
    let mut right = Vec::with_capacity(count);
    for frame in 0..count {
        let (l, r) = read_frame_stereo(source, start_frame as usize + frame);
        let l = if l.is_finite() {
            l.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let r = if r.is_finite() {
            r.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        left.push(l);
        right.push(r);
        mono.push((l + r) * 0.5);
    }
    Some(SampleWindow {
        start_frame,
        mono: mono.into(),
        left: left.into(),
        right: right.into(),
    })
}

fn install(key: WindowKey, window: SampleWindow) {
    let evicted = {
        let Ok(mut reg) = registry().lock() else {
            return;
        };
        reg.known.insert(key.clone(), Some(window));
        reg.order.push_back(key);
        let mut evicted = Vec::new();
        while reg.order.len() > MAX_RESIDENT {
            if let Some(old) = reg.order.pop_front() {
                reg.known.remove(&old);
                evicted.push(old);
            }
        }
        evicted
    };
    let _ = evicted;
}

fn forget(key: &WindowKey) {
    if let Ok(mut reg) = registry().lock() {
        reg.known.remove(key);
        reg.order.retain(|item| item != key);
    }
}

/// Whether the current zoom is past the peak ladder and should draw samples.
pub fn sample_view_needed(pixels_per_second: f32, sample_rate: u32) -> bool {
    if sample_rate == 0 {
        return false;
    }
    let frames_per_pixel = sample_rate as f32 / pixels_per_second.max(1.0);
    frames_per_pixel <= SAMPLE_VIEW_MAX_FRAMES_PER_PIXEL
}

/// Drop cached PCM for an asset after the source is rewritten.
pub fn forget_asset(cache_key: &str) {
    let Ok(mut reg) = registry().lock() else {
        return;
    };
    reg.known.retain(|key, _| key.cache_key != cache_key);
    reg.order.retain(|key| key.cache_key != cache_key);
}

/// Ask for the PCM covering source frames `[first_frame, last_frame)`.
///
/// Render-path safe: lock + bounded send, no I/O.
pub fn note_needed(
    cache_key: &str,
    source_path: &str,
    first_frame: u64,
    last_frame: u64,
    total_frames: u64,
) {
    if source_path.trim().is_empty() || total_frames == 0 || last_frame <= first_frame {
        return;
    }
    let aligned = (first_frame / ALIGN_FRAMES) * ALIGN_FRAMES;
    let end = last_frame.min(total_frames);
    let count = (end.saturating_sub(aligned) as usize).clamp(1, MAX_WINDOW_FRAMES) as u32;
    let key = WindowKey {
        cache_key: cache_key.to_string(),
        start_frame: aligned,
        frame_count: count,
    };
    {
        let Ok(mut reg) = registry().lock() else {
            return;
        };
        if reg.known.contains_key(&key) {
            return;
        }
        reg.known.insert(key.clone(), None);
    }
    if sender()
        .try_send(SampleRequest {
            key: key.clone(),
            path: source_path.to_string(),
        })
        .is_err()
    {
        forget(&key);
    }
}

pub fn get_window(cache_key: &str, first_frame: u64, last_frame: u64) -> Option<SampleWindowView> {
    let aligned = (first_frame / ALIGN_FRAMES) * ALIGN_FRAMES;
    let Ok(reg) = registry().lock() else {
        return None;
    };
    reg.known.iter().find_map(|(key, window)| {
        if key.cache_key != cache_key {
            return None;
        }
        let window = window.as_ref()?;
        let end = window.start_frame + window.mono.len() as u64;
        if window.start_frame <= aligned && end >= last_frame.min(aligned + 1) {
            Some(SampleWindowView {
                start_frame: window.start_frame,
                mono: Arc::clone(&window.mono),
                left: Arc::clone(&window.left),
                right: Arc::clone(&window.right),
            })
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_view_engages_past_the_peak_ladder() {
        assert!(!sample_view_needed(100.0, 48_000));
        assert!(sample_view_needed(8_000.0, 48_000));
        assert!(!sample_view_needed(10_000.0, 0));
    }
}
