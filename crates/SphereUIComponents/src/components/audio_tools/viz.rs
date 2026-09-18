//! DSP visualizations for audio tool windows.
//!
//! Each view is a single `canvas` so enlarging the window grows plot area, not
//! spacing. Analyzer curves are remapped in display space and painted as
//! anti-aliased contours — FFT bin count never equals on-screen rectangles.

use gpui::{canvas, fill, point, px, size, Bounds, IntoElement, Pixels, Styled, Window};

use crate::theme::Colors;

use super::graph::{self, plot_canvas, AnalyzerPlot, SpectrumLayer};

pub use super::graph::{DisplaySmoothing, GraphDraw, GraphStyle};

const DB_FLOOR: f32 = graph::DB_FLOOR;
const DB_CEIL: f32 = graph::DB_CEIL;

pub fn hop_levels(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }
    let hop = (samples.len() / buckets).max(1);
    (0..buckets)
        .map(|i| {
            let start = i * hop;
            let end = (start + hop).min(samples.len());
            if start >= end {
                return 0.0;
            }
            let mut acc = 0.0f32;
            for sample in &samples[start..end] {
                acc += *sample * *sample;
            }
            (acc / (end - start) as f32).sqrt()
        })
        .collect()
}

pub fn lin_to_db(lin: f32) -> f32 {
    if !lin.is_finite() || lin <= 1.0e-9 {
        DB_FLOOR
    } else {
        (20.0 * lin.abs().log10()).clamp(DB_FLOOR, DB_CEIL)
    }
}

fn canvas_size(bounds: Bounds<Pixels>) -> (f32, f32) {
    (
        f32::from(bounds.size.width).max(0.0),
        f32::from(bounds.size.height).max(0.0),
    )
}

fn quad(
    window: &mut Window,
    bounds: Bounds<Pixels>,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    color: gpui::Rgba,
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    window.paint_quad(fill(
        Bounds::new(bounds.origin + point(px(x), px(y)), size(px(w), px(h))),
        color,
    ));
}

fn db_y(db: f32, height: f32) -> f32 {
    let t = ((db - DB_FLOOR) / (DB_CEIL - DB_FLOOR)).clamp(0.0, 1.0);
    height * (1.0 - t)
}

pub fn spectrum_view(
    magnitudes_db: &[f32],
    averaged_db: &[f32],
    peak_hold_db: &[f32],
    sample_rate: u32,
    draw: GraphDraw,
) -> impl IntoElement {
    let overlay = matches!(draw.style, GraphStyle::Overlay);
    let mut layers = Vec::new();
    if !peak_hold_db.is_empty() {
        layers.push(SpectrumLayer {
            label: "Hold",
            db: peak_hold_db.to_vec(),
            color: Colors::status_warning(),
            fill_alpha: 0.0,
            line: true,
            fill: false,
        });
    }
    if overlay && !averaged_db.is_empty() {
        layers.push(SpectrumLayer {
            label: "Avg",
            db: averaged_db.to_vec(),
            color: Colors::status_success(),
            fill_alpha: 0.12,
            line: true,
            fill: true,
        });
    }
    layers.push(SpectrumLayer {
        label: "In",
        db: magnitudes_db.to_vec(),
        color: Colors::accent_primary(),
        fill_alpha: 0.22,
        line: true,
        fill: true,
    });
    plot_canvas(AnalyzerPlot {
        sample_rate,
        layers,
        draw,
        ..AnalyzerPlot::default()
    })
}

pub fn noise_profile_view(
    magnitudes_db: &[f32],
    profile: &[f32],
    sample_rate: u32,
    reduction_db: f32,
    threshold_db: f32,
    draw: GraphDraw,
) -> impl IntoElement {
    let mut layers = vec![SpectrumLayer {
        label: "In",
        db: magnitudes_db.to_vec(),
        color: Colors::accent_primary(),
        fill_alpha: 0.20,
        line: true,
        fill: true,
    }];
    if !profile.is_empty() {
        let profile_db: Vec<f32> = profile.iter().copied().map(lin_to_db).collect();
        let reduced: Vec<f32> = profile_db
            .iter()
            .map(|db| db - reduction_db.max(0.0))
            .collect();
        layers.push(SpectrumLayer {
            label: "Profile",
            db: profile_db,
            color: Colors::status_warning(),
            fill_alpha: 0.10,
            line: true,
            fill: true,
        });
        layers.push(SpectrumLayer {
            label: "Floor",
            db: reduced,
            color: Colors::status_success(),
            fill_alpha: 0.0,
            line: true,
            fill: false,
        });
    }
    plot_canvas(AnalyzerPlot {
        sample_rate,
        layers,
        threshold_db: Some(threshold_db),
        draw,
        ..AnalyzerPlot::default()
    })
}

