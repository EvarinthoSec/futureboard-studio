//! Offline FFT snapshots for the Spectrum Analyzer window.
//!
//! The FFT itself is intended to run on a worker thread. The functions below
//! allocate and must never be called from an audio callback.

use rustfft::{FftPlanner, num_complex::Complex32};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FftSize {
    N512 = 512,
    N1024 = 1024,
    N2048 = 2048,
    N4096 = 4096,
    N8192 = 8192,
    N16384 = 16384,
}

impl FftSize {
    pub const ALL: [Self; 6] = [
        Self::N512,
        Self::N1024,
        Self::N2048,
        Self::N4096,
        Self::N8192,
        Self::N16384,
    ];

    pub const fn size(self) -> usize {
        self as usize
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::N512 => "512",
            Self::N1024 => "1024",
            Self::N2048 => "2048",
            Self::N4096 => "4096",
            Self::N8192 => "8192",
            Self::N16384 => "16384",
        }
    }

    pub fn from_size(size: usize) -> Self {
        match size {
            512 => Self::N512,
            1024 => Self::N1024,
            4096 => Self::N4096,
            8192 => Self::N8192,
            16384 => Self::N16384,
            _ => Self::N2048,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpectrumWindow {
    #[default]
    Hann,
    BlackmanHarris,
}

impl SpectrumWindow {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hann => "Hann",
            Self::BlackmanHarris => "Blackman-Harris",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpectrumSmoothing {
    #[default]
    None,
    SixthOctave,
    TwelfthOctave,
}

impl SpectrumSmoothing {
    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::SixthOctave => "1/6 octave",
            Self::TwelfthOctave => "1/12 octave",
        }
    }

    fn fraction(self) -> Option<f32> {
        match self {
            Self::None => None,
            Self::SixthOctave => Some(1.0 / 6.0),
            Self::TwelfthOctave => Some(1.0 / 12.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpectrumMode {
    RealtimePlayback,
    #[default]
    SelectionAverage,
    SelectionPeak,
    StaticCursor,
}

impl SpectrumMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::RealtimePlayback => "Realtime Playback",
            Self::SelectionAverage => "Selection Average",
            Self::SelectionPeak => "Selection Peak",
            Self::StaticCursor => "Static Cursor",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumSnapshot {
    pub sample_rate: u32,
    pub fft_size: usize,
    pub magnitudes_db: Vec<f32>,
    pub peak_hold_db: Vec<f32>,
}

pub fn window_table(kind: SpectrumWindow, size: usize) -> Vec<f32> {
    if size <= 1 {
        return vec![1.0; size];
    }
    match kind {
        SpectrumWindow::Hann => (0..size)
            .map(|n| {
                let s = (std::f32::consts::PI * n as f32 / size as f32).sin();
                s * s
            })
            .collect(),
        SpectrumWindow::BlackmanHarris => (0..size)
            .map(|n| {
                let x = std::f32::consts::TAU * n as f32 / size as f32;
                0.35875 - 0.48829 * x.cos() + 0.14128 * (2.0 * x).cos() - 0.01168 * (3.0 * x).cos()
            })
            .collect(),
    }
}

fn to_db(mag: f32, fft_size: usize) -> f32 {
    let n = mag / (fft_size as f32 * 0.5).max(1.0);
    if n <= 1.0e-9 {
        -120.0
    } else {
        (20.0 * n.log10()).clamp(-120.0, 12.0)
    }
}

fn octave_smooth(mags: &[f32], sample_rate: u32, fft_size: usize, fraction: f32) -> Vec<f32> {
    let mut out = vec![-120.0_f32; mags.len()];
    let sr = sample_rate as f32;
    for bin in 1..mags.len() {
        let freq = bin as f32 * sr / fft_size as f32;
        if freq <= 0.0 {
            continue;
        }
        let ratio = 2.0_f32.powf(fraction * 0.5);
        let lo = (freq / ratio * fft_size as f32 / sr).floor() as usize;
        let hi = (freq * ratio * fft_size as f32 / sr).ceil() as usize;
        let lo = lo.max(1).min(mags.len() - 1);
        let hi = hi.max(lo + 1).min(mags.len());
        let mut sum = 0.0_f32;
        let mut n = 0.0_f32;
        for m in &mags[lo..hi] {
            sum += *m;
            n += 1.0;
        }
        out[bin] = if n > 0.0 { sum / n } else { mags[bin] };
    }
    out[0] = mags[0];
    out
}

/// Average or peak magnitude spectrum of a mono buffer.
pub fn analyze_spectrum(
    mono: &[f32],
    sample_rate: u32,
    fft_size: FftSize,
    window: SpectrumWindow,
    smoothing: SpectrumSmoothing,
    peak_mode: bool,
    peak_hold: Option<&[f32]>,
) -> Option<SpectrumSnapshot> {
    let size = fft_size.size();
    if mono.len() < size || sample_rate == 0 {
        return None;
    }
    let hop = size / 4;
    let win = window_table(window, size);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(size);
    let mut scratch = vec![Complex32::new(0.0, 0.0); fft.get_inplace_scratch_len()];
    let mut buf = vec![Complex32::new(0.0, 0.0); size];
    let bins = size / 2;
    let mut acc = vec![0.0_f32; bins];
    let mut frames = 0usize;
    let mut pos = 0usize;
    while pos + size <= mono.len() {
        for i in 0..size {
            buf[i] = Complex32::new(mono[pos + i] * win[i], 0.0);
        }
        fft.process_with_scratch(&mut buf, &mut scratch);
        for bin in 0..bins {
            let mag = buf[bin].norm();
            if peak_mode {
                acc[bin] = acc[bin].max(mag);
            } else {
                acc[bin] += mag;
            }
        }
        frames += 1;
        pos += hop;
        if peak_mode && frames > 64 && pos + size * 4 < mono.len() {
            pos += hop * 3;
        }
    }
    if frames == 0 {
        return None;
    }
    if !peak_mode {
        for mag in &mut acc {
            *mag /= frames as f32;
        }
    }
    let mut magnitudes_db: Vec<f32> = acc.iter().map(|m| to_db(*m, size)).collect();
    if let Some(fraction) = smoothing.fraction() {
        magnitudes_db = octave_smooth(&magnitudes_db, sample_rate, size, fraction);
    }
    let mut peak_hold_db = magnitudes_db.clone();
    if let Some(prev) = peak_hold {
        for (i, db) in peak_hold_db.iter_mut().enumerate() {
            if let Some(old) = prev.get(i) {
                *db = db.max(*old);
            }
        }
    }
    Some(SpectrumSnapshot {
        sample_rate,
        fft_size: size,
        magnitudes_db,
        peak_hold_db,
    })
}

/// One FFT of a window taken from a ring of recent stereo frames.
pub fn analyze_ring_window(
    left: &[f32],
    right: &[f32],
    sample_rate: u32,
    fft_size: FftSize,
    window: SpectrumWindow,
    smoothing: SpectrumSmoothing,
    peak_hold: Option<&[f32]>,
) -> Option<SpectrumSnapshot> {
    let n = left.len().min(right.len());
    if n == 0 {
        return None;
    }
    let mono: Vec<f32> = (0..n).map(|i| (left[i] + right[i]) * 0.5).collect();
    analyze_spectrum(
        &mono,
        sample_rate,
        fft_size,
        window,
        smoothing,
        false,
        peak_hold,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_peaks_near_its_bin() {
        let sr = 48_000u32;
        let freq = 1_000.0_f32;
        let mono: Vec<f32> = (0..8_192)
            .map(|i| (std::f32::consts::TAU * freq * i as f32 / sr as f32).sin())
            .collect();
        let snap = analyze_spectrum(
            &mono,
            sr,
            FftSize::N2048,
            SpectrumWindow::Hann,
            SpectrumSmoothing::None,
            false,
            None,
        )
        .expect("spectrum");
        let bin = (freq * 2048.0 / sr as f32).round() as usize;
        let peak_bin = snap
            .magnitudes_db
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i)
            .unwrap();
        assert!((peak_bin as i32 - bin as i32).abs() <= 2);
    }
}
