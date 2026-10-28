// SPDX-License-Identifier: MPL-2.0

//! The `koru.color` picker: nine accent tokens, the current one checked, and
//! a Clear row while one is set.
//!
//! On the [`LanguagePicker`] template — a flat, fieldless list — because
//! choosing one of nine named tones is the same interaction as choosing one
//! of ten grammars. The value it commits is the **storage token** (D5's rule
//! for color too: presentation lives in the theme, the document stores the
//! name), through [`Outcome::SetAttr`] / [`Outcome::ClearAttr`].
//!
//! The same nine rows tint a callout's box, opened with
//! [`ColorPicker::for_callout`]; that commits [`Outcome::SetCalloutTint`].
//!
//! [`LanguagePicker`]: super::language_picker::LanguagePicker

use winit::keyboard::NamedKey;

use super::Outcome;
use crate::trigger::Trigger;
use koru_core::NodeId;
use koru_core::attributes::{AccentColor, KEY_COLOR};
use koru_core::node::AttrValue;
use koru_draw::text::TextEngine;
use koru_draw::theme;

/// Display labels for the nine tokens, in [`AccentColor::ALL`] order.
pub const LABELS: [&str; 9] = [
    "Red", "Orange", "Yellow", "Green", "Teal", "Blue", "Purple", "Pink", "Gray",
];

pub struct ColorPicker {
    pub trigger: Trigger,
    /// The nodes the choice applies to — one for `/color`, several for a
    /// multi-select apply from the context menu.
    pub nodes: Vec<NodeId>,
    /// What the (first) node carries now, checked in the list. Also decides
    /// whether the Clear row exists: clearing nothing is not an offer.
    pub current: Option<AccentColor>,
    pub sel: usize,
    /// Measured from the labels; floored by `panel_size`.
    pub width: f32,
    /// Whether the choice tints a callout box rather than setting
    /// `koru.color` — see [`ColorPicker::for_callout`].
    pub callout: bool,
}

impl ColorPicker {
    /// Open for `nodes`, preselecting the current token the way the language
    /// dropdown preselects the chip's grammar.
    pub fn open_for(trigger: Trigger, nodes: Vec<NodeId>, current: Option<AccentColor>) -> Self {
        let sel = match current {
            Some(c) => {
                // Past the Clear row, which exists exactly when `current` does.
                1 + AccentColor::ALL.iter().position(|t| *t == c).unwrap_or(0)
            }
            None => 0,
        };
        Self {
            trigger,
            nodes,
            current,
            sel,
            width: 0.0,
            callout: false,
        }
    }

    /// Open for the rows of one callout box, `current` being its tint. The
    /// rows, the order and the Clear row are the attribute picker's; only
    /// what a choice commits differs.
    pub fn for_callout(trigger: Trigger, nodes: Vec<NodeId>, current: Option<AccentColor>) -> Self {
        Self {
            callout: true,
            ..Self::open_for(trigger, nodes, current)
        }
    }

    /// Whether row 0 is Clear.
    pub fn offers_clear(&self) -> bool {
        self.current.is_some()
    }

    pub fn row_count(&self) -> usize {
        AccentColor::ALL.len() + usize::from(self.offers_clear())
    }

    /// What sits on row `i`: `None` is the Clear row, `Some` a token.
    pub fn row(&self, i: usize) -> Option<Option<AccentColor>> {
        if self.offers_clear() {
            if i == 0 {
                return Some(None);
            }
            return AccentColor::ALL.get(i - 1).copied().map(Some);
        }
        AccentColor::ALL.get(i).copied().map(Some)
    }

    pub fn measure_width(engine: &mut TextEngine) -> f32 {
        let widest = LABELS
            .iter()
            .chain(std::iter::once(&"Clear"))
            .map(|label| engine.simple_layout(label, theme::role::ui()).width())
            .fold(0.0_f32, f32::max);
        // Icon/swatch column plus the trailing check column.
        widest + 72.0
    }

    pub fn height(&self) -> f32 {
        theme::PANEL_PAD * 2.0 + self.row_count() as f32 * theme::ROW_H
    }

