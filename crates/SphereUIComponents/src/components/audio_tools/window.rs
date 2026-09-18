//! Independent GPUI windows for audio editor analysis and processing tools.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use SphereAudioProcessor::{
    AudioClipProcessor, ChannelTransform, DcOffsetProcessor, DeclickParams, DehumParams,
    DehumProcessor, FftSize, FrequencyFocus, KeyEstimate, LoudnessMeasurement,
    NormalizeMeasurement, NormalizeMode, NormalizeParams, SpectralDenoiseParams,
    SpectralGainParams, SpectrumMode, SpectrumSmoothing, SpectrumSnapshot, SpectrumWindow,
    StftSettings, StretchAlgorithm, StretchMode, StretchParams, TempoCandidate,
    TransientDetectParams, TransientMarker, analyze_loudness, analyze_spectrum,
    apply_channel_transform_interleaved, apply_gain_interleaved, apply_spectral_gain, db_to_lin,
    declick_interleaved, detect_transients, downmix_interleaved, estimate_bpm_candidates,
    estimate_key_ranked, learn_noise_profile, measure_dc_offset, measure_normalize, measure_phase,
    reduce_noise_stft, render_stretch_interleaved, replace_frame_range, resample_interleaved,
    semitone_to_pitch_ratio, slice_frames, write_wav_f32,
};
use gpui::prelude::FluentBuilder;
use gpui::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Pixels, Render, Styled,
    Window, WindowBackgroundAppearance, WindowBounds, WindowHandle, WindowKind, div, px, size,
};
use sphere_audio_editor::{AudioRepairModule, AudioToolKind, AudioToolSession, AudioToolTarget};

use crate::components::controls::{FbButtonKind, fb_button, fb_checkbox};
use crate::components::timeline::Timeline;
use crate::components::title_bar::external_window_titlebar;
use crate::theme::{Colors, radius, space, typography};
use crate::window_position::{apply_owner_display, centered_window_bounds};

use super::preview::ClipPreviewOverride;

pub const AUDIO_TOOL_WINDOW_MIN_WIDTH: f32 = 360.0;
pub const AUDIO_TOOL_WINDOW_MIN_HEIGHT: f32 = 240.0;

const REFRESH: Duration = Duration::from_millis(50);

pub enum AudioToolCommand {
    Preview(ClipPreviewOverride),
    ClearPreview(String),
    MutateClip {
        clip_id: String,
        label: &'static str,
        mutate: Box<dyn FnOnce(&mut crate::components::timeline::timeline_state::ClipState) + Send>,
    },
    AddMarkers {
        beats: Vec<f64>,
        label: &'static str,
    },
    AddWarpMarkers {
        clip_id: String,
        frames: Vec<u64>,
    },
    SliceClip {
        clip_id: String,
        beats: Vec<f32>,
    },
    UseOriginalBpm {
        clip_id: String,
        bpm: f64,
    },
    AddTempoPoint {
        beat: f64,
        bpm: f64,
    },
    ReplaceSource {
        clip_id: String,
        path: PathBuf,
        sample_rate: u32,
    },
}

#[derive(Clone)]
pub struct AudioToolWindowCallbacks {
    pub on_command: Arc<dyn Fn(AudioToolCommand, &mut App) + Send + Sync>,
    pub on_close: Arc<dyn Fn(AudioToolKind, Bounds<Pixels>, &mut App) + Send + Sync>,
}

pub struct AudioToolWindowManager {
    pub windows: HashMap<AudioToolKind, WindowHandle<AudioToolWindow>>,
    pub last_bounds: HashMap<AudioToolKind, Bounds<Pixels>>,
    pub previews: HashMap<String, ClipPreviewOverride>,
}

impl Default for AudioToolWindowManager {
    fn default() -> Self {
        Self {
            windows: HashMap::new(),
            last_bounds: HashMap::new(),
            previews: HashMap::new(),
        }
    }
}

impl AudioToolWindowManager {
    pub fn prune(&mut self, cx: &mut App) {
        self.windows
            .retain(|_, handle| handle.update(cx, |_, _, _| {}).is_ok());
    }

    pub fn follow_selection(&mut self, target: &AudioToolTarget, cx: &mut App) {
        for handle in self.windows.values() {
            let _ = handle.update(cx, |window, _win, cx| {
                if window.session.follow_selection && !window.session.pin_target {
                    window.session.target = target.clone();
                    DirectAudio::analysis_tap().set_target_clip(Some(&target.clip_id));
                    cx.notify();
                }
            });
        }
    }

