// SPDX-License-Identifier: MPL-2.0

//! The two menus that hang off the selection bubble.
//!
//! The **block type** dropdown on its left — what the focused line *is* — and
//! the **overflow** on its right, for the commands that did not earn a place on
//! the front. Between them they hold every button the deleted toolbar had
//! except Undo and Redo, which are `Ctrl+Z` and `Ctrl+Shift+Z` and never needed
//! a button of their own.
//!
//! The list submenu is here too: the bubble's List button opens the three
//! supported styles, and keeps their glyphs and labels in one place.

use koru_core::node::Heading;
use koru_draw::geometry::Rect;
use koru_draw::icons;

use crate::docview::{Command, State};
use crate::menu::{Chord as MenuChord, Menu, Row};

pub const WIDTH: f32 = 208.0;
/// List labels are longer than the heading labels in the adjacent menu. Give
/// their shortcut column the same gap rather than letting `Ctrl+Shift+…` crowd
/// the words it describes.
const LIST_WIDTH: f32 = 240.0;

// ------------------------------------------------------------------ block type

/// What the block-type menu turns the focused line into.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BlockChoice {
    Text,
    Heading(Heading),
}

/// The rows, top to bottom.
///
/// **Only what a line of prose can *be*.** A task list, a divider and the list
/// styles were all here and are not any more: this menu opens off a text
/// selection, so it answers "what kind of *text* is this" — and a list is a
/// block that wraps lines rather than a way of setting one. They live in the
/// `+` and `/` menu, which is where a block belongs.
const BLOCKS: [BlockChoice; 5] = [
    BlockChoice::Text,
    BlockChoice::Heading(Heading::H1),
    BlockChoice::Heading(Heading::H2),
    BlockChoice::Heading(Heading::H3),
    BlockChoice::Heading(Heading::H4),
];

impl BlockChoice {
    fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Heading(Heading::H1) => "Heading 1",
            Self::Heading(Heading::H2) => "Heading 2",
            Self::Heading(Heading::H3) => "Heading 3",
            Self::Heading(_) => "Heading 4",
        }
    }

    /// The chord that does the same thing. A menu that teaches its own
    /// shortcuts costs one string per row — the reason the toolbar's tooltips
    /// carried them, kept now that the tooltips have gone.
    fn shortcut(self) -> Option<MenuChord> {
        use koru_keys::ShortcutId;
        Some(MenuChord::Id(match self {
            Self::Text => ShortcutId::BodyText,
            Self::Heading(Heading::H1) => ShortcutId::Heading1,
            Self::Heading(Heading::H2) => ShortcutId::Heading2,
            Self::Heading(Heading::H3) => ShortcutId::Heading3,
            Self::Heading(Heading::H4) => ShortcutId::Heading4,
            Self::Heading(_) => return None,
        }))
    }

    fn icon(self) -> char {
        match self {
            // `ph-text-align-left` — four ragged lines, the universal mark for
            // a paragraph. It is also the Align-left command's glyph, in the
            // other menu; Phosphor's generated table has no plain-paragraph
            // icon, and `docs/features/design/DESIGN.md` forbids inventing a codepoint.
            Self::Text => icons::ALIGN_LEFT,
            Self::Heading(Heading::H1) => icons::HEADING_1,
            Self::Heading(Heading::H2) => icons::HEADING_2,
            Self::Heading(Heading::H3) => icons::HEADING_3,
            Self::Heading(_) => icons::HEADING_4,
        }
    }

    /// Whether this is what the focused line already is.
    fn current(self, state: &State) -> bool {
        match self {
            Self::Text => state.heading.is_none(),
            Self::Heading(h) => state.heading == Some(h),
        }
    }
}

pub fn block_rows(state: &State) -> Vec<Row> {
    let mut out = vec![Row::Heading("TURN INTO")];
    for choice in BLOCKS {
        let mut row = Row::item(choice.label())
            .with_icon(choice.icon())
            .checked(choice.current(state));
        if let Some(chord) = choice.shortcut() {
            row = row.with_shortcut(chord);
        }
        out.push(row);
    }
    out
}

