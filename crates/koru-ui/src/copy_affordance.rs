// SPDX-License-Identifier: MPL-2.0

//! Copy icon that confirms, then returns to copy.
//!
//! Click writes plain text through [`clipboard::put_plain`] and reports
//! [`Outcome::Copied`]. The caller raises the app toast with [`TOAST`]. This
//! widget does not draw a toast — the container already owns the one.
//!
//! A copy the widget cannot make itself — a picture, rasterised by the
//! shell on its next pass — still confirms the same way through
//! [`CopyAffordance::confirm`]: the glyph turns to the check, holds, and
//! returns, so every copy in the app looks like every other whatever wrote
//! the clipboard.

use std::time::{Duration, Instant};

use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::tween::Tween;
use koru_draw::{icons, scene, theme};
use vello::Scene;
use vello::kurbo::Affine;

use crate::clipboard;
use crate::icon_button::{self, Metrics, State};
use crate::tooltip::{Tooltip, TooltipContent};

const HOLD: Duration = Duration::from_millis(800);
const MORPH: Duration = Duration::from_millis(theme::GLYPH_MORPH_MS as u64);

const METRICS: Metrics = Metrics {
    icon_size: 11.0,
    padding_x: 0.0,
    padding_y: 0.0,
    inset_top: 0.0,
    inset_bottom: 0.0,
    radius: 3.0,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Copied,
}

#[derive(Clone, Debug)]
pub struct CopyAffordance {
    tween: Tween,
    hold_until: Option<Instant>,
    tooltip: Tooltip,
}

impl Default for CopyAffordance {
    fn default() -> Self {
        Self {
            tween: Tween::new(theme::GLYPH_MORPH_MS, false),
            hold_until: None,
            tooltip: Tooltip::default(),
        }
    }
}

impl CopyAffordance {
    /// Same sentence the link chip already uses, so there is one toast identity.
    pub const TOAST: &'static str = "Copied to Clipboard";

    pub fn click(&mut self, text: &str, now: Instant) -> Outcome {
        clipboard::put_plain(text);
        self.confirm(now);
        Outcome::Copied
    }

    /// Show the confirmation without writing anything: the glyph morphs to
    /// the check, holds, and morphs back. For a copy someone else makes —
    /// the shell putting a picture on the clipboard after this press.
    pub fn confirm(&mut self, now: Instant) {
        self.tooltip.clear();
        self.tween.set(true, now);
        self.hold_until = Some(now + MORPH + HOLD);
    }

    /// Whether the check is showing or on its way in or out — the span the
    /// icon is drawn pressed.
    pub fn confirming(&self) -> bool {
        self.tween.is_on() || self.tween.progress() > 0.0
    }

    pub fn glyph(&self) -> char {
        if self.tween.is_on() || self.tween.progress() >= 0.5 {
            icons::CHECK
        } else {
            icons::COPY
        }
    }

    pub fn maintain(&mut self, now: Instant) -> bool {
        let mut changed = self.tween.advance(now);
        if self
            .hold_until
            .is_some_and(|until| now >= until && self.tween.is_on() && !self.tween.animating())
        {
            self.tween.set(false, now);
            self.hold_until = None;
            changed = true;
        }
        changed | self.tooltip.maintain(now)
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.tween.animating() || self.tooltip.animating(now)
    }

    pub fn maintenance_deadline(&self) -> Option<Instant> {
        [self.hold_until, self.tooltip.maintenance_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn set_hover(&mut self, slot: Option<Rect>, now: Instant) -> bool {
        let content = slot.map(|rect| TooltipContent::new(1, rect, "Copy"));
        self.tooltip.set_hover(content, now)
    }

    pub fn draw_icon(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        slot: Rect,
        hovered: bool,
    ) {
        let confirming = self.confirming();
        if !hovered && !confirming {
            scene::fill(
                canvas,
                root,
                theme::p().chip_bg,
                &scene::rrect(slot.x, slot.y, slot.w, slot.h, METRICS.radius),
            );
        }
        icon_button::draw_with(
            canvas,
            root,
            engine,
            slot,
            self.glyph(),
            State {
                hovered,
                active: confirming,
                disabled: false,
            },
            METRICS,
        );
    }

    pub fn draw_tooltip(
        &self,
        canvas: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        viewport: Rect,
        now: Instant,
    ) {
        self.tooltip.draw(canvas, root, engine, viewport, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard;

    #[test]
    fn click_copies_and_confirms_then_returns_to_copy() {
        let mut affordance = CopyAffordance::default();
        let now = Instant::now();
        assert_eq!(affordance.click("fn main() {}", now), Outcome::Copied);
        assert_eq!(clipboard::one_line().as_deref(), Some("fn main() {}"));
        assert_eq!(affordance.glyph(), icons::CHECK);

        let after_hold = now + MORPH + HOLD + Duration::from_millis(1);
        affordance.maintain(after_hold);
        affordance.maintain(after_hold + MORPH);
        assert_eq!(affordance.glyph(), icons::COPY);
    }

    /// A confirmation without a copy leaves the clipboard alone and runs
    /// the same morph, hold and return.
    #[test]
    fn confirm_shows_the_check_without_touching_the_clipboard() {
        clipboard::put_plain("before");
        let mut affordance = CopyAffordance::default();
        let now = Instant::now();
        assert!(!affordance.confirming());
        affordance.confirm(now);
        assert_eq!(clipboard::one_line().as_deref(), Some("before"));
        assert_eq!(affordance.glyph(), icons::CHECK);
        assert!(affordance.confirming());
        assert!(affordance.maintenance_deadline().is_some());

        let after_hold = now + MORPH + HOLD + Duration::from_millis(1);
        affordance.maintain(after_hold);
        affordance.maintain(after_hold + MORPH);
        assert_eq!(affordance.glyph(), icons::COPY);
        assert!(!affordance.confirming());
    }

    #[test]
    fn toast_copy_matches_the_link_chip() {
        assert_eq!(CopyAffordance::TOAST, "Copied to Clipboard");
    }
}
