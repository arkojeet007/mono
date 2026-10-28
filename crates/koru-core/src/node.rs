// SPDX-License-Identifier: MPL-2.0

//! The node envelope and its three content shapes.
//!
//! Not every kind has text and not every kind nests: an image is a blob with no
//! text and no children, a table is markdown source with no children, a task is
//! a line that nests arbitrarily. The envelope is shared, the content is a sum
//! type, and [`Kind`] decides which combinations are legal.
//!
//! A **pill is the rendering of an attribute, not the attribute itself.**
//! Attributes live in [`Node::attrs`] where a query can reach them without
//! parsing text; a mark on an anchor character in the text says "draw attribute
//! `x` here". An attribute set from a properties panel simply has no pill.

use crate::ids::{AttrId, DocumentReference, NodeId, Ref};
use crate::time::{DateOnly, FloatingDateTime, Temporal, TemporalKind, ZonedDateTime};

/// What a node *is*. The grouping levels a user invents — "Project",
/// "Initiative", "Phase" — are **not** kinds; they are [`NodeMeta::level_name`]
/// on an ordinary task. Nothing here dictates hierarchy.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    /// A line with a checkbox. A task with children is a group.
    Task,
    /// A line of a document's body.
    Paragraph,
    /// A quoted line in a document. It is a text-shaped leaf: Enter may split
    /// it into another quote, but hierarchy belongs to structural blocks
    /// rather than to the quotation's visual inset.
    Quote,
    /// A highlighted box in a document, drawn tinted with an icon. A
    /// container like [`ListBlock`](Kind::ListBlock): it carries no text of
    /// its own, and its children are the lines inside it — paragraphs, and
    /// lists with their items. The icon and tint live on the container
    /// ([`crate::callout`]). Never a row of its own; the editor draws the
    /// box around its children's rows.
    Callout,
    /// Its text is the title; its children are the body.
    Document,
    /// A named, opaque workspace file. Its bytes live in the blob store and
    /// its `Blob` content records only their hash and MIME type. Files are
    /// roots of their own Store documents and never occur inside a document
    /// body; documents point at them through routed `attachment` attributes.
    File,
    /// A list container. Its children are the items (paragraphs); the node's
    /// own text is empty and its label comes from [`NodeMeta::name`], the way
    /// an image's does. The list style (bullet/checkbox/numbered/…) lives on
    /// [`NodeMeta::list_style`]. Openable as its own view, and revealable from
    /// the explore column like an image.
    ListBlock,
    /// Markdown source. Rendering it as a grid is a view concern.
    Table,
    /// A multi-line code block. Exempt from the one-line rule — its text may
    /// contain newlines, which parley treats as mandatory breaks inside one
    /// `Layout`. Syntax colour is derived at layout time and never stored; the
    /// grammar lives in [`NodeMeta::language`].
    Code,
    Image,
    /// Treated as an image for now.
    Canvas,
    /// Text is the person's name; email and role live in meta.
    Contact,
    /// A horizontal rule. No text, no children — a leaf block like `Image`, but
    /// text-shaped so it slots into a document body as a row. Its text is always
    /// empty ([`Node::check`] rejects prose on it).
    Divider,
    /// The hidden per-workspace trash. Never a row, never opened, never named by
    /// a person — a *place*, so that binning a document is an ordinary move
    /// inside the library realm rather than a flag every reader has to remember.
    ///
    /// It lives as a child of the library root, is created the first time
    /// something is trashed, and is filtered out of `children(Parent::Root)`
    /// so that every walk in the tree skips it without asking. See
    /// [ADR-69](../../../docs/decisions/README.md) and `docs/architecture/DATAMODEL.md` §The trash is a
    /// place.
    Trash,
}

/// Which content shape a kind requires.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Shape {
    Text,
    Blob,
}

impl Kind {
    /// Whether this kind may have children.
    pub fn accepts_children(self) -> bool {
        matches!(
            self,
            Self::Task
                | Self::Paragraph
                | Self::Callout
                | Self::Document
                | Self::ListBlock
                | Self::Trash
        )
    }

    /// The content shape this kind requires. [`Content::Embed`] is accepted by
    /// any kind, because an embed replaces the node's own content with a
    /// pointer at someone else's.
    pub fn shape(self) -> Shape {
        match self {
            Self::Task
            | Self::Paragraph
            | Self::Quote
            | Self::Callout
            | Self::Document
            | Self::ListBlock
            | Self::Table
            | Self::Code
            | Self::Contact
            | Self::Divider
            | Self::Trash => Shape::Text,
            Self::Image | Self::Canvas | Self::File => Shape::Blob,
        }
    }

    /// Kinds the outline view shows as rows with checkboxes.
    pub fn is_task_like(self) -> bool {
        matches!(self, Self::Task)
    }

    /// Whether this kind's text may legally contain newlines. The one-line rule
    /// is the invariant the editor rests on, and [`Code`] (and [`Table`]'s
    /// markdown source) are the deliberate exceptions: a code block is one
    /// `Row` whose parley `Layout` breaks on each `\n` rather than a node per
    /// line. Gates the newline rejection in [`Node::check`], `set_text` and
    /// `insert_text`.
    ///
    /// [`Code`]: Kind::Code
    /// [`Table`]: Kind::Table
    pub fn allows_newlines(self) -> bool {
        matches!(self, Self::Code | Self::Table)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Task => "task",
            Self::Paragraph => "paragraph",
            Self::Quote => "quote",
            Self::Callout => "callout",
            Self::Document => "document",
            Self::File => "file",
            Self::ListBlock => "listblock",
            Self::Table => "table",
            Self::Code => "code",
            Self::Image => "image",
            Self::Canvas => "canvas",
            Self::Contact => "contact",
            Self::Divider => "divider",
            Self::Trash => "trash",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "task" => Self::Task,
            "paragraph" => Self::Paragraph,
            "quote" => Self::Quote,
            "callout" => Self::Callout,
            "document" => Self::Document,
            "file" => Self::File,
            "listblock" => Self::ListBlock,
            "table" => Self::Table,
            "code" => Self::Code,
            "image" => Self::Image,
            "canvas" => Self::Canvas,
            "contact" => Self::Contact,
            "divider" => Self::Divider,
            "trash" => Self::Trash,
            _ => return None,
        })
    }
}

