// SPDX-License-Identifier: MPL-2.0

//! Shared treatment for standalone icon-only buttons.
//!
//! Geometry and paint deliberately match the caption `+` and pane toggles:
//! one square hover chip, a dim resting glyph, a normal hover glyph, and the
//! filled Phosphor family for an on state.

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub struct State {
    pub hovered: bool,
    pub active: bool,
    pub disabled: bool,
}

/// The hover treatment. `Default` is the neutral tint; `Danger` is the
/// caption Close's red, kept as a tone of the same button rather than a
/// second component.
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq)]
pub enum Tone {
    #[default]
    Default,
    Danger,
}

/// An icon control with a typed click result.
///
/// The widget deliberately returns an action instead of storing a callback:
/// drawing must stay independent from the application that gives a click its
/// meaning.  A caller can still read naturally as
/// `IconButton::new(Icon::Pane, rect).on_click(Action::ToggleContext)` while
/// the UI crate remains unaware of windows, documents, and menus.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct IconButton<Action = ()> {
    glyph: char,
    slot: Rect,
    action: Action,
    state: State,
    tone: Tone,
}

impl IconButton {
    pub const fn new(glyph: char, slot: Rect) -> Self {
        Self {
            glyph,
            slot,
            action: (),
            state: State {
                hovered: false,
                active: false,
                disabled: false,
            },
            tone: Tone::Default,
        }
    }
}

impl<Action> IconButton<Action> {
    pub fn on_click<Next>(self, action: Next) -> IconButton<Next> {
        IconButton {
            glyph: self.glyph,
            slot: self.slot,
            action,
            state: self.state,
            tone: self.tone,
        }
    }

    pub const fn with_state(mut self, state: State) -> Self {
        self.state = state;
        self
    }

    pub const fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub const fn rect(&self) -> Rect {
        self.slot
    }

    /// The glyph the button shows — for a caller, or a test, reading back
    /// what a toolbar is drawing.
    pub const fn glyph(&self) -> char {
        self.glyph
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.slot.contains(x, y)
    }

    pub fn action_at(self, x: f32, y: f32) -> Option<Action>
    where
        Action: Copy,
    {
        (!self.state.disabled && self.contains(x, y)).then_some(self.action)
    }

    #[track_caller]
    pub fn draw(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine) {
        draw_toned(
            canvas, root, engine, self.slot, self.glyph, self.state, self.tone,
        );
    }
}

/// Geometry and type size for an icon-only control.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Metrics {
    pub icon_size: f32,
    /// Recommended clear space on either horizontal side of the glyph.
    pub padding_x: f32,
    /// Recommended clear space above and below the glyph.
    pub padding_y: f32,
    pub inset_top: f32,
    pub inset_bottom: f32,
    pub radius: f32,
}

impl Metrics {
    pub const STANDARD: Self = Self {
        icon_size: theme::CAPTION_ICON,
        padding_x: 0.0,
        padding_y: 0.0,
        inset_top: theme::TAB_INSET_TOP,
        inset_bottom: theme::TAB_INSET_BOTTOM,
        radius: 6.0,
    };

    /// Search-panel controls: 10% smaller glyphs with 4 px on every side.
    pub const COMPACT: Self = Self {
        icon_size: theme::CAPTION_ICON * 0.90,
        padding_x: 4.0,
        padding_y: 4.0,
        inset_top: 0.0,
        inset_bottom: 0.0,
        radius: 6.0,
    };

    pub const fn control_width(self) -> f32 {
        self.icon_size + self.padding_x * 2.0
    }

    pub const fn control_height(self) -> f32 {
        self.icon_size + self.padding_y * 2.0
    }
}

/// The visual chip inside a button's full hit slot.
pub fn hover_chip(slot: Rect) -> Rect {
    hover_chip_with(slot, Metrics::STANDARD)
}

