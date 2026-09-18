//! The Audio Repair module navigator.
//!
//! A labeled vertical list, not an abbreviation rail: the module name is the
//! primary identifier and the glyph supports it. One registration table drives
//! order, labels, icons and availability, so adding a repair module is a row in
//! [`MODULES`] plus an SVG file — no layout code changes.
//!
//! Ownership: this module owns the navigator's geometry and row states. The
//! selected module and the per-row focus handles live on `RepairSurface`;
//! activation goes back through `AudioToolWindow`.

use gpui::prelude::FluentBuilder;
use gpui::{
    div, px, svg, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    ParentElement, Role, StatefulInteractiveElement, Styled,
};
use sphere_audio_editor::AudioRepairModule;

use crate::assets;
use crate::components::controls::{fb_section_label, fb_tooltip};
use crate::theme::{elevation, radius, size, space, typography, Colors};

use super::window::AudioToolWindow;

/// Navigator width. Sized so the longest label ("Noise Reduction") sits beside
/// its glyph and the unavailable marker without truncating.
pub(super) const MODULE_LIST_WIDTH: f32 = 164.0;
/// Glyph box. The family is drawn on an 18 px grid and reads down to this size.
const ICON_SIZE: f32 = 16.0;
/// Leading selected marker, matching the app's other sidebar rails.
const MARKER_W: f32 = 2.0;

/// One module's static registration.
#[derive(Clone, Copy)]
pub(super) struct AudioRepairModuleMeta {
    pub id: AudioRepairModule,
    pub label: &'static str,
    pub short_label: &'static str,
    pub icon: &'static str,
    pub available: bool,
}

/// Build a row from its id.
///
/// Labels and availability are read off the id rather than typed again here, so
/// the navigator cannot advertise a name the rest of the tool does not use, or
/// offer a processor the commit path would refuse to run.
const fn meta(id: AudioRepairModule, icon: &'static str) -> AudioRepairModuleMeta {
    AudioRepairModuleMeta {
        id,
        label: id.label(),
        short_label: id.short_label(),
        icon,
        available: id.is_available(),
    }
}

/// The registered modules, in navigator order.
pub(super) const MODULES: [AudioRepairModuleMeta; AudioRepairModule::ALL.len()] = [
    meta(
        AudioRepairModule::Denoise,
        assets::ICON_REPAIR_NOISE_REDUCTION_PATH,
    ),
    meta(
        AudioRepairModule::DeClick,
        assets::ICON_REPAIR_DE_CLICK_PATH,
    ),
    meta(AudioRepairModule::DeHum, assets::ICON_REPAIR_DE_HUM_PATH),
    meta(
        AudioRepairModule::SpectralRepair,
        assets::ICON_REPAIR_SPECTRAL_PATH,
    ),
    meta(
        AudioRepairModule::DeReverb,
        assets::ICON_REPAIR_DE_REVERB_PATH,
    ),
    meta(
        AudioRepairModule::DeBleed,
        assets::ICON_REPAIR_DE_BLEED_PATH,
    ),
    meta(
        AudioRepairModule::DeFeedback,
        assets::ICON_REPAIR_DE_FEEDBACK_PATH,
    ),
    meta(
        AudioRepairModule::DrumSilencer,
        assets::ICON_REPAIR_DRUM_SILENCER_PATH,
    ),
];

pub(super) fn index_of(module: AudioRepairModule) -> usize {
    MODULES
        .iter()
        .position(|meta| meta.id == module)
        .unwrap_or(0)
}

/// The next row the keyboard can land on, or `None` at the ends.
///
/// Unimplemented modules are skipped: they stay visible for the roadmap but
/// parking focus on something that cannot be activated is a dead end.
pub(super) fn step_selectable(from: usize, delta: i32) -> Option<usize> {
    let mut index = from as i32;
    loop {
        index += delta;
        let candidate = usize::try_from(index).ok().filter(|i| *i < MODULES.len())?;
        if MODULES[candidate].available {
            return Some(candidate);
        }
    }
}

/// The module navigator: a section label over one row per registered module.
pub(super) fn repair_module_list(
    active: AudioRepairModule,
    focus: &[FocusHandle],
    cx: &mut Context<AudioToolWindow>,
) -> impl IntoElement {
    div()
        .id("repair-modules")
        .flex_none()
        .w(px(MODULE_LIST_WIDTH))
        .h_full()
        .flex()
        .flex_col()
        .gap(px(space::HAIR))
        .py(px(space::SNUG))
        .bg(Colors::surface_sidebar())
        .border_r(px(1.0))
        .border_color(Colors::border_subtle())
        .overflow_y_scroll()
        .child(
            div()
                .px(px(space::SNUG + space::TIGHT))
                .pb(px(space::TIGHT))
                .child(fb_section_label("MODULES")),
        )
        .children(
            MODULES
                .iter()
                .zip(focus)
                .enumerate()
                .map(|(index, (meta, handle))| {
                    repair_module_item(*meta, index, meta.id == active, handle, cx)
                }),
        )
}