    pub fn close_all(&mut self, cx: &mut App) {
        for handle in self.windows.values() {
            let _ = handle.update(cx, |_this, window, _cx| {
                window.remove_window();
            });
        }
        self.windows.clear();
        self.previews.clear();
        DirectAudio::analysis_tap().set_target_clip(None);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum TimePitchMode {
    Off,
    Stretch,
    FitDuration,
    FollowTempo,
}

pub struct AudioToolWindow {
    pub(crate) session: AudioToolSession,
    timeline: Entity<Timeline>,
    callbacks: AudioToolWindowCallbacks,
    status: String,
    // Spectrum
    spectrum_mode: SpectrumMode,
    fft_size: FftSize,
    spectrum_window: SpectrumWindow,
    smoothing: SpectrumSmoothing,
    peak_hold: bool,
    spectrum: Option<SpectrumSnapshot>,
    // Loudness / normalize / dc / bpm / key / transients
    loudness: Option<LoudnessMeasurement>,
    normalize: NormalizeParams,
    measurement: Option<NormalizeMeasurement>,
    dc: SphereAudioProcessor::DcOffset,
    bpm_min: f32,
    bpm_max: f32,
    bpm: Vec<TempoCandidate>,
    keys: Vec<KeyEstimate>,
    user_key: Option<KeyEstimate>,
    transients: Vec<TransientMarker>,
    transient_params: TransientDetectParams,
    freq_focus: FrequencyFocus,
    // Time/pitch
    time_mode: TimePitchMode,
    stretch_percent: f64,
    pitch_semi: f32,
    pitch_cents: f32,
    preserve_transients: bool,
    // Channel / phase / resample / repair
    channel: ChannelTransform,
    phase_ms: bool,
    phase_corr: f32,
    resample_target: u32,
    repair_module: AudioRepairModule,
    denoise: SpectralDenoiseParams,
    learned_noise: Option<Vec<f32>>,
    declick: DeclickParams,
    dehum: DehumParams,
    spectral_gain_db: f32,
    spectrum_busy: bool,
}

impl AudioToolWindow {
    fn new(
        session: AudioToolSession,
        timeline: Entity<Timeline>,
        callbacks: AudioToolWindowCallbacks,
        cx: &mut Context<Self>,
    ) -> Self {
        DirectAudio::analysis_tap().set_target_clip(Some(&session.target.clip_id));
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this
                    .update(cx, |this, cx| {
                        this.tick_realtime(cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let target_sr = session.target.sample_rate.max(44_100);
        Self {
            session,
            timeline,
            callbacks,
            status: String::new(),
            spectrum_mode: SpectrumMode::SelectionAverage,
            fft_size: FftSize::N2048,
            spectrum_window: SpectrumWindow::Hann,
            smoothing: SpectrumSmoothing::None,
            peak_hold: false,
            spectrum: None,
            loudness: None,
            normalize: NormalizeParams::default(),
            measurement: None,
            dc: SphereAudioProcessor::DcOffset::default(),
            bpm_min: 60.0,
            bpm_max: 200.0,
            bpm: Vec::new(),
            keys: Vec::new(),
            user_key: None,
            transients: Vec::new(),
            transient_params: TransientDetectParams::default(),
            freq_focus: FrequencyFocus::FullBand,
            time_mode: TimePitchMode::Off,
            stretch_percent: 100.0,
            pitch_semi: 0.0,
            pitch_cents: 0.0,
            preserve_transients: true,
            channel: ChannelTransform::Stereo,
            phase_ms: false,
            phase_corr: 0.0,
            resample_target: if target_sr == 44_100 { 48_000 } else { 44_100 },
            repair_module: AudioRepairModule::Denoise,
            denoise: SpectralDenoiseParams::default(),
            learned_noise: None,
            declick: DeclickParams::default(),
            dehum: DehumParams::default(),
            spectral_gain_db: 0.0,
            spectrum_busy: false,
        }
    }

    fn tick_realtime(&mut self, cx: &mut Context<Self>) {
        match self.session.tool_kind {
            AudioToolKind::SpectrumAnalyzer
                if self.spectrum_mode == SpectrumMode::RealtimePlayback && !self.spectrum_busy =>
            {
                let size = self.fft_size.size();
                let mut left = vec![0.0; size];
                let mut right = vec![0.0; size];
                let n = DirectAudio::analysis_tap().copy_recent(&mut left, &mut right);
                if n >= size / 2 {
                    left.truncate(n);
                    right.truncate(n);
                    let sr = self.session.target.sample_rate.max(44_100);
                    let fft_size = self.fft_size;
                    let window = self.spectrum_window;
                    let smoothing = self.smoothing;
                    let hold = self
                        .peak_hold
                        .then(|| self.spectrum.as_ref().map(|s| s.peak_hold_db.clone()))
                        .flatten();
                    self.spectrum_busy = true;
                    let host = cx.entity().downgrade();
                    cx.spawn(async move |_, cx| {
                        let snap = cx
                            .background_executor()
                            .spawn(async move {
                                SphereAudioProcessor::analyze_ring_window(
                                    &left,
                                    &right,
                                    sr,
                                    fft_size,
                                    window,
                                    smoothing,
                                    hold.as_deref(),
                                )
                            })
                            .await;
                        let _ = host.update(cx, |this, cx| {
                            this.spectrum_busy = false;
                            if snap.is_some() {
                                this.spectrum = snap;
                            }
                            cx.notify();
                        });
                    })
                    .detach();
                }
            }
            AudioToolKind::PhaseAnalyzer => {
                let mut left = vec![0.0; 2048];
                let mut right = vec![0.0; 2048];
                let n = DirectAudio::analysis_tap().copy_recent(&mut left, &mut right);
                if n > 16 {
                    let m = measure_phase(&left[..n], &right[..n], self.phase_ms);
                    self.phase_corr = m.correlation;
                }
            }
            _ => {}
        }
    }

    fn spawn_analyze(&mut self, cx: &mut Context<Self>) {
        let kind = self.session.tool_kind;
        let path = self.session.target.source_path.clone();
        let selection = self.session.target.time_selection;
        let spectral = self.session.target.spectral_selection;
        let fft_size = self.fft_size;
        let window = self.spectrum_window;
        let smoothing = self.smoothing;
        let peak = self.spectrum_mode == SpectrumMode::SelectionPeak;
        let peak_hold = if self.peak_hold {
            self.spectrum.as_ref().map(|s| s.peak_hold_db.clone())
        } else {
            None
        };
        let bpm_min = self.bpm_min;
        let bpm_max = self.bpm_max;
        let transient_params = {
            let nyquist = self.session.target.sample_rate as f32 * 0.5;
            let (lo, hi) = self.freq_focus.band(nyquist);
            TransientDetectParams {
                freq_low_hz: lo,
                freq_high_hz: hi,
                ..self.transient_params
            }
        };
        let normalize = self.normalize;
        self.session.analyzing = true;
        self.status = "Analyzing…".to_string();
        let host = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let path = path.ok_or_else(|| "clip has no source file".to_string())?;
                    let buffer = DirectAudio::load_audio_file(&path)?;
                    let channels = buffer.channels.max(1);
                    let mut samples = buffer.samples;
                    if let Some(sel) = selection {
                        samples = slice_frames(&samples, channels, sel.start_frame, sel.end_frame);
                    }
                    let mono = downmix_interleaved(&samples, channels);
                    Ok::<_, String>((samples, mono, channels, buffer.sample_rate))
                })
                .await;
            let _ = host.update(cx, |this, cx| {
                this.session.analyzing = false;
                match result {
                    Ok((samples, mono, channels, sr)) => {
                        this.status.clear();
                        match kind {
                            AudioToolKind::SpectrumAnalyzer => {
                                this.spectrum = analyze_spectrum(
                                    &mono,
                                    sr,
                                    fft_size,
                                    window,
                                    smoothing,
                                    peak,
                                    peak_hold.as_deref(),
                                );
                            }
                            AudioToolKind::Loudness | AudioToolKind::Normalize => {
                                this.loudness = analyze_loudness(&samples, channels, sr);
                                this.measurement =
                                    Some(measure_normalize(&samples, channels, sr, normalize));
                            }
                            AudioToolKind::DcOffset => {
                                this.dc = measure_dc_offset(&samples, channels);
                            }
                            AudioToolKind::BpmAnalysis => {
                                this.bpm =
                                    estimate_bpm_candidates(&mono, sr as f32, bpm_min, bpm_max);
                            }
                            AudioToolKind::KeyAnalysis => {
                                this.keys = estimate_key_ranked(&mono, sr as f32);
                            }
                            AudioToolKind::TransientDetector => {
                                this.transients = detect_transients(&mono, sr, transient_params);
                                this.status = format!("{} transients", this.transients.len());
                            }
                            AudioToolKind::PhaseAnalyzer => {
                                if channels >= 2 {
                                    let mut l = Vec::new();
                                    let mut r = Vec::new();
                                    for frame in samples.chunks(channels) {
                                        l.push(frame[0]);
                                        r.push(frame[1]);
                                    }
                                    this.phase_corr =
                                        measure_phase(&l, &r, this.phase_ms).correlation;
                                }
                            }
                            AudioToolKind::SpectralProcessor => {
                                let _ = spectral;
                                this.status = format!(
                                    "region {}–{} Hz",
                                    spectral.map(|s| s.min_hz).unwrap_or(0.0),
                                    spectral.map(|s| s.max_hz).unwrap_or(0.0)
                                );
                            }
                            _ => {}
                        }
                    }
                    Err(error) => this.status = error,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn spawn_learn_noise(&mut self, cx: &mut Context<Self>) {
        let path = self.session.target.source_path.clone();
        let selection = self.session.target.time_selection;
        self.status = "Learning noise profile…".to_string();
        let host = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let path = path.ok_or_else(|| "clip has no source file".to_string())?;
                    let buffer = DirectAudio::load_audio_file(&path)?;
                    let channels = buffer.channels.max(1);
                    let mut samples = buffer.samples;
                    if let Some(sel) = selection {
                        samples = slice_frames(&samples, channels, sel.start_frame, sel.end_frame);
                    }
                    let mono = downmix_interleaved(&samples, channels);
                    Ok::<_, String>(learn_noise_profile(&mono, 2048, 512))
                })
                .await;
            let _ = host.update(cx, |this, cx| {
                match result {
                    Ok(profile) => {
                        this.learned_noise = Some(profile);
                        this.status = "Noise profile learned".to_string();
                    }
                    Err(error) => this.status = error,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dispatch(&self, command: AudioToolCommand, cx: &mut App) {
        (self.callbacks.on_command)(command, cx);
    }

    fn emit_preview(&mut self, cx: &mut App) {
        if !self.session.preview_enabled {
            self.dispatch(
                AudioToolCommand::ClearPreview(self.session.target.clip_id.clone()),
                cx,
            );
            return;
        }
        let mut preview = ClipPreviewOverride::identity(self.session.target.clip_id.clone());
        preview.bypass = self.session.preview_bypassed;
        match self.session.tool_kind {
            AudioToolKind::Normalize => {
                if let Some(m) = self.measurement {
                    preview.extra_gain = Some(db_to_lin(m.required_gain_db));
                }
            }
            AudioToolKind::ChannelTools => preview.channel = Some(self.channel),
            AudioToolKind::DcOffset => {
                preview.dc_remove = Some(true);
                preview.dc_left = self.dc.left;
                preview.dc_right = self.dc.right;
            }
            AudioToolKind::TimePitch => {
                preview.stretch_ratio = Some((self.stretch_percent / 100.0).clamp(0.05, 20.0));
                preview.pitch_semitones = Some(self.pitch_semi + self.pitch_cents / 100.0);
            }
            AudioToolKind::AudioRepair if self.repair_module == AudioRepairModule::DeHum => {
                preview.dehum = Some(self.dehum);
            }
            AudioToolKind::AudioRepair if self.repair_module == AudioRepairModule::Denoise => {
                preview.denoise_amount = Some((self.denoise.reduction_db / 24.0).clamp(0.0, 1.0));
            }
            _ => {}
        }
        self.dispatch(AudioToolCommand::Preview(preview), cx);
    }

    fn apply(&mut self, cx: &mut Context<Self>) {
        let clip_id = self.session.target.clip_id.clone();
        match self.session.tool_kind {
            AudioToolKind::Normalize => {
                let Some(m) = self.measurement else {
                    self.status = "Analyze first".to_string();
                    return;
                };
                let gain_db = m.required_gain_db;
                self.apply_offline_pcm(cx, move |samples, _channels, _sr| {
                    let mut out = samples.to_vec();
                    apply_gain_interleaved(&mut out, gain_db);
                    Ok(out)
                });
                return;
            }
            AudioToolKind::ChannelTools => {
                let channel = self.channel;
                self.apply_offline_pcm(cx, move |samples, channels, _sr| {
                    Ok(apply_channel_transform_interleaved(
                        samples, channels, channel,
                    ))
                });
                return;
            }
            AudioToolKind::DcOffset => {
                let dc = self.dc;
                self.apply_offline_pcm(cx, move |samples, _channels, _sr| {
                    let mut processor = DcOffsetProcessor::new(dc, true);
                    let mut out = vec![0.0; samples.len()];
                    processor.process(samples, &mut out);
                    Ok(out)
                });
                return;
            }
            AudioToolKind::TimePitch => {
                let percent = self.stretch_percent;
                let semi = self.pitch_semi;
                let cents = self.pitch_cents;
                let mode = self.time_mode;
                let time_ratio = match mode {
                    TimePitchMode::Off => 1.0,
                    TimePitchMode::FollowTempo
                    | TimePitchMode::Stretch
                    | TimePitchMode::FitDuration => (percent / 100.0).clamp(0.05, 20.0) as f32,
                };
                let pitch_ratio = semitone_to_pitch_ratio(semi, cents);
                if (time_ratio - 1.0).abs() < 1.0e-4 && (pitch_ratio - 1.0).abs() < 1.0e-4 {
                    self.status = "Nothing to apply".to_string();
                    return;
                }
                self.apply_offline_pcm(cx, move |samples, channels, sr| {
                    let params = StretchParams {
                        mode: StretchMode::Manual,
                        algorithm: StretchAlgorithm::PreservePitch,
                        time_ratio,
                        pitch_ratio,
                        preserve_pitch: true,
                        quality: 0.75,
                        ..StretchParams::default()
                    };
                    render_stretch_interleaved(samples, channels, sr, &params)
                        .map_err(|error| error.to_string())
                });
                return;
            }
            AudioToolKind::AudioRepair if self.repair_module == AudioRepairModule::DeHum => {
                let params = self.dehum;
                self.apply_offline_pcm(cx, move |samples, _channels, sr| {
                    let mut processor = DehumProcessor::new(sr, params);
                    processor.reset();
                    let mut out = vec![0.0; samples.len()];
                    processor.process(samples, &mut out);
                    Ok(out)
                });
                return;
            }
            AudioToolKind::AudioRepair if self.repair_module == AudioRepairModule::Denoise => {
                if let Some(profile) = self.learned_noise.clone() {
                    let params = self.denoise;
                    self.apply_offline_pcm(cx, move |samples, channels, sr| {
                        let mono = downmix_interleaved(samples, channels);
                        let out_mono = reduce_noise_stft(&mono, sr, &profile, params);
                        let mut out = Vec::with_capacity(out_mono.len() * channels);
                        for sample in out_mono {
                            for _ in 0..channels {
                                out.push(sample);
                            }
                        }
                        Ok(out)
                    });
                    return;
                }
                let amount = (self.denoise.reduction_db / 24.0).clamp(0.0, 1.0);
                self.dispatch(
                    AudioToolCommand::MutateClip {
                        clip_id: clip_id.clone(),
                        label: "De-Noise",
                        mutate: Box::new(move |clip| {
                            clip.stretch.denoise_amount = amount;
                        }),
                    },
                    cx,
                );
            }
            AudioToolKind::AudioRepair if self.repair_module == AudioRepairModule::DeClick => {
                let params = self.declick;
                self.apply_offline_pcm(cx, move |samples, channels, _sr| {
                    Ok(declick_interleaved(samples, channels, params).0)
                });
                return;
            }
            AudioToolKind::SpectralProcessor => {
                let gain = db_to_lin(self.spectral_gain_db);
                let sel = self.session.target.spectral_selection;
                let (window_start, _) = self.clip_source_window(cx);
                let window_start = window_start as i64;
                self.apply_offline_pcm(cx, move |samples, channels, sr| {
                    let mono = downmix_interleaved(samples, channels);
                    let params = SpectralGainParams {
                        start_frame: sel
                            .map(|s| (s.start_frame - window_start).max(0))
                            .unwrap_or(0),
                        end_frame: sel
                            .map(|s| (s.end_frame - window_start).max(0))
                            .unwrap_or(i64::MAX),
                        min_hz: sel.map(|s| s.min_hz).unwrap_or(0.0),
                        max_hz: sel.map(|s| s.max_hz).unwrap_or(f32::MAX),
                        gain,
                        fade_bins: 4,
                    };
                    let out_mono = apply_spectral_gain(&mono, sr, params, StftSettings::default());
                    let mut out = Vec::with_capacity(out_mono.len() * channels);
                    for sample in out_mono {
                        for _ in 0..channels {
                            out.push(sample);
                        }
                    }
                    Ok(out)
                });
                return;
            }
            AudioToolKind::Resample => {
                let target = self.resample_target;
                self.apply_offline_pcm(cx, move |samples, channels, sr| {
                    resample_interleaved(samples, channels, sr, target).map_err(|e| e.to_string())
                });
                return;
            }
            _ => {}
        }
        self.dispatch(AudioToolCommand::ClearPreview(clip_id), cx);
        self.session.preview_enabled = false;
        self.session.dirty = false;
    }

    fn clip_source_window(&self, cx: &App) -> (u64, u64) {
        self.timeline
            .read(cx)
            .state
            .find_clip(&self.session.target.clip_id)
            .map(|(_, clip)| {
                let start = clip.stretch.source_start_samples;
                let end = if clip.stretch.source_end_samples > start {
                    clip.stretch.source_end_samples
                } else {
                    clip.stretch.original_duration_samples
                };
                (start, end)
            })
            .unwrap_or((0, 0))
    }

    fn apply_offline_pcm(
        &mut self,
        cx: &mut Context<Self>,
        process: impl FnOnce(&[f32], usize, u32) -> Result<Vec<f32>, String> + Send + 'static,
    ) {
        let clip_id = self.session.target.clip_id.clone();
        let path = self.session.target.source_path.clone();
        let selection = self.session.target.time_selection;
        let tool_kind = self.session.tool_kind;
        let (source_start, source_end) = self.clip_source_window(cx);
        let target_rate = if tool_kind == AudioToolKind::Resample {
            self.resample_target
        } else {
            self.session.target.sample_rate
        };
        self.status = "Rendering…".to_string();
        let host = cx.entity().downgrade();
        let on_command = self.callbacks.on_command.clone();
        let path_clip_id = clip_id.clone();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let path = path.ok_or_else(|| "clip has no source file".to_string())?;
                    let buffer = DirectAudio::load_audio_file(&path)?;
                    let channels = buffer.channels.max(1);
                    let total_frames = (buffer.samples.len() / channels.max(1)) as u64;
                    let window_start = source_start.min(total_frames);
                    let window_end = if source_end > window_start {
                        source_end.min(total_frames)
                    } else {
                        total_frames
                    };
                    // Bounce this clip's audible window only. A shared source
                    // (duplicates, split halves) must keep the original file so
                    // the other clips do not inherit a crop.
                    let mut clip_samples = slice_frames(
                        &buffer.samples,
                        channels,
                        window_start as i64,
                        window_end as i64,
                    );
                    if clip_samples.is_empty() {
                        return Err("clip source window is empty".to_string());
                    }
                    let process_whole_clip = matches!(
                        tool_kind,
                        AudioToolKind::TimePitch
                            | AudioToolKind::Resample
                            | AudioToolKind::SpectralProcessor
                    );
                    if process_whole_clip {
                        clip_samples = process(&clip_samples, channels, buffer.sample_rate)?;
                    } else if let Some(sel) = selection {
                        let local_start =
                            (sel.start_frame.max(0) as u64).saturating_sub(window_start);
                        let local_end = (sel.end_frame.max(0) as u64)
                            .saturating_sub(window_start)
                            .max(local_start);
                        let region = slice_frames(
                            &clip_samples,
                            channels,
                            local_start as i64,
                            local_end as i64,
                        );
                        if !region.is_empty() {
                            let processed = process(&region, channels, buffer.sample_rate)?;
                            replace_frame_range(
                                &mut clip_samples,
                                channels,
                                local_start as i64,
                                &processed,
                            );
                        }
                    } else {
                        clip_samples = process(&clip_samples, channels, buffer.sample_rate)?;
                    }
                    let out_path =
                        processed_output_path(std::path::Path::new(&path), &path_clip_id);
                    write_wav_f32(&out_path, &clip_samples, channels as u16, target_rate)
                        .map_err(|e| e.to_string())?;
                    Ok::<_, String>(out_path)
                })
                .await;
            let _ = host.update(cx, |this, cx| {
                match result {
                    Ok(path) => {
                        (on_command)(
                            AudioToolCommand::ReplaceSource {
                                clip_id: clip_id.clone(),
                                path,
                                sample_rate: target_rate,
                            },
                            cx,
                        );
                        this.status = "Applied".to_string();
                        this.dispatch(AudioToolCommand::ClearPreview(clip_id), cx);
                    }
                    Err(error) => this.status = error,
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel(&mut self, cx: &mut App) {
        self.dispatch(
            AudioToolCommand::ClearPreview(self.session.target.clip_id.clone()),
            cx,
        );
        self.session.preview_enabled = false;
        self.session.dirty = false;
    }
}

fn sanitized_clip_id(clip_id: &str) -> String {
    let sanitized: String = clip_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "clip".to_string()
    } else {
        sanitized
    }
}

/// Derived bounce next to the source, unique per clip so a split sibling or
/// duplicate cannot be overwritten. Re-applying the same clip replaces its
/// own file instead of stacking suffixes.
fn processed_output_path(source: &std::path::Path, clip_id: &str) -> PathBuf {
    let mut out = source.to_path_buf();
    let stem = out
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("clip");
    let clip_tag = sanitized_clip_id(clip_id);
    let suffix = format!(".{clip_tag}.processed");
    let stem = stem.strip_suffix(&suffix).unwrap_or(stem);
    let stem = stem.strip_suffix("-processed").unwrap_or(stem);
    out.set_file_name(format!("{stem}.{clip_tag}.processed.wav"));
    out
}

fn row_label(label: &str) -> impl IntoElement {
    div()
        .text_size(px(typography::DENSE_LABEL))
        .text_color(Colors::text_muted())
        .child(label.to_string())
}

fn row_value(value: impl Into<String>) -> impl IntoElement {
    div()
        .text_size(px(typography::UI_SM))
        .text_color(Colors::text_primary())
        .child(value.into())
}

fn info_row(label: &str, value: impl Into<String>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(22.0))
        .child(row_label(label))
        .child(row_value(value))
}

impl Render for AudioToolWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.session.tool_kind;
        let analysis = kind.is_analysis_only();
        let on_close = self.callbacks.on_close.clone();
        let target = self.session.target.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(Colors::surface_base())
            .text_color(Colors::text_primary())
            .font(crate::theme::ui_font())
            .child(external_window_titlebar(
                kind.label(),
                "audio-tool-close",
                move |window, cx| {
                    on_close(kind, window.bounds(), cx);
                    window.remove_window();
                },
            ))
            .child(
                div()
                    .flex_none()
                    .px(px(space::SECTION))
                    .py(px(space::BASE))
                    .border_b(px(1.0))
                    .border_color(Colors::border_subtle())
                    .child(info_row("Target", target.target_label(kind)))
                    .child(info_row("Source", target.summary_line())),
            )
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .px(px(space::SECTION))
                    .py(px(space::BASE))
                    .overflow_hidden()
                    .child(self.tool_body(cx)),
            )
            .child(
                div()
                    .flex_none()
                    .px(px(space::SECTION))
                    .py(px(space::BASE))
                    .border_t(px(1.0))
                    .border_color(Colors::border_subtle())
                    .flex()
                    .flex_col()
                    .gap(px(space::BASE))
                    .child(
                        div()
                            .text_size(px(typography::UI_XS))
                            .text_color(Colors::text_muted())
                            .child(if self.session.analyzing {
                                format!("Analyzing… {:.0}%", self.session.analyze_progress * 100.0)
                            } else {
                                self.status.clone()
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .flex()
                                    .gap(px(space::LOOSE))
                                    .child(fb_checkbox(
                                        "follow-sel",
                                        "Follow Selection",
                                        self.session.follow_selection,
                                        true,
                                        cx.listener(|this, _, _, cx| {
                                            this.session.follow_selection =
                                                !this.session.follow_selection;
                                            this.session.pin_target =
                                                !this.session.follow_selection;
                                            cx.notify();
                                        }),
                                    ))
                                    .child(fb_checkbox(
                                        "pin-target",
                                        "Pin Target",
                                        self.session.pin_target,
                                        true,
                                        cx.listener(|this, _, _, cx| {
                                            this.session.pin_target = !this.session.pin_target;
                                            this.session.follow_selection =
                                                !this.session.pin_target;
                                            cx.notify();
                                        }),
                                    ))
                                    .when(!analysis, |this| {
                                        this.child(fb_checkbox(
                                            "preview",
                                            "Preview",
                                            self.session.preview_enabled,
                                            true,
                                            cx.listener(|this, _, _, cx| {
                                                this.session.preview_enabled =
                                                    !this.session.preview_enabled;
                                                this.emit_preview(cx);
                                                cx.notify();
                                            }),
                                        ))
                                        .child(
                                            fb_checkbox(
                                                "bypass",
                                                "Bypass",
                                                self.session.preview_bypassed,
                                                self.session.preview_enabled,
                                                cx.listener(|this, _, _, cx| {
                                                    this.session.preview_bypassed =
                                                        !this.session.preview_bypassed;
                                                    this.emit_preview(cx);
                                                    cx.notify();
                                                }),
                                            ),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap(px(space::BASE))
                                    .when(analysis, |this| {
                                        this.child(fb_button(
                                            "close",
                                            "Close",
                                            FbButtonKind::Default,
                                            true,
                                            {
                                                let on_close = self.callbacks.on_close.clone();
                                                move |_, window, cx| {
                                                    on_close(kind, window.bounds(), cx);
                                                    window.remove_window();
                                                }
                                            },
                                        ))
                                    })
                                    .when(!analysis, |this| {
                                        this.child(fb_button(
                                            "cancel",
                                            "Cancel",
                                            FbButtonKind::Default,
                                            true,
                                            cx.listener(|this, _, window, cx| {
                                                this.cancel(cx);
                                                (this.callbacks.on_close)(
                                                    this.session.tool_kind,
                                                    window.bounds(),
                                                    cx,
                                                );
                                                window.remove_window();
                                            }),
                                        ))
                                        .child(fb_button(
                                            "apply",
                                            "Apply",
                                            FbButtonKind::Primary,
                                            true,
                                            cx.listener(|this, _, _, cx| {
                                                this.apply(cx);
                                                cx.notify();
                                            }),
                                        ))
                                    }),
                            ),
                    ),
            )
    }
}

impl AudioToolWindow {
    fn tool_body(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let analyze = cx.listener(|this, _, _, cx| this.spawn_analyze(cx));
        match self.session.tool_kind {
            AudioToolKind::SpectrumAnalyzer => self.spectrum_body(analyze, cx).into_any_element(),
            AudioToolKind::Loudness => self.loudness_body(analyze).into_any_element(),
            AudioToolKind::Normalize => self.normalize_body(analyze, cx).into_any_element(),
            AudioToolKind::TransientDetector => self.transient_body(analyze, cx).into_any_element(),
            AudioToolKind::TimePitch => self.time_pitch_body(cx).into_any_element(),
            AudioToolKind::Resample => self.resample_body(cx).into_any_element(),
            AudioToolKind::ChannelTools => self.channel_body(cx).into_any_element(),
            AudioToolKind::PhaseAnalyzer => self.phase_body(cx).into_any_element(),
            AudioToolKind::DcOffset => self.dc_body(analyze).into_any_element(),
            AudioToolKind::BpmAnalysis => self.bpm_body(analyze, cx).into_any_element(),
            AudioToolKind::KeyAnalysis => self.key_body(analyze).into_any_element(),
            AudioToolKind::AudioRepair => self.repair_body(cx).into_any_element(),
            AudioToolKind::SpectralProcessor => self.spectral_body(cx).into_any_element(),
            AudioToolKind::SpectrogramSettings => {
                self.spectrogram_settings_body(cx).into_any_element()
            }
        }
    }

    fn spectrum_body(
        &self,
        analyze: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let bars = self.spectrum.as_ref().map(|snap| {
            let count = snap.magnitudes_db.len().min(240).max(1);
            div()
                .flex()
                .items_end()
                .gap(px(1.0))
                .h(px(180.0))
                .w_full()
                .children((0..count).map(|i| {
                    let idx = i * snap.magnitudes_db.len() / count;
                    let db = snap.magnitudes_db.get(idx).copied().unwrap_or(-120.0);
                    let hold = snap.peak_hold_db.get(idx).copied().unwrap_or(db);
                    let h = ((db + 120.0) / 132.0).clamp(0.0, 1.0) * 180.0;
                    let hold_h = ((hold + 120.0) / 132.0).clamp(0.0, 1.0) * 180.0;
                    div()
                        .flex_1()
                        .h(px(hold_h.max(h)))
                        .bg(Colors::with_alpha(Colors::accent_primary(), 0.85))
                }))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(space::BASE))
            .child(info_row("Mode", self.spectrum_mode.label()))
            .child(info_row("FFT", self.fft_size.label()))
            .child(info_row("Window", self.spectrum_window.label()))
            .child(info_row("Smoothing", self.smoothing.label()))
            .child(info_row(
                "Peak Hold",
                if self.peak_hold { "On" } else { "Off" },
            ))
            .child(info_row("Range", "20 Hz → Nyquist"))
            .children(bars)
            .child(
                div()
                    .flex()
                    .gap(px(space::BASE))
                    .child(fb_button(
                        "spec-mode",
                        "Cycle Mode",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.spectrum_mode = match this.spectrum_mode {
                                SpectrumMode::RealtimePlayback => SpectrumMode::SelectionAverage,
                                SpectrumMode::SelectionAverage => SpectrumMode::SelectionPeak,
                                SpectrumMode::SelectionPeak => SpectrumMode::StaticCursor,
                                SpectrumMode::StaticCursor => SpectrumMode::RealtimePlayback,
                            };
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "spec-fft",
                        "Cycle FFT",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.fft_size = match this.fft_size {
                                FftSize::N512 => FftSize::N1024,
                                FftSize::N1024 => FftSize::N2048,
                                FftSize::N2048 => FftSize::N4096,
                                FftSize::N4096 => FftSize::N8192,
                                FftSize::N8192 => FftSize::N16384,
                                FftSize::N16384 => FftSize::N512,
                            };
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "spec-win",
                        "Cycle Window",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.spectrum_window = match this.spectrum_window {
                                SpectrumWindow::Hann => SpectrumWindow::BlackmanHarris,
                                SpectrumWindow::BlackmanHarris => SpectrumWindow::Hann,
                            };
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "spec-smooth",
                        "Cycle Smooth",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.smoothing = match this.smoothing {
                                SpectrumSmoothing::None => SpectrumSmoothing::SixthOctave,
                                SpectrumSmoothing::SixthOctave => SpectrumSmoothing::TwelfthOctave,
                                SpectrumSmoothing::TwelfthOctave => SpectrumSmoothing::None,
                            };
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "spec-hold",
                        if self.peak_hold {
                            "Peak Hold Off"
                        } else {
                            "Peak Hold On"
                        },
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.peak_hold = !this.peak_hold;
                            cx.notify();
                        }),
                    )),
            )
            .child(fb_button(
                "spec-analyze",
                "Analyze",
                FbButtonKind::Default,
                true,
                analyze,
            ))
    }

    fn loudness_body(
        &self,
        analyze: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let m = self.loudness;
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row(
                "Momentary",
                m.map(|v| format!("{:.1} LUFS", v.momentary_lufs))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "Short-Term",
                m.map(|v| format!("{:.1} LUFS", v.shortterm_lufs))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "Integrated",
                m.map(|v| format!("{:.1} LUFS", v.integrated_lufs))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "LRA",
                m.map(|v| format!("{:.1} LU", v.loudness_range))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "True Peak",
                m.map(|v| format!("{:.1} dBTP", v.true_peak_dbtp))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "Peak",
                m.map(|v| format!("{:.1} dBFS", v.peak_dbfs))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(fb_button(
                "lufs-analyze",
                "Analyze",
                FbButtonKind::Default,
                true,
                analyze,
            ))
    }

    fn normalize_body(
        &self,
        analyze: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let m = self.measurement;
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row("Mode", self.normalize.mode.label()))
            .child(info_row(
                "Peak target",
                format!("{:.2} dBFS", self.normalize.target_peak_dbfs),
            ))
            .child(info_row(
                "True Peak target",
                format!("{:.2} dBTP", self.normalize.target_true_peak_dbtp),
            ))
            .child(info_row(
                "Loudness target",
                format!("{:.1} LUFS", self.normalize.target_lufs),
            ))
            .child(info_row(
                "Current Peak",
                m.map(|v| format!("{:.1} dBFS", v.peak_dbfs))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "LUFS-I",
                m.and_then(|v| v.lufs_i)
                    .map(|v| format!("{v:.1}"))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "True Peak",
                m.map(|v| format!("{:.1} dBTP", v.true_peak_dbtp))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "Required Gain",
                m.map(|v| format!("{:+.1} dB", v.required_gain_db))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(
                div()
                    .flex()
                    .gap(px(space::BASE))
                    .child(fb_button(
                        "norm-peak",
                        "Peak",
                        if self.normalize.mode == NormalizeMode::Peak {
                            FbButtonKind::Primary
                        } else {
                            FbButtonKind::Default
                        },
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.normalize.mode = NormalizeMode::Peak;
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "norm-tp",
                        "True Peak",
                        if self.normalize.mode == NormalizeMode::TruePeak {
                            FbButtonKind::Primary
                        } else {
                            FbButtonKind::Default
                        },
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.normalize.mode = NormalizeMode::TruePeak;
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "norm-lufs",
                        "Loudness",
                        if self.normalize.mode == NormalizeMode::Loudness {
                            FbButtonKind::Primary
                        } else {
                            FbButtonKind::Default
                        },
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.normalize.mode = NormalizeMode::Loudness;
                            cx.notify();
                        }),
                    )),
            )
            .child(fb_button(
                "norm-analyze",
                "Analyze",
                FbButtonKind::Default,
                true,
                analyze,
            ))
    }

    fn transient_body(
        &self,
        analyze: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row(
                "Sensitivity",
                format!("{:.0}%", self.transient_params.sensitivity * 100.0),
            ))
            .child(info_row(
                "Minimum Gap",
                format!("{:.0} ms", self.transient_params.min_gap_ms),
            ))
            .child(info_row("Frequency Focus", self.freq_focus.label()))
            .child(info_row(
                "Results",
                format!("{} transients", self.transients.len()),
            ))
            .child(fb_button(
                "tr-analyze",
                "Analyze",
                FbButtonKind::Default,
                true,
                analyze,
            ))
            .child(
                div()
                    .flex()
                    .gap(px(space::BASE))
                    .child(fb_button(
                        "tr-markers",
                        "Add Markers",
                        FbButtonKind::Default,
                        !self.transients.is_empty(),
                        cx.listener(|this, _, _, cx| {
                            let sr = this.session.target.sample_rate.max(1) as f64;
                            let start = this
                                .timeline
                                .read(cx)
                                .state
                                .find_clip(&this.session.target.clip_id)
                                .map(|(_, c)| c.start_beat as f64)
                                .unwrap_or(0.0);
                            let spb = this.timeline.read(cx).state.seconds_per_beat() as f64;
                            let beats = this
                                .transients
                                .iter()
                                .map(|m| start + (m.source_frame as f64 / sr) / spb.max(1.0e-6))
                                .collect();
                            this.dispatch(
                                AudioToolCommand::AddMarkers {
                                    beats,
                                    label: "Add Transient Markers",
                                },
                                cx,
                            );
                        }),
                    ))
                    .child(fb_button(
                        "tr-warp",
                        "Create Warp Markers",
                        FbButtonKind::Default,
                        !self.transients.is_empty(),
                        cx.listener(|this, _, _, cx| {
                            this.dispatch(
                                AudioToolCommand::AddWarpMarkers {
                                    clip_id: this.session.target.clip_id.clone(),
                                    frames: this
                                        .transients
                                        .iter()
                                        .map(|m| m.source_frame)
                                        .collect(),
                                },
                                cx,
                            );
                        }),
                    ))
                    .child(fb_button(
                        "tr-slice",
                        "Slice",
                        FbButtonKind::Default,
                        !self.transients.is_empty(),
                        cx.listener(|this, _, _, cx| {
                            let sr = this.session.target.sample_rate.max(1) as f32;
                            let start = this
                                .timeline
                                .read(cx)
                                .state
                                .find_clip(&this.session.target.clip_id)
                                .map(|(_, c)| c.start_beat)
                                .unwrap_or(0.0);
                            let spb = this.timeline.read(cx).state.seconds_per_beat();
                            let beats = this
                                .transients
                                .iter()
                                .map(|m| start + (m.source_frame as f32 / sr) / spb.max(1.0e-6))
                                .collect();
                            this.dispatch(
                                AudioToolCommand::SliceClip {
                                    clip_id: this.session.target.clip_id.clone(),
                                    beats,
                                },
                                cx,
                            );
                        }),
                    ))
                    .child(fb_button(
                        "tr-quant",
                        "Quantize",
                        FbButtonKind::Default,
                        false,
                        |_, _, _| {},
                    )),
            )
    }

    fn time_pitch_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let clip = self
            .timeline
            .read(cx)
            .state
            .find_clip(&self.session.target.clip_id)
            .map(|(_, c)| c.clone());
        let duration = clip
            .as_ref()
            .map(|c| c.duration_beats * self.timeline.read(cx).state.seconds_per_beat())
            .unwrap_or(0.0);
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row(
                "Time mode",
                match self.time_mode {
                    TimePitchMode::Off => "Off",
                    TimePitchMode::Stretch => "Stretch",
                    TimePitchMode::FitDuration => "Fit Duration",
                    TimePitchMode::FollowTempo => "Follow Tempo",
                },
            ))
            .child(info_row("Ratio", format!("{:.3} %", self.stretch_percent)))
            .child(info_row("Target Duration", format!("{duration:.3} s")))
            .child(info_row(
                "Original BPM",
                clip.as_ref()
                    .and_then(|c| c.stretch.bpm_source)
                    .map(|b| format!("{b:.3}"))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row("Semitones", format!("{:.0}", self.pitch_semi)))
            .child(info_row("Cents", format!("{:.0}", self.pitch_cents)))
            .child(info_row("Formant", "Neutral"))
            .child(info_row(
                "Preserve Transients",
                if self.preserve_transients {
                    "On"
                } else {
                    "Off"
                },
            ))
            .child(info_row("Quality", "High"))
            .child(
                div()
                    .flex()
                    .gap(px(space::BASE))
                    .child(fb_button(
                        "tp-off",
                        "Off",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.time_mode = TimePitchMode::Off;
                            this.stretch_percent = 100.0;
                            this.emit_preview(cx);
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "tp-stretch",
                        "Stretch",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.time_mode = TimePitchMode::Stretch;
                            this.emit_preview(cx);
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "tp-plus",
                        "+1 st",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.pitch_semi = (this.pitch_semi + 1.0).clamp(-24.0, 24.0);
                            this.emit_preview(cx);
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "tp-minus",
                        "−1 st",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.pitch_semi = (this.pitch_semi - 1.0).clamp(-24.0, 24.0);
                            this.emit_preview(cx);
                            cx.notify();
                        }),
                    )),
            )
    }

    fn resample_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row(
                "Current",
                format!("{} Hz", self.session.target.sample_rate),
            ))
            .child(info_row("Target", format!("{} Hz", self.resample_target)))
            .child(info_row("Quality", "High"))
            .child(info_row("Mode", "Offline Render"))
            .child(fb_button(
                "resample-target",
                "Cycle Target Rate",
                FbButtonKind::Default,
                true,
                cx.listener(|this, _, _, cx| {
                    this.resample_target = match this.resample_target {
                        44_100 => 48_000,
                        48_000 => 88_200,
                        88_200 => 96_000,
                        _ => 44_100,
                    };
                    cx.notify();
                }),
            ))
    }

    fn channel_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div().flex().flex_col().gap(px(space::TIGHT)).children(
            ChannelTransform::ALL.into_iter().map(|mode| {
                fb_button(
                    format!("ch-{}", mode.to_tag()),
                    mode.label(),
                    if self.channel == mode {
                        FbButtonKind::Primary
                    } else {
                        FbButtonKind::Default
                    },
                    true,
                    cx.listener(move |this, _, _, cx| {
                        this.channel = mode;
                        this.emit_preview(cx);
                        cx.notify();
                    }),
                )
            }),
        )
    }

    fn phase_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let x = ((self.phase_corr + 1.0) * 0.5).clamp(0.0, 1.0);
        div()
            .flex()
            .flex_col()
            .gap(px(space::BASE))
            .child(info_row("Mode", if self.phase_ms { "M/S" } else { "L/R" }))
            .child(info_row("Correlation", format!("{:.2}", self.phase_corr)))
            .child(
                div()
                    .h(px(10.0))
                    .w_full()
                    .rounded(px(radius::PILL))
                    .bg(Colors::surface_canvas())
                    .child(
                        div().relative().size_full().child(
                            div()
                                .absolute()
                                .left(gpui::relative(x))
                                .w(px(8.0))
                                .h_full()
                                .bg(Colors::accent_primary()),
                        ),
                    ),
            )
            .child(row_label("−1 ← 0 → +1"))
            .child(fb_button(
                "phase-mode",
                if self.phase_ms { "Use L/R" } else { "Use M/S" },
                FbButtonKind::Default,
                true,
                cx.listener(|this, _, _, cx| {
                    this.phase_ms = !this.phase_ms;
                    cx.notify();
                }),
            ))
    }

    fn dc_body(
        &self,
        analyze: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row("Left", format!("{:+.4}", self.dc.left)))
            .child(info_row("Right", format!("{:+.4}", self.dc.right)))
            .child(fb_button(
                "dc-analyze",
                "Analyze",
                FbButtonKind::Default,
                true,
                analyze,
            ))
    }

