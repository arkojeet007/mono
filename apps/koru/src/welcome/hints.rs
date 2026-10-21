// SPDX-License-Identifier: MPL-2.0

//! *Learn your way around*: the chord reminders the row opens inline, looked
//! up in `koru_keys`' registry rather than written out.
//!
//! These were the ordinary Welcome's *Getting around* block, and their source
//! of truth is unchanged. The dictation line is hidden for now.

/// One line the user can type to get somewhere. Not a key map — the chords are
/// owned by the container; this is a reminder of them.
///
/// **The id, not the chord.** All four were once spelled out here and all four
/// were wrong on macOS. They are looked up at draw time through
/// `koru_keys::show_hint`, which also means the block goes quiet when guide
/// hints are off.
pub(super) const HINTS: [(koru_keys::ShortcutId, &str); 4] = [
    (koru_keys::ShortcutId::NewTab, "new tab"),
    (
        koru_keys::ShortcutId::ToggleExplore,
        "explore the workspace",
    ),
    (
        koru_keys::ShortcutId::ToggleContext,
        "comments, assets and threads",
    ),
    (koru_keys::ShortcutId::NextTab, "move between tabs"),
];

/// The lines to draw, chord and all — **empty when guide hints are off**.
///
/// One function so the measure and the paint cannot disagree.
pub(super) fn hint_lines() -> Vec<(String, &'static str)> {
    HINTS
        .iter()
        .filter_map(|(id, what)| {
            koru_keys::show_hint(*id, koru_keys::Section::Guide).map(|chord| (chord, *what))
        })
        .collect()
}

/// The chord beside *Make your first document*: the registry's New tab
/// binding — a new tab is a new document. `None` when guide hints are
/// off.
pub(super) fn new_document_chord() -> Option<String> {
    koru_keys::show_hint(koru_keys::ShortcutId::NewTab, koru_keys::Section::Guide)
}