/// A content-addressed blob living on disk, never in the CRDT — putting image
/// bytes in the document would make every sync carry them.
///
/// **Self-describing: `<algorithm>:<digest>`.** A bare digest with no prefix is
/// [`BlobHash::FNV1A`], which is what every hash written before this existed is.
///
/// The prefix is here because the algorithm has to change and the string is
/// *stored*, in the meta map of every image node ever made. FNV-1a addresses
/// local content perfectly well but does not authenticate it, and a server that
/// dedups blobs across profiles turns a collision into somebody else's picture
/// appearing in your document. Adopting BLAKE3 then becomes `b3:<64 hex>`
/// alongside — something [`algorithm`] can tell apart at read time — rather than
/// a migration of every node in every workspace.
///
/// [`algorithm`]: BlobHash::algorithm
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct BlobHash(pub String);

impl BlobHash {
    /// The only algorithm there has been, and what an unprefixed hash means.
    pub const FNV1A: &'static str = "fnv1a";

    pub fn new(algorithm: &str, digest: &str) -> Self {
        Self(format!("{algorithm}:{digest}"))
    }

    /// What hashed this. `FNV1A` for a bare digest written before the prefix
    /// existed — those are not unknown, they are the old ones.
    pub fn algorithm(&self) -> &str {
        match self.0.split_once(':') {
            Some((alg, _)) => alg,
            None => Self::FNV1A,
        }
    }

    pub fn digest(&self) -> &str {
        match self.0.split_once(':') {
            Some((_, digest)) => digest,
            None => &self.0,
        }
    }

    /// Whether this hash carries its algorithm, or predates the prefix. The
    /// storage layer asks so an old blob keeps its old file name and is not
    /// stored a second time under a new one.
    pub fn is_legacy(&self) -> bool {
        !self.0.contains(':')
    }
}

/// The semantic value behind a pill.
#[derive(Clone, PartialEq, Debug)]
pub enum AttrValue {
    /// A calendar date — [`DateOnly`]. A date is a *value*: it has no identity
    /// and therefore no scope.
    ///
    /// **Never an instant, at any layer.** `2026-08-30` means August 30 to
    /// every collaborator in every timezone; converting it through UTC midnight
    /// would show different days to different people ([ADR-183]).
    Date(DateOnly),
    /// A wall-clock date and time with **no zone** — [`FloatingDateTime`]. It
    /// means the same clock reading wherever it is read, which is what a
    /// *personal* timed value (`koru.plan`) wants: it follows the person.
    ///
    /// [ADR-183]: ../../../docs/decisions/ADR-201-225.md#adr-206-timemodel--koru-has-three-temporal-types-not-one-with-a-nullable-timezone
    FloatingDateTime(FloatingDateTime),
    /// A stable instant **plus the zone it was meant in** — [`ZonedDateTime`].
    /// The form a *shared* timed value (`koru.due`) takes: travelling changes
    /// how it is drawn and never what it means.
    ///
    /// There is deliberately no fourth variant for "an instant without a
    /// zone". Three types, and the type carries the meaning — see
    /// [`crate::time`].
    ZonedDateTime(ZonedDateTime),
    /// A number. `f64`, which is what every imported format and every
    /// projection speaks; a count and a percentage are both this.
    Number(f64),
    Bool(bool),
    /// A pointer to a node — a contact, a document — which does have a scope.
    Ref(Ref),
    /// A Store document. Its current profile/workspace is resolved on open.
    Document(DocumentReference),
    Url(String),
    Text(String),
}

impl AttrValue {
    /// The attribute name a query filters on — also the stable storage type
    /// token. An older build reads a token it does not know as *absent*, so
    /// every new variant here is additive rather than a schema bump.
    pub fn key(&self) -> &'static str {
        match self {
            Self::Date(_) => TemporalKind::Date.token(),
            Self::FloatingDateTime(_) => TemporalKind::Floating.token(),
            Self::ZonedDateTime(_) => TemporalKind::Zoned.token(),
            Self::Number(_) => "number",
            Self::Bool(_) => "bool",
            Self::Ref(_) => "ref",
            // Kept as `ref` on disk and at projection boundaries so older
            // readers preserve the attribute even though they cannot follow
            // its location-independent route.
            Self::Document(_) => "ref",
            Self::Url(_) => "url",
            Self::Text(_) => "text",
        }
    }

    /// This value as one of the three temporal types, or `None` when it is not
    /// temporal at all.
    ///
    /// The one door for code that handles all three: it gets a [`Temporal`]
    /// and has to match, so a forgotten case is a compile error rather than a
    /// value that quietly means something else.
    pub fn temporal(&self) -> Option<Temporal> {
        match self {
            Self::Date(d) => Some(Temporal::Date(*d)),
            Self::FloatingDateTime(f) => Some(Temporal::Floating(*f)),
            Self::ZonedDateTime(z) => Some(Temporal::Zoned(z.clone())),
            _ => None,
        }
    }
}

impl From<Temporal> for AttrValue {
    fn from(value: Temporal) -> Self {
        match value {
            Temporal::Date(d) => Self::Date(d),
            Temporal::Floating(f) => Self::FloatingDateTime(f),
            Temporal::Zoned(z) => Self::ZonedDateTime(z),
        }
    }
}

/// One attribute. `role` is what the attribute *means* to a view — "due",
/// "assignee", "attachment" — so two date attributes can be a start and a due
/// date without the query guessing.
#[derive(Clone, PartialEq, Debug)]
pub struct Attr {
    pub id: AttrId,
    pub role: String,
    pub value: AttrValue,
    /// Display text when it cannot be derived from the value.
    ///
    /// A date renders itself and a reference borrows its target's name, but a
    /// link's caption ("Terms page") is user-authored content with nowhere else
    /// to live. Presentation stays in `NodeMeta`; this is data.
    pub label: Option<String>,
}

impl Attr {
    pub fn new(role: impl Into<String>, value: AttrValue) -> Self {
        Self {
            id: AttrId::new(),
            role: role.into(),
            value,
            label: None,
        }
    }

