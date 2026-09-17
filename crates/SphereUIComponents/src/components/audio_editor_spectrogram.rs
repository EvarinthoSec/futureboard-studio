//! Background spectrogram jobs for the native audio editor.
//!
//! File I/O, PCM conversion, STFT and image assembly all happen on GPUI's
//! background executor; the render path only selects already-built tiles for
//! the current viewport.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use gpui::RenderImage;
use image::{Frame, ImageBuffer, Rgba};
use smallvec::SmallVec;
use sphere_audio_editor::{
    analyze_spectrogram, AudioEditorViewMode, FrequencyScale, SpectrogramAnalysis,
    SpectrogramSettings, SpectrogramTileView, SpectrogramViewModel,
};

const MAX_CACHED_FILES: usize = 3;

#[derive(Clone)]
pub struct RenderedSpectrogram {
    pub duration_seconds: f32,
    pub sample_rate: u32,
    pub max_frequency_hz: f32,
    pub tiles: Vec<SpectrogramTileView>,
    pub spectrum_db: Arc<[f32]>,
}

struct SpectrogramCache {
    entries: HashMap<String, Arc<RenderedSpectrogram>>,
    order: VecDeque<String>,
}

static CACHE: OnceLock<Mutex<SpectrogramCache>> = OnceLock::new();

fn cache() -> &'static Mutex<SpectrogramCache> {
    CACHE.get_or_init(|| {
        Mutex::new(SpectrogramCache {
            entries: HashMap::new(),
            order: VecDeque::new(),
        })
    })
}

pub fn cached_or_analyze(
    path: &str,
    frequency_scale: FrequencyScale,
) -> Result<Arc<RenderedSpectrogram>, String> {
    let key = format!("{path}|{frequency_scale:?}");
    if let Ok(mut cache) = cache().lock() {
        if let Some(value) = cache.entries.get(&key).cloned() {
            touch(&mut cache.order, &key);
            return Ok(value);
        }
    }

    let buffer = DirectAudio::load_audio_file(path)?;
    if buffer.channels == 0 || buffer.frames == 0 {
        return Err("audio source contains no frames".to_string());
    }
    let channels = buffer.channels;
    let mono: Vec<f32> = buffer
        .samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / channels as f32)
        .collect();
    let analysis = analyze_spectrogram(
        &mono,
        buffer.sample_rate,
        SpectrogramSettings {
            frequency_scale,
            ..SpectrogramSettings::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let rendered = Arc::new(render_analysis(analysis)?);

    if let Ok(mut cache) = cache().lock() {
        cache.entries.insert(key.clone(), Arc::clone(&rendered));
        touch(&mut cache.order, &key);
        while cache.order.len() > MAX_CACHED_FILES {
            if let Some(old_key) = cache.order.pop_front() {
                cache.entries.remove(&old_key);
            }
        }
    }
    Ok(rendered)
}

fn touch(order: &mut VecDeque<String>, key: &str) {
    if let Some(position) = order.iter().position(|item| item == key) {
        order.remove(position);
    }
    order.push_back(key.to_string());
}

fn render_analysis(analysis: SpectrogramAnalysis) -> Result<RenderedSpectrogram, String> {
    let mut tiles = Vec::with_capacity(analysis.tiles.len());
    for tile in analysis.tiles {
        let image =
            ImageBuffer::<Rgba<u8>, Vec<u8>>::from_raw(tile.width, tile.height, tile.rgba.to_vec())
                .ok_or_else(|| "invalid spectrogram tile dimensions".to_string())?;
        let image = Arc::new(RenderImage::new(SmallVec::from_elem(Frame::new(image), 1)));
        tiles.push(SpectrogramTileView {
            start_seconds: tile.start_seconds,
            duration_seconds: tile.duration_seconds,
            image,
        });
    }
    Ok(RenderedSpectrogram {
        duration_seconds: analysis.duration_seconds,
        sample_rate: analysis.sample_rate,
        max_frequency_hz: analysis.max_frequency_hz,
        tiles,
        spectrum_db: analysis.spectrum_db,
    })
}

pub fn loading_view_model(label: impl Into<String>) -> SpectrogramViewModel {
    SpectrogramViewModel {
        ready: false,
        status_label: label.into(),
        is_error: false,
        ..SpectrogramViewModel::default()
    }
}

pub fn error_view_model(message: impl Into<String>) -> SpectrogramViewModel {
    SpectrogramViewModel {
        ready: false,
        status_label: message.into(),
        is_error: true,
        ..SpectrogramViewModel::default()
    }
}

pub fn to_view_model(
    rendered: &RenderedSpectrogram,
    mode: AudioEditorViewMode,
) -> SpectrogramViewModel {
    let wants_spectrum = mode == AudioEditorViewMode::Spectrum;
    SpectrogramViewModel {
        ready: true,
        status_label: String::new(),
        is_error: false,
        tiles: if wants_spectrum {
            Vec::new()
        } else {
            rendered.tiles.clone()
        },
        spectrum_db: Arc::clone(&rendered.spectrum_db),
        sample_rate: rendered.sample_rate,
        max_frequency_hz: rendered.max_frequency_hz,
        duration_seconds: rendered.duration_seconds,
    }
}
