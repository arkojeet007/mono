// SPDX-License-Identifier: MPL-2.0

//! Chords, and the table that turns one into an [`Action`].
//!
//! Data rather than a match, so a user can be given their own table later
//! without touching the editor. It also makes the collisions visible: read the
//! table and you can see that `Ctrl+Shift+Left` is outdent, and therefore *not*
//! extend-selection-by-word.
//!
//! ## Two platform traps this must keep surviving
//!
//! Both were real bugs, and both are pinned by tests below.
//!
//! - **Ctrl+I arrives as `NamedKey::Tab`** on Windows. A chord written as
//!   [`On::Letter`] compares through [`ctrl_letter`], which normalises it — so
//!   Ctrl+I is italic while a bare Tab still indents.
//! - **Ctrl+letter arrives as an ASCII control code**, not a character: Ctrl+K
//!   is `0x0B`. `ctrl_letter` maps both encodings back to the letter.

use winit::keyboard::{Key, ModifiersState, NamedKey};

use koru_core::node::{Align, Heading};
use koru_core::{ListStyle, Mark, Script};

use super::action::Action;

/// The key half of a chord.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum On {
    /// A named key: Enter, Tab, an arrow.
    Named(NamedKey),
    /// A letter, matched through [`ctrl_letter`] so both encodings hit.
    Letter(char),
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct Chord {
    pub on: On,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Chord {
    const fn new(on: On, ctrl: bool, shift: bool, alt: bool) -> Self {
        Self {
            on,
            ctrl,
            shift,
            alt,
        }
    }

    fn matches(&self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        // `self.ctrl` has always meant *primary*, not Control — this table grew
        // a macOS arm before anything else in the tree did. The definition now
        // lives in `koru-keys`, so the shell and the container get the same
        // answer instead of each writing their own.
        if koru_keys::has_foreign_primary(mods)
            || self.ctrl != koru_keys::has_primary(mods)
            || self.shift != mods.shift_key()
            || self.alt != mods.alt_key()
        {
            return false;
        }
        match self.on {
            On::Named(n) => matches!(key, Key::Named(k) if *k == n),
            On::Letter(c) => ctrl_letter(key, text) == Some(c),
        }
    }
}

const fn named(n: NamedKey) -> On {
    On::Named(n)
}

/// The chord a keystroke is, or `None` if it is just typing.
pub fn resolve(key: &Key, text: Option<&str>, mods: ModifiersState) -> Option<Action> {
    DEFAULT
        .iter()
        .find(|(chord, _)| chord.matches(key, text, mods))
        .map(|(_, action)| *action)
}

/// Recover a modified Backspace reported as an ASCII control character.
///
/// **Lives in `koru-ui`** and is re-exported here, beside `ctrl_letter` and
/// for the same reason: `TextField` needs it, and a one-line prompt box may
/// not link the node model to delete a word.
pub use koru_ui::keys::recover_modified_editing_key;

/// The character a Ctrl chord is on, normalised.
///
/// Ctrl+K reaches us as `Character("k")` on some platforms and as the ASCII
/// control character (0x0B) on others — Windows in particular delivers the
/// control code in `text`. Matching only on `"k"` is why Ctrl+K did nothing.
///
/// Punctuation has the same split and a smaller alphabet of it: Ctrl+`\` is
/// 0x1C, and the three codes after it belong to `]`, `^` and `_`. Anything else
/// printable is simply itself, which is what lets a chord be written on a key
/// that is not a letter at all.
///
/// **The normalisation itself is `koru_keys::letter`**, and this table asks it
/// rather than keeping an answer of its own — the row above already resolves
/// *primary* through the same crate, and a chord that agreed about the modifier
/// but not about the letter would be worse than one that agreed about neither.
/// `koru_ui::keys::ctrl_letter` is the same function a second time, because a
/// `TextField` has to normalise a key and `koru-ui` does not link `koru-keys`;
/// the two are kept identical by hand until one of them can be deleted.
pub fn ctrl_letter(key: &Key, text: Option<&str>) -> Option<char> {
    koru_keys::letter(key, text)
}

const C: bool = true;
const S: bool = true;
const _: bool = false;

/// The default map, in match order.
///
/// Order matters only where two rows could both match. Indenting is on Tab
/// alone now, so the horizontal arrows no longer collide with motion; the
/// vertical pair still does, which is why reorder is listed first.
pub const DEFAULT: &[(Chord, Action)] = &[
    // ------------------------------------------------ structure, before motion
    //
    // Reorder is on Ctrl+Shift and nothing else.
    (
        Chord::new(named(NamedKey::ArrowUp), C, S, false),
        Action::Reorder(-1),
    ),
    (
        Chord::new(named(NamedKey::ArrowDown), C, S, false),
        Action::Reorder(1),
    ),
    (
        Chord::new(named(NamedKey::ArrowUp), C, false, false),
        Action::MoveParagraph(-1),
    ),
    (
        Chord::new(named(NamedKey::ArrowDown), C, false, false),
        Action::MoveParagraph(1),
    ),
    (
        Chord::new(named(NamedKey::ArrowUp), false, false, true),
        Action::TogglePageTitle,
    ),
    (
        Chord::new(named(NamedKey::ArrowDown), false, false, true),
        Action::TogglePageTitle,
    ),
    // Horizontal Ctrl+Shift is selection, not structure — Tab and Shift+Tab are
    // the only way to indent.
    (
        Chord::new(named(NamedKey::ArrowLeft), C, S, false),
        Action::MoveLeft {
            by_word: true,
            extend: true,
        },
    ),
    (
        Chord::new(named(NamedKey::ArrowRight), C, S, false),
        Action::MoveRight {
            by_word: true,
            extend: true,
        },
    ),
    // ------------------------------------------------------------- formatting
    //
    // Before Tab, so Ctrl+I is italic rather than indent on Windows.
    (
        Chord::new(On::Letter('b'), C, false, false),
        Action::ToggleMark(Mark::Bold),
    ),
    (
        Chord::new(On::Letter('i'), C, false, false),
        Action::ToggleMark(Mark::Italic),
    ),
    (
        Chord::new(On::Letter('u'), C, false, false),
        Action::ToggleMark(Mark::Underline),
    ),
    (
        Chord::new(On::Letter('s'), C, S, false),
        Action::ToggleMark(Mark::Strike),
    ),
    // Inline code. Ctrl+Shift+C — plain Ctrl+E belongs to the container's
    // sidebar toggle, so the editor never sees it; the shifted form does.
    (
        Chord::new(On::Letter('c'), C, S, false),
        Action::ToggleMark(Mark::Code),
    ),
    // A code block (block-level). Ctrl+` — the backtick is the code key, and
    // the container claims no punctuation chord, so this always reaches the editor.
    (
        Chord::new(On::Letter('`'), C, false, false),
        Action::CodeBlock,
    ),
    (
        Chord::new(On::Letter('h'), C, S, false),
        Action::CycleHighlight,
    ),
    // Alignment on **Ctrl+L / Ctrl+E / Ctrl+R** — Word's and Docs' scheme, and
    // the reason it is not the shifted form: `Ctrl+Shift+L` is registered as a
    // global hotkey by enough Windows utilities (vendor keyboard/mouse suites,
    // clipboard managers) that on a machine with one the key never reaches the
    // window at all. Nothing in the key map can answer for a key the process is
    // never handed, and there is no way to detect it — so the chord moved.
    //
    // The cost is that plain `Ctrl+E` had to leave the container: it toggled the
    // explore pane, and the container resolves before the editor ever sees a
    // key. It is now `Ctrl+Shift+E`, which is what VS Code's Explorer uses — so
    // both halves of this swap landed on the chord their own tradition already
    // spells that way. See `mainview::keys`.
    (
        Chord::new(On::Letter('l'), C, false, false),
        Action::SetAlign(Align::Start),
    ),
    (
        Chord::new(On::Letter('e'), C, false, false),
        Action::SetAlign(Align::Center),
    ),
    (
        Chord::new(On::Letter('r'), C, false, false),
        Action::SetAlign(Align::End),
    ),
    // Headings, on Ctrl+Alt+digit — what Word and Docs use, and the only shape
    // left. Ctrl+1..9 selects a tab in the container, and Shift+digit is the
    // layout's punctuation rather than a digit at all.
    //
    // This is the one place Alt is bound, against the argument above. The trap
    // it buys: **on Windows Ctrl+Alt is AltGr**, so on a German or Polish layout
    // AltGr+2 normally types `@`. The chord matches the logical key, which is
    // still `Character("2")`, so it fires and the character is not typed. A
    // deliberate trade, in the same family as Ctrl+I arriving as Tab — and the
    // reason `SetHeading(None)` is on Ctrl+Alt+0 rather than on a letter.
    //
    // The mirror-image trap: on a layout whose AltGr table has *no* entry for
    // the digit (the US one included), `ToUnicodeEx` has nothing to say, so
    // winit reports `Key::Unidentified` with no text and this table never sees
    // the digit at all. The shell rebuilds the logical key from the physical
    // one before dispatch (`alt_digit` in `koru-workspace-host::app`).
    (
        Chord::new(On::Letter('0'), C, false, true),
        Action::SetHeading(None),
    ),
    (
        Chord::new(On::Letter('1'), C, false, true),
        Action::SetHeading(Some(Heading::H1)),
    ),
    (
        Chord::new(On::Letter('2'), C, false, true),
        Action::SetHeading(Some(Heading::H2)),
    ),
    (
        Chord::new(On::Letter('3'), C, false, true),
        Action::SetHeading(Some(Heading::H3)),
    ),
    (
        Chord::new(On::Letter('4'), C, false, true),
        Action::SetHeading(Some(Heading::H4)),
    ),
    // Lists follow the Word/Google Docs convention. The shell restores the
    // physical number-row key before this table sees it because Shift commonly
    // turns the logical 7/8/9 into layout-specific punctuation.
    (
        Chord::new(On::Letter('7'), C, S, false),
        Action::SetListStyle(ListStyle::Numbered),
    ),
    (
        Chord::new(On::Letter('8'), C, S, false),
        Action::SetListStyle(ListStyle::Bullet),
    ),
    (
        Chord::new(On::Letter('9'), C, S, false),
        Action::SetListStyle(ListStyle::Checkbox),
    ),
    // Ctrl+`\`, and Ctrl+Space is bound to nothing at all. Space is a character
    // key, and a chord on it competes with typing one in a way no other chord
    // here does — `\` is unclaimed, next to Enter, and never typed by accident.
    (
        Chord::new(On::Letter('\\'), C, false, false),
        Action::ClearFormatting,
    ),
    (
        Chord::new(On::Letter('='), C, S, false),
        Action::SetScript(Script::Super),
    ),
    (
        Chord::new(On::Letter('-'), C, S, false),
        Action::SetScript(Script::Sub),
    ),
    // ------------------------------------------------------------------- text
    (
        Chord::new(named(NamedKey::Enter), C, false, false),
        Action::ToggleDone,
    ),
    // **The shifted form ticks the box the row draws**, which the unshifted one
    // above does not: in a document `Ctrl+Enter` leaves a code block or opens a
    // paragraph under a table or prose, so a checklist item had no key at all.
    // See `koru_keys::ShortcutId::TickCheckbox` for why this chord.
    (
        Chord::new(named(NamedKey::Enter), C, S, false),
        Action::TickCheckbox,
    ),
    // **The shifted sibling of Cut saves a drawn diagram's picture.** Applies
    // only on such a row — the handler refuses everything else, so the chord
    // is free to fall through. See `koru_keys::ShortcutId::SaveDiagramAs`.
    (
        Chord::new(On::Letter('x'), C, S, false),
        Action::SaveDiagramAs,
    ),
    (
        Chord::new(named(NamedKey::Enter), false, false, false),
        Action::SplitLine,
    ),
    (
        // Shift+Enter on a block opens a paragraph above it; on text, a split.
        Chord::new(named(NamedKey::Enter), false, true, false),
        Action::SplitLineAbove,
    ),
    (
        Chord::new(named(NamedKey::Enter), false, false, true),
        Action::InsertParagraphAbove,
    ),
    (
        Chord::new(named(NamedKey::Backspace), false, false, false),
        Action::DeleteBack { by_word: false },
    ),
    (
        Chord::new(named(NamedKey::Backspace), C, false, false),
        Action::DeleteBack { by_word: true },
    ),
    (
        Chord::new(named(NamedKey::Delete), false, false, false),
        Action::DeleteForward { by_word: false },
    ),
    (
        Chord::new(named(NamedKey::Delete), C, false, false),
        Action::DeleteForward { by_word: true },
    ),
    // --------------------------------------------------------------- document
    (
        Chord::new(On::Letter('k'), C, false, false),
        Action::OpenLinkPrompt,
    ),
    (
        Chord::new(named(NamedKey::Space), C, S, false),
        Action::OpenEmojiPicker,
    ),
    (
        Chord::new(On::Letter('d'), C, false, false),
        Action::DeleteRows,
    ),
    // ---------------------------------------------------------------- clipboard
    (Chord::new(On::Letter('c'), C, false, false), Action::Copy),
    (Chord::new(On::Letter('x'), C, false, false), Action::Cut),
    (Chord::new(On::Letter('v'), C, false, false), Action::Paste),
    (Chord::new(On::Letter('v'), C, S, false), Action::PastePlain),
    (Chord::new(On::Letter('z'), C, false, false), Action::Undo),
    (Chord::new(On::Letter('z'), C, S, false), Action::Redo),
    // Windows muscle memory — same as Ctrl+Shift+Z.
    (Chord::new(On::Letter('y'), C, false, false), Action::Redo),
    // ------------------------------------------------------------------- view
    (
        Chord::new(named(NamedKey::F4), false, false, false),
        Action::ToggleHideCompleted,
    ),
    (
        Chord::new(named(NamedKey::F5), false, false, false),
        Action::ToggleSharing,
    ),
    // ------------------------------------------------------------------ caret
    //
    // Tab last of the named keys, so the formatting rows above claim Ctrl+I.
    (
        Chord::new(named(NamedKey::Tab), false, false, false),
        Action::Indent(1),
    ),
    (
        Chord::new(named(NamedKey::Tab), false, S, false),
        Action::Indent(-1),
    ),
    (
        Chord::new(named(NamedKey::ArrowLeft), false, false, false),
        Action::MoveLeft {
            by_word: false,
            extend: false,
        },
    ),
    (
        Chord::new(named(NamedKey::ArrowLeft), false, S, false),
        Action::MoveLeft {
            by_word: false,
            extend: true,
        },
    ),
    (
        Chord::new(named(NamedKey::ArrowLeft), C, false, false),
        Action::MoveLeft {
            by_word: true,
            extend: false,
        },
    ),
    (
        Chord::new(named(NamedKey::ArrowRight), false, false, false),
        Action::MoveRight {
            by_word: false,
            extend: false,
        },
    ),
    (
        Chord::new(named(NamedKey::ArrowRight), false, S, false),
        Action::MoveRight {
            by_word: false,
            extend: true,
        },
    ),
    (
        Chord::new(named(NamedKey::ArrowRight), C, false, false),
        Action::MoveRight {
            by_word: true,
            extend: false,
        },
    ),
    (
        Chord::new(named(NamedKey::ArrowUp), false, false, false),
        Action::MoveUp { extend: false },
    ),
    (
        Chord::new(named(NamedKey::ArrowUp), false, S, false),
        Action::MoveUp { extend: true },
    ),
    (
        Chord::new(named(NamedKey::ArrowDown), false, false, false),
        Action::MoveDown { extend: false },
    ),
    (
        Chord::new(named(NamedKey::ArrowDown), false, S, false),
        Action::MoveDown { extend: true },
    ),
    (
        Chord::new(named(NamedKey::PageUp), false, false, false),
        Action::PageUp { extend: false },
    ),
    (
        Chord::new(named(NamedKey::PageUp), false, S, false),
        Action::PageUp { extend: true },
    ),
    (
        Chord::new(named(NamedKey::PageDown), false, false, false),
        Action::PageDown { extend: false },
    ),
    (
        Chord::new(named(NamedKey::PageDown), false, S, false),
        Action::PageDown { extend: true },
    ),
    (
        Chord::new(named(NamedKey::Home), false, false, false),
        Action::LineStart { extend: false },
    ),
    (
        Chord::new(named(NamedKey::Home), false, S, false),
        Action::LineStart { extend: true },
    ),
    (
        Chord::new(named(NamedKey::End), false, false, false),
        Action::LineEnd { extend: false },
    ),
    (
        Chord::new(named(NamedKey::End), false, S, false),
        Action::LineEnd { extend: true },
    ),
    (
        Chord::new(On::Letter('a'), C, false, false),
        Action::SelectAll,
    ),
    (
        Chord::new(named(NamedKey::Escape), false, false, false),
        Action::CollapseSelection,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: ModifiersState = ModifiersState::empty();

    fn ch(c: &str) -> Key {
        Key::Character(c.into())
    }

    fn key(n: NamedKey) -> Key {
        Key::Named(n)
    }

    /// Ctrl+letter arrives as a letter on some platforms and as an ASCII
    /// control code on others — in the key on some, in the text on others.
    /// All of them have to reach the same chord.
    #[test]
    fn ctrl_letter_accepts_both_encodings() {
        assert_eq!(ctrl_letter(&ch("k"), None), Some('k'));
        assert_eq!(ctrl_letter(&ch("K"), None), Some('k'));
        assert_eq!(
            ctrl_letter(&ch("\u{0b}"), None),
            Some('k'),
            "0x0B is Ctrl+K"
        );
        assert_eq!(
            ctrl_letter(&key(NamedKey::Space), Some("\u{0b}")),
            Some('k'),
            "and the same code carried in the text"
        );
        assert_eq!(ctrl_letter(&ch("\u{01}"), None), Some('a'));
        assert_eq!(ctrl_letter(&key(NamedKey::Enter), None), None);
    }

    #[test]
    fn modified_backspace_recovers_both_raw_encodings() {
        for raw in ["\u{8}", "\u{7f}"] {
            assert_eq!(
                recover_modified_editing_key(&ch(raw), ModifiersState::CONTROL),
                Some(Key::Named(NamedKey::Backspace))
            );
        }
        assert_eq!(
            recover_modified_editing_key(&ch("h"), ModifiersState::CONTROL),
            None
        );
    }

    /// The trap that shipped two broken chords: Windows delivers Ctrl+I as Tab.
    #[test]
    fn ctrl_i_is_italic_and_a_bare_tab_indents() {
        assert_eq!(
            resolve(&key(NamedKey::Tab), None, koru_keys::PRIMARY),
            Some(Action::ToggleMark(Mark::Italic))
        );
        assert_eq!(
            resolve(&key(NamedKey::Tab), None, NONE),
            Some(Action::Indent(1))
        );
    }

    /// Alt is swallowed by the Windows system menu and some Linux WMs, which is
    /// why *structure* is never bound to it — it used to be the *primary* chord
    /// for reorder and indent, with Ctrl+Shift as the fallback, which had it
    /// exactly backwards. Ctrl+Alt+digit is one exception, and Alt+Up/Down is
    /// the other: the page-title toggle (see `alt_arrows_toggle_the_page_title`)
    /// is a convenience jump, not structure, so a WM eating it loses nothing.
    #[test]
    fn horizontal_alt_arrows_are_unbound() {
        for k in [NamedKey::ArrowLeft, NamedKey::ArrowRight] {
            assert_eq!(resolve(&key(k), None, ModifiersState::ALT), None, "{k:?}");
        }
    }

    /// Ctrl+Alt+digit sets a heading; Ctrl+digit does not, because the container
    /// claims it for selecting a tab and resolves it before a view is asked.
    #[test]
    fn ctrl_alt_digits_set_headings_and_plain_ctrl_digits_do_not() {
        let ca = koru_keys::PRIMARY | ModifiersState::ALT;
        assert_eq!(
            resolve(&ch("1"), None, ca),
            Some(Action::SetHeading(Some(Heading::H1)))
        );
        assert_eq!(
            resolve(&ch("4"), None, ca),
            Some(Action::SetHeading(Some(Heading::H4)))
        );
        assert_eq!(resolve(&ch("0"), None, ca), Some(Action::SetHeading(None)));
        assert_eq!(resolve(&ch("5"), None, ca), None, "only four are offered");

        for d in ["0", "1", "4"] {
            assert_eq!(
                resolve(&ch(d), None, koru_keys::PRIMARY),
                None,
                "Ctrl+{d} belongs to the tab strip"
            );
        }
    }

    #[test]
    fn ctrl_shift_digits_apply_lists() {
        let mods = koru_keys::PRIMARY.union(ModifiersState::SHIFT);
        assert_eq!(
            resolve(&ch("7"), None, mods),
            Some(Action::SetListStyle(ListStyle::Numbered))
        );
        assert_eq!(
            resolve(&ch("8"), None, mods),
            Some(Action::SetListStyle(ListStyle::Bullet))
        );
        assert_eq!(
            resolve(&ch("9"), None, mods),
            Some(Action::SetListStyle(ListStyle::Checkbox))
        );
    }

    #[test]
    fn alt_enter_inserts_a_paragraph_above() {
        assert_eq!(
            resolve(&key(NamedKey::Enter), None, ModifiersState::ALT),
            Some(Action::InsertParagraphAbove)
        );
    }

    /// Cut and *Save diagram as…* share a letter and differ by Shift, as the
    /// registry says they do.
    #[test]
    fn shift_turns_cut_into_save_diagram_as() {
        assert_eq!(
            resolve(&ch("x"), None, koru_keys::PRIMARY),
            Some(Action::Cut)
        );
        assert_eq!(
            resolve(&ch("x"), None, koru_keys::PRIMARY | ModifiersState::SHIFT),
            Some(Action::SaveDiagramAs)
        );
    }

    /// The vertical pair still collides — reorder and extend-selection are both
    /// on an arrow — so the table's order decides, and structure wins.
    #[test]
    fn ctrl_shift_reorders_vertically_and_selects_horizontally() {
        let cs = koru_keys::PRIMARY | ModifiersState::SHIFT;
        assert_eq!(
            resolve(&key(NamedKey::ArrowUp), None, cs),
            Some(Action::Reorder(-1))
        );
        assert_eq!(
            resolve(&key(NamedKey::ArrowLeft), None, cs),
            Some(Action::MoveLeft {
                by_word: true,
                extend: true
            }),
            "horizontal is selection now; only Tab indents"
        );
    }

    #[test]
    fn ctrl_arrows_move_between_paragraphs() {
        assert_eq!(
            resolve(&key(NamedKey::ArrowUp), None, koru_keys::PRIMARY),
            Some(Action::MoveParagraph(-1))
        );
        assert_eq!(
            resolve(&key(NamedKey::ArrowDown), None, koru_keys::PRIMARY),
            Some(Action::MoveParagraph(1))
        );
    }

    #[test]
    fn alt_arrows_toggle_the_page_title() {
        for named_key in [NamedKey::ArrowUp, NamedKey::ArrowDown] {
            assert_eq!(
                resolve(&key(named_key), None, ModifiersState::ALT),
                Some(Action::TogglePageTitle)
            );
        }
    }

    #[test]
    fn ctrl_d_deletes() {
        assert_eq!(
            resolve(&ch("d"), None, koru_keys::PRIMARY),
            Some(Action::DeleteRows)
        );
    }

    #[test]
    fn ctrl_shift_v_pastes_plain_text() {
        assert_eq!(
            resolve(
                &ch("v"),
                None,
                koru_keys::PRIMARY.union(ModifiersState::SHIFT),
            ),
            Some(Action::PastePlain)
        );
    }

    #[test]
    fn ctrl_shift_space_opens_the_native_emoji_picker() {
        assert_eq!(
            resolve(
                &key(NamedKey::Space),
                None,
                koru_keys::PRIMARY | ModifiersState::SHIFT,
            ),
            Some(Action::OpenEmojiPicker)
        );
    }

    #[test]
    fn shift_extends_and_ctrl_moves_by_word() {
        assert_eq!(
            resolve(&key(NamedKey::ArrowLeft), None, ModifiersState::SHIFT),
            Some(Action::MoveLeft {
                by_word: false,
                extend: true
            })
        );
        assert_eq!(
            resolve(&key(NamedKey::ArrowLeft), None, koru_keys::PRIMARY),
            Some(Action::MoveLeft {
                by_word: true,
                extend: false
            })
        );
    }

    /// Plain typing is not a chord — it has to fall through to the text path.
    #[test]
    fn ordinary_typing_resolves_to_nothing() {
        assert_eq!(resolve(&ch("a"), Some("a"), NONE), None);
        assert_eq!(resolve(&ch(" "), Some(" "), NONE), None);
    }

    /// Page Up and Page Down move the caret, and Shift extends — the page
    /// counterparts of the arrow pair.
    #[test]
    fn page_up_and_page_down_move_and_shift_extends() {
        assert_eq!(
            resolve(&key(NamedKey::PageDown), None, NONE),
            Some(Action::PageDown { extend: false })
        );
        assert_eq!(
            resolve(&key(NamedKey::PageDown), None, ModifiersState::SHIFT),
            Some(Action::PageDown { extend: true })
        );
        assert_eq!(
            resolve(&key(NamedKey::PageUp), None, NONE),
            Some(Action::PageUp { extend: false })
        );
        assert_eq!(
            resolve(&key(NamedKey::PageUp), None, ModifiersState::SHIFT),
            Some(Action::PageUp { extend: true })
        );
    }

    /// Ctrl+Z and Ctrl+Shift+Z are different actions on the same letter, which
    /// only works if the matcher compares shift exactly. Ctrl+Y is redo too
    /// (Windows muscle memory).
    #[test]
    fn shift_distinguishes_undo_from_redo() {
        assert_eq!(
            resolve(&ch("z"), None, koru_keys::PRIMARY),
            Some(Action::Undo)
        );
        assert_eq!(
            resolve(&ch("z"), None, koru_keys::PRIMARY | ModifiersState::SHIFT),
            Some(Action::Redo)
        );
        // **The primary modifier, not literally Control.** The table binds
        // `y` with `C`, which means ⌘ on macOS — and a chord there refuses the
        // foreign primary outright, so asserting `CONTROL` fails on a Mac while
        // passing on Windows, which is the whole class of bug `koru-keys`
        // exists to stop.
        assert_eq!(
            resolve(&ch("y"), None, koru_keys::PRIMARY),
            Some(Action::Redo)
        );
    }

    /// No chord may appear twice: the second would be unreachable, and a user
    /// remapping the first would silently get the other.
    #[test]
    fn no_chord_is_shadowed_by_an_earlier_one() {
        for (i, (chord, action)) in DEFAULT.iter().enumerate() {
            if let Some((_, first)) = DEFAULT[..i].iter().find(|(c, _)| c == chord) {
                panic!("{chord:?} maps to {action:?} but is already taken by {first:?}");
            }
        }
    }
    /// **The registry is not a description; it is checked against this table.**
    ///
    /// Every editor-layer entry in `koru_keys::REGISTRY` must actually resolve
    /// here. Without this the registry would be a fifth copy of the truth and
    /// would drift from the four resolvers within a release — which is exactly
    /// why it was not written until it could be verified.
    ///
    /// The reverse direction is deliberately *not* asserted: this table also
    /// binds pure motion (arrows, Home/End, Page keys) that no menu names and
    /// no user remaps, and listing those in the registry would pad it without
    /// making anything safer.
    #[test]
    fn every_editor_shortcut_in_the_registry_resolves_here() {
        use koru_keys::{Layer, On as KOn, registry};
        for entry in registry::of_layer(Layer::Editor) {
            let key = match entry.chord.on {
                KOn::Named(n) => Key::Named(n),
                KOn::Letter(c) => Key::Character(c.to_string().into()),
            };
            let text = match entry.chord.on {
                KOn::Letter(c) => Some(c.to_string()),
                KOn::Named(_) => None,
            };
            let got = resolve(&key, text.as_deref(), entry.chord.mods);
            assert!(
                got.is_some(),
                "the registry says {:?} is `{}`, but the editor's key map does \
                 not bind it — one of the two moved without the other",
                entry.id,
                koru_keys::label(&entry.chord),
            );
        }
    }
    /// **The three marks resolve identically**, which is what makes a report of
    /// "⌘B and ⌘U work but ⌘I does not" a *delivery* problem rather than a
    /// binding one. Worth pinning: `letter` maps `NamedKey::Tab` back to `'i'`
    /// for Windows, and that is exactly the kind of special case that later
    /// looks like the culprit when the real fault is elsewhere.
    #[test]
    fn the_three_marks_resolve_the_same_way() {
        let mods = koru_keys::PRIMARY;
        for (c, want) in [
            ('b', Mark::Bold),
            ('i', Mark::Italic),
            ('u', Mark::Underline),
        ] {
            assert_eq!(
                resolve(&ch(&c.to_string()), Some(&c.to_string()), mods),
                Some(Action::ToggleMark(want)),
                "the primary modifier with {c}"
            );
        }
    }
}
