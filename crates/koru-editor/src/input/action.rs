// SPDX-License-Identifier: MPL-2.0

//! What the editor can be asked to do.
//!
//! A vocabulary, deliberately separate from the keys that reach it: the same
//! `Action` arrives from a chord, a toolbar button, and — later — a menu or a
//! remapped key the user chose. Naming it is what makes a key map data rather
//! than a match.
//!
//! Each action is **fully specified**. `MoveLeft { by_word, extend }` rather than
//! `MoveLeft` plus a look at the modifiers, because a user who remaps a chord is
//! choosing the whole behaviour, not half of it.

use koru_core::node::{Align, Heading};
use koru_core::{ListStyle, Mark, Script};

/// Which layer of the editor performs an action.
///
/// The shared editor performs [`Scope::Shared`] actions; a view performs the
/// rest and hands down what is not its own. A view simply has no arm for an
/// action outside its scope — which is what replaced asking "which view am I?"
/// six times over.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Scope {
    Shared,
    /// Acts on the *tree*: nesting, sibling order, completion.
    ///
    /// **Named for what it does, not for who does it.** It was `Outline`, after
    /// the view that owned these four — and when that view was deleted the
    /// actions did not go with it, they moved into the document
    /// ([ADR-90](../../../../docs/decisions/ADR-076-100.md#adr-90-onetree--one-loro-document-is-one-documents-nodes-and-the-realm-is-gone)).
    /// A variant named after a deleted view reads as dead code, which is how a
    /// live scope gets "cleaned up".
    Structural,
    Document,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Action {
    // ------------------------------------------------------------ the caret
    MoveLeft {
        by_word: bool,
        extend: bool,
    },
    MoveRight {
        by_word: bool,
        extend: bool,
    },
    MoveUp {
        extend: bool,
    },
    MoveDown {
        extend: bool,
    },
    /// Move to the adjacent paragraph, always landing at its text start.
    /// Negative is up.
    MoveParagraph(i32),
    /// Toggle between the page title and the remembered body caret.
    TogglePageTitle,
    /// Negative is up. The keyboard counterpart of clicking the scrollbar's
    /// track: the caret travels one viewport and lands at the same screen
    /// height, so typing picks up where the eye does. With `extend`, it grows
    /// the row selection by a page instead.
    PageUp {
        extend: bool,
    },
    PageDown {
        extend: bool,
    },
    LineStart {
        extend: bool,
    },
    LineEnd {
        extend: bool,
    },
    SelectAll,
    CollapseSelection,

    // ------------------------------------------------------------- the text
    SplitLine,
    /// Shift+Enter. On a non-text block (a divider, an image) it opens an empty
    /// paragraph *above* the block, the mirror of [`SplitLine`](Action::SplitLine)'s
    /// below; on a text row it behaves like an ordinary split.
    SplitLineAbove,
    /// Alt+Enter. Insert an empty paragraph immediately above the focused row
    /// without splitting or otherwise changing that row.
    InsertParagraphAbove,
    DeleteBack {
        by_word: bool,
    },
    DeleteForward {
        by_word: bool,
    },
    /// A literal tab. Only prose has one; an outline's Tab means [`Indent`].
    ///
    /// [`Indent`]: Action::Indent
    InsertTab,
    /// Delete the focused row, or every selected row once a selection exists.
    ///
    /// Shared, not the outline's: a document paragraph is as deletable as a
    /// task. A deleted row's children outdent into its place rather than going
    /// with it.
    DeleteRows,

    // ------------------------------------------------------------ the clipboard
    //
    // Each acts on the row selection when there is one, on the text selection
    // when there is one of those, and on the whole focused row when there is
    // neither — which is what makes Ctrl+C mean "copy this task" from a caret
    // sitting in one. A paste *replaces* whichever of those it finds.
    Copy,
    Cut,
    Paste,
    /// Paste only the clipboard's visible text, dropping row metadata and
    /// inline marks. The same operation backs Ctrl+Shift+V and the document
    /// context menu, so the two entry points cannot drift.
    PastePlain,

    // -------------------------------------------------------- the formatting
    ToggleMark(Mark),
    SetScript(Script),
    CycleHighlight,
    SetAlign(Align),
    /// `None` is body text, unconditionally. `Some(h)` on a line already at `h`
    /// clears it, so one control is both apply and undo — the bargain
    /// [`SetAlign`](Action::SetAlign) already makes.
    ///
    /// Performed only by a view that offers headings (`ViewState`), not
    /// gated through [`Scope`]: the `/` menu and the `#` shorthand reach the
    /// same operation without ever resolving an action.
    SetHeading(Option<Heading>),
    /// Toggle the requested list style on the selected rows. Rows that share a
    /// ListBlock with the selection are deliberately left unchanged. Shared by
    /// the keyboard and the selection bubble.
    SetListStyle(ListStyle),
    /// Toggle the focused row between a paragraph and a code block. A code
    /// block with newlines cannot go back (the one-line rule would refuse it),
    /// so in that case it is a no-op. Reached via `Ctrl+` `` ` ``.
    CodeBlock,
    /// Drop a horizontal rule at the caret. Performed only by a view that offers
    /// dividers (`ViewState`), like [`SetHeading`](Action::SetHeading): the `/`
    /// menu and the `---` shorthand reach the same operation without an action.
    InsertDivider,
    ClearFormatting,

    // ---------------------------------------------------------- the document
    Undo,
    Redo,
    OpenLinkPrompt,
    OpenEmojiPicker,

    // --------------------------------------------------------- the structure
    ToggleDone,
    /// **Tick or untick the box the focused row draws**, and nothing else —
    /// the same [`Body::set_done`](crate::Body::set_done) a press on the box
    /// performs.
    ///
    /// [`Scope::Shared`], unlike its four neighbours here, and deliberately:
    /// this acts on the *row* rather than on the tree, so every view that
    /// draws a checkbox answers it the same way. [`ToggleDone`](Self::ToggleDone)
    /// is structural because the document reinterprets it per block kind;
    /// this one has one meaning everywhere.
    ///
    /// **Not the task lifecycle.** Completing a task outright cascades through
    /// everything checkable below it and is the row menu's explicit *Complete*
    /// (SPEC §5, ADR-345); this is only the drawn box, which on a rich task is
    /// only the box. On a row that draws none it is refused — `act` returns
    /// `false` and the key falls through unhandled, which is the same nothing
    /// a press on empty space produces.
    TickCheckbox,
    /// **Ask the shell to save the focused diagram's picture** — the menu's
    /// *Save as…* on a drawn `mermaid` block, from the keyboard.
    ///
    /// [`Scope::Shared`], like [`TickCheckbox`](Self::TickCheckbox), and
    /// refused the same way: the handler sets the view's
    /// `wants_save_diagram_as` only when the focused row is a diagram
    /// showing its drawing — closed, and drawn — and answers `false` on
    /// everything else, so the chord falls through unclaimed rather than
    /// meaning something different per row. The dialog, the raster and the
    /// file are the shell's; the editor only names the block.
    SaveDiagramAs,
    ToggleHideCompleted,
    /// Negative is up.
    Reorder(i32),
    /// Negative outdents.
    Indent(i32),

    // -------------------------------------------------------------- the view
    ToggleSharing,
}

