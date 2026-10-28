// SPDX-License-Identifier: MPL-2.0

//! What typing may turn a row into — the **composer policy**.
//!
//! A document offers everything: headings from `#`, lists from `- `, the `/`
//! menu, marks, links that make themselves out of typed URLs. The AI chat's
//! composer is also a Koru document, edited by this editor so that `@` can put
//! a document *in the text* as a chip (AGENTUX I6, decision X1) — but it is a
//! message, not a page. Its person must never see a heading appear because a
//! line began with `#`, a menu open on `/`, or a paragraph become a list. What
//! it keeps is text, line breaks, undo, and the `@` picker.
//!
//! **One value, asked where each behaviour lives**, rather than a second editor
//! or a filter in front of this one: the shorthands, the trigger arming, the
//! autolink and the key map each ask [`Policy`] at the point they would act, so
//! a behaviour added later has one question to answer and a test that says it
//! was asked. The capability flags on `ViewState` (`headings_enabled` and its
//! neighbours) already refuse most block shapes; [`ViewState::apply_policy`]
//! turns them all off together so the two cannot disagree.
//!
//! A line break in the composer is a new **plain paragraph** — the model keeps
//! a paragraph's text free of `\n` (`Kind::allows_newlines`) — and the host
//! joins paragraphs with one newline when it sends. That is the reversible
//! shape of "one block with soft breaks"; see `docs/features/koru-ai/ARCHITECTURE.md`.
//!
//! [`ViewState::apply_policy`]: crate::ViewState::apply_policy

use crate::input::action::Action;
use crate::trigger::TriggerKind;

/// Which set of editing behaviours a view offers.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Policy {
    /// Everything this editor does. Every document, outline and task view.
    #[default]
    Document,
    /// Text, line breaks, undo and `@` document chips — nothing that changes a
    /// row's shape, marks its text, or opens a menu other than the `@` picker.
    Composer,
}

impl Policy {
    /// Whether a key-map action may run.
    ///
    /// Exhaustive on purpose: a new [`Action`] is a compile error here until
    /// somebody decides whether a chat message may have it.
    pub fn allows(self, action: Action) -> bool {
        if self == Self::Document {
            return true;
        }
        match action {
            // The caret, the selection and the text itself.
            Action::MoveLeft { .. }
            | Action::MoveRight { .. }
            | Action::MoveUp { .. }
            | Action::MoveDown { .. }
            | Action::MoveParagraph(_)
            | Action::PageUp { .. }
            | Action::PageDown { .. }
            | Action::LineStart { .. }
            | Action::LineEnd { .. }
            | Action::SelectAll
            | Action::CollapseSelection
            // A split is a new plain paragraph; the host sends on Enter and
            // turns Shift+Enter and Ctrl+J into this.
            | Action::SplitLine
            | Action::SplitLineAbove
            | Action::InsertParagraphAbove
            | Action::DeleteBack { .. }
            | Action::DeleteForward { .. }
            | Action::InsertTab
            | Action::DeleteRows
            | Action::Copy
            | Action::Cut
            // Both pastes arrive as plain text at the caret — see
            // `clipboard::paste_lines`.
            | Action::Paste
            | Action::PastePlain
            | Action::Undo
            | Action::Redo => true,
            // Marks, block shapes, menus and document commands.
            Action::ToggleMark(_)
            | Action::SetScript(_)
            | Action::CycleHighlight
            | Action::SetAlign(_)
            | Action::SetHeading(_)
            | Action::SetListStyle(_)
            | Action::CodeBlock
            | Action::ToggleCallout
            | Action::InsertDivider
            | Action::ClearFormatting
            | Action::OpenLinkPrompt
            | Action::OpenEmojiPicker
            | Action::TogglePageTitle
            | Action::ToggleDone
            | Action::TickCheckbox
            | Action::SaveDiagramAs
            | Action::ToggleHideCompleted
            | Action::Reorder(_)
            | Action::Indent(_)
            | Action::ToggleSharing => false,
        }
    }

    /// Whether a typed trigger character may open its picker.
    ///
    /// The composer keeps `@` — the reason it is an editor at all — and refuses
    /// the `/` menu, the `:` emoji completion and the `#tag` chooser: each of
    /// them would take Enter away from *send* in a field where Enter sends.
    pub fn arms(self, kind: TriggerKind) -> bool {
        match self {
            Self::Document => true,
            Self::Composer => kind == TriggerKind::At,
        }
    }

    /// Whether typing may reshape text on its own: a typed URL becoming a link
    /// pill, a selection and `` ` `` becoming inline code, `@` over a selection
    /// opening the move picker. A message keeps exactly what was typed.
    pub fn rewrites_typing(self) -> bool {
        self == Self::Document
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::node::Heading;

    #[test]
    fn a_document_allows_everything() {
        assert!(Policy::Document.allows(Action::SetHeading(Some(Heading::H1))));
        assert!(Policy::Document.arms(TriggerKind::Slash));
        assert!(Policy::Document.rewrites_typing());
    }

    #[test]
    fn the_composer_keeps_text_and_at_and_refuses_shapes_and_menus() {
        let composer = Policy::Composer;
        assert!(composer.allows(Action::SplitLine));
        assert!(composer.allows(Action::Undo));
        assert!(composer.allows(Action::Paste));
        assert!(!composer.allows(Action::SetHeading(Some(Heading::H1))));
        assert!(!composer.allows(Action::InsertDivider));
        assert!(!composer.allows(Action::ToggleDone));
        assert!(!composer.allows(Action::OpenLinkPrompt));
        assert!(composer.arms(TriggerKind::At));
        assert!(!composer.arms(TriggerKind::Slash));
        assert!(!composer.arms(TriggerKind::Colon));
        assert!(!composer.arms(TriggerKind::Hash));
        assert!(!composer.rewrites_typing());
    }
}