    fn bpm_body(
        &self,
        analyze: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row("Minimum BPM", format!("{:.0}", self.bpm_min)))
            .child(info_row("Maximum BPM", format!("{:.0}", self.bpm_max)))
            .children(self.bpm.iter().take(3).map(|c| {
                info_row(
                    "Candidate",
                    format!("{:.2}  ({:.0}%)", c.bpm, c.confidence * 100.0),
                )
            }))
            .child(fb_button(
                "bpm-analyze",
                "Analyze",
                FbButtonKind::Default,
                true,
                analyze,
            ))
            .child(
                div()
                    .flex()
                    .gap(px(space::BASE))
                    .child(fb_button(
                        "bpm-use",
                        "Use as Original BPM",
                        FbButtonKind::Default,
                        self.bpm.first().is_some(),
                        cx.listener(|this, _, _, cx| {
                            if let Some(best) = this.bpm.first() {
                                this.dispatch(
                                    AudioToolCommand::UseOriginalBpm {
                                        clip_id: this.session.target.clip_id.clone(),
                                        bpm: best.bpm as f64,
                                    },
                                    cx,
                                );
                            }
                        }),
                    ))
                    .child(fb_button(
                        "bpm-tempo",
                        "Add Tempo Marker",
                        FbButtonKind::Default,
                        self.bpm.first().is_some(),
                        cx.listener(|this, _, _, cx| {
                            if let Some(best) = this.bpm.first() {
                                let beat = this
                                    .timeline
                                    .read(cx)
                                    .state
                                    .find_clip(&this.session.target.clip_id)
                                    .map(|(_, c)| c.start_beat as f64)
                                    .unwrap_or(0.0);
                                this.dispatch(
                                    AudioToolCommand::AddTempoPoint {
                                        beat,
                                        bpm: best.bpm as f64,
                                    },
                                    cx,
                                );
                            }
                        }),
                    )),
            )
    }

    fn key_body(
        &self,
        analyze: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    ) -> impl IntoElement {
        let detected = self.keys.first();
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row(
                "Detected",
                detected
                    .map(|k| k.display_label())
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "Confidence",
                detected
                    .map(|k| format!("{:.2}", k.confidence))
                    .unwrap_or_else(|| "—".into()),
            ))
            .child(info_row(
                "User key",
                self.user_key
                    .map(|k| k.display_label())
                    .unwrap_or_else(|| "—".into()),
            ))
            .children(
                self.keys
                    .iter()
                    .skip(1)
                    .take(2)
                    .map(|k| info_row("Alternate", k.display_label())),
            )
            .child(fb_button(
                "key-analyze",
                "Analyze",
                FbButtonKind::Default,
                true,
                analyze,
            ))
    }

    fn repair_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let modules = [
            AudioRepairModule::Denoise,
            AudioRepairModule::DeClick,
            AudioRepairModule::DeHum,
            AudioRepairModule::DeReverb,
            AudioRepairModule::DeBleed,
            AudioRepairModule::DeFeedback,
            AudioRepairModule::DrumSilencer,
            AudioRepairModule::SpectralRepair,
        ];
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .children(modules.into_iter().map(|module| {
                fb_button(
                    format!("repair-{}", module.label()),
                    if module.is_available() {
                        module.label().to_string()
                    } else {
                        format!("{} (unavailable)", module.label())
                    },
                    if self.repair_module == module {
                        FbButtonKind::Primary
                    } else {
                        FbButtonKind::Default
                    },
                    module.is_available(),
                    cx.listener(move |this, _, _, cx| {
                        this.repair_module = module;
                        cx.notify();
                    }),
                )
            }))
            .child(match self.repair_module {
                AudioRepairModule::Denoise => div()
                    .flex()
                    .flex_col()
                    .gap(px(space::TIGHT))
                    .child(info_row(
                        "Reduction",
                        format!("{:.0} dB", self.denoise.reduction_db),
                    ))
                    .child(info_row(
                        "Threshold",
                        format!("{:.0} dB", self.denoise.threshold_db),
                    ))
                    .child(info_row(
                        "Noise profile",
                        if self.learned_noise.is_some() {
                            "Learned"
                        } else {
                            "None"
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .gap(px(space::BASE))
                            .child(fb_button(
                                "dn-minus",
                                "−3 dB",
                                FbButtonKind::Default,
                                true,
                                cx.listener(|this, _, _, cx| {
                                    this.denoise.reduction_db =
                                        (this.denoise.reduction_db - 3.0).max(0.0);
                                    this.emit_preview(cx);
                                    cx.notify();
                                }),
                            ))
                            .child(fb_button(
                                "dn-plus",
                                "+3 dB",
                                FbButtonKind::Default,
                                true,
                                cx.listener(|this, _, _, cx| {
                                    this.denoise.reduction_db =
                                        (this.denoise.reduction_db + 3.0).min(24.0);
                                    this.emit_preview(cx);
                                    cx.notify();
                                }),
                            ))
                            .child(fb_button(
                                "dn-learn",
                                "Learn Noise Profile",
                                FbButtonKind::Default,
                                true,
                                cx.listener(|this, _, _, cx| this.spawn_learn_noise(cx)),
                            )),
                    )
                    .into_any_element(),
                AudioRepairModule::DeClick => div()
                    .flex()
                    .flex_col()
                    .gap(px(space::TIGHT))
                    .child(info_row(
                        "Sensitivity",
                        format!("{:.0}%", self.declick.sensitivity * 100.0),
                    ))
                    .child(info_row(
                        "Max Click Width",
                        format!("{} smp", self.declick.max_click_width),
                    ))
                    .child(fb_button(
                        "dc-sens",
                        "Cycle Sensitivity",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.declick.sensitivity = if this.declick.sensitivity < 0.5 {
                                0.65
                            } else if this.declick.sensitivity < 0.85 {
                                0.9
                            } else {
                                0.35
                            };
                            cx.notify();
                        }),
                    ))
                    .into_any_element(),
                AudioRepairModule::DeHum => div()
                    .flex()
                    .flex_col()
                    .gap(px(space::TIGHT))
                    .child(info_row("Base", format!("{:.0} Hz", self.dehum.base_hz)))
                    .child(info_row("Harmonics", format!("{}", self.dehum.harmonics)))
                    .child(info_row(
                        "Reduction",
                        format!("{:.0} dB", self.dehum.reduction_db),
                    ))
                    .child(fb_button(
                        "dh-base",
                        "Cycle 50/60 Hz",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.dehum.base_hz = if (this.dehum.base_hz - 50.0).abs() < 1.0 {
                                60.0
                            } else {
                                50.0
                            };
                            this.emit_preview(cx);
                            cx.notify();
                        }),
                    ))
                    .into_any_element(),
                AudioRepairModule::SpectralRepair => div()
                    .child(row_label(
                        "Use Spectral Processing with a spectral selection.",
                    ))
                    .into_any_element(),
                _ => div()
                    .child(row_label("This module is not available yet."))
                    .into_any_element(),
            })
    }

    fn spectral_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sel = self.session.target.spectral_selection;
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row(
                "Time",
                sel.map(|s| format!("{}–{}", s.start_frame, s.end_frame))
                    .unwrap_or_else(|| "entire clip".into()),
            ))
            .child(info_row(
                "Frequency",
                sel.map(|s| format!("{:.0}–{:.0} Hz", s.min_hz, s.max_hz))
                    .unwrap_or_else(|| "full band".into()),
            ))
            .child(info_row(
                "Gain",
                format!("{:+.1} dB", self.spectral_gain_db),
            ))
            .child(row_label("Apply writes a derived WAV via STFT gain."))
            .child(
                div()
                    .flex()
                    .gap(px(space::BASE))
                    .child(fb_button(
                        "sg-minus",
                        "−3 dB",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.spectral_gain_db = (this.spectral_gain_db - 3.0).max(-120.0);
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "sg-plus",
                        "+3 dB",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.spectral_gain_db = (this.spectral_gain_db + 3.0).min(24.0);
                            cx.notify();
                        }),
                    ))
                    .child(fb_button(
                        "sg-silence",
                        "Silence",
                        FbButtonKind::Default,
                        true,
                        cx.listener(|this, _, _, cx| {
                            this.spectral_gain_db = -120.0;
                            cx.notify();
                        }),
                    )),
            )
    }

    fn spectrogram_settings_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(px(space::TIGHT))
            .child(info_row("FFT", self.fft_size.label()))
            .child(info_row("Window", self.spectrum_window.label()))
            .child(row_label("These settings drive the Spectrum Analyzer FFT."))
            .child(fb_button(
                "specset-fft",
                "Cycle FFT",
                FbButtonKind::Default,
                true,
                cx.listener(|this, _, _, cx| {
                    this.fft_size = match this.fft_size {
                        FftSize::N512 => FftSize::N1024,
                        FftSize::N1024 => FftSize::N2048,
                        FftSize::N2048 => FftSize::N4096,
                        FftSize::N4096 => FftSize::N8192,
                        FftSize::N8192 => FftSize::N16384,
                        FftSize::N16384 => FftSize::N512,
                    };
                    cx.notify();
                }),
            ))
    }
}