    /// An attribute carrying its own caption — a link, typically.
    pub fn labelled(role: impl Into<String>, value: AttrValue, label: impl Into<String>) -> Self {
        Self {
            label: Some(label.into()),
            ..Self::new(role, value)
        }
    }

    /// The value as a **calendar date**, and only if that is what it is. A
    /// floating or zoned value answers `None` here — ask
    /// [`temporal`](Self::temporal) and match, rather than treating a missing
    /// date as an absent attribute.
    pub fn as_date(&self) -> Option<DateOnly> {
        match self.value {
            AttrValue::Date(d) => Some(d),
            _ => None,
        }
    }

    /// The value as one of the three temporal types.
    pub fn temporal(&self) -> Option<Temporal> {
        self.value.temporal()
    }

    pub fn as_number(&self) -> Option<f64> {
        match self.value {
            AttrValue::Number(n) => Some(n),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.value {
            AttrValue::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match &self.value {
            AttrValue::Text(t) => Some(t),
            _ => None,
        }
    }

    pub fn as_ref_target(&self) -> Option<Ref> {
        match self.value {
            AttrValue::Ref(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_document_reference(&self) -> Option<DocumentReference> {
        match self.value {
            AttrValue::Document(reference) => Some(reference),
            _ => None,
        }
    }
}

/// How a block's lines sit within their column.
///
/// Direction-aware rather than absolute: `Start` is left in a left-to-right
/// language and right in a right-to-left one, which is what a person means by
/// "aligned to the margin". A toolbar labels them left/centre/right because
/// that is what they look like, but the stored value follows the text.
#[derive(Copy, Clone, PartialEq, Eq, Default, Debug)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

impl Align {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Center => "center",
            Self::End => "end",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "start" => Some(Self::Start),
            "center" => Some(Self::Center),
            "end" => Some(Self::End),
            _ => None,
        }
    }
}

/// A block's level in the document's heading hierarchy.
///
/// A property of the *block*, like [`Align`], and deliberately not a [`Kind`]:
/// making a paragraph a heading and back must not be structural, and a heading
/// still takes children ([ADR-23](../../../docs/decisions/README.md)).
///
/// Six levels, because that is what every document format agrees on. Only the
/// first four have a button — which levels a toolbar offers is the toolbar's
/// business — but a document that arrives carrying an H5 renders as one rather
/// than being flattened into prose.
///
/// `Ord` is load-bearing rather than decoration: a table of contents asks for
/// "everything shallower than three", and that only works if the levels compare.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Heading {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
}

impl Heading {
    pub const ALL: [Self; 6] = [Self::H1, Self::H2, Self::H3, Self::H4, Self::H5, Self::H6];

    /// 1–6: what a person means by "heading two", and what the snapshot stores.
    ///
    /// The number rather than a name, unlike [`Align::as_str`], because a
    /// heading level is *ordinal* — see [`Heading`].
    pub fn level(self) -> u8 {
        match self {
            Self::H1 => 1,
            Self::H2 => 2,
            Self::H3 => 3,
            Self::H4 => 4,
            Self::H5 => 5,
            Self::H6 => 6,
        }
    }

    /// A stored level, made safe.
    ///
    /// Zero and below is not a heading at all. A level past six comes from a
    /// peer running a build that knows more of them than this one does, and the
    /// deepest heading we can draw is a truer answer than silently demoting a
    /// deliberate heading to body text.
    pub fn from_level(level: i64) -> Option<Self> {
        (level >= 1).then(|| Self::ALL[level.min(6) as usize - 1])
    }
}

/// How a [`Kind::ListBlock`] container renders its item children.
///
/// The style lives on the **container** node's [`NodeMeta`], not on each item:
/// a list is one block whose items share a marker shape. Items are ordinary
/// paragraphs; `Checkbox` is the one style that interacts with state — it draws
/// a checkbox and uses the item's `meta.done`, the way a task does.
///
/// The marker's ordinal — the `3` in `3.`, the `c` in `c.` — is **not stored**.
/// It is computed from the run of item siblings, which is the only choice two
/// concurrently-editing peers converge on. The renderer counts; the model
/// carries only the style.
///
/// Letter and roman are lowercase for now — the Word/Docs default. An uppercase
/// variant is additive (a wider enum), not a rewrite.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ListStyle {
    /// A filled bullet, `•`.
    Bullet,
    /// A tickable checkbox that uses the item's `done` state — a task list.
    Checkbox,
    /// `1.`, `2.`, `3.` …
    Numbered,
    /// `a.`, `b.`, `c.` …
    Letter,
    /// `i.`, `ii.`, `iii.` …
    Roman,
}

impl ListStyle {
    pub const ALL: [Self; 5] = [
        Self::Bullet,
        Self::Checkbox,
        Self::Numbered,
        Self::Letter,
        Self::Roman,
    ];

    /// The stable storage key value. A name, like [`Align::as_str`]: a list
    /// style is categorical, not ordinal the way a heading level is.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bullet => "bullet",
            Self::Checkbox => "checkbox",
            Self::Numbered => "number",
            Self::Letter => "letter",
            Self::Roman => "roman",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "bullet" => Some(Self::Bullet),
            "checkbox" => Some(Self::Checkbox),
            "number" => Some(Self::Numbered),
            "letter" => Some(Self::Letter),
            "roman" => Some(Self::Roman),
            _ => None,
        }
    }

    /// Render a 1-based index the way this style draws it: `1` → `"1"`,
    /// `"a"`, `"i"` for Numbered/Letter/Roman. Bullet and Checkbox render no
    /// ordinal text — a bullet is a dot, a checkbox is drawn separately.
    /// Pure and testable; the draw site calls this.
    pub fn render(self, index: usize) -> String {
        match self {
            Self::Bullet | Self::Checkbox => String::new(),
            Self::Numbered => index.to_string(),
            Self::Letter => to_alpha(index),
            Self::Roman => to_roman(index),
        }
    }
}

/// `1` → `"a"`, `26` → `"z"`, `27` → `"aa"` (spreadsheet-column style).
fn to_alpha(n: usize) -> String {
    let mut s = String::new();
    let mut n = n;
    while n > 0 {
        n -= 1;
        s.insert(0, char::from(b'a' + (n % 26) as u8));
        n /= 26;
    }
    s
}