pub fn hum_harmonics_view(
    magnitudes_db: &[f32],
    sample_rate: u32,
    base_hz: f32,
    harmonics: u8,
    draw: GraphDraw,
) -> impl IntoElement {
    let harmonic_hz = (1..=harmonics.max(1)).map(|n| base_hz * n as f32).collect();
    plot_canvas(AnalyzerPlot {
        sample_rate,
        layers: vec![SpectrumLayer {
            label: "In",
            db: magnitudes_db.to_vec(),
            color: Colors::accent_primary(),
            fill_alpha: 0.20,
            line: true,
            fill: true,
        }],
        harmonic_hz,
        draw,
        ..AnalyzerPlot::default()
    })
}

pub fn spectral_gain_view(
    magnitudes_db: &[f32],
    sample_rate: u32,
    min_hz: f32,
    max_hz: f32,
    gain_db: f32,
    draw: GraphDraw,
) -> impl IntoElement {
    plot_canvas(AnalyzerPlot {
        sample_rate,
        layers: vec![SpectrumLayer {
            label: "In",
            db: magnitudes_db.to_vec(),
            color: Colors::accent_primary(),
            fill_alpha: 0.20,
            line: true,
            fill: true,
        }],
        threshold_db: Some(gain_db),
        band: Some((min_hz.max(20.0), max_hz)),
        draw,
        ..AnalyzerPlot::default()
    })
}

pub fn loudness_view(
    history: &[f32],
    momentary: f32,
    shortterm: f32,
    integrated: f32,
    true_peak: f32,
    target: Option<f32>,
) -> impl IntoElement {
    let history = history.to_vec();
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 8.0 || height < 8.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            let meter_w = 64.0_f32.min(width * 0.22);
            let gap = 4.0;
            let hist_x = meter_w + gap;
            let hist_w = (width - hist_x).max(0.0);
            let meters = [
                (momentary, Colors::accent_primary()),
                (shortterm, Colors::meter_low()),
                (integrated, Colors::accent_purple()),
                (true_peak, Colors::meter_high()),
            ];
            let bar_w = ((meter_w - 3.0) / 4.0).max(4.0);
            for (i, (value, color)) in meters.iter().enumerate() {
                let x = i as f32 * (bar_w + 1.0);
                quad(window, bounds, x, 0.0, bar_w, height, Colors::meter_rail());
                let y = db_y(*value, height);
                quad(window, bounds, x, y, bar_w, height - y, *color);
            }
            if hist_w > 2.0 {
                if let Some(target) = target {
                    let y = db_y(target, height);
                    quad(
                        window,
                        bounds,
                        hist_x,
                        y,
                        hist_w,
                        1.0,
                        Colors::with_alpha(Colors::status_warning(), 0.8),
                    );
                }
                if !history.is_empty() {
                    let hist_bounds = Bounds::new(
                        bounds.origin + point(px(hist_x), px(0.0)),
                        size(px(hist_w), px(height)),
                    );
                    let db_series: Vec<f32> = history
                        .iter()
                        .map(|level| {
                            ((lin_to_db(*level) - DB_FLOOR) / (DB_CEIL - DB_FLOOR)).clamp(0.0, 1.0)
                        })
                        .collect();
                    graph::paint_series_contour(
                        window,
                        hist_bounds,
                        &db_series,
                        Colors::accent_primary(),
                        0.22,
                    );
                }
            }
        },
    )
    .size_full()
}

pub fn envelope_markers_view(
    envelope: &[f32],
    marker_norm: &[(f32, f32)],
    highlight: Option<usize>,
) -> impl IntoElement {
    let envelope = envelope.to_vec();
    let markers = marker_norm.to_vec();
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 2.0 || height < 2.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            if envelope.len() >= 2 {
                graph::paint_series_contour(
                    window,
                    bounds,
                    &envelope,
                    Colors::accent_primary(),
                    0.22,
                );
            }
            for (i, (pos, strength)) in markers.iter().enumerate() {
                let x = pos.clamp(0.0, 1.0) * width;
                let selected = highlight == Some(i);
                let alpha = if selected {
                    0.95
                } else {
                    0.35 + strength.clamp(0.0, 1.0) * 0.5
                };
                let w = if selected { 2.0 } else { 1.0 };
                quad(
                    window,
                    bounds,
                    x,
                    0.0,
                    w,
                    height,
                    Colors::with_alpha(Colors::status_warning(), alpha),
                );
            }
        },
    )
    .size_full()
}

