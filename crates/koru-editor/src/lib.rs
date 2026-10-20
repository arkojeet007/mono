// SPDX-License-Identifier: MPL-2.0

//! Editing a list of paragraphs — the component every view of a document uses.
//!
//! There are two ways in, and the two views in this repo take one each:
//!
//! - [`Editor`] is the whole thing in a struct. Embed one and you have rows,
//!   a caret, marks, pills, pickers, undo and a key map.
//! - [`Editing`] is the same vocabulary as a trait. Implement it on your own
//!   type — five accessors — and your struct *is* an editor, with `body` and
//!   `view` as its own fields rather than something it wraps.
//!
//! `apps/koru-document` takes the first; the task outline app (since deleted)
//! took the second, because it had structure a document does not and wanted
//! its own action dispatch rather than one it had to refuse half of.
//!
//! ## What is deliberately absent
//!
//! No window, no event loop, no notion of *which* view this is. A window is
//! `koru-workspace-host`'s, and what a row means — a task, a paragraph, a calendar
//! entry — belongs to whoever draws it. This crate knows only that a document
//! is a list of nodes carrying text.
//!
//! ## The three layers
//!
//! Each depends only on the one below it, which is what lets two views share
//! all of their text editing without sharing anything else:
//!
//! - [`paragraph`] — one node's text. Caret, selection, word motion, marks,
//!   pills. It knows nothing about its neighbours.
//! - [`body`] — the list. Rebuild from the document, focus, row hit-testing,
//!   the row selection, and the operations that touch *two* nodes.
//! - [`editor`] — what needs both, plus the window's own state.

/// What a paragraph's text is to a screen reader, and what an assistive
/// technology's request to select text means in bytes. No platform, no
/// identity, no editing — see the module's own doc.
pub mod accessibility;
pub mod body;
pub mod browser;
pub mod chips;
pub mod clipboard;
pub mod components;
/// A `mermaid` code block drawn in place: the per-view state behind the
/// drawing, the request seam the host answers, and the words a block says
/// when it is not drawn.
pub mod diagram;
pub mod dictation;
pub mod draw;
pub mod editor;
pub mod input;
/// The words of a row, as an objective — shared by the two delegation gestures
/// a document raises.
pub mod objective;
pub mod paragraph;
pub mod pillinfo;
/// What typing may turn a row into: everything, or the AI chat composer's
/// text-and-`@` subset.
pub mod policy;
pub mod reminders;
pub mod rowlayout;
pub mod table;
/// The seam onto the six canonical task commands, and the sink a host drains
/// for the activity they owe.
pub mod tasks;
pub mod trigger;
pub mod utils;
pub mod view_state;

pub use body::Body;
pub use diagram::{DiagramEntry, DiagramRequest, DiagramState};
pub use dictation::{Affinity, Composition, Mapped, ProvisionalRun};
pub use editor::{AttrPicker, Editing, Editor, prepare};
pub use objective::row_words;
pub use paragraph::{Paragraph, TaskRow};
pub use policy::Policy;
pub use view_state::{
    DelegateRequest, DragRow, Embedded, Hover, OpenWhere, PasteBatch, PasteReplacement,
    SaveDiagramAs, SearchHighlight, ViewState,
};
pub mod emoji;

/// What the shell should do with a file the user activated from a body pill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileAction {
    Open,
    Reveal,
    Save,
    /// Put a trashed file back where it came from.
    Restore,
}
