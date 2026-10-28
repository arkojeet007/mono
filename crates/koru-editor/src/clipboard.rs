// SPDX-License-Identifier: MPL-2.0

//! What a copy captured, and how it reaches the rest of the machine.
//!
//! Two clipboards, kept in step. The **system** one holds plain text, so a task
//! copied here pastes into Slack and a paragraph copied from a browser pastes in
//! as tasks. The **internal** one holds the rows themselves — marks, pills,
//! completion, nesting — because none of that survives a trip through a string.
//!
//! A paste prefers the internal payload, but only while it is still *true*:
//! [`take`] re-reads the system clipboard and compares it with the text the last
//! copy wrote. Anything copied in another application since then wins, which is
//! what stops a stale payload being pasted over what the user actually copied.
//!
//! **Process-global**, like the theme. A copy in one window has to paste in
//! another, and the clipboard is a property of the machine rather than of a
//! view. The lock is only ever held across a `Vec` clone and an OS call.

use std::borrow::Cow;
#[cfg(test)]
use std::cell::RefCell;
#[cfg(not(test))]
use std::sync::{Mutex, OnceLock};

use koru_core::attributes::{
    AttributeDefinition, KEY_DUE, KEY_TAG, KEY_TASK, KEY_TASK_STATE, Lifecycle, TaskReason,
    ValueType, system_definition,
};
use koru_core::ids::TagId;
use koru_core::node::{Align, Attr, AttrValue, BlobHash, Heading, Kind, ListStyle};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::tags::{TagLanding, resolve_copied_tag};
use koru_core::tasks::{TaskFields, faithful_import, task_capable};
use koru_core::{NodeId, TableLayout, TextRun, TextStyle, Utf8Pos, WorkspaceId, node::Node};

use crate::components::link_prompt;
use crate::editor::Editing;
use crate::pillinfo;
use crate::trigger;
use crate::view_state::ViewState;

/// One copied row, with everything a paste needs to rebuild it.
///
/// `runs` rather than a `String`: a `TextRun` carries the marks, the value
/// styles and — where it is a pill anchor — the `AttrId` that names which
/// attribute belongs there. `attrs` carries the attributes themselves, since
/// the id alone means nothing in the document being pasted into.
/// An image row on the internal clipboard — hash and display meta only. Bytes
/// stay in the blob store; the shell embeds them into CF_HTML on the way out.
#[derive(Clone, Debug)]
pub struct CopiedImage {
    pub hash: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub name: Option<String>,
}

/// How a copied list item nests, matching shell [`RichListSpec`](koru_workspace_host::RichListSpec).
///
/// Style lives on the invisible `ListBlock` container in the model; items are
/// ordinary paragraphs. The clipboard carries both so paste can rebuild the
/// containers — without this, Koru→Koru paste dropped every bullet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopiedListSpec {
    pub style: ListStyle,
    /// Nest level of the enclosing list (0 = top-level list in the selection).
    pub depth: u16,
}

/// The callout box a copied row was inside, modelled on [`CopiedListSpec`]
/// for the same reason: the box is an invisible container (`Kind::Callout`)
/// whose look lives on it, and the rows inside are ordinary paragraphs and
/// list items. Set on every row inside a box, so a paste can rebuild the
/// container — the way [`paste_rows`] rebuilds list blocks.
///
/// A run of rows carrying the same spec at one depth is one box; a row
/// whose spec differs opens another at that depth, and a row with none
/// closes every box. Two boxes of the same look side by side therefore
/// paste as one, which is the trade a list's spec makes too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopiedCalloutSpec {
    /// The box's icon as stored — `None` is [`koru_core::callout::DEFAULT_ICON`].
    pub icon: Option<String>,
    /// The box's tint token as stored — `None` is the default tint.
    pub color: Option<String>,
    /// Nest level of the enclosing box (0 = a box with no box around it).
    pub depth: u16,
}

/// One **node-level** attribute a copy took with it: a `koru.due`, `koru.tag`,
/// `koru.color` or `koru.priority` value, or a **custom** key the workspace
/// defined. These are the attributes with no pill anchor in the row's text —
/// that absence is what keeps the text clean, and it is exactly why the
/// anchor-keyed [`CopiedRow::attrs`] mechanism could never carry them.
///
/// A custom one lands only when the paste stays inside the workspace the copy
/// left; see [`split_attrs`] and [`apply_node_attrs`].
#[derive(Clone, Debug)]
pub struct CopiedAttr {
    /// Role and value as read from the source node. The source's `AttrId` is
    /// **not** reused: a paste sets the attribute afresh, so the destination
    /// mints its own id and cardinality-one folding still applies to a row that
    /// already carried the key.
    pub attr: Attr,
    /// A `koru.tag`'s **name** in the workspace it left, resolved through that
    /// view's tag table at copy time. A `TagId` names a record in one
    /// workspace's system document and means nothing in another's, so the name
    /// is the only thing that can find the same tag on the far side of a
    /// cross-workspace paste. `None` for every other role, and for a tag whose
    /// own table had already forgotten it.
    pub tag_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CopiedRow {
    pub runs: Vec<TextRun>,
    /// The attributes a **pill** in `runs` anchors, keyed by the `AttrId` the
    /// anchor carries. Node-level attributes live in [`node_attrs`] instead,
    /// and the two are kept apart on purpose: `insert_runs` applies an
    /// attribute only where a run names it, which is what a pill paste means.
    ///
    /// [`node_attrs`]: Self::node_attrs
    pub attrs: Vec<Attr>,
    /// What the *block* carries — its due date, its tags, its color, its
    /// priority. Re-applied by [`apply_node_attrs`], the way `apply_meta`
    /// re-applies the rest of the node's non-text state.
    pub node_attrs: Vec<CopiedAttr>,
    /// What this row *was*. A paste adapts it to the row it lands beside, so a
    /// task copied into a document arrives as prose — see [`paste_rows`].
    pub kind: Kind,
    pub done: bool,
    pub level_name: Option<String>,
    /// The callout box this row was inside, when it was in one — its look
    /// and nesting, so the paste rebuilds the box around the rows that carry
    /// it. The box itself is never a row, so this is where it travels.
    pub callout: Option<CopiedCalloutSpec>,
    pub align: Option<Align>,
    pub heading: Option<Heading>,
    pub table_layout: Option<TableLayout>,
    /// A code block's grammar — `NodeMeta::language`, verbatim. It rides with
    /// the row so the block is still `rust` wherever in Koru it lands, and so
    /// the plain-text flavor can leave as a fence that names it. `None` for
    /// every other kind, and for a code block left on auto-detect.
    pub language: Option<String>,
    /// Depth *relative to the shallowest row copied*, so a subtree keeps its
    /// shape wherever it lands. The first row is always zero. For list items in
    /// a flat document body, nesting is carried by [`list`] instead.
    pub depth: u16,
    /// Set when this row is a picture. Koru→Koru paste rebuilds a blob node;
    /// outgoing CF_HTML emits a `data-koru-hash` placeholder the shell fills.
    pub image: Option<CopiedImage>,
    /// List membership when this row was an item under a `ListBlock`.
    pub list: Option<CopiedListSpec>,
}

impl CopiedRow {
    /// The text alone, with each pill replaced by the label it draws.
    ///
    /// The anchor character is invisible and means nothing outside this
    /// document, so pasting a row into a chat window would otherwise drop a
    /// zero-width joiner where the date pill was.
    pub fn to_text(&self, labels: &dyn Fn(&Attr) -> String) -> String {
        let mut out = String::new();
        for run in &self.runs {
            match run
                .pill
                .and_then(|id| self.attrs.iter().find(|a| a.id == id))
            {
                Some(attr) => out.push_str(&labels(attr)),
                None if run.is_pill() => {}
                None => out.push_str(&run.text),
            }
        }
        out
    }

    /// The markdown prefix this row leaves with, for the plain-text flavour.
    ///
    /// **Export only, and it must stay that way.** There are no markdown
    /// delimiters in the text — a heading is `NodeMeta.heading`, the same way
    /// emphasis is a Loro mark. This exists because every other application
    /// understands `## Heading` and dropping the level on the way out is a real
    /// loss. Nothing reads it back: `paste_plain_text` leaves a leading `#`
    /// exactly where the user typed it.
    fn markdown_prefix(&self) -> String {
        if self.kind == Kind::Quote {
            return "> ".into();
        }
        if let Some(spec) = self.list {
            let indent = "  ".repeat(spec.depth as usize);
            let marker = match spec.style {
                ListStyle::Numbered | ListStyle::Letter | ListStyle::Roman => "1. ",
                ListStyle::Checkbox => "- [ ] ",
                ListStyle::Bullet => "- ",
            };
            return format!("{indent}{marker}");
        }
        match self.heading.map(Heading::level) {
            Some(1) => "# ".into(),
            Some(2) => "## ".into(),
            Some(3) => "### ".into(),
            Some(4) => "#### ".into(),
            Some(5) => "##### ".into(),
            Some(6) => "###### ".into(),
            _ => String::new(),
        }
    }
}

/// What a copy put on the clipboard.
#[derive(Clone, Debug)]
pub enum Payload {
    /// Whole rows: pasting makes rows.
    ///
    /// The second half is the **workspace the copy left**, as the view knew it
    /// ([`ViewState::workspace`](crate::view_state::ViewState::workspace)). It
    /// rides with the rows rather than beside the clipboard's `wrote` string
    /// because it is a fact about *this* payload, and a payload outlives the
    /// window that made it. `None` means the source could not say, which the
    /// paste reads as "not the same workspace".
    Rows(Vec<CopiedRow>, Option<WorkspaceId>),
    /// Part of one row: pasting inserts into the line at the caret.
    Inline(Vec<TextRun>, Vec<Attr>),
    /// One line with no runs behind it — what a prompt field has to offer.
    ///
    /// It exists so that a field copy can *replace* the rich payload. Setting
    /// the system text and leaving the old payload behind would make [`take`]
    /// compare equal, decide the payload was still current, and paste rows the
    /// user copied ten minutes ago.
    Text(String),
}

/// What a paste found, once the system clipboard has had its say.
pub enum Taken {
    /// Rows, and the workspace they were copied out of — see
    /// [`Payload::Rows`].
    Rows(Vec<CopiedRow>, Option<WorkspaceId>),
    Inline(Vec<TextRun>, Vec<Attr>),
    /// Plain text that **this process** put on the clipboard (a prompt-field
    /// copy). It carries no images, so it always pastes as text.
    Text(String),
    /// Text from **another application** — a browser, Word, a terminal. It
    /// pastes exactly as [`Taken::Text`] does; the variant exists so a document
    /// view can ask the shell to append any pictures the clipboard carries
    /// alongside the text (a Word copy's inline images, embedded in the RTF).
    ForeignText(String),
    Nothing,
}

#[derive(Default)]
struct Clipboard {
    payload: Option<Payload>,
    /// Exactly what the last copy wrote to the system clipboard. The payload is
    /// only trustworthy while this is still what the system holds.
    wrote: String,
}

#[cfg(not(test))]
fn clipboard() -> &'static Mutex<Clipboard> {
    static IT: OnceLock<Mutex<Clipboard>> = OnceLock::new();
    // Poisoning is ignored: a panic in one window must not take copy and paste
    // away from every other one.
    IT.get_or_init(Mutex::default)
}

#[cfg(not(test))]
fn with_clipboard<R>(f: impl FnOnce(&mut Clipboard) -> R) -> R {
    let mut clipboard = clipboard()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    f(&mut clipboard)
}

#[cfg(test)]
thread_local! {
    static TEST_CLIPBOARD: RefCell<Clipboard> = RefCell::new(Clipboard::default());
}

#[cfg(test)]
fn with_clipboard<R>(f: impl FnOnce(&mut Clipboard) -> R) -> R {
    TEST_CLIPBOARD.with(|clipboard| f(&mut clipboard.borrow_mut()))
}

/// Clipboard fixtures are thread-local, so legacy tests can retain their
/// serialization guard without imposing a process-wide lock.
#[cfg(test)]
fn lock() {}

/// Test substitute for the platform text clipboard. `Unavailable` is the
/// initial state because a rich HTML copy is completed by the workspace host;
/// until that happens, the editor must continue to trust its internal payload.
#[cfg(test)]
#[derive(Clone, Debug, Default)]
enum FixtureText {
    #[default]
    Unavailable,
    Text(String),
}

#[cfg(test)]
#[derive(Default)]
struct FixtureBackend(std::sync::Mutex<FixtureText>);

#[cfg(test)]
impl koru_clipboard::Backend for FixtureBackend {
    fn text(&self) -> Result<Option<String>, koru_clipboard::Error> {
        match &*self.0.lock().unwrap_or_else(|error| error.into_inner()) {
            FixtureText::Unavailable => Err(koru_clipboard::Error::new(
                koru_clipboard::ErrorCode::Unavailable,
            )),
            FixtureText::Text(text) => Ok(Some(text.clone())),
        }
    }

    fn set_text(&self, text: &str) -> Result<(), koru_clipboard::Error> {
        *self.0.lock().unwrap_or_else(|error| error.into_inner()) =
            FixtureText::Text(text.to_owned());
        Ok(())
    }
}

#[cfg(test)]
thread_local! {
    static SYSTEM_FIXTURE: koru_clipboard::Clipboard<FixtureBackend> =
        koru_clipboard::Clipboard::new(FixtureBackend::default());
}

#[cfg(not(test))]
fn platform_text() -> koru_clipboard::TextPayload {
    koru_clipboard::system().text()
}

#[cfg(test)]
fn platform_text() -> koru_clipboard::TextPayload {
    SYSTEM_FIXTURE.with(koru_clipboard::Clipboard::text)
}

#[cfg(not(test))]
fn platform_set_text(text: &str) -> bool {
    koru_clipboard::system().set_text(text).is_ok()
}

#[cfg(test)]
fn platform_set_text(text: &str) -> bool {
    SYSTEM_FIXTURE.with(|clipboard| clipboard.set_text(text).is_ok())
}

/// Temporary diagnostic for the paste decision. Off unless
/// `KORU_CLIPBOARD_DEBUG` is truthy; stderr only (the shell also writes a log
/// file). Remove once the paste regression is pinned down.
pub(crate) fn clip_dbg(line: impl AsRef<str>) {
    if koru_env::switch(koru_env::name::KORU_CLIPBOARD_DEBUG) {
        eprintln!("[koru:clip] {}", line.as_ref());
    }
}

/// Undo/paste-history diagnostic. On when `KORU_UNDO_DEBUG` **or**
/// `KORU_CLIPBOARD_DEBUG` is truthy — one env covers the paste→Ctrl+Z chain.
pub fn undo_dbg(line: impl AsRef<str>) {
    if koru_env::switch(koru_env::name::KORU_UNDO_DEBUG)
        || koru_env::switch(koru_env::name::KORU_CLIPBOARD_DEBUG)
    {
        eprintln!("[koru:undo] {}", line.as_ref());
    }
}

/// What the machine's own clipboard has to say.
///
/// Three states, not two, and the missing one was the middle. When the
/// clipboard holds a file or an image, `get_text` fails — and reading that as
/// "there is no clipboard" made a paste fall back on *this* process's last
/// copy. Copy a file in Explorer, press Ctrl+V here, and out came whatever you
/// had copied in Koru twenty minutes ago.
enum System {
    /// Nothing to read: some Linux sessions have no clipboard, another process
    /// may be holding it for a moment, or this is a test. Not an error worth
    /// surfacing, and copy still works within the process without one.
    Absent,
    /// There is a clipboard and what it holds is not text. Something else was
    /// copied since, so the payload here is stale however rich it is.
    NotText,
    Text(String),
}

fn system_text() -> System {
    match platform_text() {
        koru_clipboard::TextPayload::Text(text) if text.is_empty() => {
            clip_dbg("system_text: NotText (empty text — likely an image-only clipboard)");
            System::NotText
        }
        koru_clipboard::TextPayload::Text(text) => {
            clip_dbg(format!("system_text: Text({} bytes)", text.len()));
            System::Text(text)
        }
        koru_clipboard::TextPayload::NonText => {
            clip_dbg("system_text: NotText (ContentNotAvailable)");
            System::NotText
        }
        // A clipboard momentarily held by another process is *unreadable*
        // rather than occupied — throwing a good payload away over a transient
        // lock would be its own bug.
        koru_clipboard::TextPayload::Unavailable => {
            clip_dbg("system_text: Absent (shared backend unavailable)");
            System::Absent
        }
    }
}

/// Whether the system clipboard holds something that is not text — a picture,
/// or a file copied in Explorer. The editor does not read those bytes, but it
/// can tell the workspace host to try the shared image backend, since
/// `take` has already thrown the rich payload away as stale at this point.
pub fn system_has_non_text() -> bool {
    matches!(system_text(), System::NotText)
}

fn set_system_text(text: &str) -> bool {
    platform_set_text(text)
}

/// Put plain text on the **system** clipboard only.
///
/// For text that did not come out of a document and must not paste back in as
/// though it had — an error message, a path. The internal payload is
/// deliberately left alone: it is compared against the system clipboard and
/// found stale, so the next paste is this text as plain text, which is what
/// copying an error message means.
///
/// Contrast [`put`], which is the editor copying its own content and wants both.
pub fn put_system_text(text: &str) -> bool {
    set_system_text(text)
}

/// Put a copy on both clipboards.
pub fn put(payload: Payload, text: String) {
    set_system_text(&text);
    with_clipboard(|clipboard| {
        clipboard.payload = Some(payload);
        clipboard.wrote = text;
    });
}

/// Forget the last copy. Used when an image is copied instead of text — the
/// image goes on the system clipboard through the shell, and a stale text
/// payload must not come back on the next paste.
pub fn clear() {
    with_clipboard(|clipboard| {
        clipboard.payload = None;
        clipboard.wrote.clear();
    });
}

/// Put one line of plain text on both clipboards, from somewhere with no runs
/// to offer — a prompt field.
///
/// Through `put` rather than round the side of it, so the rich payload is
/// **replaced**: a fresh `wrote` beside a stale payload would make [`take`]
/// believe the last copy was still current.
pub fn put_plain(text: &str) {
    put(Payload::Text(text.to_owned()), text.to_owned());
}

/// What to paste.
///
/// The system clipboard is asked first and it is the one that decides: if it no
/// longer holds what this process wrote, something else was copied since, and
/// the rich payload is out of date however good it is.
pub fn take() -> Taken {
    let system = system_text();
    let clip = with_clipboard(|clipboard| (clipboard.wrote.clone(), clipboard.payload.clone()));

    let ours = match &system {
        // No clipboard at all — the internal payload is all there is.
        System::Absent => true,
        // Somebody else's content, and it is not text we can use either.
        System::NotText => false,
        System::Text(text) => clipboard_text_matches(&clip.0, text),
    };

    if ours {
        match clip.1 {
            Some(Payload::Rows(rows, source)) => return Taken::Rows(rows, source),
            Some(Payload::Inline(runs, attrs)) => return Taken::Inline(runs, attrs),
            // Plain text is what `Taken::Text` already means, so a field copy
            // pastes into a row as text without any special case downstream.
            Some(Payload::Text(text)) => return Taken::Text(text),
            None => {}
        }
    }

    match system {
        System::Text(text) if !text.is_empty() => Taken::ForeignText(text),
        _ => Taken::Nothing,
    }
}

/// Windows' Unicode-text clipboard may canonicalise newlines to CRLF even
/// when Koru wrote LF. That is still our text and therefore still authorises
/// the richer in-process payload.
fn clipboard_text_matches(wrote: &str, read: &str) -> bool {
    !wrote.is_empty() && wrote.replace("\r\n", "\n") == read.replace("\r\n", "\n")
}

/// The clipboard as one line of plain text, for a field that can hold nothing
/// else — a name, a URL.
///
/// Newlines become spaces rather than being refused: pasting three lines into a
/// one-line prompt should give you something to edit, not nothing.
pub fn one_line() -> Option<String> {
    let rendered = match take() {
        Taken::Text(text) | Taken::ForeignText(text) => text,
        Taken::Nothing => return None,
        // Rows or a range of one: the text half is what a field can take, and
        // that is exactly what the last copy wrote out.
        Taken::Rows(..) | Taken::Inline(..) => with_clipboard(|clipboard| clipboard.wrote.clone()),
    };
    let flat = rendered.replace(['\n', '\r', '\t'], " ");
    (!flat.trim().is_empty()).then_some(flat)
}

/// Begin a test with clean, thread-local rich and platform state.
///
#[cfg(test)]
pub(crate) fn fixture() {
    TEST_CLIPBOARD.with(|clipboard| *clipboard.borrow_mut() = Clipboard::default());
    SYSTEM_FIXTURE.with(|clipboard| {
        *clipboard
            .backend()
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = FixtureText::Unavailable;
    });
}

/// Split pasted text the way a document would read it.
///
/// `\r\n` and a lone `\r` both count, because text arrives here from anywhere.
/// A trailing newline does **not** make a final empty row — copying a line from
/// a browser usually brings one, and it would paste as a blank task every time.
pub fn split_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(str::to_owned)
        .collect();
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// What one tab is worth, in columns.
///
/// Four, because that is a tab in almost every application a paste could come
/// from, and because it makes one tab and four spaces the *same* one level —
/// the two commonest ways indented text arrives. It never affects a round trip
/// through this module's own text half: [`render`] writes one tab per level, so
/// any width reads them back the same.
const TAB_COLUMNS: usize = 4;

/// A line's leading whitespace measured in columns, and the line without it.
///
/// A tab advances to the next tab stop rather than counting as one character,
/// so a line indented with a tab and a line indented with four spaces measure
/// the same and sort correctly against each other.
fn lead_width(line: &str) -> (usize, &str) {
    let mut width = 0;
    for (at, ch) in line.char_indices() {
        match ch {
            ' ' => width += 1,
            '\t' => width = (width / TAB_COLUMNS + 1) * TAB_COLUMNS,
            _ => return (width, &line[at..]),
        }
    }
    // Whitespace all the way to the end: blank, whatever it is made of.
    (width, "")
}

/// How deep each line of a pasted block sits, and each line without its indent.
///
/// **The whole block at once**, which is the point. Indentation only means
/// anything relative to the lines around it, and a function looking at one line
/// can only guess at the scale. Guessing is what made a four-space indent
/// arrive two levels deep, three spaces arrive one level deep, and a single
/// leading space vanish along with the level it should have made.
///
/// The rule is the one outliners and indentation-sensitive languages use: keep
/// a stack of the widths currently open, and a line wider than the top opens a
/// level, narrower closes as many as it must, equal stays. The block decides
/// its own step size, so tabs, two spaces, four spaces and a mixture of them
/// all read correctly without anything being told which to expect.
///
/// The awkward inputs, and what each does:
///
/// - **The first line is indented** — a fragment taken out of the middle of
///   something. The shallowest line in the block is the baseline, so it lands
///   where it is pasted rather than several levels in.
/// - **A dedent matching no open level** closes to the nearest one and reopens
///   there. A paste never refuses ragged input.
/// - **A blank line** says nothing about structure, so it keeps the depth of
///   the line above and neither opens nor closes. Reading its zero width as an
///   outdent would flatten everything after a paragraph break.
/// - **No level is ever skipped**, because a depth *is* the stack's height and
///   the stack only ever grows by one.
pub fn indents_of(lines: &[String]) -> Vec<(u16, &str)> {
    let measured: Vec<(usize, &str)> = lines.iter().map(|l| lead_width(l)).collect();

    // The shallowest line is depth zero, wherever it happens to start.
    let base = measured
        .iter()
        .filter(|(_, body)| !body.is_empty())
        .map(|(width, _)| *width)
        .min()
        .unwrap_or(0);

    let mut stack = vec![base];
    let mut depth = 0;
    let mut out = Vec::with_capacity(lines.len());
    for (width, body) in measured {
        if body.is_empty() {
            out.push((depth, body));
            continue;
        }
        while stack.len() > 1 && width < stack[stack.len() - 1] {
            stack.pop();
        }
        if width > stack[stack.len() - 1] {
            stack.push(width);
        }
        depth = (stack.len() - 1) as u16;
        out.push((depth, body));
    }
    out
}

// --------------------------------------------------------------- copying
//
// Generic over the editor rather than taking `&mut dyn Editing`, so a default
// method on the trait can call straight through without a coercion the compiler
// will not make for a possibly-unsized `Self`.

