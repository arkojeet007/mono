// SPDX-License-Identifier: MPL-2.0

//! The document's command vocabulary, and what the caret says about it.
//!
//! A document is a list of rows like any other — same keys, same editing —
//! rooted at a `Kind::Document` node rather than at the top of the tree
//! ([ADR-6](../../../docs/decisions/README.md)). So this module holds only what
//! differs: the set of commands a document offers, and the state that decides
//! which of them draw pressed.
//!
//! It used to hold a toolbar's geometry too. The toolbar is gone — the same
//! commands now reach the user through the selection bubble (`bubble.rs`), the
//! block menus (`blockmenu.rs`) and the page menu (`page_menu.rs`), each of
//! which lays this vocabulary out its own way. Keeping [`Command`] here, one
//! layer below all three, is what stops them growing three opinions about what
//! Bold means.

use koru_core::node::{Align, Heading};
use koru_core::schema::WorkspaceDoc;
use koru_core::{ListStyle, Mark, MarkSet, Script, TextStyle};

use koru_draw::icons;
use koru_draw::theme;
use koru_editor::Body;
use koru_editor::input::action::Action;

/// A command a document offers. Only commands whose model exists — the rest of
/// the block commands arrive with `Kind::Table` and friends.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Command {
    Undo,
    Redo,
    H1,
    H2,
    H3,
    H4,
    /// Opens the list-style flyout. Intercepted in `BuiltInDocument::mouse_down`
    /// before `action()` is reached: if the caret is in a ListBlock, the flyout
    /// re-styles it; if not, it wraps the current row into a new ListBlock.
    List,
    /// Apply a bullet list directly. Unlike `List`, this is a real action so
    /// the bubble and Ctrl+Shift+8 share the editor's selection-aware path.
    BulletedList,
    /// Apply a numbered list directly; the Ctrl+Shift+7 counterpart.
    NumberedList,
    /// Directly wraps the current row into a `ListBlock(Checkbox)` — a task
    /// list. Intercepted in `mouse_down`, like `List`.
    Tasks,
    /// Wrap the selected rows into one callout box, or take the box away;
    /// the `Ctrl+Shift+6` counterpart, in the list menu because a callout,
    /// like a list, wraps lines rather than styling one.
    Callout,
    /// A horizontal rule block, the same thing `/divider` and `---` produce.
    Divider,
    Bold,
    Italic,
    Underline,
    Strike,
    /// Inline code — mono with a background.
    Code,
    Highlight,
    /// The link prompt, seeded from the selection — `Ctrl+K`.
    Link,
    /// Strip every mark and colour from the selection, leaving the heading and
    /// the alignment alone.
    ClearFormatting,
    Superscript,
    Subscript,
    AlignStart,
    AlignCenter,
    AlignEnd,
}

impl Command {
    /// What pressing this button means.
    ///
    /// The button list is the toolbar's — its order, its icons, its hit
    /// testing — but what a press *does* is the same vocabulary a chord
    /// resolves to. Without this the editor grew a second twelve-arm dispatch
    /// beside `Editor::act`, and the two drifted the moment one gained a case.
    pub fn action(self) -> Action {
        match self {
            Self::Undo => Action::Undo,
            Self::Redo => Action::Redo,
            Self::H1 => Action::SetHeading(Some(Heading::H1)),
            Self::H2 => Action::SetHeading(Some(Heading::H2)),
            Self::H3 => Action::SetHeading(Some(Heading::H3)),
            Self::H4 => Action::SetHeading(Some(Heading::H4)),
            // Unreachable: the button opens the flyout. Kept total so every
            // other button's call site stays `act(cmd.action())`.
            Self::List => Action::ClearFormatting,
            Self::BulletedList => Action::SetListStyle(ListStyle::Bullet),
            Self::NumberedList => Action::SetListStyle(ListStyle::Numbered),
            Self::Tasks => Action::SetListStyle(ListStyle::Checkbox),
            Self::Callout => Action::ToggleCallout,
            Self::Divider => Action::InsertDivider,
            Self::Bold => Action::ToggleMark(Mark::Bold),
            Self::Italic => Action::ToggleMark(Mark::Italic),
            Self::Underline => Action::ToggleMark(Mark::Underline),
            Self::Strike => Action::ToggleMark(Mark::Strike),
            Self::Code => Action::ToggleMark(Mark::Code),
            Self::Highlight => Action::CycleHighlight,
            Self::Link => Action::OpenLinkPrompt,
            Self::ClearFormatting => Action::ClearFormatting,
            Self::Superscript => Action::SetScript(Script::Super),
            Self::Subscript => Action::SetScript(Script::Sub),
            Self::AlignStart => Action::SetAlign(Align::Start),
            Self::AlignCenter => Action::SetAlign(Align::Center),
            Self::AlignEnd => Action::SetAlign(Align::End),
        }
    }
}