/// Standard lowercase Roman, `1` → `"i"`, capped reasonably at `3999`.
fn to_roman(n: usize) -> String {
    let table = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut n = n;
    let mut s = String::new();
    for (v, sym) in table {
        while n >= v {
            s.push_str(sym);
            n -= v;
        }
    }
    s
}

/// The furthest a canvas coordinate or size may be from zero, in whole logical
/// pixels at 100 % zoom. Every writer clamps to it and every reader treats a
/// value beyond it as malformed, so arithmetic on two stored values (a position
/// plus a width) can never overflow an `i32`.
pub const CANVAS_LIMIT: i32 = 1_000_000;

/// Where an object sits on a canvas: absolute canvas coordinates, in whole
/// logical pixels at 100 % zoom, whatever the object is inside.
///
/// **Canvas presentation, stored as one whole value** (`meta.at = {x, y}`), so
/// two devices moving the same card at once resolve to one of the two
/// positions, never to one device's `x` and the other's `y`. Whole pixels
/// rather than floats because [`NodeMeta`] derives `Eq`, and a pixel at 100 %
/// is finer than anyone places a card. See `docs/architecture/DATAMODEL.md`
/// §Canvas positions.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct CanvasPoint {
    pub x: i32,
    pub y: i32,
}

impl CanvasPoint {
    pub fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    /// This point with both coordinates inside ±[`CANVAS_LIMIT`] — what every
    /// writer stores.
    pub fn clamped(self) -> Self {
        Self {
            x: self.x.clamp(-CANVAS_LIMIT, CANVAS_LIMIT),
            y: self.y.clamp(-CANVAS_LIMIT, CANVAS_LIMIT),
        }
    }
}

/// How large an object is drawn on a canvas, in whole logical pixels at 100 %.
///
/// `w` always; `h` only where the height is not derived — a card's height
/// follows its text and an image's follows its aspect ratio, so both store
/// `h: None`. Stored as one whole value (`meta.extent = {w[, h]}`), separate
/// from [`CanvasPoint`], so a move and a width change made at once on two
/// devices both survive.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct CanvasExtent {
    pub w: i32,
    pub h: Option<i32>,
}

impl CanvasExtent {
    pub fn new(w: i32, h: Option<i32>) -> Self {
        Self { w, h }
    }

    /// This extent with each side inside `1..=`[`CANVAS_LIMIT`] — what every
    /// writer stores. A size is never zero or negative.
    pub fn clamped(self) -> Self {
        Self {
            w: self.w.clamp(1, CANVAS_LIMIT),
            h: self.h.map(|h| h.clamp(1, CANVAS_LIMIT)),
        }
    }
}

/// Presentation and state that is not a pill: set from a properties panel
/// rather than typed into the line.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
pub struct NodeMeta {
    pub icon: Option<String>,
    /// A user-authored display name for content that has no text of its own.
    /// Images use this; `None` keeps the generated `Image N` label used by
    /// older documents.
    pub name: Option<String>,
    /// The user's own name for this level of the hierarchy. Never enforced.
    pub level_name: Option<String>,
    pub colour: Option<String>,
    pub done: bool,
    /// Alignment is a property of the *block*, not of a range within it — half
    /// a line cannot be centred — which is why it lives here and not in a mark.
    /// `None` means `Start`, so a document written before alignment existed
    /// reads back unchanged.
    pub align: Option<Align>,
    /// This block's heading level, or `None` for body text. A block property for
    /// the same reason alignment is: half a line cannot be an H2. Stored as
    /// *absent* when it is body text, so a document written before headings
    /// existed reads back unchanged.
    pub heading: Option<Heading>,
    /// The list style carried by a [`Kind::ListBlock`] container — how its
    /// item children render. `None` on every other kind. Stored absent when
    /// `None`, so a document written before list blocks existed reads back
    /// unchanged.
    pub list_style: Option<ListStyle>,
    /// The size an image is drawn at, in logical pixels, if the user has resized
    /// it. `None` means natural — fill the content column at the blob's aspect
    /// ratio. Presentation, like alignment: it syncs so every peer sees the same
    /// size, but it is not the picture's bytes or true dimensions.
    pub image_size: Option<(u32, u32)>,
    /// The grammar a code block highlights as — `"rust"`, `"py"`, … — looked up
    /// by the highlighter. `None` means auto-detect (or plain). Only meaningful
    /// on [`Kind::Code`], stored as *absent* when absent so a document written
    /// before code blocks existed reads back unchanged. The highlight colour
    /// itself is never stored: it is derived from the text and this language.
    pub language: Option<String>,
    /// The language this document's *prose* is written in (BCP 47) — the
    /// spelling override collaborators share, meaningful on a document's root.
    /// **Not** [`language`](Self::language), which is a code block's grammar.
    /// `None` means "the reader's own default" and is stored as absent, so
    /// every document written before spell check reads back unchanged
    /// (spell-check plan D4).
    pub prose_language: Option<String>,
    /// How a table uses the document width. `None` means compact, keeping old
    /// documents backward compatible. Meaningful only on [`Kind::Table`].
    pub table_layout: Option<crate::table::TableLayout>,
    /// **Canvas presentation**: where this node sits on a canvas, which is what
    /// makes it an *object* there ([`crate::canvas`]). Absent on every page
    /// node, and stored as absent, so no document written before canvases
    /// existed changes. A malformed stored value reads as `None` rather than
    /// failing the node — the object then shows among the canvas's arrivals.
    pub at: Option<CanvasPoint>,
    /// **Canvas presentation**: how large this object is drawn on a canvas.
    /// Absent on every page node. **Not** [`image_size`](Self::image_size),
    /// which is an image's size on a *page*.
    pub extent: Option<CanvasExtent>,