pub fn open_audio_tool_window(
    kind: AudioToolKind,
    target: AudioToolTarget,
    owner_bounds: Option<Bounds<Pixels>>,
    remembered: Option<Bounds<Pixels>>,
    timeline: Entity<Timeline>,
    callbacks: AudioToolWindowCallbacks,
    cx: &mut App,
) -> Result<WindowHandle<AudioToolWindow>, String> {
    let (w, h) = kind.default_size();
    let window_size = size(px(w), px(h));
    let bounds =
        remembered.unwrap_or_else(|| centered_window_bounds(owner_bounds, window_size, cx));
    let mut options = crate::platform_chrome::external_window_options_partial();
    options.window_bounds = Some(WindowBounds::Windowed(bounds));
    options.kind = WindowKind::Normal;
    options.is_resizable = true;
    options.is_minimizable = true;
    options.window_background = WindowBackgroundAppearance::Opaque;
    options.window_min_size = Some(size(
        px(AUDIO_TOOL_WINDOW_MIN_WIDTH),
        px(AUDIO_TOOL_WINDOW_MIN_HEIGHT),
    ));
    apply_owner_display(&mut options, owner_bounds, cx);
    let session = AudioToolSession::new(kind, target);
    cx.open_window(options, move |_window, cx| {
        cx.new(|cx| AudioToolWindow::new(session, timeline, callbacks, cx))
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::processed_output_path;
    use std::path::PathBuf;

    #[test]
    fn processed_path_is_unique_per_clip_and_stable_on_reapply() {
        assert_eq!(
            processed_output_path(PathBuf::from("/tmp/kick.wav").as_path(), "clip-3"),
            PathBuf::from("/tmp/kick.clip-3.processed.wav")
        );
        assert_eq!(
            processed_output_path(
                PathBuf::from("/tmp/kick.clip-3.processed.wav").as_path(),
                "clip-3"
            ),
            PathBuf::from("/tmp/kick.clip-3.processed.wav")
        );
        assert_ne!(
            processed_output_path(PathBuf::from("/tmp/kick.wav").as_path(), "clip-3"),
            processed_output_path(PathBuf::from("/tmp/kick.wav").as_path(), "clip-4")
        );
    }
}