pub fn hover_chip_with(slot: Rect, metrics: Metrics) -> Rect {
    let h = (slot.h - metrics.inset_top - metrics.inset_bottom).max(0.0);
    let side = h.min(slot.w);
    Rect::new(
        slot.x + (slot.w - side) * 0.5,
        slot.y + metrics.inset_top + (h - side) * 0.5,
        side,
        side,
    )
}

#[track_caller]
pub fn draw(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
) {
    draw_with(canvas, root, engine, slot, glyph, state, Metrics::STANDARD);
}

/// The same control with an explicit [`Tone`] — the caption Close's danger
/// hover without a second component.
#[track_caller]
pub fn draw_toned(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
    tone: Tone,
) {
    paint(
        canvas,
        root,
        engine,
        slot,
        glyph,
        state,
        Metrics::STANDARD,
        tone,
    );
}

#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_with(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
    metrics: Metrics,
) {
    paint(
        canvas,
        root,
        engine,
        slot,
        glyph,
        state,
        metrics,
        Tone::Default,
    );
}

#[allow(clippy::too_many_arguments)]
fn paint(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    glyph: char,
    state: State,
    metrics: Metrics,
    tone: Tone,
) {
    let _inspection = inspect_icon(root, slot, state, metrics, tone, Some(glyph));
    let chip = hover_chip_with(slot, metrics);
    let danger = tone == Tone::Danger;
    if state.active && !state.disabled {
        scene::fill(
            canvas,
            root,
            theme::p()
                .accent
                .multiply_alpha(if state.hovered { 0.20 } else { 0.12 }),
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, metrics.radius),
        );
    } else if state.hovered && !state.disabled {
        scene::fill(
            canvas,
            root,
            if danger {
                theme::p().close_hover
            } else {
                theme::p().hover_tint
            },
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, metrics.radius),
        );
    }
    let layout = if state.active {
        engine.icon_layout_filled(glyph, metrics.icon_size)
    } else {
        engine.icon_layout(glyph, metrics.icon_size)
    };
    let colour = if state.disabled {
        theme::p().text_dim.multiply_alpha(0.45)
    } else if state.active {
        theme::p().accent
    } else if state.hovered {
        if danger {
            theme::p().close_hover_fg
        } else {
            theme::p().text
        }
    } else {
        theme::p().text_dim
    };
    scene::draw_layout(
        canvas,
        root,
        &layout,
        slot.x + (slot.w - layout.width()) * 0.5,
        slot.y + (slot.h - layout.height()) * 0.5,
        Some(colour),
    );
}

/// Text vocabulary such as `Aa` with the same chip, state, and centring as an
/// icon button. The label is content; the control geometry remains shared.
#[allow(clippy::too_many_arguments)]
#[track_caller]
pub fn draw_label_with(
    canvas: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    slot: Rect,
    label: &str,
    style: theme::TextStyle,
    state: State,
    metrics: Metrics,
) {
    let _inspection = inspect_icon(root, slot, state, metrics, Tone::Default, None);
    let chip = hover_chip_with(slot, metrics);
    if state.active && !state.disabled {
        scene::fill(
            canvas,
            root,
            theme::p()
                .accent
                .multiply_alpha(if state.hovered { 0.20 } else { 0.12 }),
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, metrics.radius),
        );
    } else if state.hovered && !state.disabled {
        scene::fill(
            canvas,
            root,
            theme::p().hover_tint,
            &scene::rrect(chip.x, chip.y, chip.w, chip.h, metrics.radius),
        );
    }
    let text = engine.simple_layout(label, style);
    let colour = if state.disabled {
        theme::p().text_dim.multiply_alpha(0.45)
    } else if state.active {
        theme::p().accent
    } else if state.hovered {
        theme::p().text
    } else {
        theme::p().text_dim
    };
    scene::draw_layout_first_line(
        canvas,
        root,
        &text,
        slot.x + (slot.w - text.width()) * 0.5,
        crate::text_field::baseline_centred_y(&text, slot.y, slot.h),
        Some(colour),
    );
}