/// What a copy should take, decided the way every editor decides it: the
/// selection if there is one, and the line you are standing in if there is not.
///
/// `Rows` names no rows, deliberately. *Which* rows is a question about the
/// document — a row brings its whole subtree — and only [`capture`] has the
/// document to ask.
enum What {
    /// Rows are marked, or there is no text selection to prefer over them.
    Rows,
    /// Part of one row.
    Inline(usize, std::ops::Range<usize>),
}

fn what_to_copy<E: Editing + ?Sized>(e: &E) -> Option<What> {
    let body = e.body();
    if body.has_row_selection() {
        return Some(What::Rows);
    }
    let row = body.focused()?;
    let range = row.range();
    if range.is_empty() {
        // No selection at all: Ctrl+C takes the whole task, which is the point
        // of "copy a task" — nobody selects a line before copying it.
        return Some(What::Rows);
    }
    Some(What::Inline(body.focus, range))
}

/// Every row a row copy should take: the selected ones, and **everything
/// underneath them**.
///
/// A task owns its sub-tasks. `delete` takes descendants with it, so a cut that
/// captured only the rows the user had marked would remove a subtree and put a
/// single line on the clipboard — the children gone and nothing to paste back.
/// That was a real loss of work, not a missing convenience.
///
/// Read from the **document**, not from the visible rows, for the same reason:
/// a completed child hidden by `F4` is still deleted by the cut, so it has to
/// be on the clipboard too.
///
/// Only the selection *roots* are expanded — a row whose parent is also
/// selected arrives as part of its parent's subtree, and taking it twice would
/// paste it twice.
fn rows_to_copy<E: Editing + ?Sized>(e: &E, ws: &WorkspaceDoc) -> Vec<CopiedRow> {
    let roots = e.body().selection_roots(ws);
    // Depth is relative to the shallowest row copied, so a subtree lifted from
    // three levels in pastes where it lands rather than three levels deeper.
    let base = roots
        .iter()
        .filter_map(|&id| e.body().row(id))
        .map(|r| r.depth)
        .min()
        .unwrap_or(0);

    let mut out = Vec::new();
    for id in roots {
        let top = e
            .body()
            .row(id)
            .map(|r| r.depth)
            .unwrap_or(base)
            .saturating_sub(base);
        if let Some(row) = read_row(e, ws, id, top) {
            out.push(row);
        }
        // Depth-first and already in document order. ListBlock containers are
        // skipped in `read_row`; their items still appear with list specs.
        for (child, under) in ws.walk_under(id).unwrap_or_default() {
            if let Some(row) = read_row(e, ws, child, top + 1 + under as u16) {
                out.push(row);
            }
        }
    }
    out
}

/// How many ancestors of `kind` `id` has — for a list block or a callout,
/// the enclosing container's nest level + 1.
fn ancestor_count(ws: &WorkspaceDoc, id: NodeId, kind: Kind) -> u16 {
    let mut n = 0u16;
    let mut at = id;
    while let Ok(Parent::Node(p)) = ws.parent(at) {
        if ws.kind_of(p) == Some(kind) {
            n = n.saturating_add(1);
        }
        at = p;
    }
    n
}

/// Read one row out of the document, whole. `None` for invisible containers
/// (`ListBlock`, `Callout`) — those are rebuilt on paste from the
/// [`CopiedRow::list`] and [`CopiedRow::callout`] of the rows inside them.
fn read_row<E: Editing + ?Sized>(
    e: &E,
    ws: &WorkspaceDoc,
    id: NodeId,
    depth: u16,
) -> Option<CopiedRow> {
    let node = ws.read(id).ok()?;
    if matches!(node.kind, Kind::ListBlock | Kind::Callout) {
        return None;
    }
    // The box this row is in, with its look: the body keeps that on the
    // box, since the container draws no row of its own.
    let callout = e
        .body()
        .row(id)
        .and_then(|r| r.callout)
        .and_then(|callout| e.body().callout_box(callout))
        .map(|found| CopiedCalloutSpec {
            icon: found.icon.clone(),
            color: found.color.clone(),
            depth: ancestor_count(ws, id, Kind::Callout).saturating_sub(1),
        });
    let image = match &node.content {
        koru_core::Content::Blob {
            hash,
            mime,
            width,
            height,
            ..
        } => Some(CopiedImage {
            hash: hash.0.clone(),
            mime: mime.clone(),
            width: *width,
            height: *height,
            name: node.meta.name.clone(),
        }),
        _ => None,
    };
    // List style is on the enclosing ListBlock; the body caches it on the row.
    let list = e.body().row(id).and_then(|r| {
        r.list_style.map(|style| {
            let nest = ancestor_count(ws, id, Kind::ListBlock).saturating_sub(1);
            CopiedListSpec { style, depth: nest }
        })
    });
    let runs = ws.runs(id).unwrap_or_default();
    let (attrs, node_attrs) = split_attrs(e, &node, &runs);
    Some(CopiedRow {
        runs,
        attrs,
        node_attrs,
        kind: node.kind,
        done: node.meta.done,
        level_name: node.meta.level_name.clone(),
        callout,
        align: node.meta.align,
        heading: node.meta.heading,
        table_layout: node.meta.table_layout,
        language: node.meta.language.clone(),
        depth,
        image,
        list,
    })
}

/// The roles this build mints for an **inline pill**, and never as a block
/// attribute.
///
/// An unanchored attribute under one of these is an orphan — its anchor
/// character was deleted and the record outlived it — and resurrecting it as a
/// block attribute would put something on the row that nobody set.
///
/// The list is closed and code-owned. `Editor::attr_for` builds four of them
/// from a `PillKind`; `attachment` is `/attach`'s, minted by the document view
/// and by the OneNote and MCP importers; `ref` is the older spelling still in
/// documents written before either. **A role that inserts a pill anywhere in
/// the tree belongs here**, or a copy of a row whose pill lost its anchor
/// resurrects it as a block attribute. A workspace *could* define a custom key
/// spelling one of these and it would then not travel; that is the cheap
/// direction of the trade — see docs/features/trackable/DECISIONS.md Q35.
const PILL_ROLES: [&str; 6] = ["due", "assignee", "link", "document", "attachment", "ref"];

/// Split a node's attributes into the ones a pill in `runs` anchors and the
/// ones the *block* carries.
///
/// The two mechanisms are separate all the way through: an anchored attribute
/// is re-created with its pill by `insert_runs`, and a node-level one is set on
/// the pasted node by [`apply_node_attrs`]. Nothing may be in both lists, or a
/// pill's date would also become the block's due date.
///
/// **Three answers, not two.** A `koru.*` key this build defines travels
/// anywhere. A **custom** key — one the source workspace defined, whose
/// definition lives in a system document no editor can see — travels too, and
/// is dropped at the *paste* rather than here, because whether it means
/// anything depends on where it lands and this function does not know yet. An
/// unanchored attribute under a [`PILL_ROLES`] role is an orphaned pill and is
/// left behind outright: within one workspace it is the one case a same-workspace
/// check cannot tell from a custom key, so the role is what tells them apart.
fn split_attrs<E: Editing + ?Sized>(
    e: &E,
    node: &Node,
    runs: &[TextRun],
) -> (Vec<Attr>, Vec<CopiedAttr>) {
    let mut anchored = Vec::new();
    let mut carried = Vec::new();
    for attr in node.content.attrs() {
        if runs.iter().any(|run| run.pill == Some(attr.id)) {
            anchored.push(attr.clone());
            continue;
        }
        if system_definition(&attr.role).is_none() && PILL_ROLES.contains(&attr.role.as_str()) {
            continue;
        }
        let tag = if attr.role == KEY_TAG {
            // A `koru.tag` value is a `TagId` by definition; one that is not
            // names nothing anywhere and is not worth carrying.
            match attr.as_text().and_then(TagId::parse) {
                Some(id) => Some(id),
                None => continue,
            }
        } else {
            None
        };
        carried.push(CopiedAttr {
            attr: attr.clone(),
            tag_name: tag.and_then(|id| e.view().tags.resolve(id).map(|face| face.name.clone())),
        });
    }
    (anchored, carried)
}

/// The runs covering `range`, clipped to it.
///
/// A pill anchor is all-or-nothing: it is one indivisible character, and half
/// of one would be an anchor with no attribute behind it.
pub(crate) fn runs_in(runs: &[TextRun], range: &std::ops::Range<usize>) -> Vec<TextRun> {
    let mut out = Vec::new();
    for run in runs {
        let r = run.range();
        let lo = r.start.max(range.start);
        let hi = r.end.min(range.end);
        if lo >= hi {
            continue;
        }
        if run.is_pill() {
            if lo == r.start && hi == r.end {
                out.push(run.clone());
            }
            continue;
        }
        let mut clipped = run.clone();
        clipped.text = run.text[lo - r.start..hi - r.start].to_owned();
        out.push(clipped);
    }
    out
}

/// Put the selection on both clipboards. False when there was nothing to take.
pub fn copy<E: Editing + ?Sized>(e: &mut E, ws: &WorkspaceDoc) -> bool {
    let Some(payload) = capture(e, ws) else {
        return false;
    };
    let text = render(ws, &payload);
    // Arm CF_HTML + plain for the shell to write in one OpenClipboard session
    // (embedding image blobs first). Internal payload still goes on `put` so
    // Koru→Koru paste keeps structure without re-parsing HTML.
    if let Some(html) = cf_html(ws, &payload) {
        e.view_mut().wants_copy_html = Some((html, text.clone()));
        // Internal only — system text waits for the shell so HTML Format is not
        // missing when Notion/OneNote read the clipboard.
        put_internal(payload, text);
    } else {
        put(payload, text);
    }
    true
}

/// Remember a copy internally without touching the system clipboard.
fn put_internal(payload: Payload, text: String) {
    with_clipboard(|clipboard| {
        clipboard.payload = Some(payload);
        clipboard.wrote = text;
    });
}

fn capture<E: Editing + ?Sized>(e: &E, ws: &WorkspaceDoc) -> Option<Payload> {
    match what_to_copy(e)? {
        What::Rows => {
            let rows = rows_to_copy(e, ws);
            (!rows.is_empty()).then_some(Payload::Rows(rows, e.view().workspace))
        }
        What::Inline(i, range) => {
            let row = e.body().rows.get(i)?;
            let runs = runs_in(&ws.runs(row.id).unwrap_or_default(), &range);
            if runs.is_empty() {
                return None;
            }
            let attrs = ws
                .read(row.id)
                .ok()
                .map(|n| n.content.attrs().to_vec())
                .unwrap_or_default();
            Some(Payload::Inline(runs, attrs))
        }
    }
}

/// The plain-text half — what leaves for other applications.
fn render(ws: &WorkspaceDoc, payload: &Payload) -> String {
    let label = |attr: &Attr| pillinfo::info_for(ws, attr).label;
    match payload {
        Payload::Rows(rows, _) => {
            let mut out: Vec<String> = Vec::with_capacity(rows.len());
            // The callout boxes open around the row, outermost first. Every
            // line inside a box leaves quoted, the way a blockquote's lines
            // all do, and the first line of a box carries the box's icon.
            let mut boxes: Vec<CopiedCalloutSpec> = Vec::new();
            for r in rows {
                let (keep, opens) = callout_box_change(boxes.iter(), r.callout.as_ref());
                boxes.truncate(keep);
                let icon = match (&r.callout, opens) {
                    (Some(spec), true) => {
                        boxes.push(spec.clone());
                        format!("{} ", koru_core::callout::icon_of(spec.icon.as_deref()))
                    }
                    _ => String::new(),
                };
                // A divider is the one row whose text is empty on purpose: it
                // leaves as a thematic break rather than as a blank line.
                if r.kind == Kind::Divider {
                    out.push(format!("{}---", "\t".repeat(r.depth as usize)));
                    continue;
                }
                if r.image.is_some() {
                    // Empty line keeps interleave structure without inventing
                    // a label other apps would paste as prose.
                    out.push(String::new());
                    continue;
                }
                // A code block leaves as the fence the Markdown exporter
                // writes — the same helper, so the two agree byte for byte —
                // with its language on the opening line. Bare lines were a
                // paragraph per line in the next editor; a fence is a code
                // block there, and comes back as one through the Markdown
                // reader. Never indented: a fence has to start the line, and
                // a code block lives in a document body, which is flat.
                if r.kind == Kind::Code {
                    let fence = koru_core::markdown::fenced_code(
                        r.language.as_deref().unwrap_or(""),
                        &r.to_text(&label),
                    );
                    // Inside a callout box every line of the fence is quoted,
                    // as the box's other lines are.
                    let quote = "> ".repeat(boxes.len());
                    out.push(
                        fence
                            .lines()
                            .map(|line| format!("{quote}{line}"))
                            .collect::<Vec<_>>()
                            .join(
                                "
",
                            ),
                    );
                    continue;
                }
                // List items carry indent in `markdown_prefix` already; a
                // box's `>` sits before the marker, a nested box's before that.
                let tabs = if r.list.is_some() {
                    String::new()
                } else {
                    "\t".repeat(r.depth as usize)
                };
                out.push(format!(
                    "{tabs}{}{}{icon}{}",
                    "> ".repeat(boxes.len()),
                    r.markdown_prefix(),
                    r.to_text(&label)
                ));
            }
            out.join("\n")
        }
        Payload::Inline(runs, attrs) => {
            let one = CopiedRow {
                runs: runs.clone(),
                attrs: attrs.clone(),
                // A fragment of a line is text, not a block: node-level
                // attributes stay with the block they belong to.
                node_attrs: Vec::new(),
                kind: Kind::Task,
                done: false,
                level_name: None,
                callout: None,
                align: None,
                // A fragment of a heading is not a heading.
                heading: None,
                table_layout: None,
                language: None,
                depth: 0,
                image: None,
                list: None,
            };
            one.to_text(&label)
        }
        Payload::Text(text) => text.clone(),
    }
}

/// The CF_HTML flavour — what leaves for Word and Google Docs.
///
/// A complete `HTML Format` clipboard payload, header and byte-offsets
/// included, so a peer that reads CF_HTML sees paragraphs, headings, lists and
/// inline marks rather than the flat plain-text half. Pure: the shell does the
/// actual OS write. Returns `None` when there is nothing to export.
pub fn cf_html(ws: &WorkspaceDoc, payload: &Payload) -> Option<String> {
    let label = |attr: &Attr| pillinfo::info_for(ws, attr).label;
    let body = match payload {
        Payload::Rows(rows, _) => {
            if rows.is_empty() {
                return None;
            }
            html_rows(rows, &label)
        }
        Payload::Inline(runs, attrs) => {
            let one = CopiedRow {
                runs: runs.clone(),
                attrs: attrs.clone(),
                // A fragment of a line is text, not a block: node-level
                // attributes stay with the block they belong to.
                node_attrs: Vec::new(),
                kind: Kind::Task,
                done: false,
                level_name: None,
                callout: None,
                align: None,
                heading: None,
                table_layout: None,
                language: None,
                depth: 0,
                image: None,
                list: None,
            };
            format!("<p>{}</p>", html_runs(&one, &label))
        }
        Payload::Text(text) => format!("<p>{}</p>", html_escape(text)),
    };
    Some(wrap_cf_html(&body))
}

fn list_tag(style: ListStyle) -> &'static str {
    match style {
        ListStyle::Numbered | ListStyle::Letter | ListStyle::Roman => "ol",
        ListStyle::Bullet | ListStyle::Checkbox => "ul",
    }
}

/// What a row's [`CopiedRow::callout`] does to the boxes open around the row
/// before it, `open` (outermost first): how many of them stay open, and
/// whether the row opens a new one. A spec continues the box at its depth
/// when the look matches, opens a new box there when it does not, and no
/// spec closes every box. Shared by the text and HTML flavours and by
/// [`paste_rows`], so the three read a run of rows the same way.
fn callout_box_change<'a>(
    mut open: impl ExactSizeIterator<Item = &'a CopiedCalloutSpec>,
    spec: Option<&CopiedCalloutSpec>,
) -> (usize, bool) {
    let Some(spec) = spec else {
        return (0, false);
    };
    let depth = (spec.depth as usize).min(open.len());
    if open.len() > depth && open.nth(depth) == Some(spec) {
        (depth + 1, false)
    } else {
        (depth, true)
    }
}

/// Emit rows as HTML, nesting `<ul>`/`<ol>` from [`CopiedRow::list`] and
/// `<aside>` from [`CopiedRow::callout`].
fn html_rows(rows: &[CopiedRow], label: &dyn Fn(&Attr) -> String) -> String {
    let mut out = String::new();
    // Each open list also has an open `<li>`. A child list belongs *inside*
    // that item; closing the item first produces invalid sibling `<ul>` tags,
    // which clipboard consumers repair by flattening the hierarchy.
    let mut open: Vec<ListStyle> = Vec::new();
    let close_to = |out: &mut String, open: &mut Vec<ListStyle>, levels: usize| {
        while open.len() > levels {
            if let Some(style) = open.pop() {
                out.push_str(&format!("</li></{}>", list_tag(style)));
            }
        }
    };
    // The callout boxes open around the row, outermost first: each is an
    // `<aside>` — what other apps' exports write for a callout — holding
    // every row inside it, lists included, with the box's icon leading its
    // first row.
    let mut boxes: Vec<CopiedCalloutSpec> = Vec::new();
    for row in rows {
        let (keep, opens) = callout_box_change(boxes.iter(), row.callout.as_ref());
        if keep < boxes.len() || opens {
            // A list ends with the box it is in.
            close_to(&mut out, &mut open, 0);
        }
        while boxes.len() > keep {
            boxes.pop();
            out.push_str("</aside>");
        }
        let lead = match (&row.callout, opens) {
            (Some(spec), true) => {
                boxes.push(spec.clone());
                out.push_str("<aside>");
                format!(
                    "{} ",
                    html_escape(koru_core::callout::icon_of(spec.icon.as_deref()))
                )
            }
            _ => String::new(),
        };
        if let Some(img) = &row.image {
            close_to(&mut out, &mut open, 0);
            out.push_str(&html_image_placeholder(img));
            continue;
        }
        if let Some(spec) = row.list {
            // Ragged payloads cannot skip a level: paste uses the same clamp.
            let depth = (spec.depth as usize).min(open.len());
            close_to(&mut out, &mut open, depth + 1);

            if open.len() > depth && open[depth] != spec.style {
                close_to(&mut out, &mut open, depth);
            }

            if open.len() == depth {
                out.push_str(&format!("<{}><li>", list_tag(spec.style)));
                open.push(spec.style);
            } else {
                // Same list, next sibling item.
                out.push_str("</li><li>");
            }
            out.push_str(&lead);
            out.push_str(&html_runs(row, label));
            continue;
        }
        close_to(&mut out, &mut open, 0);
        if row.kind == Kind::Code {
            out.push_str(&html_code(row, label));
            continue;
        }
        match row.heading.map(Heading::level) {
            Some(l @ 1..=6) => {
                out.push_str(&format!("<h{l}>"));
                out.push_str(&lead);
                out.push_str(&html_runs(row, label));
                out.push_str(&format!("</h{l}>"));
            }
            _ => {
                if row.kind == Kind::Table
                    && let Some(html) = koru_blocks::table::to_html(&row.to_text(label))
                {
                    out.push_str(&html);
                    continue;
                }
                if row.kind == Kind::Quote {
                    out.push_str("<blockquote><p>");
                    out.push_str(&lead);
                    out.push_str(&html_runs(row, label));
                    out.push_str("</p></blockquote>");
                } else {
                    out.push_str("<p>");
                    out.push_str(&lead);
                    out.push_str(&html_runs(row, label));
                    out.push_str("</p>");
                }
            }
        }
    }
    close_to(&mut out, &mut open, 0);
    for _ in boxes.drain(..) {
        out.push_str("</aside>");
    }
    out
}

/// Wrap an HTML fragment body in the CF_HTML header with correct offsets.
fn wrap_cf_html(body: &str) -> String {
    // Fixed-width 10-digit offset fields keep the header length constant, so
    // the offsets can be computed in terms of it.
    let header = "Version:0.9\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    let pre = "<html>\r\n<body>\r\n<!--StartFragment-->";
    let post = "<!--EndFragment-->\r\n</body>\r\n</html>";
    let start_html = header.len();
    let start_fragment = start_html + pre.len();
    let end_fragment = start_fragment + body.len();
    let end_html = end_fragment + post.len();
    let mut out = String::with_capacity(end_html);
    out.push_str(&format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n"
    ));
    out.push_str(pre);
    out.push_str(body);
    out.push_str(post);
    out
}