impl Action {
    pub fn scope(self) -> Scope {
        match self {
            Self::ToggleDone | Self::ToggleHideCompleted | Self::Reorder(_) | Self::Indent(_) => {
                Scope::Structural
            }
            Self::InsertTab => Scope::Document,
            _ => Scope::Shared,
        }
    }

    /// Whether a multi-row selection survives this.
    ///
    /// Two kinds do: an extending vertical motion, which is *making* the
    /// selection, and the actions that act on the whole of it. Everything else
    /// belongs to the focused row, so leaving rows highlighted while it acted
    /// on one would be a lie.
    ///
    /// **All four clipboard actions are in the second group**, and for two of
    /// them that is not obvious. `Cut` *reads* the selection before it deletes
    /// it, and `Paste` replaces it — so both need it still there. Leaving `Cut`
    /// off this list is what made Ctrl+X cut one row out of five: `act` cleared
    /// the marks, and the cut then found only the focused row to take.
    ///
    /// **`DeleteBack` and `DeleteForward` are in the second group too.** With a
    /// row selection, they delete the whole rows — the same as `DeleteRows` —
    /// rather than clearing the marks and deleting a character. With no row
    /// selection there is nothing to clear, so the text-delete path runs as
    /// before.
    pub fn keeps_row_selection(self) -> bool {
        matches!(
            self,
            Self::MoveUp { extend: true }
                | Self::MoveDown { extend: true }
                | Self::PageUp { extend: true }
                | Self::PageDown { extend: true }
                | Self::SelectAll
                | Self::DeleteRows
                | Self::DeleteBack { .. }
                | Self::DeleteForward { .. }
                | Self::Reorder(_)
                | Self::Indent(_)
                | Self::SetListStyle(_)
                // Formatting acts on every selected row, not only the focused
                // one — clearing the marks first left Ctrl+B bolding a single
                // line out of five.
                | Self::ToggleMark(_)
                | Self::SetScript(_)
                | Self::CycleHighlight
                | Self::ClearFormatting
                | Self::SetAlign(_)
                | Self::SetHeading(_)
                | Self::Copy
                | Self::Cut
                | Self::Paste
                | Self::PastePlain
        )
    }