    // ---- the trash. Four fields, all absent on anything that has never been
    // binned. `in_trash` is answered by *position*, not by these — they are
    // what a restore reads and what the other window says out loud.
    /// When this node was moved to the trash, in unix milliseconds.
    ///
    /// Passed in by the caller, never read from a clock: this crate has none,
    /// deliberately (see `profile.rs`'s `created`). `None` on a node that was
    /// binned by a build that did not record it — a state, not an error.
    pub trashed_at: Option<i64>,
    /// The parent this node had when it was trashed, so a restore can put it
    /// back. `None` means the top of the library — which is also what a stale
    /// origin resolves to, so the two agree by construction.
    ///
    /// No sibling index is stored with it. An index is wrong the moment any
    /// sibling moves, and "restore to position 3" landing arbitrarily is worse
    /// than a consistent append.
    pub origin: Option<NodeId>,
    /// Who moved it to the trash, as a **self-declared display label**.
    ///
    /// Not an identity and not derived from one. There is no author attribution
    /// in this model — a Loro `PeerID` and a `--peer` name are replica handles,
    /// never people ([ADR-39](../../../docs/decisions/README.md)) — so nothing infers who
    /// acted. Instead the client that acts writes its own label at the moment it
    /// acts, and every other window reads it back as ordinary CRDT data.
    /// `None` is normal, and every message built from it has a form with no
    /// "by" clause: a wrong name is worse than no name.
    pub trashed_by: Option<String>,
    /// Who took it back out, on the same terms as [`trashed_by`]. Cleared the
    /// next time the node is trashed, so exactly one of the two is ever live.
    ///
    /// [`trashed_by`]: NodeMeta::trashed_by
    pub restored_by: Option<String>,
}

/// The three shapes a node's content can take.
#[derive(Clone, PartialEq, Debug)]
pub enum Content {
    /// One line. Structure comes from the tree, not from newlines.
    Text { text: String, attrs: Vec<Attr> },
    Blob {
        hash: BlobHash,
        mime: String,
        width: Option<u32>,
        height: Option<u32>,
        /// **Attributes live on the node, whatever shape its content is.** A
        /// blob node carried none until Tasks: an image and a file are
        /// task-capable (SPEC §4), task state *is* attributes, and a due that
        /// could be written and not read back would be a deadline that
        /// vanished. There are no pills here — a pill is an anchor in text, and
        /// a blob has none — so these are the whole-node attributes: a due, a
        /// priority, a tag, a task marker.
        attrs: Vec<Attr>,
    },
    /// A reference placed in the tree — the block-level counterpart of an
    /// inline pill. This is how a profile-library image appears inside a task.
    Embed { target: Ref },
}

impl Content {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            attrs: Vec::new(),
        }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    /// A blob whose bytes live in the store, carrying no attributes yet —
    /// the shape [`Node::image`] and [`Node::file`] build.
    pub fn blob(
        hash: BlobHash,
        mime: impl Into<String>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Self {
        Self::Blob {
            hash,
            mime: mime.into(),
            width,
            height,
            attrs: Vec::new(),
        }
    }

    pub fn attrs(&self) -> &[Attr] {
        match self {
            Self::Text { attrs, .. } | Self::Blob { attrs, .. } => attrs,
            Self::Embed { .. } => &[],
        }
    }

    fn shape(&self) -> Option<Shape> {
        match self {
            Self::Text { .. } => Some(Shape::Text),
            Self::Blob { .. } => Some(Shape::Blob),
            // An embed borrows whatever shape its target has.
            Self::Embed { .. } => None,
        }
    }
}

#[derive(Clone, PartialEq, Debug)]
pub struct Node {
    pub id: NodeId,
    pub kind: Kind,
    pub meta: NodeMeta,
    pub content: Content,
}

/// Why a node or an operation was rejected. Returned rather than panicking, so
/// a malformed op from a third-party tool cannot take the app down.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ShapeError {
    /// e.g. an `Image` carrying `Content::Text`.
    WrongContent { kind: Kind, expected: Shape },
    /// e.g. adding a child to an `Image`, `Table` or `Contact`.
    ChildrenNotAllowed { kind: Kind },
    /// Text nodes hold one line; structure comes from the tree.
    NewlineInText,
    /// A textless text-shape kind (a `Divider`) was given non-empty text.
    NonEmptyText { kind: Kind },
}

impl std::fmt::Display for ShapeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongContent { kind, expected } => {
                write!(f, "{} requires {:?} content", kind.as_str(), expected)
            }
            Self::ChildrenNotAllowed { kind } => {
                write!(f, "{} cannot have children", kind.as_str())
            }
            Self::NewlineInText => {
                write!(f, "a text node holds one line; use a sibling node instead")
            }
            Self::NonEmptyText { kind } => {
                write!(f, "{} carries no text", kind.as_str())
            }
        }
    }
}

impl std::error::Error for ShapeError {}

impl Node {
    /// Build a node, rejecting illegal kind/content combinations.
    pub fn new(kind: Kind, content: Content) -> Result<Self, ShapeError> {
        Self::check(kind, &content)?;
        Ok(Self {
            id: NodeId::new(),
            kind,
            meta: NodeMeta::default(),
            content,
        })
    }

    /// A task line — the overwhelmingly common case.
    pub fn task(text: impl Into<String>) -> Result<Self, ShapeError> {
        Self::new(Kind::Task, Content::text(text))
    }

    /// A line of a document's body.
    pub fn paragraph(text: impl Into<String>) -> Result<Self, ShapeError> {
        Self::new(Kind::Paragraph, Content::text(text))
    }

    /// A quotation line in a document.
    pub fn quote(text: impl Into<String>) -> Result<Self, ShapeError> {
        Self::new(Kind::Quote, Content::text(text))
    }

    /// A callout box, empty: its lines are inserted under it. `icon` is one
    /// emoji (`None` is the default) and `tint` an `AccentColor` token
    /// (`None` is the default) — see [`crate::callout`].
    pub fn callout(icon: Option<&str>, tint: Option<&str>) -> Result<Self, ShapeError> {
        let mut node = Self::new(Kind::Callout, Content::text(""))?;
        node.meta.icon = icon.filter(|icon| !icon.is_empty()).map(str::to_owned);
        node.meta.colour = tint.filter(|tint| !tint.is_empty()).map(str::to_owned);
        Ok(node)
    }

    /// A code block. Its text may span many lines — newlines are legal for
    /// [`Kind::Code`], and parley treats each as a mandatory break inside one
    /// `Layout`. The grammar is set separately via `NodeMeta::language`.
    pub fn code(text: impl Into<String>) -> Result<Self, ShapeError> {
        Self::new(Kind::Code, Content::text(text))
    }