    pub fn key(&mut self, named: Option<NamedKey>) -> Outcome {
        let n = self.row_count();
        match named {
            Some(NamedKey::ArrowDown) => {
                self.sel = (self.sel + 1) % n;
                Outcome::Consumed
            }
            Some(NamedKey::ArrowUp) => {
                self.sel = (self.sel + n - 1) % n;
                Outcome::Consumed
            }
            Some(NamedKey::Enter) | Some(NamedKey::Tab) => self.choose(self.sel),
            // Fieldless: swallow, so characters do not land in the document.
            _ => Outcome::Consumed,
        }
    }

    pub fn click(&self, local_y: f32) -> Outcome {
        let row = (local_y / theme::ROW_H).floor();
        if row < 0.0 {
            return Outcome::Consumed;
        }
        self.choose(row as usize)
    }

    fn choose(&self, i: usize) -> Outcome {
        if self.callout {
            return match self.row(i) {
                Some(tint) => Outcome::SetCalloutTint(self.nodes.clone(), tint),
                None => Outcome::Close,
            };
        }
        match self.row(i) {
            Some(None) => Outcome::ClearAttr(self.nodes.clone(), KEY_COLOR.to_owned()),
            Some(Some(token)) => Outcome::SetAttr(
                self.nodes.clone(),
                KEY_COLOR.to_owned(),
                AttrValue::Text(token.as_str().to_owned()),
            ),
            None => Outcome::Close,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes() -> Vec<NodeId> {
        vec![NodeId::new()]
    }

    #[test]
    fn a_token_commits_its_storage_name() {
        let p = ColorPicker::open_for(Trigger::at_start(), nodes(), None);
        assert!(!p.offers_clear(), "nothing set, nothing to clear");
        assert_eq!(p.row_count(), 9);
        let chosen = p.choose(0);
        assert!(
            matches!(&chosen, Outcome::SetAttr(_, key, AttrValue::Text(t))
                if key == KEY_COLOR && t == "red"),
            "row 0 with no Clear row is red, got {chosen:?}"
        );
    }

    #[test]
    fn a_set_color_is_preselected_and_clearable() {
        let p = ColorPicker::open_for(Trigger::at_start(), nodes(), Some(AccentColor::Teal));
        assert!(p.offers_clear());
        assert_eq!(p.row_count(), 10);
        let teal = AccentColor::ALL
            .iter()
            .position(|t| *t == AccentColor::Teal)
            .unwrap();
        assert_eq!(p.sel, 1 + teal, "current token is where Enter lands");
        assert!(matches!(
            p.choose(0),
            Outcome::ClearAttr(_, key) if key == KEY_COLOR
        ));
    }

    #[test]
    fn a_callout_picker_commits_a_tint_not_an_attribute() {
        let p = ColorPicker::for_callout(Trigger::at_start(), nodes(), Some(AccentColor::Blue));
        assert!(p.offers_clear());
        assert!(matches!(p.choose(0), Outcome::SetCalloutTint(_, None)));
        assert!(matches!(
            p.choose(1),
            Outcome::SetCalloutTint(_, Some(AccentColor::Red))
        ));
    }

    #[test]
    fn every_label_names_a_token_in_order() {
        assert_eq!(LABELS.len(), AccentColor::ALL.len());
        for (label, token) in LABELS.iter().zip(AccentColor::ALL) {
            assert_eq!(label.to_lowercase(), token.as_str());
        }
    }

    #[test]
    fn arrows_wrap_and_enter_chooses() {
        let mut p = ColorPicker::open_for(Trigger::at_start(), nodes(), None);
        p.sel = p.row_count() - 1;
        p.key(Some(NamedKey::ArrowDown));
        assert_eq!(p.sel, 0);
        p.key(Some(NamedKey::ArrowUp));
        assert_eq!(p.sel, p.row_count() - 1);
        assert!(matches!(
            p.key(Some(NamedKey::Enter)),
            Outcome::SetAttr(_, _, _)
        ));
    }
}
