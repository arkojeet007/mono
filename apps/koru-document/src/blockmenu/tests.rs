// SPDX-License-Identifier: MPL-2.0

//! What each row of the two bubble menus means.
//!
//! Both `*_choice_at` functions do index arithmetic over a list they do not
//! build — a heading row here — which is precisely the kind of off-by-one that
//! shows up as "the menu does the wrong thing" rather than as a compile error.

use super::*;
use crate::menu::Chord as MenuChord;
use koru_core::ListStyle;

fn plain() -> State {
    State::default()
}

fn labels(rows: &[Row]) -> Vec<&str> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Item { label, .. } => Some(label.as_ref()),
            _ => None,
        })
        .collect()
}

fn ticked(rows: &[Row]) -> Vec<usize> {
    (0..rows.len())
        .filter(|&i| {
            matches!(
                rows[i],
                Row::Item {
                    trailing: crate::menu::Trailing::Check,
                    ..
                }
            )
        })
        .collect()
}

/// Every selectable row of the block menu resolves to a choice, and the
/// heading row resolves to none.
#[test]
fn every_block_row_resolves_to_its_own_choice() {
    let rows = block_rows(&plain());
    assert_eq!(block_choice_at(0), None, "the TURN INTO heading");
    for (n, expected) in BLOCKS.iter().enumerate() {
        assert_eq!(block_choice_at(n + 1), Some(*expected));
    }
    assert_eq!(block_choice_at(rows.len()), None, "past the end");
    assert_eq!(rows.len(), BLOCKS.len() + 1);
}

/// **The menu is only what a line of prose can be.** It opens off a text
/// selection, so it answers "what kind of *text* is this" — and a list, a task
/// list and a divider are all blocks, which belong to the `+` and `/` menu.
#[test]
fn the_block_menu_offers_only_text_and_headings() {
    assert_eq!(
        labels(&block_rows(&plain())),
        vec!["Text", "Heading 1", "Heading 2", "Heading 3", "Heading 4"]
    );
}

/// A plain paragraph shows the paragraph icon, and exactly one row is ticked.
#[test]
fn a_plain_paragraph_reads_as_text() {
    let state = plain();
    assert_eq!(block_icon(&state), icons::ALIGN_LEFT);
    let rows = block_rows(&state);
    assert_eq!(
        ticked(&rows)
            .into_iter()
            .map(block_choice_at)
            .collect::<Vec<_>>(),
        vec![Some(BlockChoice::Text)]
    );
}

/// A heading shows its own level's icon, and Text stops being ticked.
#[test]
fn a_heading_reads_as_its_level() {
    let state = State {
        heading: Some(Heading::H2),
        ..State::default()
    };
    assert_eq!(block_icon(&state), icons::HEADING_2);
    let rows = block_rows(&state);
    assert_eq!(
        ticked(&rows)
            .into_iter()
            .map(block_choice_at)
            .collect::<Vec<_>>(),
        vec![Some(BlockChoice::Heading(Heading::H2))]
    );
}

/// A list item is still *text* as far as this menu is concerned: being in a
/// list says nothing about what kind of line it is.
#[test]
fn a_line_inside_a_list_still_reads_as_text() {
    let state = State {
        list_style: Some(ListStyle::Bullet),
        ..State::default()
    };
    assert_eq!(block_icon(&state), icons::ALIGN_LEFT);
}

/// Every row carries its chord. A menu that teaches its own shortcuts costs one
/// string per row — the reason the deleted toolbar's tooltips carried them.
#[test]
fn the_block_rows_carry_their_chords() {
    let chords: Vec<Option<MenuChord>> = block_rows(&plain())
        .iter()
        .filter_map(|r| match r {
            Row::Item { shortcut, .. } => Some(*shortcut),
            _ => None,
        })
        .collect();
    // **The ids, not the rendering.** What each row *is* stays fixed; how it
    // is written follows the platform, which is the whole point of routing
    // these through `koru_keys::registry`.
    use koru_keys::ShortcutId::*;
    assert_eq!(
        chords,
        vec![
            Some(MenuChord::Id(BodyText)),
            Some(MenuChord::Id(Heading1)),
            Some(MenuChord::Id(Heading2)),
            Some(MenuChord::Id(Heading3)),
            Some(MenuChord::Id(Heading4)),
        ]
    );
    // …and the rendering is this platform's. On macOS these are ⌥⌘-digits, so
    // a test asserting `Ctrl+Alt+0` would have been wrong there for ever.
    let text = MenuChord::Id(Heading1).text();
    assert!(text.ends_with('1'), "{text}");
    if cfg!(target_os = "macos") {
        assert!(text.contains('⌘') && text.contains('⌥'), "{text}");
    } else {
        assert_eq!(text, "Ctrl+Alt+1");
    }
}

/// The overflow's rows and its index arithmetic agree.
#[test]
fn every_overflow_row_resolves_to_its_own_command() {
    let rows = more_rows(&plain());
    for (i, row) in rows.iter().enumerate() {
        match row {
            Row::Item { label, .. } => {
                let cmd =
                    more_choice_at(i).unwrap_or_else(|| panic!("row {i} ({label}) means nothing"));
                assert_eq!(more_label(cmd), *label, "row {i}");
            }
            other => panic!("the overflow is all items, not {other:?}"),
        }
    }
    assert_eq!(more_choice_at(rows.len()), None, "past the end");
}

#[test]
fn every_list_row_resolves_to_its_own_command() {
    let rows = list_rows(&plain());
    assert_eq!(
        labels(&rows),
        vec!["Bulleted list", "Numbered list", "Checklist", "Callout"]
    );
    for (i, expected) in [
        Command::BulletedList,
        Command::NumberedList,
        Command::Tasks,
        Command::Callout,
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(list_choice_at(i), Some(expected));
    }
    let shortcuts: Vec<_> = rows
        .iter()
        .filter_map(|row| match row {
            Row::Item { shortcut, .. } => *shortcut,
            _ => None,
        })
        .collect();
    assert_eq!(
        shortcuts,
        vec![
            MenuChord::Id(koru_keys::ShortcutId::BulletedList),
            MenuChord::Id(koru_keys::ShortcutId::NumberedList),
            MenuChord::Id(koru_keys::ShortcutId::Checklist),
            MenuChord::Id(koru_keys::ShortcutId::Callout),
        ]
    );
    assert_eq!(list_choice_at(rows.len()), None, "past the end");
}

/// **Alignment left the overflow for the bar.** It is what people look for
/// first, and behind a `⋯` nobody found it — so what is left here is the pair
/// that is genuinely occasional.
#[test]
fn the_overflow_holds_only_the_scripts() {
    assert_eq!(
        labels(&more_rows(&plain())),
        vec!["Superscript", "Subscript"]
    );
    let bar: Vec<_> = crate::bubble::buttons(koru_draw::geometry::Rect::new(0.0, 0.0, 600.0, 40.0))
        .into_iter()
        .map(|p| p.button)
        .collect();
    for cmd in [Command::AlignStart, Command::AlignCenter, Command::AlignEnd] {
        assert!(
            bar.contains(&crate::bubble::Button::Cmd(cmd)),
            "{cmd:?} is on the bar"
        );
    }
    assert!(
        bar.contains(&crate::bubble::Button::List),
        "List is on the bar"
    );
}