/// One navigator row: `[icon] [label]`, plus the unavailable marker.
fn repair_module_item(
    meta: AudioRepairModuleMeta,
    index: usize,
    active: bool,
    focus: &FocusHandle,
    cx: &mut Context<AudioToolWindow>,
) -> impl IntoElement {
    let plane = Colors::surface_sidebar();
    let rest = if active {
        Colors::composite(plane, Colors::state_selected())
    } else {
        Colors::with_alpha(plane, 0.0)
    };
    // Hover lifts the row's own rest fill; a selected row keeps its tint.
    let hover_base = if active { rest } else { plane };
    let hover = Colors::composite(hover_base, Colors::state_hover());
    let pressed = Colors::composite(hover_base, Colors::state_recessed());
    let ring = Colors::state_focus_ring();
    let (label_tint, icon_tint) = if !meta.available {
        (Colors::text_disabled(), Colors::text_disabled())
    } else if active {
        (Colors::text_primary(), Colors::accent_primary())
    } else {
        (Colors::text_secondary(), Colors::text_muted())
    };
    let tooltip = if meta.available {
        meta.id.canvas_hint()
    } else {
        "Not available in this build yet."
    };

    div()
        .id(("repair-module", index))
        .role(Role::Button)
        .aria_label(meta.label)
        .aria_selected(active)
        .aria_disabled(!meta.available)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(space::SNUG))
        .h(px(size::DEFAULT))
        .mx(px(space::TIGHT))
        .px(px(space::SNUG))
        .rounded(px(radius::CONTROL))
        .bg(rest)
        .tooltip(fb_tooltip(tooltip))
        .child(
            svg()
                .path(meta.icon)
                .size(px(ICON_SIZE))
                .flex_none()
                .text_color(icon_tint),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(typography::UI_XS))
                .font_weight(if active {
                    gpui::FontWeight::SEMIBOLD
                } else {
                    gpui::FontWeight::MEDIUM
                })
                .text_color(label_tint)
                .child(meta.label),
        )
        .when(!meta.available, |row| {
            row.child(
                div()
                    .flex_none()
                    .text_size(px(typography::DENSE_CAPTION))
                    .text_color(Colors::text_faint())
                    .child("Soon"),
            )
        })
        .when(active, |row| {
            // Leading accent marker, overlaid rather than bordered: growing a
            // border would reflow the row every time selection moved. Inset by
            // the corner radius so it rides the straight part of the edge.
            row.child(
                div()
                    .absolute()
                    .left_0()
                    .top(px(radius::CONTROL))
                    .bottom(px(radius::CONTROL))
                    .w(px(MARKER_W))
                    .bg(Colors::accent_primary()),
            )
        })
        .when(meta.available, |row| {
            row.track_focus(focus)
                .tab_stop(true)
                .focus_visible(move |style| style.shadow(elevation::focus_ring(ring)))
                .cursor(gpui::CursorStyle::PointingHand)
                .hover(move |style| style.bg(hover))
                .active(move |style| style.bg(pressed))
                .on_click(cx.listener(move |this, _, window, cx| {
                    // Clicking also moves the keyboard position, so arrow keys
                    // continue from where the pointer left off.
                    this.focus_repair_module(index, window, cx);
                    this.activate_repair_module(meta.id, cx);
                }))
                .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                    match event.keystroke.key.as_str() {
                        "enter" | "numpad_enter" | "space" => {
                            cx.stop_propagation();
                            this.activate_repair_module(meta.id, cx);
                        }
                        "up" | "arrow_up" => {
                            cx.stop_propagation();
                            this.step_repair_module_focus(index, -1, window, cx);
                        }
                        "down" | "arrow_down" => {
                            cx.stop_propagation();
                            this.step_repair_module_focus(index, 1, window, cx);
                        }
                        _ => {}
                    }
                }))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_module_is_registered_once_in_navigator_order() {
        assert_eq!(MODULES.len(), AudioRepairModule::ALL.len());
        for (meta, expected) in MODULES.iter().zip(AudioRepairModule::ALL) {
            assert_eq!(meta.id, expected);
        }
    }

    #[test]
    fn rows_carry_a_readable_label_not_an_initialism() {
        for meta in MODULES {
            assert!(
                meta.label.len() > 4,
                "{} is too short to recognize",
                meta.label
            );
            assert!(!meta.short_label.is_empty());
        }
    }

    #[test]
    fn every_row_resolves_an_external_icon_asset() {
        for meta in MODULES {
            assert!(
                meta.icon.starts_with("icons/audio_repair/"),
                "{} is not an audio_repair asset",
                meta.icon
            );
            assert!(
                assets::audio_repair_icon(meta.icon).is_some_and(|svg| svg.contains("<svg")),
                "{} does not resolve to an SVG",
                meta.icon
            );
        }
    }

    #[test]
    fn keyboard_navigation_skips_unavailable_modules_and_stops_at_the_ends() {
        let first_unavailable = MODULES
            .iter()
            .position(|meta| !meta.available)
            .expect("the roadmap rows are part of the list");
        assert_eq!(step_selectable(0, -1), None);
        assert_eq!(step_selectable(first_unavailable - 1, 1), None);
        for index in 0..first_unavailable.saturating_sub(1) {
            assert_eq!(step_selectable(index, 1), Some(index + 1));
        }
    }
}