impl Command {
    pub fn icon(self) -> char {
        match self {
            Self::Undo => icons::UNDO,
            Self::Redo => icons::REDO,
            Self::H1 => icons::HEADING_1,
            Self::H2 => icons::HEADING_2,
            Self::H3 => icons::HEADING_3,
            Self::H4 => icons::HEADING_4,
            Self::List => icons::LIST,
            Self::BulletedList => icons::LIST,
            Self::NumberedList => icons::LIST_NUM,
            Self::Tasks => icons::LIST_CHECKS,
            Self::Callout => icons::LIGHTBULB,
            Self::Divider => icons::DIVIDER,
            Self::Bold => icons::BOLD,
            Self::Italic => icons::ITALIC,
            Self::Underline => icons::UNDERLINE,
            Self::Strike => icons::STRIKE,
            Self::Code => icons::CODE,
            Self::Highlight => icons::HIGHLIGHT,
            Self::Link => icons::LINK,
            Self::ClearFormatting => icons::CLEAR,
            Self::Superscript => icons::SUPERSCRIPT,
            Self::Subscript => icons::SUBSCRIPT,
            Self::AlignStart => icons::ALIGN_LEFT,
            Self::AlignCenter => icons::ALIGN_CENTER,
            Self::AlignEnd => icons::ALIGN_RIGHT,
        }
    }

    /// Shown on hover, with the chord that does the same thing — a toolbar
    /// that teaches its own shortcuts costs one string per button.
    pub fn tooltip(self) -> &'static str {
        match self {
            Self::Undo => "Undo  Ctrl+Z",
            Self::Redo => "Redo  Ctrl+Shift+Z",
            Self::H1 => "Heading 1  Ctrl+Alt+1",
            Self::H2 => "Heading 2  Ctrl+Alt+2",
            Self::H3 => "Heading 3  Ctrl+Alt+3",
            Self::H4 => "Heading 4  Ctrl+Alt+4",
            Self::List => "List",
            Self::BulletedList => "Bulleted list  Ctrl+Shift+8",
            Self::NumberedList => "Numbered list  Ctrl+Shift+7",
            Self::Tasks => "Checklist  Ctrl+Shift+9",
            Self::Callout => "Callout  Ctrl+Shift+6",
            Self::Divider => "Divider  ---",
            Self::Bold => "Bold  Ctrl+B",
            Self::Italic => "Italic  Ctrl+I",
            Self::Underline => "Underline  Ctrl+U",
            Self::Strike => "Strikethrough  Ctrl+Shift+S",
            Self::Code => "Code  Ctrl+Shift+C",
            Self::Highlight => "Highlight  Ctrl+Shift+H",
            Self::Link => "Link  Ctrl+K",
            Self::ClearFormatting => "Clear formatting  Ctrl+\\",
            Self::Superscript => "Superscript",
            Self::Subscript => "Subscript",
            Self::AlignStart => "Align left  Ctrl+Shift+L",
            Self::AlignCenter => "Centre  Ctrl+Shift+E",
            Self::AlignEnd => "Align right  Ctrl+Shift+R",
        }
    }

    /// The flag this button toggles, if it is one. `None` for commands whose
    /// state is a value or an action rather than a flag.
    fn mark(self) -> Option<Mark> {
        Some(match self {
            Self::Bold => Mark::Bold,
            Self::Italic => Mark::Italic,
            Self::Underline => Mark::Underline,
            Self::Strike => Mark::Strike,
            Self::Code => Mark::Code,
            _ => return None,
        })
    }
}