    /// A GFM Markdown table. Its source is canonical; the editable grid is a
    /// derived projection owned by the editor.
    pub fn table(source: impl Into<String>) -> Result<Self, ShapeError> {
        Self::new(Kind::Table, Content::text(source))
    }

    /// A document. Its text is the **title**; its children are the body.
    pub fn document(title: impl Into<String>) -> Result<Self, ShapeError> {
        Self::new(Kind::Document, Content::text(title))
    }

    /// An opaque workspace file. The caller sets `meta.name` to the original
    /// filename; dimensions are deliberately absent because a File is not an
    /// image block even when its MIME type is an image.
    pub fn file(
        name: impl Into<String>,
        hash: BlobHash,
        mime: impl Into<String>,
    ) -> Result<Self, ShapeError> {
        let mut node = Self::new(Kind::File, Content::blob(hash, mime, None, None))?;
        node.meta.name = Some(name.into());
        Ok(node)
    }

    /// A list container. Its own text is empty — the node's label comes from
    /// `meta.name`, the way an image's does, and its children are the items.
    /// Set the style with `WorkspaceDoc::set_list_style` after inserting.
    pub fn list_block() -> Result<Self, ShapeError> {
        Self::new(Kind::ListBlock, Content::text(""))
    }

    /// A horizontal rule — a leaf block with no text and no children. Insert one
    /// between paragraphs to break a document into sections.
    pub fn divider() -> Result<Self, ShapeError> {
        Self::new(Kind::Divider, Content::text(""))
    }

    /// The hidden trash root. Its text is empty and stays empty — nothing shows
    /// it, so there is nothing for a name to label.
    ///
    /// Do not build one directly: `WorkspaceDoc` creates it lazily, the first
    /// time something is trashed, and unions duplicates if two peers made one at
    /// once. This constructor exists so that creation has one shape.
    pub fn trash() -> Result<Self, ShapeError> {
        Self::new(Kind::Trash, Content::text(""))
    }

    /// An image. The bytes live in the blob store, addressed by `hash`; the node
    /// holds only the hash, mime type and dimensions — never the bytes
    /// ([ADR-12](../../../docs/decisions/ADR-001-025.md#adr-12--blob-bytes-never-enter-the-crdt)).
    pub fn image(
        hash: BlobHash,
        mime: impl Into<String>,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<Self, ShapeError> {
        Self::new(Kind::Image, Content::blob(hash, mime, width, height))
    }

    pub(crate) fn check(kind: Kind, content: &Content) -> Result<(), ShapeError> {
        if let Content::Text { text, .. } = content {
            // One line per node is the invariant the whole editor rests on:
            // one parley Layout per node, and Enter makes a sibling. Code (and
            // table markdown source) are the deliberate exceptions — see
            // `Kind::allows_newlines`.
            if !kind.allows_newlines() && text.contains('\n') {
                return Err(ShapeError::NewlineInText);
            }
            // A divider is text-shaped so it can sit in a body, but it carries
            // no text of its own — a rule with prose on it is not a rule. A
            // callout is a container the same way: its lines are its
            // children, and text on the box itself would be a line nothing
            // can edit.
            if matches!(kind, Kind::Divider | Kind::Callout) && !text.is_empty() {
                return Err(ShapeError::NonEmptyText { kind });
            }
        }
        match content.shape() {
            // Embeds are legal on any kind.
            None => Ok(()),
            Some(shape) if shape == kind.shape() => Ok(()),
            Some(_) => Err(ShapeError::WrongContent {
                kind,
                expected: kind.shape(),
            }),
        }
    }

    /// Whether `child` may be placed under this node.
    pub fn may_adopt(&self) -> Result<(), ShapeError> {
        if self.kind.accepts_children() {
            Ok(())
        } else {
            Err(ShapeError::ChildrenNotAllowed { kind: self.kind })
        }
    }

    pub fn with_meta(mut self, meta: NodeMeta) -> Self {
        self.meta = meta;
        self
    }

    pub fn with_id(mut self, id: NodeId) -> Self {
        self.id = id;
        self
    }

    /// The attribute filling `role`, if any — "due", "assignee".
    pub fn attr(&self, role: &str) -> Option<&Attr> {
        self.content.attrs().iter().find(|a| a.role == role)
    }

    pub fn attrs_with_role<'a>(&'a self, role: &'a str) -> impl Iterator<Item = &'a Attr> {
        self.content.attrs().iter().filter(move |a| a.role == role)
    }

    /// The attribute carrying this node's due value — `koru.due` first, then
    /// the pre-registry bare `"due"` role older documents wrote.
    fn due_attr(&self) -> Option<&Attr> {
        self.attr(crate::attributes::KEY_DUE)
            .or_else(|| self.attr("due"))
    }

    /// This node's due, as whichever of the three temporal types it is.
    ///
    /// **It answers the value, not a date**, and that is the fix for the sharp
    /// edge this used to have: `due()` returned `Option<NaiveDate>` and a
    /// timed due came back as `None` — indistinguishable from *no due at all*,
    /// so every caller that only wanted a day silently dropped half the
    /// deadlines in the workspace. Now a caller matches, and the case it does
    /// not handle is a compile error.
    ///
    /// A zoned due's calendar day is deliberately not answerable here:
    /// `koru-core` carries no timezone database, and the UTC day is the wrong
    /// day for a third of the planet. Ask
    /// [`ZonedDateTime::date_in`](crate::time::ZonedDateTime::date_in) with a
    /// zone in hand.
    pub fn due(&self) -> Option<Temporal> {
        self.due_attr().and_then(Attr::temporal)
    }

    /// The due as a **calendar date**, when it is one — or when it is a
    /// floating wall clock, whose day needs no zone.
    ///
    /// A *zoned* due answers `None`, because its day depends on where it is
    /// read. Named so the refusal is visible at the call site, unlike the old
    /// `due()`.
    pub fn due_date_only(&self) -> Option<DateOnly> {
        self.due().and_then(|t| t.zone_free_date())
    }

    /// The tags on this node, as ids. Names live in the workspace system
    /// document; a node stores only the stable [`TagId`].
    ///
    /// [`TagId`]: crate::ids::TagId
    pub fn tag_ids(&self) -> impl Iterator<Item = crate::ids::TagId> + '_ {
        self.attrs_with_role(crate::attributes::KEY_TAG)
            .filter_map(|a| a.as_text().and_then(crate::ids::TagId::parse))
    }