pub fn goniometer_view(
    trail: &[(f32, f32)],
    correlation: f32,
    correlation_history: &[f32],
) -> impl IntoElement {
    let trail = trail.to_vec();
    let history = correlation_history.to_vec();
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 8.0 || height < 8.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            let meter_w = 28.0_f32.min(width * 0.18);
            let plot = (width - meter_w - 6.0).min(height);
            let ox = ((width - meter_w - 6.0 - plot) * 0.5).max(0.0);
            let oy = ((height - plot) * 0.5).max(0.0);
            let axis = Colors::with_alpha(Colors::text_faint(), 0.35);
            quad(window, bounds, ox, oy + plot * 0.5, plot, 1.0, axis);
            quad(window, bounds, ox + plot * 0.5, oy, 1.0, plot, axis);
            let n = trail.len().max(1);
            for (i, (x, y)) in trail.iter().enumerate() {
                let px_x = ox + (*x * 0.5 + 0.5).clamp(0.0, 1.0) * plot;
                let px_y = oy + (1.0 - (*y * 0.5 + 0.5).clamp(0.0, 1.0)) * plot;
                let a = 0.15 + (i as f32 / n as f32) * 0.85;
                quad(
                    window,
                    bounds,
                    px_x,
                    px_y,
                    2.0,
                    2.0,
                    Colors::with_alpha(Colors::accent_primary(), a),
                );
            }
            let mx = width - meter_w;
            quad(
                window,
                bounds,
                mx,
                0.0,
                meter_w,
                height,
                Colors::meter_rail(),
            );
            let mid = height * 0.5;
            let corr = correlation.clamp(-1.0, 1.0);
            let bar_h = corr.abs() * mid;
            let y = if corr >= 0.0 { mid - bar_h } else { mid };
            let color = if corr >= 0.0 {
                Colors::meter_low()
            } else {
                Colors::meter_high()
            };
            quad(
                window,
                bounds,
                mx + 4.0,
                y,
                meter_w - 8.0,
                bar_h.max(1.0),
                color,
            );
            quad(window, bounds, mx, mid, meter_w, 1.0, Colors::text_faint());
            if history.len() > 1 {
                let n = history.len();
                for (i, value) in history.iter().enumerate() {
                    let x = ox + (i as f32 / (n - 1) as f32) * plot;
                    let y = oy + (1.0 - (*value * 0.5 + 0.5).clamp(0.0, 1.0)) * plot;
                    quad(
                        window,
                        bounds,
                        x,
                        y,
                        1.0,
                        1.0,
                        Colors::with_alpha(Colors::status_warning(), 0.5),
                    );
                }
            }
        },
    )
    .size_full()
}

pub fn dc_view(left: f32, right: f32, envelope: &[f32]) -> impl IntoElement {
    let envelope = envelope.to_vec();
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 4.0 || height < 4.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            let mid = height * 0.5;
            quad(
                window,
                bounds,
                0.0,
                mid,
                width,
                1.0,
                Colors::with_alpha(Colors::text_faint(), 0.35),
            );
            if !envelope.is_empty() {
                let n = envelope.len();
                let col_w = (width / n as f32).max(1.0);
                let peak = envelope.iter().copied().fold(1.0e-4, f32::max);
                for (i, level) in envelope.iter().enumerate() {
                    let h = (*level / peak).clamp(0.0, 1.0) * height * 0.42;
                    quad(
                        window,
                        bounds,
                        i as f32 * col_w,
                        mid - h,
                        col_w.max(1.0),
                        h * 2.0,
                        Colors::with_alpha(Colors::accent_primary(), 0.45),
                    );
                }
            }
            let scale = 8.0;
            let ly = mid - left.clamp(-0.25, 0.25) * height * scale;
            let ry = mid - right.clamp(-0.25, 0.25) * height * scale;
            quad(
                window,
                bounds,
                0.0,
                ly,
                width,
                2.0,
                Colors::accent_primary(),
            );
            quad(
                window,
                bounds,
                0.0,
                ry,
                width,
                2.0,
                Colors::status_warning(),
            );
        },
    )
    .size_full()
}