/// The colours the canvas swatch cycles through. `None` is "follow the theme
/// canvas"; the rest come from [`theme::canvas_presets`] — theme-aware (light
/// tints in light mode, dark tints in dark) so the canvas always reads as
/// darker than the page by a visible margin in either theme.
pub fn canvas_bg_presets() -> [Option<[u8; 4]>; 4] {
    let p = theme::canvas_presets();
    [None, Some(p[0]), Some(p[1]), Some(p[2])]
}

/// Advance the canvas colour to the next preset, wrapping. A colour that is not
/// a preset jumps to the first (Auto); a preset one moves to the next.
pub fn cycle_canvas_bg(current: Option<[u8; 4]>) -> Option<[u8; 4]> {
    let presets = canvas_bg_presets();
    let next = presets
        .iter()
        .position(|c| *c == current)
        .map(|i| (i + 1) % presets.len())
        .unwrap_or(0);
    presets[next]
}

/// Which buttons draw as pressed, read from the document at the caret.
///
/// Computed once per frame rather than per button, because every answer comes
/// from the same two lookups.
#[derive(Default)]
pub struct State {
    pub marks: MarkSet,
    pub style: TextStyle,
    pub align: Align,
    pub heading: Option<Heading>,
    pub list_style: Option<ListStyle>,
    /// Whether the focused row is in a callout box, head or inside.
    pub in_callout: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

impl State {
    /// `armed` is what the next keystroke would carry — see `Editor::armed`.
    /// It wins over what is under the caret, because it is what typing would
    /// actually produce, and a button that lies about that is worse than none.
    pub fn read(
        ws: &WorkspaceDoc,
        outline: &Body,
        armed: (Option<MarkSet>, Option<TextStyle>),
    ) -> Self {
        let Some(row) = outline.focused() else {
            return Self::default();
        };
        let range = row.range();
        // With a selection, what covers all of it; with a caret, what text
        // typed here would inherit. Both are intersections, so a mixed
        // selection reports nothing rather than one run's answer.
        let (marks, style) = if range.is_empty() {
            (
                ws.marks_at(row.id, range.start).unwrap_or_default(),
                ws.style_at(row.id, range.start).unwrap_or_default(),
            )
        } else {
            (
                ws.marks_in(row.id, range.clone()).unwrap_or_default(),
                ws.style_in(row.id, range).unwrap_or_default(),
            )
        };
        Self {
            marks: armed.0.unwrap_or(marks),
            style: armed.1.unwrap_or(style),
            align: row.align,
            heading: row.heading,
            list_style: row.list_style,
            in_callout: row.callout.is_some(),
            can_undo: ws.can_undo(),
            can_redo: ws.can_redo(),
        }
    }

    pub fn is_active(&self, cmd: Command) -> bool {
        if let Some(mark) = cmd.mark() {
            return self.marks.has(mark);
        }
        match cmd {
            Command::H1 => self.heading == Some(Heading::H1),
            Command::H2 => self.heading == Some(Heading::H2),
            Command::H3 => self.heading == Some(Heading::H3),
            Command::H4 => self.heading == Some(Heading::H4),
            Command::Highlight => self.style.highlight.is_some(),
            Command::Superscript => self.style.script == Some(Script::Super),
            Command::Subscript => self.style.script == Some(Script::Sub),
            Command::AlignStart => self.align == Align::Start,
            Command::AlignCenter => self.align == Align::Center,
            Command::AlignEnd => self.align == Align::End,
            Command::List => self.list_style.is_some(),
            Command::BulletedList => self.list_style == Some(ListStyle::Bullet),
            Command::NumberedList => self.list_style == Some(ListStyle::Numbered),
            Command::Tasks => self.list_style == Some(ListStyle::Checkbox),
            Command::Callout => self.in_callout,
            _ => false,
        }
    }

    /// Whether pressing this would do anything.
    ///
    /// The bubble only exists while there *is* a selection, so almost nothing
    /// in it can be dead — which is most of why the toolbar's twenty
    /// permanently-dimmable buttons became a bubble in the first place. Undo
    /// and Redo are the two whose answer depends on history, and they no longer
    /// have a button at all.
    pub fn is_enabled(&self, cmd: Command) -> bool {
        match cmd {
            Command::Undo => self.can_undo,
            Command::Redo => self.can_redo,
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests;