    /// Which list this node is in, as an id — or `None`, which is the normal
    /// state. Names, colors and order live in the workspace system document;
    /// a node stores only the stable [`ListId`], and an id that table has
    /// forgotten resolves to no list rather than to a broken one.
    ///
    /// [`ListId`]: crate::ids::ListId
    pub fn list_id(&self) -> Option<crate::ids::ListId> {
        self.attr(crate::attributes::KEY_LIST)
            .and_then(Attr::as_text)
            .and_then(crate::ids::ListId::parse)
    }

    /// This node's priority, or `None` — and unset is the normal state, never
    /// a fifth token.
    pub fn priority(&self) -> Option<crate::attributes::Priority> {
        self.attr(crate::attributes::KEY_PRIORITY)
            .and_then(Attr::as_text)
            .and_then(crate::attributes::Priority::parse)
    }

    /// This node's accent color token, as stored. Presentation resolves the
    /// token to a theme color; nothing here is a hex value. A token from a
    /// newer build's palette is still returned — round-tripping it beats
    /// dropping it.
    pub fn color_token(&self) -> Option<&str> {
        self.attr(crate::attributes::KEY_COLOR)
            .and_then(Attr::as_text)
    }

    pub fn assignees(&self) -> impl Iterator<Item = Ref> + '_ {
        self.attrs_with_role("assignee")
            .filter_map(Attr::as_ref_target)
    }
}

#[cfg(test)]
mod tests {
    use chrono::NaiveDate;

    use super::*;

    #[test]
    fn only_task_paragraph_document_and_listblock_nest() {
        for kind in [Kind::Task, Kind::Paragraph, Kind::Document, Kind::ListBlock] {
            assert!(kind.accepts_children(), "{kind:?} should nest");
        }
        for kind in [
            Kind::Table,
            Kind::Code,
            Kind::Quote,
            Kind::Image,
            Kind::Canvas,
            Kind::Contact,
            Kind::Divider,
        ] {
            assert!(!kind.accepts_children(), "{kind:?} must not nest");
        }
    }

    #[test]
    fn a_table_is_text_but_a_leaf() {
        let t = Node::new(Kind::Table, Content::text("| a | b |")).unwrap();
        assert_eq!(
            t.may_adopt(),
            Err(ShapeError::ChildrenNotAllowed { kind: Kind::Table })
        );
    }

    #[test]
    fn a_divider_is_an_empty_leaf() {
        let d = Node::divider().unwrap();
        assert_eq!(d.kind, Kind::Divider);
        assert_eq!(d.content.as_text(), Some(""));
        assert!(!d.kind.accepts_children());
        assert!(!d.kind.is_task_like());
    }

    #[test]
    fn a_quote_is_a_text_leaf() {
        let quote = Node::quote("Trust everything else is handled.").unwrap();
        assert_eq!(quote.kind, Kind::Quote);
        assert_eq!(
            quote.content.as_text(),
            Some("Trust everything else is handled.")
        );
        assert!(!quote.kind.accepts_children());
        assert_eq!(Kind::parse(quote.kind.as_str()), Some(Kind::Quote));
    }

    #[test]
    fn a_divider_rejects_text() {
        assert_eq!(
            Node::new(Kind::Divider, Content::text("no prose on a rule")).unwrap_err(),
            ShapeError::NonEmptyText {
                kind: Kind::Divider
            }
        );
    }

    #[test]
    fn a_divider_is_text_shaped_so_it_can_carry_an_embed() {
        let target = Ref::new(NodeId::new());
        assert!(Node::new(Kind::Divider, Content::Embed { target }).is_ok());
    }

    #[test]
    fn blob_kinds_reject_text_content() {
        let err = Node::new(Kind::Image, Content::text("not a blob")).unwrap_err();
        assert_eq!(
            err,
            ShapeError::WrongContent {
                kind: Kind::Image,
                expected: Shape::Blob
            }
        );
    }

    #[test]
    fn text_kinds_reject_blob_content() {
        let blob = Content::blob(BlobHash("9f2a".into()), "image/png", None, None);
        let err = Node::new(Kind::Task, blob).unwrap_err();
        assert_eq!(
            err,
            ShapeError::WrongContent {
                kind: Kind::Task,
                expected: Shape::Text
            }
        );
    }

    /// An embed replaces the node's content with a pointer, so it is legal
    /// regardless of the kind's usual shape.
    #[test]
    fn embeds_are_legal_on_any_kind() {
        let target = Ref::new(NodeId::new());
        for kind in [Kind::Task, Kind::Image, Kind::Document, Kind::Table] {
            assert!(
                Node::new(kind, Content::Embed { target }).is_ok(),
                "{kind:?} should accept an embed"
            );
        }
    }

    /// One line per node is what keeps one parley Layout per node working.
    #[test]
    fn text_nodes_reject_newlines() {
        assert_eq!(
            Node::task("two\nlines").unwrap_err(),
            ShapeError::NewlineInText
        );
        assert!(Node::task("one line").is_ok());
    }

    /// A code block is the deliberate exception: its text is many lines, and
    /// parley breaks on each `\n` inside one `Layout` rather than one node per
    /// line. Table markdown source is the same.
    #[test]
    fn code_and_table_allow_newlines() {
        assert!(Node::code("fn main() {\n    println!(\"hi\")\n}").is_ok());
        assert!(Node::new(Kind::Table, Content::text("| a |\n|---|\n| 1 |")).is_ok());
        // A single-line code block is fine too.
        assert!(Node::code("let x = 1;").is_ok());
        // Code is still Text-shape: blob content is rejected, newline or not.
        let err = Node::new(
            Kind::Code,
            Content::blob(BlobHash("h".into()), "image/png", None, None),
        )
        .unwrap_err();
        assert_eq!(
            err,
            ShapeError::WrongContent {
                kind: Kind::Code,
                expected: Shape::Text
            }
        );
    }