    /// Whether this still applies with no active line.
    ///
    /// Clicking empty space drops the caret, and most actions have nothing to
    /// act on afterwards. The window-level toggles do.
    pub fn works_without_a_caret(self) -> bool {
        matches!(
            self,
            Self::ToggleHideCompleted
                | Self::ToggleSharing
                | Self::PageUp { .. }
                | Self::PageDown { .. }
                | Self::Undo
                | Self::Redo
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every action is one view's or shared — and the two view-specific sets
    /// must not overlap, or a chord would mean two things at once.
    #[test]
    fn the_two_views_claim_different_actions() {
        assert_eq!(Action::Indent(1).scope(), Scope::Structural);
        assert_eq!(Action::InsertTab.scope(), Scope::Document);
        assert_eq!(Action::SplitLine.scope(), Scope::Shared);
        assert_eq!(Action::SplitLineAbove.scope(), Scope::Shared);
        assert_eq!(Action::InsertParagraphAbove.scope(), Scope::Shared);
    }

    /// All four clipboard actions act on the whole row selection, and for two
    /// of them that is not obvious — cut *reads* the selection before deleting
    /// it, and paste replaces it.
    ///
    /// Leaving `Cut` off the list made Ctrl+X cut one row out of five, silently:
    /// `act` cleared the marks before the cut could read them. Every clipboard
    /// test at the time called `cut()` directly and so never saw it.
    #[test]
    fn the_clipboard_acts_on_the_whole_row_selection() {
        assert!(Action::Copy.keeps_row_selection());
        assert!(Action::Cut.keeps_row_selection());
        assert!(Action::Paste.keeps_row_selection());
        assert!(Action::PastePlain.keeps_row_selection());
        assert!(Action::SetListStyle(ListStyle::Bullet).keeps_row_selection());

        // Formatting keeps the row selection so it can act on every marked row.
        assert!(Action::ToggleMark(Mark::Bold).keeps_row_selection());
        assert!(Action::CycleHighlight.keeps_row_selection());
        assert!(Action::ClearFormatting.keeps_row_selection());
        assert!(Action::SetAlign(Align::Center).keeps_row_selection());

        // ...and everything that belongs to the focused row still ends it.
        assert!(!Action::SplitLine.keeps_row_selection());
        assert!(!Action::MoveDown { extend: false }.keeps_row_selection());
    }

    /// The toggles that survive a dropped caret are exactly the window's, not
    /// the line's. Anything editing text here would act on nothing.
    #[test]
    fn only_window_toggles_work_without_a_caret() {
        assert!(Action::ToggleSharing.works_without_a_caret());
        assert!(Action::ToggleHideCompleted.works_without_a_caret());
        assert!(Action::Undo.works_without_a_caret());
        assert!(Action::Redo.works_without_a_caret());
        assert!(!Action::SplitLine.works_without_a_caret());
        assert!(!Action::ToggleMark(Mark::Bold).works_without_a_caret());
    }
}