/// Close is vocabulary, not a separate visual treatment.
#[track_caller]
fn inspect_icon(
    root: Affine,
    slot: Rect,
    state: State,
    metrics: Metrics,
    tone: Tone,
    glyph: Option<char>,
) -> koru_draw::inspect::Scope {
    koru_draw::inspect::scope(
        "KoruIconButton",
        "component",
        root,
        scene::rect(slot.x, slot.y, slot.w, slot.h),
        |r| {
            r.property(
                "implementation",
                "koru_ui::icon_button",
                "shared implementation",
            );
            r.property(
                "state",
                format!("{state:?}"),
                "instance state; focus not exposed by this API",
            );
            if tone != Tone::Default {
                r.property(
                    "tone",
                    format!("{tone:?}"),
                    "caller property; the danger hover is the caption Close's",
                );
            }
            r.property(
                "metrics",
                format!("{metrics:?}"),
                if metrics == Metrics::STANDARD {
                    "Metrics::STANDARD"
                } else if metrics == Metrics::COMPACT {
                    "Metrics::COMPACT"
                } else {
                    "caller override; review unknown"
                },
            );
            r.property(
                "hit slot",
                format!("{slot:?}"),
                "caller rectangle; host semantics not implied",
            );
            r.property(
                "paint chip",
                format!("{:?}", hover_chip_with(slot, metrics)),
                "derived by hover_chip_with",
            );
            if let Some(glyph) = glyph {
                if let Some((name, _)) =
                    icons::ALL.iter().find(|(_, codepoint)| *codepoint == glyph)
                {
                    r.property("icon name", name, "icons::ALL; aliases may share a glyph");
                }
                r.property(
                    "Phosphor codepoint",
                    format!("U+{:04X}", glyph as u32),
                    "caller icon; docs/features/koruui/ICONS.md",
                );
            }
            r.property("layout", "centered glyph or label", "shared implementation");
            r.property("docs", "docs/features/koruui/COMPONENTS.md", "catalog");
        },
    )
}

pub const CLOSE: char = icons::WINDOW_CLOSE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chip_is_square_centered_and_clear_of_the_caption_hairline() {
        let slot = Rect::new(10.0, 4.0, 40.0, theme::CAPTION_H);
        let chip = hover_chip(slot);
        assert_eq!(chip.w, chip.h);
        assert!((chip.x + chip.w / 2.0 - (slot.x + slot.w / 2.0)).abs() < 0.01);
        assert!(chip.y > slot.y);
        assert!(chip.y + chip.h < slot.y + slot.h);
    }

    #[test]
    fn compact_metrics_reduce_glyph_and_use_four_pixel_padding() {
        assert_eq!(
            Metrics::COMPACT.icon_size,
            Metrics::STANDARD.icon_size * 0.90
        );
        assert_eq!(Metrics::COMPACT.padding_x, 4.0);
        assert_eq!(Metrics::COMPACT.padding_y, 4.0);
        assert_eq!(
            Metrics::COMPACT.control_width(),
            Metrics::COMPACT.icon_size + 8.0
        );
        assert_eq!(
            Metrics::COMPACT.control_height(),
            Metrics::COMPACT.icon_size + 8.0
        );
    }

    #[test]
    fn typed_click_action_only_arrives_inside_an_enabled_button() {
        #[derive(Copy, Clone, Debug, PartialEq)]
        enum Action {
            TogglePane,
        }

        let button = IconButton::new(icons::PANEL_RIGHT, Rect::new(10.0, 20.0, 28.0, 28.0))
            .on_click(Action::TogglePane);
        assert_eq!(button.action_at(12.0, 22.0), Some(Action::TogglePane));
        assert_eq!(button.action_at(9.0, 22.0), None);

        let disabled = button.with_state(State {
            disabled: true,
            ..State::default()
        });
        assert_eq!(disabled.action_at(12.0, 22.0), None);
    }
}