    #[test]
    fn a_group_is_just_a_task_with_a_level_name() {
        let group = Node::task("Phase 1").unwrap().with_meta(NodeMeta {
            level_name: Some("Phase".into()),
            icon: Some("🚀".into()),
            ..Default::default()
        });
        assert_eq!(
            group.kind,
            Kind::Task,
            "no separate 'group' or 'project' kind"
        );
        assert!(group.may_adopt().is_ok());
        assert_eq!(group.meta.level_name.as_deref(), Some("Phase"));
    }

    #[test]
    fn attributes_are_reachable_without_touching_the_text() {
        let due = DateOnly::new(NaiveDate::from_ymd_opt(2026, 8, 2).unwrap());
        let kim = Ref::new(NodeId::new());
        let node = Node {
            content: Content::Text {
                text: "Reward flow".into(),
                attrs: vec![
                    Attr::new("due", AttrValue::Date(due)),
                    Attr::new("assignee", AttrValue::Ref(kim)),
                ],
            },
            ..Node::task("").unwrap()
        };

        assert_eq!(node.due(), Some(Temporal::Date(due)));
        assert_eq!(node.due_date_only(), Some(due));
        assert_eq!(node.assignees().collect::<Vec<_>>(), vec![kim]);
        // The text was never parsed to answer either question.
        assert_eq!(node.content.as_text(), Some("Reward flow"));
    }

    /// Two dates on one task are a start and a due date, told apart by role.
    #[test]
    fn role_distinguishes_two_attributes_of_the_same_type() {
        let start = DateOnly::new(NaiveDate::from_ymd_opt(2026, 7, 28).unwrap());
        let due = DateOnly::new(NaiveDate::from_ymd_opt(2026, 8, 2).unwrap());
        let node = Node {
            content: Content::Text {
                text: "Reward flow".into(),
                attrs: vec![
                    Attr::new("start", AttrValue::Date(start)),
                    Attr::new("due", AttrValue::Date(due)),
                ],
            },
            ..Node::task("").unwrap()
        };
        assert_eq!(node.due(), Some(Temporal::Date(due)));
        assert_eq!(node.attr("start").and_then(Attr::as_date), Some(start));
    }

    #[test]
    fn kind_round_trips_through_storage() {
        for kind in [
            Kind::Task,
            Kind::Paragraph,
            Kind::Document,
            Kind::ListBlock,
            Kind::Table,
            Kind::Code,
            Kind::Image,
            Kind::Canvas,
            Kind::Contact,
            Kind::Divider,
            Kind::Trash,
        ] {
            assert_eq!(Kind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(Kind::parse("nonsense"), None);
    }

    /// The trash root nests — everything binned lives under it — and it is
    /// text-shaped with no text, like a divider. If it stopped nesting, every
    /// `move_node` into it would be refused by `may_adopt`.
    #[test]
    fn the_trash_root_is_an_empty_container() {
        let t = Node::trash().unwrap();
        assert_eq!(t.kind, Kind::Trash);
        assert_eq!(t.content.as_text(), Some(""));
        assert!(t.kind.accepts_children());
        assert!(!t.kind.is_task_like());
        assert!(t.may_adopt().is_ok());
    }

    /// An older build reads `"trash"` as an unknown kind, which is what makes it
    /// safe not to bump `SCHEMA_VERSION`: `read_node` refuses the node, so the
    /// trash and everything in it is simply invisible there rather than
    /// half-understood.
    #[test]
    fn an_unknown_kind_name_is_none_rather_than_a_default() {
        assert_eq!(Kind::parse("wastebasket"), None);
        assert_eq!(Kind::parse(""), None);
    }

    /// The enum's order and the number a snapshot stores must not drift apart:
    /// they are the same fact written twice.
    #[test]
    fn every_heading_level_round_trips_through_its_number() {
        for heading in Heading::ALL {
            assert_eq!(Heading::from_level(heading.level() as i64), Some(heading));
        }
    }

    /// A level this build does not know is still a deliberate heading, so it
    /// clamps to the deepest one that can be drawn rather than reading back as
    /// prose. Zero and below was never a heading at all.
    #[test]
    fn a_heading_level_from_a_newer_peer_clamps_to_the_deepest_one_this_build_draws() {
        assert_eq!(Heading::from_level(9), Some(Heading::H6));
        assert_eq!(Heading::from_level(0), None);
        assert_eq!(Heading::from_level(-1), None);
    }

    /// **A blob node carries attributes**, and used to carry none.
    ///
    /// The change is Tasks': an image and a file are task-capable (SPEC §4),
    /// task state is attributes, and a due that could be written and not read
    /// back is a deadline that vanished. What a blob still has none of is
    /// *text*, so it has no pills either — these are whole-node attributes.
    #[test]
    fn blob_nodes_carry_attributes_but_no_text() {
        let mut img = Node::new(
            Kind::Image,
            Content::blob(BlobHash("9f2a".into()), "image/png", Some(64), Some(64)),
        )
        .unwrap();
        assert!(img.content.attrs().is_empty(), "none to begin with");
        assert_eq!(img.content.as_text(), None);

        if let Content::Blob { attrs, .. } = &mut img.content {
            attrs.push(Attr::new(
                crate::attributes::KEY_DUE,
                AttrValue::Date(DateOnly::new(NaiveDate::from_ymd_opt(2026, 9, 10).unwrap())),
            ));
        }
        assert_eq!(
            img.due(),
            Some(Temporal::Date(DateOnly::new(
                NaiveDate::from_ymd_opt(2026, 9, 10).unwrap()
            )))
        );
    }

    /// The image constructor is the one callers reach for; it builds the same
    /// node the long form does.
    #[test]
    fn image_constructor_builds_a_blob_node() {
        let img = Node::image(BlobHash("9f2a".into()), "image/png", Some(64), Some(64)).unwrap();
        assert_eq!(img.kind, Kind::Image);
        assert!(img.may_adopt().is_err(), "an image is a leaf");
        match &img.content {
            Content::Blob {
                hash,
                mime,
                width,
                height,
                ..
            } => {
                assert_eq!(hash.0, "9f2a");
                assert_eq!(mime, "image/png");
                assert_eq!((*width, *height), (Some(64), Some(64)));
            }
            other => panic!("not a blob: {other:?}"),
        }
    }
}