pub fn block_choice_at(i: usize) -> Option<BlockChoice> {
    i.checked_sub(1).and_then(|n| BLOCKS.get(n).copied())
}

/// What the bubble's block-type button shows for the focused line: the
/// current choice's icon — the same glyph its menu row carries, so the button
/// reads as the ticked row rather than as a second vocabulary.
pub fn block_icon(state: &State) -> char {
    BLOCKS
        .iter()
        .find(|c| c.current(state))
        .map_or_else(|| BlockChoice::Text.icon(), |c| c.icon())
}

// ---------------------------------------------------------------- list styles

/// The list flyout is deliberately one short menu: every choice is visible
/// at once and the active one is checked, matching the mark buttons. The
/// callout is here because it is the same kind of thing as a list: a block
/// that wraps the selected lines rather than a way of setting one.
const LISTS: [Command; 4] = [
    Command::BulletedList,
    Command::NumberedList,
    Command::Tasks,
    Command::Callout,
];

fn list_label(cmd: Command) -> &'static str {
    match cmd {
        Command::BulletedList => "Bulleted list",
        Command::NumberedList => "Numbered list",
        Command::Tasks => "Checklist",
        Command::Callout => "Callout",
        _ => "",
    }
}

fn list_shortcut(cmd: Command) -> MenuChord {
    use koru_keys::ShortcutId;
    MenuChord::Id(match cmd {
        Command::BulletedList => ShortcutId::BulletedList,
        Command::NumberedList => ShortcutId::NumberedList,
        Command::Tasks => ShortcutId::Checklist,
        Command::Callout => ShortcutId::Callout,
        _ => unreachable!("only list commands reach this menu"),
    })
}

pub fn list_rows(state: &State) -> Vec<Row> {
    LISTS
        .into_iter()
        .map(|cmd| {
            Row::item(list_label(cmd))
                .with_icon(cmd.icon())
                .with_shortcut(list_shortcut(cmd))
                .checked(state.is_active(cmd))
        })
        .collect()
}

pub fn list_choice_at(i: usize) -> Option<Command> {
    LISTS.get(i).copied()
}

// ------------------------------------------------------------------- overflow

/// The commands behind the bubble's `⋯`.
///
/// Alignment used to be here and is on the bar now — it is what people look for
/// first, and behind a `⋯` nobody found it. What is left is the pair that is
/// genuinely occasional.
const MORE: [Command; 2] = [Command::Superscript, Command::Subscript];

fn more_label(cmd: Command) -> &'static str {
    match cmd {
        Command::Superscript => "Superscript",
        Command::Subscript => "Subscript",
        _ => "",
    }
}

/// The chord for an overflow row. `None` for the two that have never had one —
/// a blank column says "no shortcut" more honestly than an invented chord.
fn more_shortcut(cmd: Command) -> Option<MenuChord> {
    use koru_keys::ShortcutId;
    Some(MenuChord::Id(match cmd {
        Command::Superscript => ShortcutId::Superscript,
        Command::Subscript => ShortcutId::Subscript,
        _ => return None,
    }))
}

pub fn more_rows(state: &State) -> Vec<Row> {
    MORE.into_iter()
        .map(|cmd| {
            let row = Row::item(more_label(cmd))
                .with_icon(cmd.icon())
                .checked(state.is_active(cmd));
            match more_shortcut(cmd) {
                Some(chord) => row.with_shortcut(chord),
                None => row,
            }
        })
        .collect()
}

pub fn more_choice_at(i: usize) -> Option<Command> {
    MORE.get(i).copied()
}

/// A menu anchored under `button`.
pub fn open(button: Rect) -> Menu {
    Menu::new(button, WIDTH)
}

/// Open the list flyout with room between its longer labels and shortcuts.
pub fn open_list(button: Rect) -> Menu {
    Menu::new(button, LIST_WIDTH)
}

#[cfg(test)]
mod tests;