pub fn before_after_bars(before_db: f32, after_db: f32, target_db: f32) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 4.0 || height < 4.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            let target_y = db_y(target_db, height);
            quad(
                window,
                bounds,
                0.0,
                target_y,
                width,
                1.0,
                Colors::with_alpha(Colors::status_warning(), 0.85),
            );
            let bar_w = (width * 0.28).max(12.0);
            let gap = (width - bar_w * 2.0) / 3.0;
            let by = db_y(before_db, height);
            let ay = db_y(after_db, height);
            quad(
                window,
                bounds,
                gap,
                0.0,
                bar_w,
                height,
                Colors::meter_rail(),
            );
            quad(
                window,
                bounds,
                gap,
                by,
                bar_w,
                height - by,
                Colors::with_alpha(Colors::text_muted(), 0.85),
            );
            quad(
                window,
                bounds,
                gap * 2.0 + bar_w,
                0.0,
                bar_w,
                height,
                Colors::meter_rail(),
            );
            quad(
                window,
                bounds,
                gap * 2.0 + bar_w,
                ay,
                bar_w,
                height - ay,
                Colors::accent_primary(),
            );
        },
    )
    .size_full()
}

pub fn ratio_view(before: f32, after: f32) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 4.0 || height < 4.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            let max = before.max(after).max(0.01);
            let row_h = (height * 0.28).max(8.0);
            let y0 = height * 0.22;
            let y1 = height * 0.58;
            quad(
                window,
                bounds,
                8.0,
                y0,
                (before / max) * (width - 16.0),
                row_h,
                Colors::with_alpha(Colors::text_muted(), 0.85),
            );
            quad(
                window,
                bounds,
                8.0,
                y1,
                (after / max) * (width - 16.0),
                row_h,
                Colors::accent_primary(),
            );
        },
    )
    .size_full()
}

pub fn candidate_bars(values: &[(f32, f32)], highlight: usize) -> impl IntoElement {
    let values = values.to_vec();
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 4.0 || height < 4.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            if values.is_empty() {
                return;
            }
            let n = values.len() as f32;
            let gap = 4.0;
            let bar_w = ((width - gap * (n + 1.0)) / n).max(6.0);
            let peak = values.iter().map(|(_, c)| *c).fold(1.0e-4_f32, f32::max);
            for (i, (_, confidence)) in values.iter().enumerate() {
                let x = gap + i as f32 * (bar_w + gap);
                let h = (*confidence / peak).clamp(0.0, 1.0) * (height - 8.0);
                let color = if i == highlight {
                    Colors::accent_primary()
                } else {
                    Colors::with_alpha(Colors::accent_primary(), 0.4)
                };
                quad(window, bounds, x, height - h, bar_w, h, color);
            }
        },
    )
    .size_full()
}

pub fn pitch_class_view(tonic: u8, confidences: &[f32]) -> impl IntoElement {
    let confidences = {
        let mut bins = [0.0f32; 12];
        for (i, value) in confidences.iter().take(12).enumerate() {
            bins[i] = *value;
        }
        bins
    };
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 4.0 || height < 4.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            let bar_w = (width / 12.0).max(2.0);
            let peak = confidences.into_iter().fold(1.0e-4_f32, f32::max);
            let black = [1usize, 3, 6, 8, 10];
            for (i, confidence) in confidences.iter().enumerate() {
                let h = (*confidence / peak).clamp(0.0, 1.0) * (height - 6.0);
                let is_black = black.contains(&i);
                let mut color = if is_black {
                    Colors::with_alpha(Colors::text_muted(), 0.7)
                } else {
                    Colors::with_alpha(Colors::text_primary(), 0.45)
                };
                if i as u8 == tonic {
                    color = Colors::accent_primary();
                }
                quad(
                    window,
                    bounds,
                    i as f32 * bar_w + 1.0,
                    height - h,
                    (bar_w - 2.0).max(1.0),
                    h.max(1.0),
                    color,
                );
            }
        },
    )
    .size_full()
}