/// Placeholder the shell rewrites to a `data:` URI once it has the blob bytes.
/// Wrapped in `<p>` so the CF_HTML fragment always ends on a closed block when
/// the selection ends on a picture — bare trailing `<img>` makes OneNote reject
/// the whole HTML Format and fall back to plain text.
fn html_image_placeholder(img: &CopiedImage) -> String {
    let mut tag = format!(
        r#"<p><img data-koru-hash="{}" data-mime="{}""#,
        html_escape(&img.hash),
        html_escape(&img.mime)
    );
    if let Some(w) = img.width {
        tag.push_str(&format!(r#" width="{w}""#));
    }
    if let Some(h) = img.height {
        tag.push_str(&format!(r#" height="{h}""#));
    }
    if let Some(name) = &img.name {
        tag.push_str(&format!(r#" alt="{}""#, html_escape(name)));
    } else {
        tag.push_str(r#" alt="""#);
    }
    tag.push_str("></p>");
    tag
}

/// A row's runs as HTML, with bold/italic/underline/strike wrapped and pills
/// rendered as their label text.
fn html_runs(row: &CopiedRow, label: &dyn Fn(&Attr) -> String) -> String {
    let mut out = String::new();
    for run in &row.runs {
        let text = match run
            .pill
            .and_then(|id| row.attrs.iter().find(|a| a.id == id))
        {
            Some(attr) => label(attr),
            None if run.is_pill() => String::new(),
            None => run.text.clone(),
        };
        if text.is_empty() {
            continue;
        }
        let escaped = html_escape(&text);
        let mut piece = escaped;
        if run.strike {
            piece = format!("<s>{piece}</s>");
        }
        if run.underline {
            piece = format!("<u>{piece}</u>");
        }
        if run.italic {
            piece = format!("<i>{piece}</i>");
        }
        if run.bold {
            piece = format!("<b>{piece}</b>");
        }
        out.push_str(&piece);
    }
    out
}

/// A code block as `<pre><code class="language-…">`, every newline kept.
///
/// `<pre>` is the one block whose whitespace a reader preserves, and the
/// `language-` class is the spelling highlighters and Markdown converters read
/// the grammar from, so the block arrives in Word, a browser or a wiki as code
/// in that language rather than as collapsed prose in `<p>`. A block left on
/// auto-detect has no class at all rather than an empty one.
fn html_code(row: &CopiedRow, label: &dyn Fn(&Attr) -> String) -> String {
    let body = html_escape(&row.to_text(label));
    match row.language.as_deref().filter(|l| !l.is_empty()) {
        Some(language) => format!(
            r#"<pre><code class="language-{}">{body}</code></pre>"#,
            html_escape(language)
        ),
        None => format!("<pre><code>{body}</code></pre>"),
    }
}

/// The four characters HTML reads as syntax, in text and in a quoted
/// attribute alike. `"` is here for the attributes — a language token or an
/// image name holding a quote would otherwise end the attribute early — and is
/// harmless in text, where `&quot;` reads back as the quote it was.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------- cutting

/// Copy, then take away what was copied.
pub fn cut<E: Editing + ?Sized>(e: &mut E, ws: &mut WorkspaceDoc) -> bool {
    let Some(what) = what_to_copy(e) else {
        return false;
    };
    if !copy(e, ws) {
        return false;
    }
    match what {
        // `delete_rows` deletes the same selection roots that `rows_to_copy`
        // just captured, and a cut takes the children with the parent (rather
        // than outdenting them) — which is why the copy has to take them too.
        // The two must agree about what "this row" means or a cut loses
        // whatever the copy missed.
        What::Rows => e.delete_rows(ws, false),
        What::Inline(i, range) => {
            let Some(id) = e.body().rows.get(i).map(|r| r.id) else {
                return false;
            };
            ws.checkpoint();
            let _ = ws.delete_text(id, range.clone());
            e.body_mut().refresh(ws, id);
            e.place_caret(range.start);
            e.settle();
            true
        }
    }
}

// --------------------------------------------------------------- pasting

/// Pour runs into a node at `at`, and report where they end.
///
/// The body moved into the model as `WorkspaceDoc::insert_runs` when moving a
/// selection to another document needed it against a document no editor holds;
/// this delegation keeps every paste call site unchanged.
fn insert_runs(
    ws: &mut WorkspaceDoc,
    id: NodeId,
    at: usize,
    runs: &[TextRun],
    attrs: &[Attr],
) -> usize {
    ws.insert_runs(id, at, runs, attrs)
}

/// One run of unformatted text — which is all text from another application is.
fn plain_run(text: String) -> TextRun {
    TextRun {
        start: Utf8Pos(0),
        end: Utf8Pos(text.len()),
        text,
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        code: false,
        style: TextStyle::default(),
        pill: None,
    }
}

/// Everything about a row that is not its text.
///
/// `kind` is the *destination's*, not the copied row's — which still decides
/// the heading and the table layout below.
///
/// **The tick is content and always travels** ([ADR-285], amended). It used to
/// be written only when `kind.is_task_like()` — that is `Kind::Task` alone — so
/// a checked *checklist item* lost its tick on the way across, which is the one
/// place a paste silently changed what it was given. `NodeMeta::done` is the
/// only authority on how the checkbox draws and nothing derives it from the
/// lifecycle, so carrying it is the whole of the fix; a tick on a row that
/// draws no box is invisible and comes back the moment the row is one again,
/// which is exactly what preserving state means.
///
/// [ADR-285]: ../../../docs/decisions/ADR-276-300.md#adr-285-taskmodel--a-task-is-a-marked-node-and-the-marker-or-a-due-date-is-the-whole-derivation
fn apply_meta(ws: &mut WorkspaceDoc, id: NodeId, row: &CopiedRow, kind: Kind) {
    if row.done {
        // Verbatim, and inside the paste's open group: `set_checkbox` is a
        // *gesture*, guarded and grouped as its own undo step, and nobody
        // ticked anything here — see [`apply_node_attrs`] for why the whole
        // carried task state lands this way.
        let _ = faithful_import::restore_done(ws, id, true);
    }
    if let Some(name) = &row.level_name {
        let _ = ws.set_level_name(id, Some(name));
    }
    if row.align.is_some() {
        let _ = ws.set_align(id, row.align);
    }
    // Only where headings mean something. A task is structured by the tree, and
    // a 30px row whose checkbox is centred on a body line would look broken —
    // so the level is dropped at the paste rather than handled in the drawing.
    if row.heading.is_some() && !kind.is_task_like() {
        let _ = ws.set_heading(id, row.heading);
    }
    if kind == Kind::Table {
        let _ = ws.set_table_layout(id, row.table_layout.unwrap_or_default());
    }
    // A code block's grammar travels with it, auto-detect (`None`) included:
    // the pasted row *is* the copied block, so its language replaces whatever
    // a reused blank code row had. Only when the row was code, though — prose
    // absorbed by a blank code row leaves that row's own language alone.
    if kind == Kind::Code && row.kind == Kind::Code {
        let _ = ws.set_language(id, row.language.as_deref());
    }
}

/// Where each row's tags land in the document being pasted into, row by row and
/// in the order each row carries them.
///
/// Resolved from the **view's** tag table — the destination workspace's, pushed
/// down by the host — and resolved *before* the paste writes anything, because
/// `e` is borrowed mutably for the rest of it. The table is read once for the
/// whole paste rather than once per row: `sorted` clones it, and a hundred
/// tagged rows against a hundred tags would otherwise be ten thousand clones.
fn tag_landings<E: Editing + ?Sized>(e: &E, rows: &[CopiedRow]) -> Vec<Vec<TagLanding>> {
    let tagged = |row: &CopiedRow| row.node_attrs.iter().any(|c| c.attr.role == KEY_TAG);
    if !rows.iter().any(tagged) {
        return vec![Vec::new(); rows.len()];
    }
    let table = e.view().tags.sorted();
    rows.iter()
        .map(|row| {
            row.node_attrs
                .iter()
                .filter(|carried| carried.attr.role == KEY_TAG)
                .filter_map(|carried| {
                    let id = carried.attr.as_text().and_then(TagId::parse)?;
                    Some(resolve_copied_tag(
                        id,
                        carried.tag_name.as_deref(),
                        table.iter().map(|(id, face)| (*id, face.name.as_str())),
                    ))
                })
                .collect()
        })
        .collect()
}

/// Re-apply the attributes the *block* carried — its due date, its tags, its
/// color, its priority. The node-level half of [`apply_meta`].
///
/// Called inside the paste's undo group, and through `*_on_all` rather than the
/// single-node setters, because those checkpoint: one Ctrl+Z has to take back
/// the paste and everything it brought with it.
///
/// `tags` is `row`'s tags already resolved against the destination, in the same
/// order — both loops filter on the role alone, so they stay in step. A tag
/// with nowhere to land yet is appended to `to_create` for the host, which owns
/// the system document the tag table lives in.
///
/// Three things are dropped rather than applied. A **code block** holds plain
/// text, the same reason a paste into one drops marks and pills. A value
/// the destination refuses — a personal key, a token outside a definition's
/// options — simply does not land: `set_attribute_on_all` validates, and half a
/// paste beats a refused one. And a **custom** key when `same_workspace` is
/// false: its definition lives in the source workspace's system document, so
/// on the far side of a workspace boundary the value has no type, no label and
/// no meaning to give it.
///
/// A custom key that *does* travel goes in through `WorkspaceDoc::set_attr`
/// rather than `set_attribute_on_all`, which knows only the `koru.*` registry
/// and answers `UnknownAttribute` for anything else. That is the same raw door
/// `place` uses for a duplicate, which is what makes the two agree again — and
/// like `place` it writes into the caller's open undo group, so one `Ctrl+Z`
/// still takes the paste and its attributes back together. The `AttrId` is
/// fresh (the source's names a record in the source's document); a value the
/// node already carries under that key is skipped, so a re-paste onto the same
/// row adds nothing.
fn apply_node_attrs(
    ws: &mut WorkspaceDoc,
    id: NodeId,
    row: &CopiedRow,
    kind: Kind,
    tags: &[TagLanding],
    same_workspace: bool,
    to_create: &mut Vec<(NodeId, String)>,
) {
    if kind == Kind::Code {
        return;
    }
    let mut landing = tags.iter();
    // The task-owned fields, gathered as they go past and written together
    // below. See [`restore_task_state`].
    let mut task = TaskFields::default();
    for carried in &row.node_attrs {
        if system_definition(&carried.attr.role).is_none() {
            // A custom key: only inside the workspace that defines it.
            if !same_workspace {
                continue;
            }
            // **Through the validated door**, not raw `set_attr` (ADR-273).
            // The definition itself lives in the source workspace's system
            // document, which no editor can see — so one is rebuilt from the
            // value's own shape, which is enough for `validate` to check the
            // write and for the command-only guard to refuse anything wearing
            // a task key's name. A value shape no `ValueType` covers is
            // dropped, the same way a value the destination refuses is: it
            // could not have been *defined* on the far side either.
            let Some(value_type) = custom_value_type(&carried.attr.value) else {
                continue;
            };
            let label = carried
                .attr
                .label
                .clone()
                .unwrap_or_else(|| carried.attr.role.clone());
            let def = AttributeDefinition::custom(carried.attr.role.clone(), label, value_type);
            let _ = ws.set_attribute_defined_on_all(&[id], &def, carried.attr.value.clone());
            continue;
        }
        // The two command-only keys and the due are task state, and they are
        // restored together after this loop rather than one write at a time —
        // the generic doors refuse the first two by design.
        if carried.attr.role == KEY_TASK {
            if let AttrValue::Text(token) = &carried.attr.value {
                task.marker = TaskReason::parse(token);
            }
            continue;
        }
        if carried.attr.role == KEY_TASK_STATE {
            if let AttrValue::Text(token) = &carried.attr.value {
                task.lifecycle = Lifecycle::parse(token).unwrap_or_default();
            }
            continue;
        }
        if carried.attr.role == KEY_DUE {
            task.due = carried.attr.value.temporal();
            continue;
        }
        if carried.attr.role == KEY_TAG {
            // The same predicate `tag_landings` filtered on, spelled the same
            // way: the two walks have to name the same attributes in the same
            // order, or a row's second tag would be applied under the first
            // one's answer.
            if carried.attr.as_text().and_then(TagId::parse).is_none() {
                continue;
            }
            match landing.next() {
                Some(TagLanding::Existing(tag)) => {
                    let _ = ws.add_tag_to_all(&[id], *tag);
                }
                Some(TagLanding::Create(name)) => to_create.push((id, name.clone())),
                None => {}
            }
            continue;
        }
        let _ = ws.set_attribute_on_all(&[id], &carried.attr.role, carried.attr.value.clone());
    }
    restore_task_state(ws, id, kind, &task);
}

/// The `ValueType` a custom definition rebuilt from a carried value would
/// declare. `None` for the three shapes no `ValueType` covers — a reference, a
/// store document, a URL — which is why they are dropped rather than guessed
/// at: a definition that called one of them `Text` would validate a write the
/// source workspace's own definition never would.
fn custom_value_type(value: &AttrValue) -> Option<ValueType> {
    Some(match value {
        AttrValue::Text(_) => ValueType::Text,
        AttrValue::Number(_) => ValueType::Number,
        AttrValue::Bool(_) => ValueType::Boolean,
        AttrValue::Date(_) => ValueType::Date,
        AttrValue::ZonedDateTime(_) => ValueType::ZonedDateTime,
        AttrValue::FloatingDateTime(_) => ValueType::FloatingDateTime,
        AttrValue::Ref(_) | AttrValue::Document(_) | AttrValue::Url(_) => return None,
    })
}

/// Put the copied row's task state back on the node it landed as — the marker,
/// the canonical lifecycle and the due, **verbatim**.
///
/// SPEC §6: a pasted task keeps its due, lifecycle, marker and *subtree* state
/// and is an independent task rather than an alias, which is what `plant` and
/// `duplicate` already gave and what the clipboard path did not. Three things
/// decide the shape of this, and each of them rules something out:
///
/// - **The whole paste is one undo step**, and every canonical command
///   `checkpoint()`s before it opens its own `atomic` — so running one here
///   would close the paste's group mid-way and turn a paste of a list into a
///   row of separate Ctrl+Z steps.
/// - **`complete_task` cascades downward.** A completed parent whose copied
///   child was open would come back with the child completed, which is the
///   subtree state SPEC §6 says survives being changed by the act of pasting.
/// - **The marker is not re-derived.** `TaskReason` records how the task came
///   to exist, and pasting is not a new reason.
///
/// So the *fields* are a restore, through the one named door for restoring
/// them, while the *gesture* stays a user command: a paste still classifies
/// and can reopen the task it lands in (`tasks::reclassify`, wired at the
/// paste commit), which is what makes it not an import.
///
/// A kind that cannot carry task state gets nothing — a task pasted into a
/// quote is prose there, and SPEC §4's exclusion is enforced rather than
/// hidden.
fn restore_task_state(ws: &mut WorkspaceDoc, id: NodeId, kind: Kind, task: &TaskFields) {
    let carries_state =
        task.marker.is_some() || task.due.is_some() || task.lifecycle != Lifecycle::default();
    if !carries_state || !task_capable(kind) {
        return;
    }
    let _ = faithful_import::restore_task(ws, id, task);
}

/// Paste whatever the clipboard holds. False when it holds nothing usable.
pub fn paste<E: Editing + ?Sized>(e: &mut E, ws: &mut WorkspaceDoc) -> bool {
    let taken = take();
    clip_dbg(format!("editor paste: taken={}", taken_label(&taken)));
    match taken {
        Taken::Rows(rows, source) => paste_rows(e, ws, &rows, source),
        Taken::Inline(runs, attrs) => paste_inline(e, ws, &runs, &attrs),
        Taken::Text(text) => paste_plain_text(e, ws, &text),
        // Text from outside may carry images the editor cannot see (a Word
        // document's inline pictures, embedded in the RTF). Only a document
        // view can show them, and only the shell can read the bytes; here we
        // defer — carrying the text in the request — and let `service_images`
        // interleave any RTF pictures with it. The shell uses the carried text
        // for the no-picture case, so it never re-reads the clipboard (which
        // raced and dropped plain text last time). The task outline (images
        // disabled) and a document with no focused row paste the text instantly.
        Taken::ForeignText(text) => {
            if let Some((anchor, replacing)) = offer_rich_paste(e, ws) {
                e.view_mut().wants_paste_rich = Some((anchor, replacing, text));
                clip_dbg("foreign_text: deferred to shell (wants_paste_rich armed)");
                return true;
            }
            clip_dbg("foreign_text: pasting instantly (no images / no focus)");
            paste_plain_text(e, ws, &text)
        }
        // Nothing readable as text — but the system clipboard may still hold a
        // picture or copied files. Only a view that shows images may take that
        // offer, and only the shell can read the bytes; here we plant the
        // request and let `service_images` follow up.
        Taken::Nothing => {
            let has_non_text = system_has_non_text();
            let images_enabled = e.view().images_enabled;
            let focused = e.body().focused_id().is_some();
            let armed = offer_image_paste(e, ws, has_non_text);
            clip_dbg(format!(
                "nothing: system_has_non_text={has_non_text} images_enabled={images_enabled} focused={focused} armed={armed}"
            ));
            armed
        }
    }
}

/// Paste only the clipboard payload's visible text.
///
/// This deliberately starts from [`take`], the same authority normal paste
/// uses, so a stale internal payload can never beat content copied elsewhere.
/// Rows and inline runs are flattened from their structured form rather than
/// from the system clipboard's export spelling: an H2 copied by Koru exports
/// as `## Heading` for other applications, but *Paste as plain text* means
/// `Heading` without either the heading metadata or Markdown punctuation.
pub fn paste_unformatted<E: Editing + ?Sized>(e: &mut E, ws: &mut WorkspaceDoc) -> bool {
    let Some(text) = unformatted_text(ws) else {
        return false;
    };
    paste_plain_text(e, ws, &text)
}

/// The clipboard's visible text, flattened the way
/// [`paste_unformatted`] flattens it. `None` when it holds nothing readable.
fn unformatted_text(ws: &WorkspaceDoc) -> Option<String> {
    let label = |attr: &Attr| pillinfo::info_for(ws, attr).label;
    let text = match take() {
        Taken::Rows(rows, _) => rows
            .iter()
            .map(|row| {
                if row.kind == Kind::Divider {
                    return format!("{}---", "\t".repeat(row.depth as usize));
                }
                if row.image.is_some() {
                    return String::new();
                }
                format!("{}{}", "\t".repeat(row.depth as usize), row.to_text(&label))
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Taken::Inline(runs, attrs) => CopiedRow {
            runs,
            attrs,
            node_attrs: Vec::new(),
            kind: Kind::Task,
            done: false,
            level_name: None,
            callout: None,
            align: None,
            heading: None,
            table_layout: None,
            language: None,
            depth: 0,
            image: None,
            list: None,
        }
        .to_text(&label),
        Taken::Text(text) | Taken::ForeignText(text) => text,
        Taken::Nothing => return None,
    };
    Some(text)
}

/// Paste under the composer policy: the clipboard's visible text, **at the
/// caret**, one line per plain paragraph (`crate::policy`).
///
/// Not [`paste_plain_text`], which lands several lines as rows *after* the
/// focused one — the right thing for a page and the wrong thing for a message,
/// where pasting three lines into the middle of a sentence has to read exactly
/// as it would in any text box.
pub fn paste_as_lines<E: Editing + ?Sized>(e: &mut E, ws: &mut WorkspaceDoc) -> bool {
    let Some(text) = unformatted_text(ws) else {
        return false;
    };
    insert_lines(e, ws, &text)
}

/// Insert `text` at the caret, replacing any selection, with each line break a
/// split into a new paragraph of the focused row's kind — **one undo step**.
///
/// The model keeps `\n` out of a paragraph (`Kind::allows_newlines`), so a
/// line break *is* a new paragraph; a host that wants the text back joins the
/// paragraphs with one newline. `\r\n` and a lone `\r` count as one break.
/// Answers whether anything was inserted.
pub fn insert_lines<E: Editing + ?Sized>(e: &mut E, ws: &mut WorkspaceDoc, text: &str) -> bool {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
        return false;
    }
    let Some((mut node, range)) = e.target() else {
        return false;
    };
    e.body_mut().clear_selection();
    ws.checkpoint();
    ws.begin_group();
    if !range.is_empty() {
        let _ = ws.delete_text(node, range.clone());
    }
    let mut at = range.start;
    for (i, line) in normalized.split('\n').enumerate() {
        if i > 0 {
            match ws.split(node, at) {
                Ok(tail) => {
                    node = tail;
                    at = 0;
                }
                Err(_) => break,
            }
        }
        if !line.is_empty() && ws.insert_text(node, at, line).is_ok() {
            at += line.len();
        }
    }
    ws.checkpoint();
    e.body_mut().rebuild(ws);
    e.body_mut().focus_on(node);
    e.place_caret(at);
    e.view_mut().desired_x = None;
    e.settle();
    true
}

/// A label for the [`Taken`] variant, for the diagnostic log.
fn taken_label(t: &Taken) -> &'static str {
    match t {
        Taken::Rows(..) => "rows",
        Taken::Inline(..) => "inline",
        Taken::Text(_) => "text",
        Taken::ForeignText(_) => "foreign_text",
        Taken::Nothing => "nothing",
    }
}

/// Arm `wants_paste_image` when a paste found nothing usable but the system
/// clipboard looks like it holds an image or copied files. `has_non_text` is
/// passed in rather than read here so the decision is unit-testable without the
/// OS clipboard — the read itself is not the interesting part.
///
/// Returns `true` (the paste is "handled", pending the shell) only when the
/// view shows images and there is a focused row to land below. Without a focus
/// there is nowhere to put the block, the same rule text paste follows.
fn offer_image_paste<E: Editing + ?Sized>(
    e: &mut E,
    ws: &WorkspaceDoc,
    has_non_text: bool,
) -> bool {
    // **`images_offline` too, and this is the route that matters most.**
    // Pasting is how a picture usually arrives; the slash command is the rarer
    // one. The bytes go to the store through `put_blob`, so with the store
    // unreachable there is nowhere to put them — and a paste that appeared to
    // work and left a broken picture is worse than one that does nothing.
    if !has_non_text || !e.view().images_enabled || e.view().images_offline {
        return false;
    }
    let Some(target) = deferred_block_paste_target(e, ws) else {
        return false;
    };
    e.view_mut().wants_paste_image = Some(target);
    true
}

/// Whether a paste of **foreign text** in an images-enabled view should defer to
/// the shell so it can interleave any pictures the clipboard carries alongside
/// the text (a Word copy). Returns the focused row the rich paste should land
/// below, or `None` to paste the text plainly — either because the view shows no
/// images (the task outline, which never rich-pastes) or because there is no
/// focused row to anchor on. Factored out so the decision is unit-testable
/// without the OS clipboard.
fn offer_rich_paste<E: Editing + ?Sized>(
    e: &mut E,
    ws: &WorkspaceDoc,
) -> Option<(NodeId, Vec<NodeId>)> {
    // Offline, the pictures in a rich paste cannot be stored — so the text is
    // pasted plainly rather than deferred to a shell that would drop them.
    if !e.view().images_enabled || e.view().images_offline {
        return None;
    }
    // A code block — a diagram's text included — takes text and nothing
    // else: a picture has no place in it, so there is nothing for the shell
    // to interleave. Pasted plainly, at the caret, the newlines read the way
    // `paste_plain_text` reads them; deferred, the text came back by the
    // rows path with its Windows line endings intact.
    if !e.body().has_row_selection()
        && e.body()
            .focused()
            .is_some_and(|row| row.kind.allows_newlines())
    {
        return None;
    }
    deferred_block_paste_target(e, ws)
}

/// Snapshot the structural target before a deferred clipboard read. A row
/// selection lands at its first visible row regardless of which end has focus,
/// matching [`paste_rows`]; only selection roots travel because deleting a
/// parent already deletes its selected descendants.
fn deferred_block_paste_target<E: Editing + ?Sized>(
    e: &E,
    ws: &WorkspaceDoc,
) -> Option<(NodeId, Vec<NodeId>)> {
    if e.body().has_row_selection() {
        let anchor = e.body().rows.iter().find(|row| row.selected)?.id;
        return Some((anchor, e.body().selection_roots(ws)));
    }
    e.body().focused_id().map(|anchor| (anchor, Vec::new()))
}

/// Paste text that came from outside this document.
///
/// One line goes in at the caret; several become rows, taking their nesting
/// from whatever indentation they arrived with. Public because the clipboard is
/// not the only way text arrives from elsewhere — a drop or an import is the
/// same question.
pub fn paste_plain_text<E: Editing + ?Sized>(e: &mut E, ws: &mut WorkspaceDoc, text: &str) -> bool {
    // A title is a single-line name: strip tabs/newlines and trim before insert.
    if e.body().focus_is_title() {
        let clean = crate::body::Body::sanitize_title(text);
        if clean.is_empty() {
            return false;
        }
        return paste_inline(e, ws, &[plain_run(clean)], &[]);
    }
    // A code block holds multi-line text in one node, so pasting code (which
    // often spans lines) goes in as one inline insert — newlines and all —
    // rather than becoming a paragraph per line.
    if e.body().focused().is_some_and(|r| r.kind.allows_newlines()) && !text.is_empty() {
        // Read the newlines the way `split_lines` reads them for prose. A
        // Windows clipboard brings `\r\n`, and a `\r` kept in the block is
        // shaped as a line break of its own — every line of a pasted
        // diagram double-spaced, with nothing on the blank lines to delete.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        return paste_inline(e, ws, &[plain_run(text)], &[]);
    }
    let lines = split_lines(text);
    if lines.len() == 1 {
        if lines[0].is_empty() {
            return false;
        }
        return paste_inline(e, ws, &[plain_run(lines[0].clone())], &[]);
    }
    // Prose does not nest ([ADR-23](../../../docs/decisions/README.md)), so a leading
    // tab in a document is *text* — what the user copied — rather than
    // structure to be read into a depth that a flat body then throws away.
    // Reading it as structure there deleted the character outright.
    let indented: Vec<(u16, &str)> = if e.body().flat {
        lines.iter().map(|line| (0, line.as_str())).collect()
    } else {
        indents_of(&lines)
    };
    let rows: Vec<CopiedRow> = indented
        .into_iter()
        .map(|(depth, body)| CopiedRow {
            runs: vec![plain_run(body.to_owned())],
            attrs: Vec::new(),
            node_attrs: Vec::new(),
            // Adapted to the destination by `paste_rows`. Text from outside has
            // no kind of its own to keep.
            kind: Kind::Task,
            done: false,
            level_name: None,
            callout: None,
            align: None,
            heading: None,
            table_layout: None,
            language: None,
            depth,
            image: None,
            list: None,
        })
        .collect();
    // Text from outside carries no block attributes, so it left no
    // workspace behind either.
    paste_rows(e, ws, &rows, None)
}

/// Turn every scheme URL in `id`'s text into a link pill, right-to-left so
/// earlier byte offsets stay valid as later URLs shrink. Part of the paste
/// group, so one Ctrl+Z removes the paste and its links together. A no-op when
/// there is nothing to link — a pill's anchor is not a URL, so already-linked
/// content is left alone.
pub fn linkify_urls_in_node(ws: &mut WorkspaceDoc, id: NodeId) -> bool {
    let Some(text) = ws
        .read(id)
        .ok()
        .and_then(|n| n.content.as_text().map(str::to_owned))
    else {
        return false;
    };
    let urls = trigger::urls_in(&text);
    if urls.is_empty() {
        return false;
    }
    for (range, url) in urls.into_iter().rev() {
        let attr = Attr::labelled(
            "link",
            AttrValue::Url(link_prompt::normalize_url(&url)),
            &url,
        );
        let _ = ws.delete_text(id, range.clone());
        let _ = ws.insert_pill(id, range.start, &attr);
    }
    true
}

fn paste_inline<E: Editing + ?Sized>(
    e: &mut E,
    ws: &mut WorkspaceDoc,
    runs: &[TextRun],
    attrs: &[Attr],
) -> bool {
    // Rows are marked: the paste replaces them, whatever shape the clipboard
    // happens to hold. Editing one row and leaving the others highlighted and
    // untouched would make Ctrl+V mean two things at once, so the selection
    // decides and `paste_rows` owns it.
    if e.body().has_row_selection() {
        let one = CopiedRow {
            runs: runs.to_vec(),
            attrs: attrs.to_vec(),
            node_attrs: Vec::new(),
            // Adapted to the destination by `paste_rows`.
            kind: Kind::Task,
            done: false,
            level_name: None,
            callout: None,
            align: None,
            heading: None,
            table_layout: None,
            language: None,
            depth: 0,
            image: None,
            list: None,
        };
        return paste_rows(e, ws, &[one], None);
    }

    let Some((id, range)) = e.target() else {
        return false;
    };
    // A code block holds plain text: pasting formatted runs into it drops the
    // marks, pills and links rather than storing them in a block that renders
    // everything as code.
    let is_code = ws.read(id).is_ok_and(|n| n.kind == Kind::Code);
    ws.checkpoint();
    ws.begin_group();
    if !range.is_empty() {
        let _ = ws.delete_text(id, range.clone());
    }
    let end = if is_code {
        let mut plain = String::new();
        for run in runs {
            if run.pill.is_none() {
                plain.push_str(&run.text);
            }
        }
        insert_runs(ws, id, range.start, &[plain_run(plain)], &[])
    } else {
        insert_runs(ws, id, range.start, runs, attrs)
    };
    // A pasted URL becomes a link, just as a typed one does. Folded into the
    // paste group so the whole paste — text and links — is one undo step. A code
    // block is the exception: source text is not an address to follow.
    let linked = if is_code {
        false
    } else {
        linkify_urls_in_node(ws, id)
    };
    ws.checkpoint();

    // A paste is one local gesture that changed this row's own text, however
    // many runs it wrote. `paste_rows` is deliberately not noted: it makes
    // nodes rather than editing one, and what a new node owes is a creation
    // event the commands already answer for (ADR-273).
    e.body_mut().note_text_edit(ws, id);
    e.body_mut().refresh(ws, id);
    // Offsets shifted if URLs shrank to anchors; land at the end of the node.
    let caret = if linked {
        ws.read(id)
            .ok()
            .and_then(|n| n.content.as_text().map(str::len))
            .unwrap_or(end)
    } else {
        end
    };
    e.place_caret(caret);
    e.view_mut().desired_x = None;
    e.settle();
    true
}

/// Rows land where the selection was, or after the focused row when there is
/// none, keeping the shape they were copied with.
///
/// **Selected rows are replaced.** A selection that crosses rows is a set of
/// whole nodes ([ADR-27](../../../docs/decisions/README.md)), so the row-level answer
/// to "paste over this" is to take those nodes out and put these in — the same
/// thing `paste_inline` does to a range of characters. Only the *tops* of the
/// selection are deleted, since `delete` takes descendants with it.
///
/// A blank focused row is reused rather than left above the paste: pressing
/// Enter and then pasting should not cost an empty line. Reuse is off when
/// replacing, because the row it would reuse is one of the ones going.
///
/// Every row takes the **kind of the row it lands beside**, which is the rule
/// `Body::append_row` already uses for Enter. A task copied into a document is
/// prose there; a paragraph copied into the outline is a task.
///
/// In a **flat** body, outline nesting lands at depth zero (ADR-23). **Lists**
/// are the exception: they rebuild as nested `ListBlock` containers from
/// [`CopiedRow::list`], the same stack algorithm as document `paste_rich`.
/// `source` is the workspace the rows were copied out of. It decides one
/// thing and nothing else: whether a **custom** attribute lands. A custom key
/// means what the workspace's system document says it means, so it travels
/// within one workspace — matching what a duplicate keeps — and is dropped
/// crossing into another, where the definition is not there to give it a type
/// or a name (docs/features/trackable/DECISIONS.md Q35). Two unknowns are not a match: a `None` on
/// either side reads as *different*, which is the safe direction.
/// The pasted nodes the **destination** gained — the ones whose completed task
/// ancestors are its, not the paste's own.
///
/// This is what `reclassify` is asked about at the paste commit, and the
/// distinction is load-bearing in both directions. A pasted node whose completed
/// ancestor arrived in the *same* paste must not reopen it: SPEC §6 says a
/// pasted subtree keeps the state each row had, so a completed parent copied
/// over an open child comes back exactly that way. A pasted node whose completed
/// ancestor was already in the document must, because that is SPEC §5's
/// "adding an unfinished structural child".
///
/// A `ListBlock` is the one exception, and it is scaffolding rather than
/// content: a flat body rebuilds copied list items into fresh containers
/// ([`PasteListLevel`]), so the container is the node whose parent is outside
/// the paste while the *item* is the unfinished child the destination actually
/// gained. A container can never be a task ([`task_capable`] excludes it), so
/// descending through one loses nothing.
fn landing_roots(ws: &WorkspaceDoc, pasted: &[NodeId]) -> Vec<NodeId> {
    let inside: std::collections::HashSet<NodeId> = pasted.iter().copied().collect();
    fn is_root(ws: &WorkspaceDoc, id: NodeId, inside: &std::collections::HashSet<NodeId>) -> bool {
        match ws.parent(id) {
            Ok(Parent::Node(up)) if inside.contains(&up) => {
                ws.read(up).is_ok_and(|n| n.kind == Kind::ListBlock) && is_root(ws, up, inside)
            }
            // A parent outside the paste — or the document root — makes this a
            // node the destination gained.
            _ => true,
        }
    }
    pasted
        .iter()
        .copied()
        .filter(|&id| ws.read(id).is_ok_and(|n| n.kind != Kind::ListBlock))
        .filter(|&id| is_root(ws, id, &inside))
        .collect()
}

fn paste_rows<E: Editing + ?Sized>(
    e: &mut E,
    ws: &mut WorkspaceDoc,
    rows: &[CopiedRow],
    source: Option<WorkspaceId>,
) -> bool {
    if rows.is_empty() {
        return false;
    }
    let flat = e.body().flat;
    let rebuild_lists = flat && rows.iter().any(|r| r.list.is_some());
    // A callout is meaningful wherever quotes are. A view that offers
    // neither (the task outline) takes the rows inside a box as prose.
    let rebuild_callouts = e.view().quotes_enabled && rows.iter().any(|r| r.callout.is_some());
    let same_workspace = source.is_some() && source == e.view().workspace;
    // Tags this workspace has never seen, and the pasted nodes waiting for
    // them. Handed to the host once the paste group has closed.
    let mut to_create: Vec<(NodeId, String)> = Vec::new();

    // Read before anything moves: `selection_roots` is defined by ancestry in
    // the tree, and the tree is about to change underneath it.
    let replacing = e.body().has_row_selection();
    let doomed = if replacing {
        e.body_mut().selection_roots(ws)
    } else {
        Vec::new()
    };

    // The row the paste lands on: the first of the selection, or the focused
    // one. The *first*, because the focus sits at whichever end of a selection
    // the user happened to stop dragging at.
    let at = if replacing {
        e.body().rows.iter().position(|r| r.selected)
    } else {
        Some(e.body().focus)
    };
    let Some(at) = at.filter(|&i| i < e.body().rows.len()) else {
        return false;
    };
    let (anchor, kind, blank) = {
        let landing = &e.body().rows[at];
        // A non-text block — a divider, or a blob — cannot absorb pasted text:
        // the paste lands after it, and the pasted rows take a text kind rather
        // than the block's. A divider only appears in documents, so the text
        // fallback is a paragraph.
        let kind = if landing.kind == Kind::Divider
            || landing.kind.shape() != koru_core::node::Shape::Text
        {
            Kind::Paragraph
        } else {
            landing.kind
        };
        (landing.id, kind, landing.text.is_empty())
    };
    // A blank row cannot absorb a list item in place — lists need a ListBlock
    // — nor a line from inside a callout, which needs its box around it.
    let first_is_list = rows.first().is_some_and(|r| r.list.is_some());
    let first_is_boxed = rebuild_callouts && rows.first().is_some_and(|r| r.callout.is_some());
    let reuse = blank
        && !replacing
        && e.body().rows[at].kind != Kind::Divider
        && !first_is_list
        && !first_is_boxed;

    // A multi-line block this view cannot hold whole lands as one row per
    // line, so nothing below ever pours a newline into a one-line kind.
    let rows = one_line_rows(rows, kind, e.view());
    let rows: &[CopiedRow] = &rows;
    // Tags are resolved against the destination's table first, while `e` can
    // still be borrowed for reading. Rows that carry none cost an empty `Vec`.
    let landings = tag_landings(e, rows);

    ws.checkpoint();
    ws.begin_group();

    // Where the next depth-zero row goes. Replacing takes the slot the block
    // vacates — the only way a paste can land at the very top of its parent;
    // otherwise it is the place directly after the focused row.
    let parent = e.body_mut().parent_of(ws, anchor);
    let mut index = ws.sibling_index(anchor).unwrap_or(0) + usize::from(!replacing);

    // Out before in, so the slot above is the one the user is looking at.
    // `reuse` is false whenever this runs, so nothing writes into a dead node.
    for &id in &doomed {
        let _ = ws.delete(id);
    }

    // The last node inserted at each relative depth, so a child knows its
    // parent. Outline (non-flat) path only.
    let mut stack: Vec<NodeId> = Vec::new();
    // Open ListBlocks while pasting list items into a flat document.
    let mut list_levels: Vec<PasteListLevel> = Vec::new();
    let mut last = None;
    let mut pasted: Vec<NodeId> = Vec::new();
    // Top-level insert_after cursor for list containers (and non-list blocks
    // after a list ends).
    let mut list_anchor: Option<NodeId> = if replacing { None } else { Some(anchor) };
    // The callout boxes being pasted into, outermost first, each with the
    // spec that opened it. Rows carrying a box's spec go under it — in a
    // flat body too, because a box is structure like a list's, not the
    // outline nesting ADR-23 flattens — until a row whose spec differs, or
    // carries none, closes it and every box opened inside it. Only where
    // callouts are offered; elsewhere the rows land as the prose they are.
    let mut callouts: Vec<(NodeId, CopiedCalloutSpec)> = Vec::new();

    for (n, row) in rows.iter().enumerate() {
        let spec = row.callout.as_ref().filter(|_| rebuild_callouts);
        let (keep, opens) = callout_box_change(callouts.iter().map(|(_, spec)| spec), spec);
        if keep < callouts.len() || opens {
            callouts.truncate(keep);
            // A box's lists end with it; a list after the box, or in a new
            // one, is a new list.
            list_levels.clear();
        }
        if opens && let Some(spec) = spec {
            let Ok(node) = Node::callout(spec.icon.as_deref(), spec.color.as_deref()) else {
                continue;
            };
            // A box inside a box goes under it; a top-level box takes the
            // slot the paste is filling, the way a list container does.
            let id = match callouts.last() {
                Some(&(outer, _)) => ws.insert(Parent::Node(outer), node).ok(),
                None => {
                    let Ok(id) = ws.insert(parent, node) else {
                        continue;
                    };
                    let _ = ws.move_node(id, parent, index);
                    index += 1;
                    list_anchor = Some(id);
                    Some(id)
                }
            };
            let Some(id) = id else { continue };
            pasted.push(id);
            callouts.push((id, spec.clone()));
        }
        let boxed = callouts.last().map(|&(callout, _)| callout);

        // --- list items in a flat document: rebuild ListBlock containers -----
        if rebuild_lists && let Some(spec) = row.list {
            let mut depth = (spec.depth as usize).min(list_levels.len());
            let mut parent_item = if depth == 0 {
                None
            } else {
                list_levels.get(depth - 1).and_then(|level| level.last_item)
            };
            if depth > 0 && parent_item.is_none() {
                depth = 0;
                parent_item = None;
            }
            list_levels.truncate((depth + 1).min(list_levels.len()));
            let reusable = list_levels
                .get(depth)
                .is_some_and(|level| level.style == spec.style && level.parent_item == parent_item);
            if !reusable {
                list_levels.truncate(depth);
                let Ok(list_node) = Node::list_block() else {
                    continue;
                };
                let list_id = if let Some(parent_item) = parent_item {
                    ws.insert(Parent::Node(parent_item), list_node).ok()
                } else if let Some(callout) = boxed {
                    // A list inside a callout is the box's: its container
                    // goes under the box, after whatever is already there.
                    ws.insert(Parent::Node(callout), list_node).ok()
                } else {
                    let Ok(id) = ws.insert(parent, list_node) else {
                        continue;
                    };
                    let _ = ws.move_node(id, parent, index);
                    index += 1;
                    list_anchor = Some(id);
                    Some(id)
                };
                let Some(list_id) = list_id else { continue };
                let _ = ws.set_list_style(list_id, Some(spec.style));
                pasted.push(list_id);
                list_levels.push(PasteListLevel {
                    container: list_id,
                    style: spec.style,
                    parent_item,
                    last_item: None,
                });
            }
            let Ok(item) = Node::new(Kind::Paragraph, koru_core::Content::text("")) else {
                continue;
            };
            let container = list_levels[depth].container;
            let Ok(id) = ws.insert(Parent::Node(container), item) else {
                continue;
            };
            insert_runs(ws, id, 0, &row.runs, &row.attrs);
            apply_meta(ws, id, row, Kind::Paragraph);
            apply_node_attrs(
                ws,
                id,
                row,
                Kind::Paragraph,
                &landings[n],
                same_workspace,
                &mut to_create,
            );
            list_levels[depth].last_item = Some(id);
            list_levels.truncate(depth + 1);
            pasted.push(id);
            last = Some(id);
            continue;
        }

        // A non-list row ends any open list run.
        list_levels.clear();

        // Nothing may skip a level: a row copied from depth 2 with no depth-1
        // row above it in the same copy is pasted one level in, not two. A flat
        // body has no levels to skip (lists already handled above).
        let depth = if flat {
            0
        } else {
            (row.depth as usize).min(stack.len())
        };

        let is_divider = row.kind == Kind::Divider;
        let is_image = row.image.is_some();
        let row_kind = kept_kind(row, e.view()).unwrap_or(kind);
        // A blank text row can absorb plain text; it cannot become a picture or
        // a divider in place — those always insert as their own node.
        let id = if n == 0 && reuse && !is_divider && !is_image {
            let _ = ws.set_kind(anchor, row_kind);
            anchor
        } else {
            let node = if is_divider {
                match Node::divider() {
                    Ok(n) => n,
                    Err(_) => continue,
                }
            } else if let Some(img) = &row.image {
                match Node::image(
                    BlobHash(img.hash.clone()),
                    img.mime.clone(),
                    img.width,
                    img.height,
                ) {
                    Ok(n) => n,
                    Err(_) => continue,
                }
            } else {
                match Node::new(row_kind, koru_core::Content::text("")) {
                    Ok(n) => n,
                    Err(_) => continue,
                }
            };
            // Inside a box the row belongs to the box, appended after what is
            // already in it; outside, its depth says where it goes.
            let under = match (boxed, depth) {
                (Some(callout), _) => Parent::Node(callout),
                (None, 0) => parent,
                (None, d) => Parent::Node(stack[d - 1]),
            };
            let Ok(id) = ws.insert(under, node) else {
                continue;
            };
            // `insert` appends; a depth-zero row belongs in the slot the paste
            // is filling, not at the end of its parent.
            if depth == 0 && boxed.is_none() {
                let _ = ws.move_node(id, under, index);
                index += 1;
            }
            list_anchor = Some(id);
            id
        };

        // A divider carries no text and no inline meta; an image carries blob
        // meta only. Text rows get runs + landing-kind meta.
        if is_image {
            if let Some(img) = &row.image {
                if let Some(name) = &img.name {
                    let _ = ws.set_name(id, Some(name.as_str()));
                }
            }
            if let Some(a) = row.align {
                let _ = ws.set_align(id, Some(a));
            }
        } else if !is_divider {
            insert_runs(ws, id, 0, &row.runs, &row.attrs);
            apply_meta(ws, id, row, row_kind);
            apply_node_attrs(
                ws,
                id,
                row,
                row_kind,
                &landings[n],
                same_workspace,
                &mut to_create,
            );
        }

        stack.truncate(depth);
        stack.push(id);
        pasted.push(id);
        last = Some(id);
        let _ = list_anchor;
    }

    // Linkify every pasted *text* row's URLs, still inside the paste group. A
    // code block is left alone, as `paste_inline` leaves it: source text is
    // not an address to follow.
    for &id in &pasted {
        if ws
            .read(id)
            .ok()
            .and_then(|n| n.content.as_text().map(|_| ()))
            .is_some()
            && ws
                .read(id)
                .is_ok_and(|n| !matches!(n.kind, Kind::ListBlock | Kind::Callout | Kind::Code))
        {
            let _ = linkify_urls_in_node(ws, id);
        }
    }
    ws.checkpoint();

    // **The paste is a user command, so it classifies** (SPEC §5, ADR-272).
    // After the group has closed — a canonical command `checkpoint()`s before
    // its own `atomic`, and running one inside would shatter the paste's single
    // Ctrl+Z — every landing root asks whether it reopened the task it landed
    // in. `reclassify` answers `None` for passive content and for a root with
    // no completed ancestor, so the common paste pays one read per root and
    // opens no group at all.
    for id in landing_roots(ws, &pasted) {
        let _ = crate::tasks::run(&mut e.body_mut().task_activity, ws, |ws| {
            koru_core::tasks::reclassify(ws, id)
        });
    }

    // A tag the destination has never heard of has to be made in its **system
    // document**, which no editor can see (ADR-176's seam) — so the request
    // goes up to the host, which creates-or-reuses by name and applies the id
    // it gets back. That apply is its own undo step, and a host with no
    // reachable system document simply never answers: the tag is dropped and
    // the rest of the paste stands.
    for (node, name) in to_create {
        e.view_mut().wants_create_tag.push(vec![node], name);
    }

    let Some(last) = last else {
        return false;
    };
    e.body_mut().clear_selection();
    e.body_mut().rebuild(ws);
    e.body_mut().focus_on(last);
    e.place_caret(usize::MAX);
    e.view_mut().desired_x = None;
    e.settle();
    // settle scrolls to the caret, but a multi-row paste can leave the last
    // line just off-screen when chrome ate the band math; land on the last
    // pasted row so large pastes end with the insertion point in view.
    if let Some(i) = e.body().index_of(last) {
        e.scroll_row_into_view(i);
        e.scroll_to_caret();
    }
    true
}

struct PasteListLevel {
    container: NodeId,
    style: ListStyle,
    parent_item: Option<NodeId>,
    last_item: Option<NodeId>,
}

/// The kind a copied block keeps where it lands, or `None` when it takes the
/// landing row's kind instead.
///
/// A table, a quote and a code block mean the same thing in every document
/// that offers them, whatever paragraph they land beside. A view that does
/// not offer the block (the task outline) adapts it to its own row kind, the
/// same way it adapts prose — and [`one_line_rows`] makes sure that adaptation
/// never costs a line.
fn kept_kind(row: &CopiedRow, view: &ViewState) -> Option<Kind> {
    match row.kind {
        Kind::Table if view.tables_enabled => Some(Kind::Table),
        Kind::Quote if view.quotes_enabled => Some(Kind::Quote),
        Kind::Code if view.code_enabled => Some(Kind::Code),
        _ => None,
    }
}

/// The rows as the destination can hold them, one node each.
///
/// A code block is one node whose text spans lines, and so is a table's
/// source. Where the view offers the block it passes through whole. Where it
/// does not, the block takes the landing row's kind — and a one-line kind
/// cannot hold a newline. `insert_runs` skips a run that fails to write, so
/// such a block used to arrive as an empty row with its text gone and nothing
/// said. It lands instead as one row per line, which is what the same text
/// pasted from another application already does. Borrowed, not cloned, in
/// the common case of nothing to split.
fn one_line_rows<'a>(
    rows: &'a [CopiedRow],
    landing: Kind,
    view: &ViewState,
) -> Cow<'a, [CopiedRow]> {
    let needs_split = |row: &CopiedRow| {
        kept_kind(row, view).is_none()
            && !landing.allows_newlines()
            && row
                .runs
                .iter()
                .any(|run| !run.is_pill() && run.text.contains('\n'))
    };
    if !rows.iter().any(needs_split) {
        return Cow::Borrowed(rows);
    }
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        if !needs_split(row) {
            out.push(row.clone());
            continue;
        }
        let text: String = row
            .runs
            .iter()
            .filter(|run| !run.is_pill())
            .map(|run| run.text.as_str())
            .collect();
        for (n, line) in text.lines().enumerate() {
            out.push(CopiedRow {
                runs: vec![plain_run(line.to_owned())],
                attrs: Vec::new(),
                // What the block carried stays with its first line.
                node_attrs: if n == 0 {
                    row.node_attrs.clone()
                } else {
                    Vec::new()
                },
                // Adapted to the destination by `paste_rows`.
                kind: Kind::Paragraph,
                done: false,
                level_name: None,
                align: row.align,
                heading: None,
                table_layout: None,
                language: None,
                depth: row.depth,
                image: None,
                list: row.list,
                // Every line stays inside the box the block was in.
                callout: row.callout.clone(),
            });
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Editor;
    use crate::input::action::Action;
    use koru_core::attributes::{KEY_COLOR, KEY_DUE, KEY_PRIORITY};
    use koru_core::node::{Attr, AttrValue};
    use koru_core::time::DateOnly;
    use koru_core::{Mark, WorkspaceId};
    use winit::keyboard::Key;

    /// The clipboard is process-global, and cargo runs tests in one process on
    /// many threads — so every test that copies has to hold this or it will
    /// paste what another test copied.

    fn editor_over(rows: &[(&str, u16)]) -> (WorkspaceDoc, Editor) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let mut stack: Vec<NodeId> = Vec::new();
        for (text, depth) in rows {
            let parent = match depth {
                0 => Parent::Root,
                d => Parent::Node(stack[*d as usize - 1]),
            };
            let id = ws.insert(parent, Node::task(*text).unwrap()).unwrap();
            stack.truncate(*depth as usize);
            stack.push(id);
        }
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.set_viewport(900.0, 560.0);
        (ws, e)
    }

    fn shape(e: &Editor) -> Vec<(String, u16)> {
        e.body
            .rows
            .iter()
            .map(|r| (r.text.clone(), r.depth))
            .collect()
    }

    /// Select rows `lo..=hi` the way a drag would.
    ///
    /// Clearing first is part of the gesture, not tidiness: a press calls
    /// `clear_selection`, and without it `extend_selection_to` extends from
    /// whatever anchor the *previous* selection left behind.
    fn select(e: &mut Editor, lo: usize, hi: usize) {
        e.body.clear_selection();
        e.body.focus = lo;
        e.body.extend_selection_to(hi);
    }

    /// Which rows are marked.
    fn selected(e: &Editor) -> Vec<usize> {
        e.body
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.selected)
            .map(|(i, _)| i)
            .collect()
    }

    /// An image-bearing clipboard only arms `wants_paste_image` when the view
    /// shows images, the clipboard is non-text, and there is a focused row to
    /// land below. The boolean is injected so the OS read stays out of the test.
    #[test]
    fn image_paste_is_offered_only_when_enabled_non_text_and_focused() {
        fixture();
        let (ws, mut e) = editor_over(&[("one", 0)]);
        e.body.focus = 0;

        // Non-text on the clipboard, but images are not offered here (the task
        // outline). A text-only no-op, exactly as before.
        assert!(!offer_image_paste(&mut e, &ws, true));
        assert!(e.view().wants_paste_image.is_none());

        // A document shows images: the offer is taken, and the focused row is
        // recorded as the anchor the new block lands below.
        e.view_mut().images_enabled = true;
        assert!(offer_image_paste(&mut e, &ws, true));
        assert_eq!(
            e.view().wants_paste_image,
            e.body().focused_id().map(|id| (id, Vec::new()))
        );

        // The non-text gate still holds: a text clipboard never arms it.
        e.view_mut().wants_paste_image = None;
        assert!(!offer_image_paste(&mut e, &ws, false));
        assert!(e.view().wants_paste_image.is_none());
    }

    /// Without a focused row there is nowhere to put the block — the same rule
    /// text paste follows — so the offer is declined even in a document.
    #[test]
    fn image_paste_needs_a_focused_row() {
        fixture();
        let (ws, mut e) = editor_over(&[("only", 0)]);
        e.view_mut().images_enabled = true;
        e.body.focus = usize::MAX; // past the end → `focused_id` is `None`
        assert!(!offer_image_paste(&mut e, &ws, true));
    }

    /// Foreign text pasted in a document defers to the shell (so it can
    /// interleave any pictures), returning the focused row to anchor on. The
    /// task outline — images disabled — never defers, and a document without a
    /// focused row has nowhere to anchor, so both fall back to pasting the text.
    #[test]
    fn rich_paste_is_offered_only_for_a_focused_document() {
        fixture();
        let (ws, mut e) = editor_over(&[("one", 0)]);
        e.body.focus = 0;

        // The task outline (images disabled) never rich-pastes.
        assert!(offer_rich_paste(&mut e, &ws).is_none());

        // A document defers, recording the focused row as the anchor.
        e.view_mut().images_enabled = true;
        assert_eq!(
            offer_rich_paste(&mut e, &ws),
            e.body().focused_id().map(|id| (id, Vec::new()))
        );

        // Without a focus there is nowhere to anchor, so it declines.
        e.body.focus = usize::MAX;
        assert!(offer_rich_paste(&mut e, &ws).is_none());
    }

    /// Foreign text pasted into a code block is never deferred: the block
    /// takes text only, so it pastes plainly, with its newlines read as
    /// `paste_plain_text` reads them. Deferred, a Windows paste reached the
    /// block with every line double-spaced, where paste-as-plain did not.
    #[test]
    fn foreign_text_into_a_code_block_is_pasted_plainly_not_deferred() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("one", 0)]);
        e.view_mut().images_enabled = true;
        e.body.focus = 0;
        assert!(
            offer_rich_paste(&mut e, &ws).is_some(),
            "a paragraph defers"
        );

        let id = e.body.rows[0].id;
        ws.set_kind(id, Kind::Code).unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;
        assert!(
            offer_rich_paste(&mut e, &ws).is_none(),
            "a code block pastes plainly"
        );
    }

    /// Deferred rich/image decoding must retain the selection from the paste
    /// gesture. The focus is at the lower end here; replacement still lands at
    /// the first selected row and removes both roots.
    #[test]
    fn deferred_paste_captures_the_selected_blocks() {
        fixture();
        let (ws, mut e) = editor_over(&[("keep", 0), ("one", 0), ("two", 0), ("tail", 0)]);
        select(&mut e, 1, 2);
        e.view_mut().images_enabled = true;

        let target = offer_rich_paste(&mut e, &ws).expect("a rich paste target");
        assert_eq!(target.0, e.body.rows[1].id, "the first selected slot");
        assert_eq!(target.1, vec![e.body.rows[1].id, e.body.rows[2].id]);
    }

    /// The headline: a task copied is a task pasted, and the original stays.
    #[test]
    fn copying_a_task_and_pasting_it_makes_a_second_one() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("first", 0), ("second", 0)]);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));

        assert_eq!(
            shape(&e),
            vec![
                ("first".into(), 0),
                ("first".into(), 0),
                ("second".into(), 0)
            ],
            "the copy lands directly below its original"
        );
    }

    /// Nesting is part of what was copied. A subtree pasted flat would be a
    /// different subtree.
    #[test]
    fn a_pasted_subtree_keeps_its_shape() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("parent", 0), ("child", 1), ("grandchild", 2)]);
        select(&mut e, 0, 2);

        assert!(copy(&mut e, &ws));
        e.body.clear_selection();
        e.body.focus = 2;
        assert!(paste(&mut e, &mut ws));

        assert_eq!(
            shape(&e),
            vec![
                ("parent".into(), 0),
                ("child".into(), 1),
                ("grandchild".into(), 2),
                ("parent".into(), 2),
                ("child".into(), 3),
                ("grandchild".into(), 4),
            ],
            "relative depth is preserved, measured from wherever it lands"
        );
    }

    /// Formatting is the whole reason the payload is runs rather than a string.
    #[test]
    fn marks_survive_a_copy_and_paste() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("bold text", 0)]);
        let id = e.body.rows[0].id;
        ws.set_mark(id, 0..4, Mark::Bold, true).unwrap();
        e.body.refresh(&ws, id);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));

        let pasted = e.body.rows[1].id;
        let runs = ws.runs(pasted).unwrap();
        assert_eq!(runs[0].text, "bold");
        assert!(runs[0].bold, "the bold run came across");
        assert!(!runs[1].bold, "and the plain one did not become bold");
    }

    #[test]
    fn a_whole_table_copy_preserves_source_kind_and_width_policy() {
        fixture();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "tables").unwrap();
        let source = "| A | B |\n| --- | ---: |\n| one | two |";
        let original = ws
            .insert(Parent::Root, Node::table(source).unwrap())
            .unwrap();
        ws.set_table_layout(original, TableLayout::Extended)
            .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.view_mut().tables_enabled = true;
        e.set_viewport(900.0, 560.0);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));

        assert_eq!(e.body.rows.len(), 2);
        let pasted = ws.read(e.body.rows[1].id).unwrap();
        assert_eq!(pasted.kind, Kind::Table);
        assert_eq!(pasted.content.as_text(), Some(source));
        assert_eq!(pasted.meta.table_layout, Some(TableLayout::Extended));
    }

    /// A pill is an attribute plus an anchor. Copying the anchor character
    /// alone would paste an invisible nothing.
    #[test]
    fn a_pill_is_pasted_as_a_pill() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("due", 0)]);
        let id = e.body.rows[0].id;
        let attr = Attr::new("due", AttrValue::Text("Friday".into()));
        ws.insert_pill(id, 3, &attr).unwrap();
        e.body.refresh(&ws, id);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));

        let pasted = ws.read(e.body.rows[1].id).unwrap();
        assert_eq!(pasted.content.attrs().len(), 1, "the attribute came too");
        let runs = ws.runs(e.body.rows[1].id).unwrap();
        assert!(
            runs.iter().any(TextRun::is_pill),
            "and it is still anchored"
        );
    }

    /// A pasted URL becomes a link, the way a typed one does — no `Ctrl+K`
    /// needed. This is the path external text takes (`paste_plain_text`), so it
    /// does not touch the process-global clipboard and needs no lock.
    #[test]
    fn a_pasted_url_becomes_a_link() {
        let (mut ws, mut e) = editor_over(&[("", 0)]);
        e.body.focus = 0;
        assert!(paste_plain_text(
            &mut e,
            &mut ws,
            "https://example.com/path"
        ));

        let node = ws.read(e.body.rows[0].id).unwrap();
        assert!(
            node.content
                .attrs()
                .iter()
                .any(|a| matches!(&a.value, AttrValue::Url(u) if u == "https://example.com/path")),
            "the pasted URL became a link"
        );
        assert!(
            !e.body.rows[0].text.contains("example"),
            "the URL text is replaced by the pill anchor"
        );
    }

    /// A code block holds plain text, so a URL pasted into one is source, not an
    /// address to follow.
    #[test]
    fn a_url_pasted_into_a_code_block_is_not_linkified() {
        let (mut ws, mut e) = editor_over(&[("", 0)]);
        let id = e.body.rows[0].id;
        ws.set_kind(id, Kind::Code).unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;
        e.place_caret(0);
        e.reflow();

        assert!(paste_plain_text(
            &mut e,
            &mut ws,
            "https://example.com/path"
        ));
        let node = ws.read(id).unwrap();
        assert_eq!(node.content.as_text(), Some("https://example.com/path"));
        assert!(
            !node
                .content
                .attrs()
                .iter()
                .any(|a| matches!(&a.value, AttrValue::Url(_))),
            "no link was created inside a code block"
        );
    }

    /// Windows newlines pasted into a code block arrive as Unix ones. The
    /// block kept the `\r`, and the shaper broke the line at it and again at
    /// the `\n`: a diagram pasted from another app showed every line
    /// double-spaced, with no blank line in the text to delete.
    #[test]
    fn windows_newlines_pasted_into_a_code_block_become_unix_ones() {
        let (mut ws, mut e) = editor_over(&[("", 0)]);
        let id = e.body.rows[0].id;
        ws.set_kind(id, Kind::Code).unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;
        e.place_caret(0);
        e.reflow();

        assert!(paste_plain_text(
            &mut e,
            &mut ws,
            "flowchart LR\r\n  A --> B\rC"
        ));
        let node = ws.read(id).unwrap();
        assert_eq!(node.content.as_text(), Some("flowchart LR\n  A --> B\nC"));
    }

    /// Pasting formatted runs into a code block drops the marks — code is plain
    /// text, not rich text.
    #[test]
    fn pasting_formatted_text_into_a_code_block_drops_marks() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("bold text", 0)]);
        let id = e.body.rows[0].id;
        ws.set_mark(id, 0..9, Mark::Bold, true).unwrap();
        e.body.refresh(&ws, id);
        e.body.focus = 0;
        e.place_selection_pair((0, 9));
        e.reflow();
        assert!(copy(&mut e, &ws));

        let (mut ws2, mut e2) = editor_over(&[("", 0)]);
        let code = e2.body.rows[0].id;
        ws2.set_kind(code, Kind::Code).unwrap();
        e2.body.rebuild(&ws2);
        e2.body.focus = 0;
        e2.place_caret(0);
        e2.reflow();

        assert!(paste(&mut e2, &mut ws2));
        assert_eq!(
            ws2.read(code).unwrap().content.as_text(),
            Some("bold text"),
            "the text itself arrived"
        );
        assert!(
            ws2.runs(code).unwrap().iter().all(|r| !r.bold),
            "the bold mark was dropped"
        );
    }

    /// A line with several URLs links them all.
    #[test]
    fn pasting_text_with_several_urls_links_them_all() {
        let (mut ws, mut e) = editor_over(&[("", 0)]);
        e.body.focus = 0;
        assert!(paste_plain_text(
            &mut e,
            &mut ws,
            "see https://a.com and https://b.com"
        ));

        let urls: Vec<String> = ws
            .read(e.body.rows[0].id)
            .unwrap()
            .content
            .attrs()
            .iter()
            .filter_map(|a| match &a.value {
                AttrValue::Url(u) => Some(u.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(urls.len(), 2, "both URLs linked");
        assert!(urls.contains(&"https://a.com".into()));
        assert!(urls.contains(&"https://b.com".into()));
    }

    /// Paste and its links are one undo step: a single Ctrl+Z removes both.
    #[test]
    fn undo_after_a_paste_with_links_removes_it_all() {
        let (mut ws, mut e) = editor_over(&[("", 0)]);
        e.body.focus = 0;
        paste_plain_text(&mut e, &mut ws, "https://example.com");
        assert!(
            ws.read(e.body.rows[0].id)
                .unwrap()
                .content
                .attrs()
                .iter()
                .any(|a| matches!(a.value, AttrValue::Url(_)))
        );

        ws.undo().unwrap();
        e.body_mut().rebuild(&ws);
        let node = ws.read(e.body.rows[0].id).unwrap();
        assert!(node.content.attrs().is_empty(), "undo removed the link");
        assert!(
            node.content.as_text().unwrap_or_default().is_empty(),
            "undo removed the pasted text"
        );
    }

    /// Completion travels with the task, so cutting and pasting a done task
    /// does not quietly un-finish it.
    #[test]
    fn a_done_task_is_pasted_done() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("finished", 0)]);
        let id = e.body.rows[0].id;
        ws.set_done(id, true).unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));

        let pasted = e.body.rows.iter().find(|r| r.id != id).unwrap();
        assert!(ws.read(pasted.id).unwrap().meta.done);
    }

    /// Cut takes them away and the clipboard keeps them.
    #[test]
    fn cutting_removes_the_rows_and_pasting_brings_them_back() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("one", 0), ("two", 0), ("three", 0)]);
        select(&mut e, 0, 1);

        assert!(cut(&mut e, &mut ws));
        assert_eq!(shape(&e), vec![("three".into(), 0)]);

        assert!(paste(&mut e, &mut ws));
        assert_eq!(
            shape(&e),
            vec![("three".into(), 0), ("one".into(), 0), ("two".into(), 0)]
        );
    }

    /// After cutting a multi-row selection the caret lands at the start of the
    /// hole — the first selected row — not whichever end the drag finished on.
    #[test]
    fn after_cutting_a_row_selection_the_caret_is_at_the_start() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("a", 0), ("b", 0), ("c", 0), ("d", 0), ("e", 0)]);
        // Drag down: focus ends at 2 (c). Landing there after the delete would
        // put the caret on e, past the first survivor.
        select(&mut e, 1, 2);
        assert!(cut(&mut e, &mut ws));
        assert_eq!(
            shape(&e),
            vec![("a".into(), 0), ("d".into(), 0), ("e".into(), 0)]
        );
        assert_eq!(e.body.focus, 1);
        assert_eq!(e.body.focused().map(|r| r.text.as_str()), Some("d"));
        assert_eq!(e.body.focused().map(|r| r.caret()), Some(0));

        // Extending upward leaves focus at the top end — same landing.
        let (mut ws, mut e) = editor_over(&[("a", 0), ("b", 0), ("c", 0), ("d", 0), ("e", 0)]);
        select(&mut e, 2, 1);
        assert!(cut(&mut e, &mut ws));
        assert_eq!(e.body.focus, 1);
        assert_eq!(e.body.focused().map(|r| r.text.as_str()), Some("d"));
    }

    /// Within a line the clipboard is text, not rows — no new task appears.
    #[test]
    fn copying_part_of_a_line_pastes_into_the_line() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("hello world", 0)]);
        e.body.focus = 0;
        e.place_selection_pair((0, 5));
        e.reflow();

        assert!(copy(&mut e, &ws));
        e.place_caret(11);
        e.reflow();
        assert!(paste(&mut e, &mut ws));

        assert_eq!(shape(&e), vec![("hello worldhello".into(), 0)]);
    }

    /// Cutting inside a line takes the characters, not the task.
    #[test]
    fn cutting_part_of_a_line_leaves_the_line() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("hello world", 0)]);
        e.body.focus = 0;
        e.place_selection_pair((0, 6));
        e.reflow();

        assert!(cut(&mut e, &mut ws));

        assert_eq!(shape(&e), vec![("world".into(), 0)]);
    }

    /// Text from another application becomes one task per line, and its
    /// indentation becomes nesting.
    #[test]
    fn pasted_text_becomes_rows() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("here", 0)]);
        e.body.focus = 0;

        assert!(paste_plain_text(
            &mut e,
            &mut ws,
            "shopping\n\tmilk\n\tbread\n"
        ));

        assert_eq!(
            shape(&e),
            vec![
                ("here".into(), 0),
                ("shopping".into(), 0),
                ("milk".into(), 1),
                ("bread".into(), 1),
            ]
        );
    }

    /// One line of text is not a new task — it belongs where the caret is.
    #[test]
    fn a_single_pasted_line_goes_in_at_the_caret() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("ab", 0)]);
        e.body.focus = 0;
        e.place_caret(1);
        e.reflow();

        assert!(paste_plain_text(&mut e, &mut ws, "XY"));

        assert_eq!(shape(&e), vec![("aXYb".into(), 0)]);
    }

    /// Pressing Enter and then pasting should not leave the blank line behind.
    #[test]
    fn pasting_into_a_blank_row_reuses_it() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("written on", 0), ("", 0)]);
        e.body.focus = 0;
        assert!(copy(&mut e, &ws));

        e.body.focus = 1;
        assert!(paste(&mut e, &mut ws));

        assert_eq!(
            shape(&e),
            vec![("written on".into(), 0), ("written on".into(), 0)],
            "no empty row left above the paste"
        );
    }

    /// The text half is what leaves for other applications, so a pill has to
    /// read as its label rather than as the anchor nobody else can see.
    #[test]
    fn the_text_half_spells_out_the_pills() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("due ", 0)]);
        let id = e.body.rows[0].id;
        ws.insert_pill(id, 4, &Attr::new("due", AttrValue::Text("Friday".into())))
            .unwrap();
        e.body.refresh(&ws, id);
        e.body.focus = 0;

        let payload = capture(&e, &ws).unwrap();
        assert_eq!(render(&ws, &payload), "due Friday");
    }

    /// Nesting has to survive the trip out to text and back, since that is the
    /// only channel another application has.
    #[test]
    fn the_text_half_indents_with_tabs() {
        fixture();
        let (ws, mut e) = editor_over(&[("parent", 0), ("child", 1)]);
        select(&mut e, 0, 1);

        let payload = capture(&e, &ws).unwrap();
        assert_eq!(render(&ws, &payload), "parent\n\tchild");
    }

    // ------------------------------------------------------ whole subtrees
    //
    // A task owns its sub-tasks, and `delete` takes descendants with it. A copy
    // that took only the marked row made a cut destroy a subtree and leave one
    // line on the clipboard — the children gone, and nothing to paste back.

    /// Copying a parent takes everything under it, however deep, without the
    /// user having to select the whole block first.
    #[test]
    fn copying_a_task_takes_its_children() {
        fixture();
        let (mut ws, mut e) = editor_over(&[
            ("keep", 0),
            ("parent", 0),
            ("child", 1),
            ("grandchild", 2),
            ("after", 0),
        ]);
        e.body.focus = 1;

        assert!(e.act(&mut ws, Action::Copy));
        e.body.clear_selection();
        e.body.focus = 4;
        assert!(e.act(&mut ws, Action::Paste));

        assert_eq!(
            shape(&e),
            vec![
                ("keep".into(), 0),
                ("parent".into(), 0),
                ("child".into(), 1),
                ("grandchild".into(), 2),
                ("after".into(), 0),
                ("parent".into(), 0),
                ("child".into(), 1),
                ("grandchild".into(), 2),
            ]
        );
    }

    /// **The reported bug.** Cutting a node three levels in took its 4th, 5th
    /// and 6th-level children away with it and put one line on the clipboard,
    /// so pasting brought back the node alone and the rest of the work was
    /// gone. A cut has to be undoable by a paste.
    #[test]
    fn cutting_a_deep_task_keeps_every_level_beneath_it() {
        fixture();
        let (mut ws, mut e) = editor_over(&[
            ("one", 0),
            ("two", 1),
            ("three", 2),
            ("four", 3),
            ("five", 4),
            ("six", 5),
        ]);
        // The middle of the tree, with three levels under it.
        e.body.focus = 2;

        assert!(e.act(&mut ws, Action::Cut));
        assert_eq!(
            shape(&e),
            vec![("one".into(), 0), ("two".into(), 1)],
            "the subtree went"
        );

        assert!(e.act(&mut ws, Action::Paste));

        // Every level came back, and each is one deeper than the last. The
        // block lands beside the row the caret is on — which after the cut is
        // the *parent* of what was taken, so the whole thing sits one level
        // shallower than it started. That is where a paste goes, not a loss:
        // the shape within the block is what the clipboard preserves.
        assert_eq!(
            shape(&e),
            vec![
                ("one".into(), 0),
                ("two".into(), 1),
                ("three".into(), 1),
                ("four".into(), 2),
                ("five".into(), 3),
                ("six".into(), 4),
            ]
        );
    }

    /// Cutting everything leaves a page with nothing to type on and nowhere to
    /// put a caret, so a line is made to take its place — the same rule that
    /// seeds a fresh workspace.
    ///
    /// The old test for this counted the doomed rows against the visible ones,
    /// which said "not all of them" when *one* selected parent had emptied the
    /// whole page: `delete` takes descendants with it.
    #[test]
    fn cutting_the_whole_outline_leaves_a_line_to_type_on() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("parent", 0), ("child", 1), ("grandchild", 2)]);
        e.body.focus = 0;

        assert!(e.act(&mut ws, Action::Cut));

        assert_eq!(shape(&e), vec![(String::new(), 0)], "one blank line");
        assert_eq!(e.body.focus, 0, "with the caret in it");
    }

    /// A row whose parent is also selected arrives as part of that parent's
    /// subtree. Taking it again would paste it twice.
    #[test]
    fn a_selected_parent_and_child_are_not_copied_twice() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("parent", 0), ("child", 1), ("tail", 0)]);
        select(&mut e, 0, 1);

        assert!(e.act(&mut ws, Action::Copy));
        e.body.clear_selection();
        e.body.focus = 2;
        assert!(e.act(&mut ws, Action::Paste));

        assert_eq!(
            shape(&e),
            vec![
                ("parent".into(), 0),
                ("child".into(), 1),
                ("tail".into(), 0),
                ("parent".into(), 0),
                ("child".into(), 1),
            ]
        );
    }

    /// A completed child hidden by `F4` is still deleted by the cut, so it has
    /// to be on the clipboard — which is why the copy reads the document rather
    /// than the visible rows.
    #[test]
    fn a_hidden_completed_child_is_copied_with_its_parent() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("parent", 0), ("done child", 1), ("tail", 0)]);
        let hidden = e.body.rows[1].id;
        ws.set_done(hidden, true).unwrap();
        e.body.hide_completed = true;
        e.body.rebuild(&ws);
        e.reflow();
        assert_eq!(shape(&e), vec![("parent".into(), 0), ("tail".into(), 0)]);

        e.body.focus = 0;
        assert!(e.act(&mut ws, Action::Copy));

        // Show them again: the copy should have taken the hidden one too.
        e.body.hide_completed = false;
        e.body.rebuild(&ws);
        e.reflow();
        e.body.clear_selection();
        e.body.focus = 2;
        assert!(e.act(&mut ws, Action::Paste));

        let texts: Vec<String> = e.body.rows.iter().map(|r| r.text.clone()).collect();
        assert_eq!(
            texts.iter().filter(|t| *t == "done child").count(),
            2,
            "the hidden child came across: {texts:?}"
        );
    }

    // ------------------------------------------------ through the dispatch
    //
    // Everything above calls `copy`/`cut`/`paste` directly. That is exactly why
    // the Cut bug shipped: `Editing::act` clears the row selection before
    // dispatching unless the action is on `keeps_row_selection`'s list, and Cut
    // was not. These drive the layer that broke, so it cannot break silently
    // again.

    /// **The regression.** Ctrl+X on a block used to cut exactly one row: `act`
    /// wiped the marks, and the cut then found only the focused row to take.
    #[test]
    fn cutting_through_an_action_takes_every_selected_row() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("one", 0), ("two", 0), ("three", 0)]);
        select(&mut e, 0, 1);

        assert!(e.act(&mut ws, Action::Cut));

        assert_eq!(shape(&e), vec![("three".into(), 0)]);
    }

    /// The same through a real chord, which covers the keymap entry and the
    /// inactive-line gate as well as `act`.
    #[test]
    fn ctrl_x_cuts_every_selected_row() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("one", 0), ("two", 0), ("three", 0)]);
        select(&mut e, 0, 1);

        assert!(e.key(
            &mut ws,
            &Key::Character("x".into()),
            None,
            koru_keys::PRIMARY
        ));

        assert_eq!(shape(&e), vec![("three".into(), 0)]);
    }

    /// Copy is on the same list and must stay there: a copy leaves what it
    /// copied selected, so it can be moved next.
    #[test]
    fn copying_through_an_action_leaves_the_rows_selected() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("one", 0), ("two", 0), ("three", 0)]);
        select(&mut e, 0, 1);

        assert!(e.act(&mut ws, Action::Copy));

        assert_eq!(selected(&e), vec![0, 1], "still selected after copying");
        assert_eq!(shape(&e).len(), 3, "and nothing was removed");
    }

    /// Pasting over a selection replaces it, the way pasting over selected text
    /// replaces the text.
    #[test]
    fn pasting_over_selected_rows_replaces_them() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("keep", 0), ("a", 0), ("b", 0), ("tail", 0)]);
        select(&mut e, 1, 2);
        assert!(e.act(&mut ws, Action::Copy));

        select(&mut e, 3, 3);
        assert!(e.act(&mut ws, Action::Paste));

        assert_eq!(
            shape(&e),
            vec![
                ("keep".into(), 0),
                ("a".into(), 0),
                ("b".into(), 0),
                ("a".into(), 0),
                ("b".into(), 0)
            ],
            "the tail row was replaced by the two copied ones"
        );
    }

    /// The case the old `sibling_index(prev) + 1` scheme could not express:
    /// there is no row above to be placed after.
    #[test]
    fn pasting_over_the_first_row_lands_at_the_top() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("first", 0), ("second", 0)]);
        e.body.focus = 1;
        assert!(e.act(&mut ws, Action::Copy));

        select(&mut e, 0, 0);
        assert!(e.act(&mut ws, Action::Paste));

        assert_eq!(shape(&e), vec![("second".into(), 0), ("second".into(), 0)]);
    }

    /// The gesture from the bug report, end to end: take a block out and put it
    /// down somewhere else, whole.
    #[test]
    fn cutting_a_block_and_pasting_it_moves_the_whole_block() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("parent", 0), ("child", 1), ("elsewhere", 0)]);
        select(&mut e, 0, 1);
        assert!(e.act(&mut ws, Action::Cut));
        assert_eq!(shape(&e), vec![("elsewhere".into(), 0)]);

        e.body.focus = 0;
        assert!(e.act(&mut ws, Action::Paste));

        assert_eq!(
            shape(&e),
            vec![
                ("elsewhere".into(), 0),
                ("parent".into(), 0),
                ("child".into(), 1)
            ],
            "the nesting came with it"
        );
    }

    /// Reuse of a blank row has to survive the restructure, through `act` as
    /// well as through `paste` directly.
    #[test]
    fn pasting_into_a_blank_row_through_an_action_still_reuses_it() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("written on", 0), ("", 0)]);
        e.body.focus = 0;
        assert!(e.act(&mut ws, Action::Copy));

        e.body.clear_selection();
        e.body.focus = 1;
        assert!(e.act(&mut ws, Action::Paste));

        assert_eq!(
            shape(&e),
            vec![("written on".into(), 0), ("written on".into(), 0)],
            "no empty row left above the paste"
        );
    }

    /// Dropping the caret drops the selection with it. Leaving the marks behind
    /// left rows visibly highlighted while every key that acts on them was
    /// being refused.
    #[test]
    fn deactivating_drops_the_row_selection() {
        fixture();
        let (_ws, mut e) = editor_over(&[("one", 0), ("two", 0)]);
        select(&mut e, 0, 1);
        assert_eq!(selected(&e), vec![0, 1]);

        e.deactivate();

        assert!(selected(&e).is_empty());
    }

    // ------------------------------------------------------- indentation

    /// The three conventions a paste actually arrives in. Each is one level,
    /// because the block decides its own step size.
    #[test]
    fn tabs_two_spaces_and_four_spaces_all_read_as_one_level() {
        let depths = |lines: &[&str]| {
            let owned: Vec<String> = lines.iter().map(|s| (*s).to_owned()).collect();
            indents_of(&owned)
                .into_iter()
                .map(|(d, _)| d)
                .collect::<Vec<_>>()
        };
        assert_eq!(depths(&["a", "\tb", "\tc"]), vec![0, 1, 1], "tabs");
        assert_eq!(depths(&["a", "  b", "  c"]), vec![0, 1, 1], "two spaces");
        assert_eq!(depths(&["a", "    b", "    c"]), vec![0, 1, 1], "four");
    }

    /// The headline of the old bug: `tabs + spaces / 2` read four spaces as two
    /// levels, and ate a lone space entirely.
    #[test]
    fn a_four_space_indent_is_one_level_not_two() {
        let lines = vec!["parent".to_owned(), "    child".to_owned()];
        assert_eq!(indents_of(&lines), vec![(0, "parent"), (1, "child")]);

        // And a single space is a level, not a deleted character.
        let lines = vec!["parent".to_owned(), " child".to_owned()];
        assert_eq!(indents_of(&lines), vec![(0, "parent"), (1, "child")]);
    }

    /// A fragment lifted out of the middle of something lands where it is
    /// pasted rather than several levels in.
    #[test]
    fn an_indented_first_line_does_not_indent_the_block() {
        let lines = vec!["    a".to_owned(), "        b".to_owned()];
        assert_eq!(indents_of(&lines), vec![(0, "a"), (1, "b")]);
    }

    /// Ragged input is never refused; it closes to the nearest open level.
    #[test]
    fn a_ragged_dedent_lands_on_the_nearest_level() {
        let lines = vec!["a".to_owned(), "\t\tb".to_owned(), "\tc".to_owned()];
        // `b` cannot skip to depth 2 with nothing at depth 1, and `c` closes
        // back to the level `b` opened.
        assert_eq!(indents_of(&lines), vec![(0, "a"), (1, "b"), (1, "c")]);
    }

    /// A blank line says nothing about structure. Reading its zero width as an
    /// outdent would flatten everything after a paragraph break.
    #[test]
    fn a_blank_line_keeps_the_depth_of_the_one_above_it() {
        let lines = vec![
            "a".to_owned(),
            "\tb".to_owned(),
            String::new(),
            "\tc".to_owned(),
        ];
        assert_eq!(
            indents_of(&lines),
            vec![(0, "a"), (1, "b"), (1, ""), (1, "c")]
        );
    }

    #[test]
    fn a_trailing_newline_is_not_an_extra_row() {
        assert_eq!(split_lines("one\ntwo\n"), vec!["one", "two"]);
        assert_eq!(split_lines("one\r\ntwo"), vec!["one", "two"]);
        // A blank line *between* two others is real, though.
        assert_eq!(split_lines("one\n\ntwo"), vec!["one", "", "two"]);
        // And so is a single empty paste.
        assert_eq!(split_lines(""), vec![""]);
    }

    /// The property that actually matters: nesting copied out as text and read
    /// back in is the same nesting. This is also what makes `TAB_COLUMNS`
    /// irrelevant to Koru's own round trip — `render` writes one tab a level,
    /// so any tab width reads them back identically.
    #[test]
    fn indentation_survives_a_round_trip_through_text() {
        fixture();
        let (ws, mut e) = editor_over(&[
            ("parent", 0),
            ("child", 1),
            ("grandchild", 2),
            ("second child", 1),
        ]);
        select(&mut e, 0, 3);

        let payload = capture(&e, &ws).expect("something to copy");
        let text = render(&ws, &payload);
        let lines = split_lines(&text);

        assert_eq!(
            indents_of(&lines),
            vec![
                (0, "parent"),
                (1, "child"),
                (2, "grandchild"),
                (1, "second child"),
            ]
        );
    }

    /// What leaves for another application says what the level was. Every other
    /// editor understands `## Heading`, and dropping it is a real loss.
    #[test]
    fn a_heading_leaves_for_another_application_with_a_markdown_prefix() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("a title", 0), ("prose", 0)]);
        let id = e.body.rows[0].id;
        ws.set_heading(id, Some(koru_core::Heading::H2)).unwrap();
        e.body.refresh(&ws, id);
        select(&mut e, 0, 1);

        let payload = capture(&e, &ws).expect("something to copy");
        assert_eq!(render(&ws, &payload), "## a title\nprose");
    }

    /// Paste as plain text strips structure from the current payload rather
    /// than round-tripping through its Markdown export spelling. Otherwise an
    /// H2 would lose the heading but gain a literal `##`, which is not what the
    /// unformatted-paste gesture promises.
    #[test]
    fn paste_unformatted_drops_heading_and_inline_marks_without_adding_markdown() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("A title", 0), ("", 0)]);
        let source = e.body.rows[0].id;
        ws.set_heading(source, Some(koru_core::Heading::H2))
            .unwrap();
        ws.set_mark(source, 0..7, Mark::Bold, true).unwrap();
        e.body.refresh(&ws, source);
        e.body.focus = 0;
        assert!(copy(&mut e, &ws));

        e.body.focus = 1;
        e.place_caret(0);
        e.reflow();
        assert!(e.act(&mut ws, Action::PastePlain));

        let pasted = &e.body.rows[1];
        assert_eq!(pasted.text, "A title");
        assert_eq!(pasted.heading, None);
        assert!(
            !ws.marks_in(pasted.id, 0..pasted.text.len())
                .unwrap_or_default()
                .has(Mark::Bold),
            "the visible text arrived without the source mark"
        );
    }

    /// ...and it is export only. Nothing parses `#` on the way in, or the old
    /// `**bold**` model would be creeping back through the clipboard.
    #[test]
    fn pasting_a_line_that_starts_with_a_hash_leaves_the_hash_in_the_text() {
        fixture();
        let (mut ws, mut e) = editor_over(&[("", 0)]);
        paste_plain_text(&mut e, &mut ws, "## not a heading\nprose");
        e.reflow();

        assert_eq!(e.body.rows[0].text, "## not a heading");
        assert_eq!(e.body.rows[0].heading, None);
    }

    // ----------------------------------------------------------- cf_html out

    /// A `cf_html` helper that takes ready rows, so the writer is testable
    /// without a document.
    fn cf_html_of(rows: &[CopiedRow]) -> Option<String> {
        cf_html(
            &WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap(),
            &Payload::Rows(rows.to_vec(), None),
        )
    }

    /// Plain paragraphs wrap in `<p>` and escape the few characters HTML cares
    /// about.
    #[test]
    fn cf_html_wraps_plain_paragraphs() {
        let rows = vec![
            CopiedRow {
                runs: vec![plain_run("a < b".into())],
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("two".into())],
                ..empty()
            },
        ];
        let html = cf_html_of(&rows).expect("some");
        assert!(html.contains("<p>a &lt; b</p>"), "{html}");
        assert!(html.contains("<p>two</p>"), "{html}");
    }

    /// Bold and italic runs wrap in `<b>`/`<i>`.
    #[test]
    fn cf_html_wraps_inline_marks() {
        let mut run = plain_run("word".into());
        run.bold = true;
        run.italic = true;
        let rows = vec![CopiedRow {
            runs: vec![run],
            ..empty()
        }];
        let html = cf_html_of(&rows).expect("some");
        assert!(html.contains("<b><i>word</i></b>"), "{html}");
    }

    /// The CF_HTML header carries the four byte offsets the format requires.
    #[test]
    fn cf_html_header_carries_offsets() {
        let rows = vec![CopiedRow {
            runs: vec![plain_run("x".into())],
            ..empty()
        }];
        let html = cf_html_of(&rows).expect("some");
        assert!(html.starts_with("Version:0.9\r\nStartHTML:"), "{html}");
        assert!(html.contains("StartFragment:"), "{html}");
        assert!(html.contains("EndFragment:"), "{html}");
        assert!(html.contains("<!--StartFragment-->"), "{html}");
        assert!(html.contains("<!--EndFragment-->"), "{html}");
    }

    /// A row-shaped `CopiedRow` with nothing set but the runs, for the writer
    /// tests above.
    fn empty() -> CopiedRow {
        CopiedRow {
            runs: Vec::new(),
            attrs: Vec::new(),
            node_attrs: Vec::new(),
            kind: Kind::Task,
            done: false,
            level_name: None,
            callout: None,
            align: None,
            heading: None,
            table_layout: None,
            language: None,
            depth: 0,
            image: None,
            list: None,
        }
    }

    /// A callout box leaves whole and lands whole: the lines inside it carry
    /// the box's look, the list inside it its list, and the paste rebuilds
    /// the container around them — in a flat body too, because a box is
    /// structure like a list's, not the outline nesting the body flattens.
    /// The text and HTML flavours carry the box as well, its icon leading
    /// the first line.
    #[test]
    fn copy_paste_keeps_a_callout_box_and_the_list_inside_it() {
        let mut src = WorkspaceDoc::create(WorkspaceId::new(), "src").unwrap();
        let doc_a = src
            .insert(Parent::Root, Node::document("A").unwrap())
            .unwrap();
        let callout = src
            .insert(
                Parent::Node(doc_a),
                Node::callout(Some("⚠️"), Some("red")).unwrap(),
            )
            .unwrap();
        src.insert(Parent::Node(callout), Node::paragraph("first").unwrap())
            .unwrap();
        let list = src
            .insert(Parent::Node(callout), Node::list_block().unwrap())
            .unwrap();
        src.set_list_style(list, Some(ListStyle::Bullet)).unwrap();
        src.insert(Parent::Node(list), Node::paragraph("item").unwrap())
            .unwrap();
        src.insert(Parent::Node(doc_a), Node::paragraph("after").unwrap())
            .unwrap();

        let mut e_src = Editor::rooted_at(&src, doc_a, true);
        e_src.body.rebuild(&src);
        for row in e_src.body.rows.iter_mut().filter(|r| !r.is_title) {
            row.selected = true;
        }
        let payload = capture(&e_src, &src).expect("copy the box");
        let Payload::Rows(rows, source) = payload.clone() else {
            panic!("expected whole rows");
        };
        let spec = CopiedCalloutSpec {
            icon: Some("⚠️".into()),
            color: Some("red".into()),
            depth: 0,
        };
        assert_eq!(
            rows.iter()
                .map(|r| (r.callout.clone(), r.list.map(|l| l.style)))
                .collect::<Vec<_>>(),
            vec![
                (Some(spec.clone()), None),
                (Some(spec), Some(ListStyle::Bullet)),
                (None, None),
            ],
            "every row inside carries the box: {rows:?}"
        );
        assert_eq!(render(&src, &payload), "> ⚠️ first\n> - item\nafter");
        let html = cf_html(&src, &payload).unwrap();
        assert!(
            html.contains("<aside><p>⚠️ first</p><ul><li>item</li></ul></aside><p>after</p>"),
            "{html}"
        );

        let mut dst = WorkspaceDoc::create(WorkspaceId::new(), "dst").unwrap();
        let doc_b = dst
            .insert(Parent::Root, Node::document("B").unwrap())
            .unwrap();
        let landing = dst
            .insert(Parent::Node(doc_b), Node::paragraph("").unwrap())
            .unwrap();
        let mut e_dst = Editor::rooted_at(&dst, doc_b, true);
        e_dst.view.quotes_enabled = true;
        e_dst.body.rebuild(&dst);
        e_dst.body.focus_on(landing);
        e_dst.place_caret(0);
        assert!(
            paste_rows(&mut e_dst, &mut dst, &rows, source),
            "paste into the other document"
        );

        let first = e_dst
            .body
            .rows
            .iter()
            .find(|r| r.text == "first")
            .expect("the first line")
            .id;
        let Parent::Node(pasted) = dst.parent(first).unwrap() else {
            panic!("the line is in a box");
        };
        let node = dst.read(pasted).unwrap();
        assert_eq!(node.kind, Kind::Callout);
        assert_eq!(node.meta.icon.as_deref(), Some("⚠️"));
        assert_eq!(node.meta.colour.as_deref(), Some("red"));
        assert_eq!(dst.parent(pasted).unwrap(), Parent::Node(doc_b));
        let rows: Vec<(String, Option<NodeId>, Option<ListStyle>)> = e_dst
            .body
            .rows
            .iter()
            .filter(|r| !r.is_title)
            .map(|r| (r.text.clone(), r.callout, r.list_style))
            .collect();
        assert_eq!(
            rows,
            vec![
                (String::new(), None, None),
                ("first".into(), Some(pasted), None),
                ("item".into(), Some(pasted), Some(ListStyle::Bullet)),
                ("after".into(), None, None),
            ],
            "the blank landing row stays, since a boxed line cannot fill it"
        );
        let children = dst.children(Parent::Node(pasted)).unwrap();
        assert_eq!(
            children.len(),
            2,
            "the line and the list block: {children:?}"
        );
        assert_eq!(children[0], first);
        assert_eq!(dst.read(children[1]).unwrap().kind, Kind::ListBlock);
        let found = e_dst.body.callout_box(pasted).expect("the box draws");
        assert_eq!(found.color.as_deref(), Some("red"));
        let after = e_dst
            .body
            .rows
            .iter()
            .find(|r| r.text == "after")
            .unwrap()
            .id;
        assert_eq!(
            dst.parent(after).unwrap(),
            Parent::Node(doc_b),
            "the row after the box is outside it"
        );
    }

    /// Nested list structure on the internal clipboard must rebuild ListBlocks
    /// when pasted into another document — not plain paragraphs.
    #[test]
    fn copy_paste_rebuilds_nested_lists_across_documents() {
        let mut src = WorkspaceDoc::create(WorkspaceId::new(), "src").unwrap();
        let doc_a = src
            .insert(Parent::Root, Node::document("A").unwrap())
            .unwrap();
        let outer = src
            .insert(Parent::Node(doc_a), Node::list_block().unwrap())
            .unwrap();
        src.set_list_style(outer, Some(ListStyle::Numbered))
            .unwrap();
        let one = src
            .insert(Parent::Node(outer), Node::paragraph("one").unwrap())
            .unwrap();
        let nested_list = src
            .insert(Parent::Node(one), Node::list_block().unwrap())
            .unwrap();
        src.set_list_style(nested_list, Some(ListStyle::Bullet))
            .unwrap();
        let nested = src
            .insert(
                Parent::Node(nested_list),
                Node::paragraph("nested").unwrap(),
            )
            .unwrap();
        let two = src
            .insert(Parent::Node(outer), Node::paragraph("two").unwrap())
            .unwrap();
        let _ = (nested, two);

        let mut e_src = Editor::rooted_at(&src, doc_a, true);
        e_src.body.rebuild(&src);
        // Select every content row (skip title).
        for row in e_src.body.rows.iter_mut().filter(|r| !r.is_title) {
            row.selected = true;
        }
        // Capture without the process-global clipboard so parallel tests cannot
        // overwrite the payload mid-flight.
        let Payload::Rows(rows, source) = capture(&e_src, &src).expect("copy the list") else {
            panic!("expected whole rows");
        };
        assert!(
            rows.iter().any(|r| r.list.is_some()),
            "list specs must be on the payload: {rows:?}"
        );

        let mut dst = WorkspaceDoc::create(WorkspaceId::new(), "dst").unwrap();
        let doc_b = dst
            .insert(Parent::Root, Node::document("B").unwrap())
            .unwrap();
        let landing = dst
            .insert(Parent::Node(doc_b), Node::paragraph("").unwrap())
            .unwrap();
        let mut e_dst = Editor::rooted_at(&dst, doc_b, true);
        e_dst.body.rebuild(&dst);
        e_dst.body.focus_on(landing);
        e_dst.place_caret(0);
        assert!(
            paste_rows(&mut e_dst, &mut dst, &rows, source),
            "paste into the other document"
        );

        let rows: Vec<_> = e_dst
            .body
            .rows
            .iter()
            .filter(|r| !r.is_title)
            .map(|r| (r.text.as_str(), r.list_style, r.depth))
            .collect();
        assert!(
            rows.iter().any(|(t, s, _)| *t == "one" && s.is_some()),
            "outer list item kept a list style: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|(t, s, d)| *t == "nested" && *s == Some(ListStyle::Bullet) && *d > 0),
            "nested bullet kept style and depth: {rows:?}"
        );
        assert!(
            rows.iter().any(|(t, s, _)| *t == "two" && s.is_some()),
            "second outer item kept a list style: {rows:?}"
        );
        // No plain-paragraph-only paste of the three lines.
        let plain_only = rows
            .iter()
            .filter(|(t, s, _)| matches!(*t, "one" | "nested" | "two") && s.is_none())
            .count();
        assert_eq!(
            plain_only, 0,
            "list items must not land as bare prose: {rows:?}"
        );
    }

    #[test]
    fn cf_html_emits_list_tags_for_list_items() {
        let rows = vec![
            CopiedRow {
                runs: vec![plain_run("Move block".into())],
                list: Some(CopiedListSpec {
                    style: ListStyle::Bullet,
                    depth: 0,
                }),
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("Features".into())],
                list: Some(CopiedListSpec {
                    style: ListStyle::Bullet,
                    depth: 0,
                }),
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("Working now".into())],
                list: Some(CopiedListSpec {
                    style: ListStyle::Bullet,
                    depth: 1,
                }),
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("Planned".into())],
                list: Some(CopiedListSpec {
                    style: ListStyle::Bullet,
                    depth: 1,
                }),
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("Later".into())],
                list: Some(CopiedListSpec {
                    style: ListStyle::Bullet,
                    depth: 1,
                }),
                ..empty()
            },
        ];
        let html = cf_html_of(&rows).expect("some");
        assert!(
            html.contains(concat!(
                "<ul><li>Move block</li><li>Features<ul>",
                "<li>Working now</li><li>Planned</li><li>Later</li>",
                "</ul></li></ul>"
            )),
            "nested list must be valid HTML so clipboard consumers keep its depth: {html}"
        );
    }

    #[test]
    fn clipboard_text_comparison_accepts_windows_newlines() {
        assert!(clipboard_text_matches("one\ntwo", "one\r\ntwo"));
        assert!(!clipboard_text_matches("one\ntwo", "one\r\nother"));
        assert!(!clipboard_text_matches("", ""));
    }

    #[test]
    fn cf_html_emits_image_placeholders() {
        let rows = vec![
            CopiedRow {
                runs: vec![plain_run("before".into())],
                ..empty()
            },
            CopiedRow {
                kind: Kind::Image,
                image: Some(CopiedImage {
                    hash: "fnv1a:abc".into(),
                    mime: "image/png".into(),
                    width: Some(10),
                    height: Some(20),
                    name: Some("shot".into()),
                }),
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("after".into())],
                ..empty()
            },
        ];
        let html = cf_html_of(&rows).expect("some");
        assert!(html.contains("<p>before</p>"), "{html}");
        assert!(
            html.contains(r#"<p><img data-koru-hash="fnv1a:abc""#),
            "{html}"
        );
        assert!(html.contains(r#"data-mime="image/png""#), "{html}");
        assert!(html.contains(r#"width="10""#), "{html}");
        assert!(html.contains("<p>after</p>"), "{html}");
    }

    /// Trailing image must end the fragment on `</p>` so OneNote accepts CF_HTML.
    #[test]
    fn cf_html_trailing_image_is_block_wrapped() {
        let rows = vec![
            CopiedRow {
                runs: vec![plain_run("text".into())],
                ..empty()
            },
            CopiedRow {
                kind: Kind::Image,
                image: Some(CopiedImage {
                    hash: "h".into(),
                    mime: "image/png".into(),
                    width: None,
                    height: None,
                    name: None,
                }),
                ..empty()
            },
        ];
        let html = cf_html_of(&rows).expect("some");
        let frag_end = html.find("<!--EndFragment-->").expect("fragment end");
        let before = html[..frag_end].trim_end();
        assert!(
            before.ends_with("</p>"),
            "fragment must end on a closed block: …{}",
            &before[before.len().saturating_sub(40)..]
        );
    }

    #[test]
    fn paste_rows_restores_copied_images() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("D").unwrap())
            .unwrap();
        let p = ws
            .insert(Parent::Node(doc), Node::paragraph("hi").unwrap())
            .unwrap();
        let mut e = Editor::rooted_at(&ws, doc, true);
        e.body.focus_on(p);
        let rows = vec![
            CopiedRow {
                runs: vec![plain_run("a".into())],
                kind: Kind::Paragraph,
                ..empty()
            },
            CopiedRow {
                kind: Kind::Image,
                image: Some(CopiedImage {
                    hash: "h1".into(),
                    mime: "image/png".into(),
                    width: Some(1),
                    height: Some(1),
                    name: None,
                }),
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("b".into())],
                kind: Kind::Paragraph,
                ..empty()
            },
        ];
        assert!(paste_rows(&mut e, &mut ws, &rows, None));
        let kinds: Vec<_> = e.body.rows.iter().map(|r| r.kind).collect();
        assert!(
            kinds.iter().any(|k| *k == Kind::Image),
            "image row restored: {kinds:?}"
        );
        let imgs: Vec<_> = e
            .body
            .rows
            .iter()
            .filter_map(|r| r.image.as_ref().map(|i| i.hash.0.as_str()))
            .collect();
        assert_eq!(imgs, ["h1"]);
    }

    // ------------------------------------------- a block's own attributes
    //
    // The four R0 attributes hang off the *node*, with no anchor in the text —
    // which is what kept them out of `attrs`, the anchor-keyed list a pill
    // paste reads. They ride in `node_attrs` instead and are re-applied by
    // `apply_node_attrs`.

    /// Give the view a tag table the way the host does, and answer the ids.
    fn seed_tags(e: &mut Editor, names: &[&str]) -> Vec<TagId> {
        let ids: Vec<TagId> = names.iter().map(|_| TagId::new()).collect();
        e.view_mut()
            .tags
            .replace(ids.iter().zip(names).map(|(id, name)| {
                (
                    *id,
                    crate::chips::TagFace {
                        name: (*name).to_owned(),
                        color: None,
                    },
                )
            }));
        ids
    }

    /// All four of them on one node: a tag, a color, a due day and a priority.
    fn set_the_four(ws: &mut WorkspaceDoc, id: NodeId, tag: TagId) {
        ws.add_tag(id, tag).unwrap();
        ws.set_attribute(id, KEY_COLOR, AttrValue::Text("teal".into()))
            .unwrap();
        ws.set_attribute(
            id,
            KEY_DUE,
            AttrValue::Date(DateOnly::parse("2026-08-30").unwrap()),
        )
        .unwrap();
        ws.set_attribute(id, KEY_PRIORITY, AttrValue::Text("high".into()))
            .unwrap();
    }

    /// What a node carries, as the four answers a test compares.
    type Four = (Vec<TagId>, Option<String>, Option<String>, Option<String>);

    fn the_four(ws: &WorkspaceDoc, id: NodeId) -> Four {
        let node = ws.read(id).unwrap();
        let of = |key: &str| {
            node.content
                .attrs()
                .iter()
                .find(|a| a.role == key)
                .and_then(|a| match &a.value {
                    AttrValue::Text(t) => Some(t.clone()),
                    AttrValue::Date(d) => Some(d.to_storage()),
                    _ => None,
                })
        };
        (
            node.tag_ids().collect(),
            of(KEY_COLOR),
            of(KEY_DUE),
            of(KEY_PRIORITY),
        )
    }

    /// The headline: a copied block arrives with its due date, its tag, its
    /// color and its priority — the same four a *duplicate* keeps.
    #[test]
    fn copying_a_block_carries_its_four_attributes() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("plan", 0), ("other", 0)]);
        let tag = seed_tags(&mut e, &["home"])[0];
        let source = e.body.rows[0].id;
        set_the_four(&mut ws, source, tag);
        e.body.refresh(&ws, source);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));

        let pasted = e.body.rows[1].id;
        assert_ne!(
            pasted, source,
            "a copy, not a second name for the same node"
        );
        assert_eq!(
            the_four(&ws, pasted),
            (
                vec![tag],
                Some("teal".to_owned()),
                Some("2026-08-30".to_owned()),
                Some("high".to_owned())
            )
        );
    }

    /// Cut is copy plus a delete, so the attributes have to travel with the
    /// block that was taken away — otherwise Ctrl+X, Ctrl+V loses them.
    #[test]
    fn cutting_a_block_carries_its_attributes_too() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("plan", 0), ("keep", 0)]);
        let tag = seed_tags(&mut e, &["home"])[0];
        let source = e.body.rows[0].id;
        set_the_four(&mut ws, source, tag);
        e.body.refresh(&ws, source);
        e.body.focus = 0;

        assert!(cut(&mut e, &mut ws));
        assert_eq!(shape(&e), vec![("keep".into(), 0)], "the block went");
        e.body.focus = 0;
        assert!(paste(&mut e, &mut ws));

        let pasted = e.body.rows.iter().find(|r| r.text == "plan").unwrap().id;
        assert_eq!(
            the_four(&ws, pasted),
            (
                vec![tag],
                Some("teal".to_owned()),
                Some("2026-08-30".to_owned()),
                Some("high".to_owned())
            )
        );
    }

    /// A selection is a set of blocks, each with its own attributes — they must
    /// not be pooled, nor taken from the first row.
    #[test]
    fn each_row_of_a_selection_keeps_its_own_attributes() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("one", 0), ("two", 0)]);
        let tags = seed_tags(&mut e, &["home", "work"]);
        let (first, second) = (e.body.rows[0].id, e.body.rows[1].id);
        ws.add_tag(first, tags[0]).unwrap();
        ws.set_attribute(first, KEY_PRIORITY, AttrValue::Text("urgent".into()))
            .unwrap();
        ws.add_tag(second, tags[1]).unwrap();
        ws.set_attribute(second, KEY_COLOR, AttrValue::Text("red".into()))
            .unwrap();
        e.body.rebuild(&ws);
        select(&mut e, 0, 1);

        assert!(copy(&mut e, &ws));
        e.body.clear_selection();
        e.body.focus = 1;
        assert!(paste(&mut e, &mut ws));

        let (tags_a, color_a, _, priority_a) = the_four(&ws, e.body.rows[2].id);
        let (tags_b, color_b, _, priority_b) = the_four(&ws, e.body.rows[3].id);
        assert_eq!(tags_a, vec![tags[0]]);
        assert_eq!(priority_a.as_deref(), Some("urgent"));
        assert_eq!(color_a, None, "the first row had no color to give");
        assert_eq!(tags_b, vec![tags[1]]);
        assert_eq!(color_b.as_deref(), Some("red"));
        assert_eq!(priority_b, None);
    }

    /// A code block holds plain text: pasting into one already drops marks and
    /// pills, and a due date on a line of source is the same mistake.
    #[test]
    fn pasting_into_a_code_block_carries_no_attributes() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("plan", 0)]);
        let tag = seed_tags(&mut e, &["home"])[0];
        let source = e.body.rows[0].id;
        set_the_four(&mut ws, source, tag);
        e.body.refresh(&ws, source);
        e.body.focus = 0;
        assert!(copy(&mut e, &ws));

        let (mut ws2, mut e2) = editor_over(&[("", 0)]);
        let code = e2.body.rows[0].id;
        ws2.set_kind(code, Kind::Code).unwrap();
        e2.body.rebuild(&ws2);
        e2.body.focus = 0;
        e2.place_caret(0);
        e2.reflow();

        assert!(paste(&mut e2, &mut ws2));
        assert_eq!(ws2.read(code).unwrap().content.as_text(), Some("plan"));
        assert_eq!(
            the_four(&ws2, code),
            (Vec::new(), None, None, None),
            "the text arrived and nothing else did"
        );
        assert!(
            e2.view().wants_create_tag.is_empty(),
            "and no tag was asked for either"
        );
    }

    /// Text from another application is text. It carries no attributes, and a
    /// paste of it must not invent any.
    #[test]
    fn external_text_carries_no_attributes() {
        let (mut ws, mut e) = editor_over(&[("", 0)]);
        e.body.focus = 0;
        assert!(paste_plain_text(&mut e, &mut ws, "one\ntwo"));
        let ids: Vec<NodeId> = e.body.rows.iter().map(|r| r.id).collect();
        for id in ids {
            assert_eq!(the_four(&ws, id), (Vec::new(), None, None, None));
        }
        assert!(e.view().wants_create_tag.is_empty());
    }

    /// The paste and everything it brought are one undo step. `set_attribute`
    /// checkpoints and `set_attribute_on_all` does not, which is the whole
    /// reason the paste uses the second — a per-attribute checkpoint would make
    /// one Ctrl+V take five Ctrl+Zs.
    #[test]
    fn one_undo_takes_back_the_paste_and_its_attributes() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("plan", 0)]);
        let tag = seed_tags(&mut e, &["home"])[0];
        let source = e.body.rows[0].id;
        set_the_four(&mut ws, source, tag);
        e.body.refresh(&ws, source);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));
        assert_eq!(e.body.rows.len(), 2);

        assert!(ws.undo().unwrap());
        e.body.rebuild(&ws);
        assert_eq!(
            shape(&e),
            vec![("plan".into(), 0)],
            "one Ctrl+Z took the pasted block back"
        );
        assert_eq!(
            the_four(&ws, source),
            (
                vec![tag],
                Some("teal".to_owned()),
                Some("2026-08-30".to_owned()),
                Some("high".to_owned())
            ),
            "and left the original's own attributes alone"
        );
    }

    /// Across workspaces a `TagId` names nothing, so the name decides: a
    /// destination that already has that tag — under any spelling the fold key
    /// calls the same — reuses its own record rather than minting a second.
    #[test]
    fn a_tag_pasted_into_another_workspace_reuses_the_matching_one() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("plan", 0)]);
        let theirs = seed_tags(&mut e, &["Home"])[0];
        let source = e.body.rows[0].id;
        ws.add_tag(source, theirs).unwrap();
        e.body.refresh(&ws, source);
        e.body.focus = 0;
        assert!(copy(&mut e, &ws));

        // Another workspace, another table: the same name, a different id.
        let (mut ws2, mut e2) = editor_over(&[("here", 0)]);
        let ours = seed_tags(&mut e2, &["home"])[0];
        e2.body.focus = 0;
        assert!(paste(&mut e2, &mut ws2));

        let pasted = e2.body.rows[1].id;
        assert_eq!(
            ws2.read(pasted).unwrap().tag_ids().collect::<Vec<_>>(),
            vec![ours],
            "the destination's own record, not the one it was copied with"
        );
        assert!(
            e2.view().wants_create_tag.is_empty(),
            "nothing had to be created"
        );
    }

    /// A tag the destination has never seen has to be **made** there, and only
    /// the host can: the table lives in the workspace system document. The
    /// paste stands either way — an unanswered request drops the tag.
    #[test]
    fn a_tag_new_to_the_destination_is_asked_of_the_host_by_name() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("plan", 0)]);
        let theirs = seed_tags(&mut e, &["Fun/Home"])[0];
        let source = e.body.rows[0].id;
        ws.add_tag(source, theirs).unwrap();
        e.body.refresh(&ws, source);
        e.body.focus = 0;
        assert!(copy(&mut e, &ws));

        let (mut ws2, mut e2) = editor_over(&[("here", 0)]);
        seed_tags(&mut e2, &["work"]);
        e2.body.focus = 0;
        assert!(paste(&mut e2, &mut ws2));

        let pasted = e2.body.rows[1].id;
        assert_eq!(
            e2.view_mut().wants_create_tag.take(),
            Some((vec![pasted], "Fun/Home".to_owned())),
            "the name travelled, and the pasted node is waiting for the id"
        );
        assert_eq!(
            ws2.read(pasted).unwrap().content.as_text(),
            Some("plan"),
            "and the block itself is already there"
        );
    }

    /// Two rows wanting the same new tag are one request, not two — the host
    /// creates once and applies to both.
    #[test]
    fn rows_wanting_the_same_new_tag_ask_once() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("one", 0), ("two", 0)]);
        let theirs = seed_tags(&mut e, &["home"])[0];
        for i in 0..2 {
            let id = e.body.rows[i].id;
            ws.add_tag(id, theirs).unwrap();
        }
        e.body.rebuild(&ws);
        select(&mut e, 0, 1);
        assert!(copy(&mut e, &ws));

        let (mut ws2, mut e2) = editor_over(&[("here", 0)]);
        e2.body.clear_selection();
        e2.body.focus = 0;
        assert!(paste(&mut e2, &mut ws2));

        let waiting = e2.view_mut().wants_create_tag.take().expect("one request");
        assert_eq!(waiting.1, "home");
        assert_eq!(waiting.0.len(), 2, "both pasted rows are waiting on it");
        assert!(
            e2.view_mut().wants_create_tag.take().is_none(),
            "and there is no second request"
        );
    }

    /// A pill's attribute stays the pill's. It is anchored in the text, so it
    /// travels in `attrs` and is re-created with its anchor — never set on the
    /// node as though the block itself were due on Friday.
    #[test]
    fn a_pill_attribute_does_not_become_a_block_attribute() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("due", 0)]);
        let id = e.body.rows[0].id;
        let attr = Attr::new("due", AttrValue::Text("Friday".into()));
        ws.insert_pill(id, 3, &attr).unwrap();
        e.body.refresh(&ws, id);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        let Taken::Rows(rows, _) = take() else {
            panic!("rows on the clipboard")
        };
        assert_eq!(rows[0].attrs.len(), 1, "the pill's attribute is anchored");
        assert!(
            rows[0].node_attrs.is_empty(),
            "and the block carries nothing of its own"
        );
    }

    // ------------------------------------------ a custom attribute (Q35)
    //
    // A key the *workspace* defined, not this build. Its definition lives in
    // the workspace system document, so it means something on one side of a
    // workspace boundary and nothing on the other — which is the whole rule.

    /// `Effort: <value>`, set the way a definition-aware caller sets one.
    fn set_effort(ws: &mut WorkspaceDoc, id: NodeId, value: &str) {
        let def = koru_core::attributes::AttributeDefinition::custom(
            "Effort",
            "Effort",
            koru_core::attributes::ValueType::Text,
        );
        ws.set_attribute_defined(id, &def, AttrValue::Text(value.into()))
            .unwrap();
    }

    fn effort(ws: &WorkspaceDoc, id: NodeId) -> Option<String> {
        ws.read(id)
            .unwrap()
            .content
            .attrs()
            .iter()
            .find(|a| a.role == "Effort")
            .and_then(|a| a.as_text().map(str::to_owned))
    }

    // ------------------------------------------------- task state (SPEC §6)
    //
    // `plant` and `duplicate` already carried a task whole, because they copy
    // the node's `Content` and `NodeMeta` verbatim; the clipboard path did not,
    // and lost the tick, the marker and the lifecycle on the way across.

    /// A pasted task keeps its **marker, lifecycle, due and tick**, and is an
    /// independent task rather than an alias.
    #[test]
    fn a_pasted_task_keeps_its_state_and_is_its_own_task() {
        let _g = lock();
        fixture();
        let (mut ws, mut e) = editor_over(&[("post the form", 0), ("elsewhere", 0)]);
        let source = e.body.rows[0].id;
        let due = DateOnly::parse("2026-09-04").unwrap();
        koru_core::tasks::set_due(&mut ws, source, koru_core::time::Temporal::Date(due)).unwrap();
        koru_core::tasks::complete_task(
            &mut ws,
            source,
            koru_core::tasks::LifecycleReason::ExplicitComplete,
            crate::tasks::today(),
        )
        .unwrap();
        e.body.refresh(&ws, source);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        e.body.focus = 1;
        assert!(paste(&mut e, &mut ws));

        let pasted = e.body.rows[2].id;
        assert_ne!(pasted, source, "a fresh identity, never an alias");
        let node = ws.read(pasted).unwrap();
        let fields = koru_core::tasks::TaskFields::of(&node);
        assert_eq!(fields.marker, Some(TaskReason::Due), "the marker travelled");
        assert_eq!(
            fields.lifecycle,
            Lifecycle::Completed,
            "and the canonical lifecycle"
        );
        assert_eq!(fields.due, Some(koru_core::time::Temporal::Date(due)));
        assert!(node.meta.done, "and the tick");
        assert!(
            koru_core::tasks::is_task_node(&ws, pasted).unwrap(),
            "it is a task where it landed"
        );
    }

    /// **A checked checklist item keeps its tick.** It did not: `apply_meta`
    /// wrote `done` only for `Kind::Task`, so the one thing a checkbox row
    /// carries was the one thing a paste dropped. The box is content
    /// (ADR-272, amended) and travels whatever the destination kind is.
    #[test]
    fn a_checked_row_keeps_its_tick_without_becoming_a_task() {
        let _g = lock();
        fixture();
        let (mut ws, mut e) = editor_over(&[("milk", 0), ("elsewhere", 0)]);
        let source = e.body.rows[0].id;
        ws.set_checkbox(source, true).unwrap();
        e.body.refresh(&ws, source);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        e.body.focus = 1;
        assert!(paste(&mut e, &mut ws));

        let pasted = e.body.rows[2].id;
        assert!(ws.read(pasted).unwrap().meta.done, "the tick travelled");
        assert!(
            !koru_core::tasks::is_task_node(&ws, pasted).unwrap(),
            "and a plain checklist item gained no task"
        );
    }

    /// **The subtree comes back as it was.** A completed parent whose child was
    /// left open stays that way: pasting is not a completion, so nothing
    /// cascades down the copy.
    #[test]
    fn a_pasted_subtree_keeps_the_state_each_row_had() {
        let _g = lock();
        fixture();
        let (mut ws, mut e) = editor_over(&[("ship it", 0), ("draft", 1), ("elsewhere", 0)]);
        let parent = e.body.rows[0].id;
        let child = e.body.rows[1].id;
        koru_core::tasks::ensure_task(&mut ws, child, TaskReason::Explicit).unwrap();
        koru_core::tasks::ensure_task(&mut ws, parent, TaskReason::Explicit).unwrap();
        // A completed parent over an open child — the residue state SPEC §5
        // allows and a cascade would quietly repair. Written through the
        // restore door rather than by completing the parent, because
        // `complete_task` would take the child with it, which is the very thing
        // this asserts a paste does not do.
        faithful_import::restore_task(
            &mut ws,
            parent,
            &koru_core::tasks::TaskFields {
                marker: Some(TaskReason::Explicit),
                due: None,
                lifecycle: Lifecycle::Completed,
                done: false,
                completed_at: None,
                pinned_today: false,
            },
        )
        .unwrap();
        e.body.rebuild(&ws);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        e.body.focus = 2;
        assert!(paste(&mut e, &mut ws));

        let pasted_parent = e.body.rows[3].id;
        let pasted_child = e.body.rows[4].id;
        assert_eq!(
            koru_core::tasks::TaskFields::of(&ws.read(pasted_parent).unwrap()).lifecycle,
            Lifecycle::Completed
        );
        assert_eq!(
            koru_core::tasks::TaskFields::of(&ws.read(pasted_child).unwrap()).lifecycle,
            Lifecycle::Open,
            "the child came back open, exactly as it was copied"
        );
    }

    /// **One Ctrl+Z still takes the whole paste back**, task state included:
    /// the carried fields join the paste's open group rather than opening one
    /// of their own.
    #[test]
    fn a_paste_carrying_task_state_is_one_undo_step() {
        let _g = lock();
        fixture();
        let (mut ws, mut e) = editor_over(&[("post the form", 0), ("elsewhere", 0)]);
        let source = e.body.rows[0].id;
        koru_core::tasks::ensure_task(&mut ws, source, TaskReason::Explicit).unwrap();
        e.body.refresh(&ws, source);
        e.body.focus = 0;
        assert!(copy(&mut e, &ws));
        e.body.focus = 1;
        assert!(paste(&mut e, &mut ws));
        assert_eq!(e.body.rows.len(), 3);

        assert!(ws.undo().unwrap());
        e.body.rebuild(&ws);
        assert_eq!(
            shape(&e),
            vec![("post the form".into(), 0), ("elsewhere".into(), 0)],
            "one Ctrl+Z took the pasted task and its state together"
        );
    }

    /// A kind that **cannot carry task state** gets none: SPEC §4's exclusion is
    /// enforced at the paste rather than hidden afterwards.
    #[test]
    fn task_state_does_not_land_on_a_kind_that_cannot_carry_it() {
        let _g = lock();
        fixture();
        let (mut ws, mut e) = editor_over(&[("post the form", 0)]);
        let source = e.body.rows[0].id;
        koru_core::tasks::ensure_task(&mut ws, source, TaskReason::Explicit).unwrap();
        e.body.refresh(&ws, source);
        e.body.focus = 0;
        assert!(copy(&mut e, &ws));

        let Taken::Rows(rows, _) = take() else {
            panic!("rows on the clipboard")
        };
        let mut target = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let quote = target
            .insert(Parent::Root, Node::quote("quoted").unwrap())
            .unwrap();
        apply_node_attrs(
            &mut target,
            quote,
            &rows[0],
            Kind::Quote,
            &[],
            true,
            &mut Vec::new(),
        );
        assert!(
            !koru_core::tasks::is_task_node(&target, quote).unwrap(),
            "a quote is not task-capable, and the marker did not land"
        );
    }

    // ------------------------------------------- the paste reopen matrix
    //
    // **Paste is a user command, not a restore** (ADR-272): the carried fields
    // come back verbatim through `faithful_import`, and the *gesture* still
    // classifies. So pasting unfinished work into a completed task reopens it,
    // reason `UNFINISHED_CHILD_ADDED`, exactly as inserting or moving one does.

    /// A completed task holding one note, and something loose to copy. The
    /// completion is written through the restore door because `complete_task`
    /// cascades, and these tests need a completed parent over content it did
    /// not tick.
    fn a_completed_task_holding_a_note() -> (WorkspaceDoc, Editor, NodeId) {
        let (mut ws, mut e) = editor_over(&[("ship it", 0), ("a note", 1), ("loose", 0)]);
        let parent = e.body.rows[0].id;
        koru_core::tasks::ensure_task(&mut ws, parent, TaskReason::Explicit).unwrap();
        faithful_import::restore_task(
            &mut ws,
            parent,
            &TaskFields {
                marker: Some(TaskReason::Explicit),
                due: None,
                lifecycle: Lifecycle::Completed,
                done: false,
                completed_at: None,
                pinned_today: false,
            },
        )
        .unwrap();
        e.body.rebuild(&ws);
        (ws, e, parent)
    }

    /// **A paste into a task's own text is one editing burst** (ADR-273).
    ///
    /// `paste_inline` is the third door text reaches a row through — beside
    /// typing and deleting — and the one that writes a whole clipboard in a
    /// single gesture, which is why it notes once rather than per run.
    /// `paste_rows` deliberately notes nothing: it makes nodes rather than
    /// editing one, and a new node's event is a creation the commands answer
    /// for.
    #[test]
    fn pasting_into_a_task_notes_its_text_changed() {
        let (mut ws, mut e, parent) = a_completed_task_holding_a_note();
        e.body.focus = 0;
        e.body.take_task_text_edits();

        assert!(paste_plain_text(&mut e, &mut ws, "urgently "));

        let noted = e.body.take_task_text_edits();
        assert_eq!(noted.len(), 1, "one paste, one entry");
        assert_eq!(noted[0].node_id(), parent);
    }

    fn lifecycle_of(ws: &WorkspaceDoc, node: NodeId) -> Lifecycle {
        TaskFields::of(&ws.read(node).unwrap()).lifecycle
    }

    /// Copy row 2 and paste it below row 1 — the note *inside* the completed
    /// task, so the paste lands as its sibling and therefore inside the task.
    fn copy_the_loose_row_into_the_task(e: &mut Editor, ws: &mut WorkspaceDoc) {
        e.body.focus = 2;
        assert!(copy(e, ws));
        e.body.focus = 1;
        e.body.take_task_activity();
        assert!(paste(e, ws));
    }

    /// **Pasting an open task into a completed one reopens it**, and owes the
    /// activity — the reopen and the `task.child.added` that explains it.
    #[test]
    fn pasting_an_unfinished_child_into_a_completed_task_reopens_it() {
        use koru_core::tasks::{ActivityIntent, ChildClass, LifecycleReason};
        let _g = lock();
        fixture();
        let (mut ws, mut e, parent) = a_completed_task_holding_a_note();
        let loose = e.body.rows[2].id;
        koru_core::tasks::ensure_task(&mut ws, loose, TaskReason::Explicit).unwrap();
        e.body.refresh(&ws, loose);

        copy_the_loose_row_into_the_task(&mut e, &mut ws);

        assert_eq!(
            lifecycle_of(&ws, parent),
            Lifecycle::Open,
            "the destination task reopened (SPEC §5)"
        );
        let owed = e.body.take_task_activity();
        assert_eq!(owed.len(), 1, "one paste, one causation id");
        assert!(
            owed[0].intents.iter().any(|i| matches!(
                i,
                ActivityIntent::Reopened {
                    reason: LifecycleReason::UnfinishedChildAdded,
                    ..
                }
            )),
            "and says why: {:?}",
            owed[0].intents
        );
        assert!(
            owed[0].intents.iter().any(|i| matches!(
                i,
                ActivityIntent::ChildAdded {
                    class: ChildClass::UnfinishedStructural,
                    reopened: true,
                    ..
                }
            )),
            "told as an add — a paste is not a move: {:?}",
            owed[0].intents
        );
    }

    /// **An already-completed child reopens nothing.**
    #[test]
    fn pasting_a_completed_child_into_a_completed_task_reopens_nothing() {
        let _g = lock();
        fixture();
        let (mut ws, mut e, parent) = a_completed_task_holding_a_note();
        let loose = e.body.rows[2].id;
        koru_core::tasks::ensure_task(&mut ws, loose, TaskReason::Explicit).unwrap();
        faithful_import::restore_task(
            &mut ws,
            loose,
            &TaskFields {
                marker: Some(TaskReason::Explicit),
                due: None,
                lifecycle: Lifecycle::Completed,
                done: false,
                completed_at: None,
                pinned_today: false,
            },
        )
        .unwrap();
        e.body.refresh(&ws, loose);

        copy_the_loose_row_into_the_task(&mut e, &mut ws);

        assert_eq!(lifecycle_of(&ws, parent), Lifecycle::Completed);
        assert!(e.body.take_task_activity().is_empty(), "and owed nothing");
    }

    /// **Passive content reopens nothing.** The loose row is prose: filing a
    /// note under a finished task does not reopen it.
    #[test]
    fn pasting_passive_content_into_a_completed_task_reopens_nothing() {
        let _g = lock();
        fixture();
        let (mut ws, mut e, parent) = a_completed_task_holding_a_note();

        copy_the_loose_row_into_the_task(&mut e, &mut ws);

        assert_eq!(lifecycle_of(&ws, parent), Lifecycle::Completed);
        assert!(e.body.take_task_activity().is_empty());
    }

    /// **A pasted subtree's own completed task is not reopened by its own
    /// child.** SPEC §6 says the subtree comes back as it was copied, so only
    /// the nodes the *destination* gained are classified — which is what
    /// [`landing_roots`] decides. The completed row arriving under a completed
    /// task is `FinishedStructural`, so nothing reopens on either side.
    #[test]
    fn a_pasted_subtree_classifies_what_the_destination_gained_and_no_more() {
        let _g = lock();
        fixture();
        let (mut ws, mut e) = editor_over(&[
            ("done outer", 0),
            ("still open", 1),
            ("ship it", 0),
            ("a note", 1),
        ]);
        let (outer, inner) = (e.body.rows[0].id, e.body.rows[1].id);
        let destination = e.body.rows[2].id;
        for node in [outer, inner, destination] {
            koru_core::tasks::ensure_task(&mut ws, node, TaskReason::Explicit).unwrap();
        }
        for node in [outer, destination] {
            faithful_import::restore_task(
                &mut ws,
                node,
                &TaskFields {
                    marker: Some(TaskReason::Explicit),
                    due: None,
                    lifecycle: Lifecycle::Completed,
                    done: false,
                    completed_at: None,
                    pinned_today: false,
                },
            )
            .unwrap();
        }
        e.body.rebuild(&ws);

        e.body.focus = 0;
        assert!(copy(&mut e, &ws));
        // Row 3 is the note inside the destination task.
        e.body.focus = 3;
        e.body.take_task_activity();
        assert!(paste(&mut e, &mut ws));

        let pasted_outer = e.body.rows[4].id;
        let pasted_inner = e.body.rows[5].id;
        assert_eq!(
            lifecycle_of(&ws, pasted_outer),
            Lifecycle::Completed,
            "its own open child did not reopen it — that state was copied, not added"
        );
        assert_eq!(lifecycle_of(&ws, pasted_inner), Lifecycle::Open);
        assert_eq!(
            lifecycle_of(&ws, destination),
            Lifecycle::Completed,
            "and what the destination gained was a finished child"
        );
        assert!(e.body.take_task_activity().is_empty());
    }

    /// **A checklist rebuilt on the way in still classifies.** In a flat body a
    /// copied list item arrives as a fresh `ListBlock` plus its item, so the
    /// node whose parent lies outside the paste is the *container* — scaffolding
    /// that can never be a task. [`landing_roots`] descends through it, or an
    /// unchecked box pasted into a completed task would reopen nothing.
    #[test]
    fn an_unchecked_item_pasted_into_a_completed_task_reopens_it_through_its_list_block() {
        let _g = lock();
        fixture();
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("d").unwrap())
            .unwrap();
        let parent = ws
            .insert(Parent::Node(doc), Node::task("ship it").unwrap())
            .unwrap();
        ws.insert(Parent::Node(parent), Node::paragraph("a note").unwrap())
            .unwrap();
        koru_core::tasks::ensure_task(&mut ws, parent, TaskReason::Explicit).unwrap();
        faithful_import::restore_task(
            &mut ws,
            parent,
            &TaskFields {
                marker: Some(TaskReason::Explicit),
                due: None,
                lifecycle: Lifecycle::Completed,
                done: false,
                completed_at: None,
                pinned_today: false,
            },
        )
        .unwrap();
        ws.enable_undo();
        let mut e = Editor::over(&ws);
        e.body.flat = true;
        e.body.zoom_to(&ws, Some(doc));
        e.set_viewport(900.0, 560.0);
        // The note inside the completed task, so the paste lands beside it.
        e.body.focus = e
            .body
            .rows
            .iter()
            .position(|r| r.text == "a note")
            .expect("the note");
        e.body.take_task_activity();

        let unchecked = CopiedRow {
            runs: vec![plain_run("write the summary".into())],
            list: Some(CopiedListSpec {
                style: ListStyle::Checkbox,
                depth: 0,
            }),
            kind: Kind::Paragraph,
            ..empty()
        };
        assert!(paste_rows(&mut e, &mut ws, &[unchecked], None));

        assert_eq!(
            lifecycle_of(&ws, parent),
            Lifecycle::Open,
            "the unchecked item is unfinished structural work, list block or not"
        );
        assert_eq!(e.body.take_task_activity().len(), 1);
    }

    /// **Inside one workspace, a paste matches a duplicate again.** Every
    /// definition is present by construction here, so the value keeps its
    /// meaning and there is nothing to validate it against that a duplicate
    /// does not also skip.
    #[test]
    fn a_custom_attribute_travels_within_one_workspace() {
        let _g = lock();
        let here = WorkspaceId::new();
        let (mut ws, mut e) = editor_over(&[("plan", 0), ("other", 0)]);
        e.view_mut().workspace = Some(here);
        let source = e.body.rows[0].id;
        set_effort(&mut ws, source, "Large");
        e.body.refresh(&ws, source);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));

        let pasted = e.body.rows[1].id;
        assert_ne!(pasted, source);
        assert_eq!(effort(&ws, pasted).as_deref(), Some("Large"));
        // One Ctrl+Z still takes the block and the attribute back together:
        // the custom write joins the paste's open group like the rest.
        assert!(ws.undo().unwrap());
        e.body.rebuild(&ws);
        assert_eq!(shape(&e), vec![("plan".into(), 0), ("other".into(), 0)]);
        assert_eq!(
            effort(&ws, source).as_deref(),
            Some("Large"),
            "and left the original alone"
        );
    }

    /// **Crossing into another workspace it is dropped**, deliberately: the
    /// definition that gives `Effort` a type, a label and a set of options is
    /// in the workspace it left. The block, its text and its system attributes
    /// still arrive.
    #[test]
    fn a_custom_attribute_is_dropped_pasting_into_another_workspace() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("plan", 0)]);
        e.view_mut().workspace = Some(WorkspaceId::new());
        let source = e.body.rows[0].id;
        set_effort(&mut ws, source, "Large");
        ws.set_attribute(source, KEY_PRIORITY, AttrValue::Text("high".into()))
            .unwrap();
        e.body.refresh(&ws, source);
        e.body.focus = 0;
        assert!(copy(&mut e, &ws));

        let (mut ws2, mut e2) = editor_over(&[("here", 0)]);
        e2.view_mut().workspace = Some(WorkspaceId::new());
        e2.body.focus = 0;
        assert!(paste(&mut e2, &mut ws2));

        let pasted = e2.body.rows[1].id;
        assert_eq!(e2.body.rows[1].text, "plan", "the block itself arrived");
        assert_eq!(effort(&ws2, pasted), None, "the custom key did not");
        assert_eq!(
            the_four(&ws2, pasted).3.as_deref(),
            Some("high"),
            "a key this build defines still travels"
        );
    }

    /// A workspace nobody named is not the same workspace as anything. Two
    /// views with no id pushed into them must not be treated as one, or the
    /// rule would hold only where the host happens to be wired.
    #[test]
    fn an_unknown_workspace_is_never_a_match() {
        let _g = lock();
        let (mut ws, mut e) = editor_over(&[("plan", 0), ("other", 0)]);
        assert_eq!(e.view().workspace, None, "nothing pushed one in");
        let source = e.body.rows[0].id;
        set_effort(&mut ws, source, "Large");
        e.body.refresh(&ws, source);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        assert!(paste(&mut e, &mut ws));
        assert_eq!(effort(&ws, e.body.rows[1].id), None);
    }

    /// **A duplicate is unchanged**, which is the behaviour the paste was
    /// brought back into line with. `place` copies the node's content whole,
    /// so a custom key rides along with everything else.
    #[test]
    fn duplicating_a_block_still_keeps_a_custom_attribute() {
        let (mut ws, e) = editor_over(&[("plan", 0)]);
        let source = e.body.rows[0].id;
        set_effort(&mut ws, source, "Large");
        let copy = ws.duplicate(source, Parent::Root, "plan").unwrap();
        assert_ne!(copy, source);
        assert_eq!(effort(&ws, copy).as_deref(), Some("Large"));
    }

    /// **An orphan is not a custom attribute**, and the same-workspace rule
    /// cannot tell them apart — the *role* is what does. An attribute under a
    /// role this build mints for a pill, with no anchor in the text, is left
    /// behind even inside one workspace: resurrecting it as a block attribute
    /// would put a due date on a row nobody set one on.
    ///
    /// Note where the orphan has to come from. Deleting the anchor **also
    /// deletes the record** — asserted below, because it is what makes the
    /// local path safe, and if it ever stopped being true this test would go
    /// quiet about the case it exists for. So the orphan is written straight
    /// onto the node, which is the shape convergence can still leave behind:
    /// one peer deletes the anchor while another sets the attribute.
    #[test]
    fn an_orphaned_pill_attribute_never_becomes_a_block_attribute() {
        let _g = lock();
        let here = WorkspaceId::new();
        let (mut ws, mut e) = editor_over(&[("due today", 0)]);
        e.view_mut().workspace = Some(here);
        let id = e.body.rows[0].id;

        // The ordinary path first: deleting the anchor takes the record with
        // it, so no orphan is made here.
        ws.insert_pill(id, 3, &Attr::new("due", AttrValue::Text("Friday".into())))
            .unwrap();
        let anchor = ws
            .runs(id)
            .unwrap()
            .into_iter()
            .find(|r| r.is_pill())
            .expect("the pill is in the text")
            .range();
        ws.delete_text(id, anchor).unwrap();
        assert!(
            ws.read(id).unwrap().content.attrs().is_empty(),
            "deleting the anchor deletes the attribute"
        );

        // The orphan itself: a pill role with nothing pointing at it.
        ws.set_attr(
            id,
            &Attr::labelled(
                "link",
                AttrValue::Url("https://example.com".into()),
                "Terms",
            ),
        )
        .unwrap();
        e.body.refresh(&ws, id);
        e.body.focus = 0;

        assert!(copy(&mut e, &ws));
        let Taken::Rows(rows, _) = take() else {
            panic!("rows on the clipboard")
        };
        assert!(
            rows[0].node_attrs.is_empty(),
            "an orphan is not carried at all: {:?}",
            rows[0].node_attrs
        );

        assert!(paste(&mut e, &mut ws));
        let pasted = e.body.rows[1].id;
        assert!(
            ws.read(pasted).unwrap().content.attrs().is_empty(),
            "and none of it reached the pasted block, in the same workspace"
        );
    }

    // ------------------------------------------- code blocks keep their language

    /// A document body — where code blocks live — over the given rows: a code
    /// block where a language is given (`Some("")` for one left on
    /// auto-detect), a paragraph otherwise. Returns the body rows' ids, title
    /// excluded.
    fn document_over(rows: &[(&str, Option<&str>)]) -> (WorkspaceDoc, Editor, Vec<NodeId>) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
        let doc = ws
            .insert(Parent::Root, Node::document("D").unwrap())
            .unwrap();
        let mut ids = Vec::new();
        for (text, code) in rows {
            let node = match code {
                Some(_) => Node::code(*text).unwrap(),
                None => Node::paragraph(*text).unwrap(),
            };
            let id = ws.insert(Parent::Node(doc), node).unwrap();
            if let Some(language) = code.filter(|l| !l.is_empty()) {
                ws.set_language(id, Some(language)).unwrap();
            }
            ids.push(id);
        }
        ws.enable_undo();
        let mut e = Editor::rooted_at(&ws, doc, true);
        // The document app offers code blocks; the default view does not.
        e.view.code_enabled = true;
        e.body.rebuild(&ws);
        (ws, e, ids)
    }

    /// The body rows, title excluded: text, kind and language.
    fn body_rows(ws: &WorkspaceDoc, e: &Editor) -> Vec<(String, Kind, Option<String>)> {
        e.body
            .rows
            .iter()
            .filter(|r| !r.is_title)
            .map(|r| {
                let node = ws.read(r.id).unwrap();
                (r.text.clone(), r.kind, node.meta.language.clone())
            })
            .collect()
    }

    /// Copy one row by focusing it with no selection: Ctrl+C takes the whole
    /// block.
    fn copy_row(e: &mut Editor, ws: &WorkspaceDoc, id: NodeId) {
        e.body.focus_on(id);
        e.place_caret(0);
        assert!(copy(e, ws), "something to copy");
    }

    /// A code block pasted beside a paragraph is still a code block, with every
    /// line and its language. It used to take the paragraph's kind, and a
    /// paragraph cannot hold a newline: `insert_runs` dropped the run and the
    /// block arrived as an empty row.
    #[test]
    fn a_code_block_pasted_beside_a_paragraph_keeps_its_lines_and_its_language() {
        fixture();
        let body = "fn main() {\n    run();\n}";
        let (ws, mut e, ids) = document_over(&[("intro", None), (body, Some("rust"))]);
        copy_row(&mut e, &ws, ids[1]);

        let (mut ws2, mut e2, ids2) = document_over(&[("landing", None)]);
        e2.body.focus_on(ids2[0]);
        e2.place_caret(usize::MAX);
        assert!(paste(&mut e2, &mut ws2));

        assert_eq!(
            body_rows(&ws2, &e2),
            vec![
                ("landing".into(), Kind::Paragraph, None),
                (body.into(), Kind::Code, Some("rust".into())),
            ]
        );
    }

    /// A one-line block has no newline to lose, so it used to arrive — as a
    /// paragraph. It is code where it lands too.
    #[test]
    fn a_one_line_code_block_arrives_as_code() {
        fixture();
        let (ws, mut e, ids) = document_over(&[("x = 1", Some(""))]);
        copy_row(&mut e, &ws, ids[0]);

        let (mut ws2, mut e2, ids2) = document_over(&[("landing", None)]);
        e2.body.focus_on(ids2[0]);
        e2.place_caret(usize::MAX);
        assert!(paste(&mut e2, &mut ws2));

        assert_eq!(
            body_rows(&ws2, &e2),
            vec![
                ("landing".into(), Kind::Paragraph, None),
                ("x = 1".into(), Kind::Code, None),
            ]
        );
    }

    /// A blank code row absorbs the paste in place and takes the pasted
    /// block's language — the row *is* the pasted block now, whatever grammar
    /// the blank row had been set to.
    #[test]
    fn a_code_block_pasted_into_a_blank_code_row_keeps_its_language() {
        fixture();
        let (ws, mut e, ids) = document_over(&[("print(1)", Some("py"))]);
        copy_row(&mut e, &ws, ids[0]);

        let (mut ws2, mut e2, ids2) = document_over(&[("", Some("rust"))]);
        e2.body.focus_on(ids2[0]);
        e2.place_caret(0);
        assert!(paste(&mut e2, &mut ws2));

        assert_eq!(
            body_rows(&ws2, &e2),
            vec![("print(1)".into(), Kind::Code, Some("py".into()))],
            "one row, reused, with the pasted block's language"
        );
    }

    /// Prose absorbed by a blank code row is not a code block arriving: the
    /// row keeps its own language.
    #[test]
    fn prose_pasted_into_a_blank_code_row_leaves_its_language_alone() {
        fixture();
        let (ws, mut e, ids) = document_over(&[("a note", None)]);
        copy_row(&mut e, &ws, ids[0]);

        let (mut ws2, mut e2, ids2) = document_over(&[("", Some("rust"))]);
        e2.body.focus_on(ids2[0]);
        e2.place_caret(0);
        assert!(paste(&mut e2, &mut ws2));

        assert_eq!(
            body_rows(&ws2, &e2),
            vec![("a note".into(), Kind::Code, Some("rust".into()))]
        );
    }

    /// A paragraph, a code block and a paragraph, copied together, arrive in
    /// that order with the code block whole between the two.
    #[test]
    fn a_mixed_selection_round_trips_with_the_code_block_between_the_paragraphs() {
        fixture();
        let body = "let x = 1;\nlet y = 2;";
        let (ws, mut e, _) = document_over(&[("one", None), (body, Some("rust")), ("two", None)]);
        for row in e.body.rows.iter_mut().filter(|r| !r.is_title) {
            row.selected = true;
        }
        assert!(copy(&mut e, &ws));

        let (mut ws2, mut e2, ids2) = document_over(&[("", None)]);
        e2.body.focus_on(ids2[0]);
        e2.place_caret(0);
        assert!(paste(&mut e2, &mut ws2));

        assert_eq!(
            body_rows(&ws2, &e2),
            vec![
                ("one".into(), Kind::Paragraph, None),
                (body.into(), Kind::Code, Some("rust".into())),
                ("two".into(), Kind::Paragraph, None),
            ]
        );
    }

    /// Where the view does not offer code blocks — the task outline — the
    /// block adapts to the view's rows, as prose does, and that costs no line:
    /// one row per line, the way the same text pasted from outside lands.
    #[test]
    fn a_code_block_pasted_where_code_is_not_offered_lands_one_row_per_line() {
        fixture();
        let (ws, mut e, ids) = document_over(&[("a\nb\nc", Some("rust"))]);
        copy_row(&mut e, &ws, ids[0]);

        let (mut ws2, mut e2) = editor_over(&[("task", 0)]);
        assert!(!e2.view.code_enabled, "the outline offers no code blocks");
        e2.body.focus = 0;
        assert!(paste(&mut e2, &mut ws2));

        assert_eq!(
            shape(&e2),
            vec![
                ("task".into(), 0),
                ("a".into(), 0),
                ("b".into(), 0),
                ("c".into(), 0),
            ]
        );
        assert!(
            e2.body.rows.iter().all(|r| r.kind == Kind::Task),
            "every line took the outline's own kind"
        );
    }

    /// Source text is not an address to follow: a URL inside a pasted code
    /// block stays text, as it does when pasted *into* a code block.
    #[test]
    fn a_url_in_a_pasted_code_block_stays_text() {
        fixture();
        let body = "see https://example.com/path";
        let (ws, mut e, ids) = document_over(&[(body, Some("md"))]);
        copy_row(&mut e, &ws, ids[0]);

        let (mut ws2, mut e2, ids2) = document_over(&[("landing", None)]);
        e2.body.focus_on(ids2[0]);
        e2.place_caret(usize::MAX);
        assert!(paste(&mut e2, &mut ws2));

        let code = e2
            .body
            .rows
            .iter()
            .find(|r| r.kind == Kind::Code)
            .expect("the code row");
        let node = ws2.read(code.id).unwrap();
        assert_eq!(node.content.as_text(), Some(body));
        assert!(
            !node
                .content
                .attrs()
                .iter()
                .any(|a| matches!(&a.value, AttrValue::Url(_))),
            "no link was made inside the code block"
        );
    }

    /// The plain-text half: a code block leaves as a fence naming its
    /// language, so the next editor reads a code block rather than bare lines.
    #[test]
    fn a_code_block_leaves_for_another_application_as_a_fence_naming_its_language() {
        fixture();
        let (ws, mut e, ids) = document_over(&[("fn main() {\n}", Some("rust"))]);
        e.body.focus_on(ids[0]);
        e.place_caret(0);

        let payload = capture(&e, &ws).expect("something to copy");
        assert_eq!(render(&ws, &payload), "```rust\nfn main() {\n}\n```");
    }

    /// A body quoting a three-backtick fence leaves inside a four-backtick one
    /// — the same bytes the Markdown exporter writes for the same body, since
    /// both come from `koru_core::markdown::fenced_code`, and the exporter's
    /// test pins this literal too.
    #[test]
    fn a_code_body_holding_a_fence_leaves_with_a_longer_one() {
        fixture();
        let body = "a\n```\nb";
        let (ws, mut e, ids) = document_over(&[(body, Some("md"))]);
        e.body.focus_on(ids[0]);
        e.place_caret(0);

        let payload = capture(&e, &ws).expect("something to copy");
        let text = render(&ws, &payload);
        assert_eq!(text, "````md\na\n```\nb\n````");
        assert_eq!(text, koru_core::markdown::fenced_code("md", body));
    }

    /// A block left on auto-detect leaves with a bare fence.
    #[test]
    fn a_code_block_without_a_language_leaves_with_a_bare_fence() {
        fixture();
        let (ws, mut e, ids) = document_over(&[("x = 1", Some(""))]);
        e.body.focus_on(ids[0]);
        e.place_caret(0);

        let payload = capture(&e, &ws).expect("something to copy");
        assert_eq!(render(&ws, &payload), "```\nx = 1\n```");
    }

    /// In a mixed copy the fence sits between the paragraphs, each on its own
    /// line.
    #[test]
    fn a_mixed_copy_puts_the_fence_between_the_paragraphs() {
        fixture();
        let (ws, mut e, _) = document_over(&[
            ("one", None),
            ("let x = 1;\nlet y = 2;", Some("rust")),
            ("two", None),
        ]);
        for row in e.body.rows.iter_mut().filter(|r| !r.is_title) {
            row.selected = true;
        }

        let payload = capture(&e, &ws).expect("something to copy");
        assert_eq!(
            render(&ws, &payload),
            "one\n```rust\nlet x = 1;\nlet y = 2;\n```\ntwo"
        );
    }

    /// A code block inside a callout box leaves as a fence whose every line is
    /// quoted, as the box's other lines are, so the box and the fence both
    /// survive a trip through plain text. The fence branch once returned out
    /// of the whole copy, which dropped every other row.
    #[test]
    fn a_fence_inside_a_callout_box_is_quoted_line_by_line() {
        fixture();
        let (ws, _, _) = document_over(&[("x", None)]);
        let boxed = Some(CopiedCalloutSpec {
            icon: None,
            color: None,
            depth: 0,
        });
        let rows = vec![
            CopiedRow {
                runs: vec![plain_run("before".into())],
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run(
                    "let a = 1;
let b = 2;"
                        .into(),
                )],
                kind: Kind::Code,
                language: Some("rust".into()),
                callout: boxed,
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("after".into())],
                ..empty()
            },
        ];
        let text = render(&ws, &Payload::Rows(rows, None));
        assert!(
            text.starts_with(
                "before
"
            ),
            "{text}"
        );
        assert!(
            text.ends_with(
                "
after"
            ),
            "{text}"
        );
        for line in ["> ```rust", "> let a = 1;", "> let b = 2;", "> ```"] {
            assert!(text.contains(line), "{line:?} in {text}");
        }
    }

    /// The HTML half: `<pre><code class="language-…">`, newlines kept, the
    /// body escaped.
    #[test]
    fn cf_html_wraps_a_code_block_in_pre_code_naming_its_language() {
        let rows = vec![CopiedRow {
            runs: vec![plain_run("if a < b && c > d {\n    say(\"hi\");\n}".into())],
            kind: Kind::Code,
            language: Some("rust".into()),
            ..empty()
        }];
        let html = cf_html_of(&rows).expect("some");
        assert!(
            html.contains(
                "<pre><code class=\"language-rust\">if a &lt; b &amp;&amp; c &gt; d {\n    say(&quot;hi&quot;);\n}</code></pre>"
            ),
            "{html}"
        );
        assert!(
            !html.contains("<p>if a"),
            "not collapsed into a paragraph: {html}"
        );
    }

    /// The language is an attribute value, so a quote or an angle bracket in
    /// it cannot end the attribute or open a tag.
    #[test]
    fn cf_html_escapes_the_language_in_the_class_attribute() {
        let rows = vec![CopiedRow {
            runs: vec![plain_run("x".into())],
            kind: Kind::Code,
            language: Some("a\"b<c&d".into()),
            ..empty()
        }];
        let html = cf_html_of(&rows).expect("some");
        assert!(
            html.contains("<pre><code class=\"language-a&quot;b&lt;c&amp;d\">x</code></pre>"),
            "{html}"
        );
    }

    /// No language, no class — rather than an empty `language-`.
    #[test]
    fn cf_html_of_a_code_block_without_a_language_has_no_class() {
        let rows = vec![CopiedRow {
            runs: vec![plain_run("x = 1".into())],
            kind: Kind::Code,
            ..empty()
        }];
        let html = cf_html_of(&rows).expect("some");
        assert!(html.contains("<pre><code>x = 1</code></pre>"), "{html}");
    }

    /// A code block between two paragraphs is its own block in the HTML too.
    #[test]
    fn cf_html_keeps_a_code_block_between_its_paragraphs() {
        let rows = vec![
            CopiedRow {
                runs: vec![plain_run("one".into())],
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("a\nb".into())],
                kind: Kind::Code,
                language: Some("sh".into()),
                ..empty()
            },
            CopiedRow {
                runs: vec![plain_run("two".into())],
                ..empty()
            },
        ];
        let html = cf_html_of(&rows).expect("some");
        assert!(
            html.contains("<p>one</p><pre><code class=\"language-sh\">a\nb</code></pre><p>two</p>"),
            "{html}"
        );
    }

    /// All four characters HTML reads as syntax, so the same escape serves
    /// text and quoted attributes.
    #[test]
    fn html_escape_covers_text_and_attribute_syntax() {
        assert_eq!(
            html_escape("<a href=\"x\">&</a>"),
            "&lt;a href=&quot;x&quot;&gt;&amp;&lt;/a&gt;"
        );
    }
}