pub fn channel_matrix_view(mode_index: usize) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 4.0 || height < 4.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            let routes: [[f32; 4]; 10] = [
                [1.0, 0.0, 0.0, 1.0], // Stereo L->L R->R
                [1.0, 1.0, 0.0, 0.0], // Left only
                [0.0, 0.0, 1.0, 1.0], // Right only
                [0.5, 0.5, 0.5, 0.5], // Mono sum
                [0.0, 1.0, 1.0, 0.0], // Swap
                [-1.0, 0.0, 0.0, 1.0],
                [1.0, 0.0, 0.0, -1.0],
                [-1.0, 0.0, 0.0, -1.0],
                [0.5, 0.5, 0.5, 0.5],
                [0.5, -0.5, -0.5, 0.5],
            ];
            let idx = mode_index.min(routes.len() - 1);
            let m = routes[idx];
            let cell = height.min(width) * 0.28;
            let labels = [
                (0.0, 0.0, m[0]),
                (0.0, 1.0, m[1]),
                (1.0, 0.0, m[2]),
                (1.0, 1.0, m[3]),
            ];
            let ox = width * 0.28;
            let oy = height * 0.22;
            for (col, row, gain) in labels {
                let x = ox + col * (cell + 10.0);
                let y = oy + row * (cell + 10.0);
                let alpha = gain.abs().clamp(0.08, 1.0);
                let color = if gain < 0.0 {
                    Colors::with_alpha(Colors::status_error(), alpha)
                } else {
                    Colors::with_alpha(Colors::accent_primary(), alpha)
                };
                quad(window, bounds, x, y, cell, cell, color);
            }
        },
    )
    .size_full()
}

pub fn resample_view(
    current_hz: f32,
    target_hz: f32,
    magnitudes_db: &[f32],
    draw: GraphDraw,
) -> impl IntoElement {
    plot_canvas(AnalyzerPlot {
        sample_rate: current_hz.max(1.0) as u32,
        layers: vec![SpectrumLayer {
            label: "In",
            db: magnitudes_db.to_vec(),
            color: Colors::accent_primary(),
            fill_alpha: 0.20,
            line: true,
            fill: true,
        }],
        harmonic_hz: vec![current_hz.max(1.0) * 0.5, target_hz.max(1.0) * 0.5],
        draw,
        ..AnalyzerPlot::default()
    })
}

pub fn time_pitch_view(stretch: f32, semitones: f32) -> impl IntoElement {
    canvas(
        |_bounds, _window, _cx| {},
        move |bounds, (), window, _cx| {
            let (width, height) = canvas_size(bounds);
            if width < 4.0 || height < 4.0 {
                return;
            }
            quad(
                window,
                bounds,
                0.0,
                0.0,
                width,
                height,
                Colors::surface_canvas(),
            );
            let orig = (width * 0.62).max(16.0);
            let stretched = (orig * stretch.clamp(0.05, 4.0)).min(width - 16.0);
            quad(
                window,
                bounds,
                8.0,
                height * 0.18,
                orig,
                height * 0.16,
                Colors::with_alpha(Colors::text_muted(), 0.8),
            );
            quad(
                window,
                bounds,
                8.0,
                height * 0.40,
                stretched,
                height * 0.16,
                Colors::accent_primary(),
            );
            let keyboard_y = height * 0.68;
            let key_h = height * 0.22;
            let keys = 25.0;
            let key_w = ((width - 16.0) / keys).max(2.0);
            let center = 12;
            let shifted = (12.0 + semitones.round()).clamp(0.0, 24.0) as i32;
            for i in 0..25 {
                let black = matches!(i % 12, 1 | 3 | 6 | 8 | 10);
                let mut color = if black {
                    Colors::with_alpha(Colors::text_faint(), 0.7)
                } else {
                    Colors::with_alpha(Colors::text_primary(), 0.28)
                };
                if i == center {
                    color = Colors::with_alpha(Colors::text_primary(), 0.55);
                }
                if i == shifted {
                    color = Colors::accent_primary();
                }
                quad(
                    window,
                    bounds,
                    8.0 + i as f32 * key_w,
                    keyboard_y,
                    (key_w - 1.0).max(1.0),
                    key_h,
                    color,
                );
            }
        },
    )
    .size_full()
}

#[cfg(test)]
mod tests {
    use super::hop_levels;

    #[test]
    fn hop_levels_covers_the_buffer() {
        let samples: Vec<f32> = (0..1000)
            .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
            .collect();
        let hops = hop_levels(&samples, 10);
        assert_eq!(hops.len(), 10);
        assert!(hops.iter().all(|v| *v > 0.4));
    }
}
