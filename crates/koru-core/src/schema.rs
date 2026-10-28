// SPDX-License-Identifier: MPL-2.0

//! Mapping the domain model onto Loro containers.
//!
//! ```text
//! Workspace doc
//! ├── meta      LoroMap    name, profile_id, schema_version
//! ├── library   LoroTree   workspace-scoped nodes
//! └── outline   LoroTree   task nodes, nested arbitrarily
//! ```
//!
//! Each tree node's `get_meta()` map holds the envelope — `id`, `kind`, the
//! [`NodeMeta`] fields — plus its content, which is a child `LoroText` for text
//! kinds or scalars for blobs and embeds.
//!
//! **Identity is a UUID in the meta map, not Loro's `TreeID`.** A `TreeID` is
//! `{peer, counter}` and is only meaningful within one document, so it would
//! change the moment a node were promoted from a workspace into a profile
//! library — breaking every reference to it. `NodeId` survives the move, which
//! is the whole point of scope-as-position.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use loro::{
    ExportMode, LoroDoc, LoroError, LoroMap, LoroValue, TreeID, TreeParentId, ValueOrContainer,
};

use crate::change::ChangeTracker;

use crate::ids::{AttrId, DocRef, DocumentId, NodeId, ProfileId, Ref, WorkspaceId};
use crate::node::{
    Align, Attr, AttrValue, BlobHash, CANVAS_LIMIT, CanvasExtent, CanvasPoint, Content, Heading,
    Kind, ListStyle, Node, NodeMeta, ShapeError,
};
use crate::query::{ResolvedRef, TaskQuery, TaskSummary, label_of, matches_node};
use crate::table::TableLayout;
use crate::text::{
    ANCHOR_LEN, ANCHOR_STR, HIGHLIGHT_KEY, Mark, MarkSet, PILL_KEY, SCRIPT_KEY, SIZE_KEY, Script,
    TextRun, TextStyle, Utf8Pos, VALUE_KEYS, normalise,
};

// Root containers.
pub(crate) const C_META: &str = "meta";
/// The document's one node tree.
///
/// There were two — `outline` and `library` — and a node's *realm* was part of
/// its identity because `mov_to` cannot cross Loro trees. The outline was the
/// first experiment and is gone; what is left is one document's worth of nodes,
/// and the workspace's list of documents moved out to the store's manifest,
/// where it is SQL rather than a CRDT tree.
const C_NODES: &str = "nodes";
pub(crate) const C_LIBRARY: &str = "library";
const C_OUTLINE: &str = "outline";

// Node meta keys.
pub(crate) const K_ID: &str = "id";
/// A document's own id, distinct from its node's.
///
/// The same key name the store's manifest uses, so the two spell the same fact
/// the same way. Only a [`Kind::Document`] node carries one — see
/// [`WorkspaceDoc::document_id`].
const K_DOC_ID: &str = "doc_id";
const K_KIND: &str = "kind";
const K_TEXT: &str = "text";
pub(crate) const K_ICON: &str = "icon";
const K_LEVEL: &str = "level_name";
const K_COLOUR: &str = "colour";
const K_DONE: &str = "done";
const K_BLOB: &str = "blob";
const K_MIME: &str = "mime";
const K_WIDTH: &str = "width";
const K_HEIGHT: &str = "height";
const K_EMBED: &str = "embed";
const K_ATTRS: &str = "attrs";
const K_ALIGN: &str = "align";
/// The block's heading level, stored as the number 1–6 rather than a name.
const K_HEADING: &str = "heading";
/// A `Kind::ListBlock` container's list style, stored as a name
/// (`bullet`/`checkbox`/…). Meaningful only on a ListBlock; absent elsewhere.
const K_LIST_STYLE: &str = "list_style";
/// The size an image is drawn at, when the user has resized it. `K_IMG_W`/`H`.
const K_IMG_W: &str = "imgw";
const K_IMG_H: &str = "imgh";
/// The grammar a code block highlights as (`"rust"`, `"py"`, …). Absent means
/// auto-detect. The highlight colour itself is derived, never stored.
const K_LANG: &str = "lang";
/// The language a document's *prose* is written in (BCP 47), the spelling
/// override collaborators share. **Not** [`K_LANG`], which is a code block's
/// grammar — reusing it would collide the two meanings on one key. Absent is
/// the normal state and means "the reader's own default"; stored as absent so
/// every document written before spell check reads back unchanged, with no
/// schema bump (spell-check plan D4).
const K_PROSE_LANGUAGE: &str = "prose_language";
/// A table's width policy. Absent means compact.
const K_TABLE_LAYOUT: &str = "table_layout";
/// When this node was moved to the trash, unix ms. Absent on everything that has
/// never been binned. Whether a node *is* in the trash is answered by position,
/// not by this — see [`WorkspaceDoc::in_trash`].
const K_TRASHED_AT: &str = "trashed_at";
/// The parent a trashed node came from, so a restore can put it back. Absent
/// means the top of the library. No sibling index is stored beside it.
const K_ORIGIN: &str = "origin";
/// A self-declared display label for whoever binned it — **not** an identity and
/// never derived from a peer id or a peer file name (ADR-39). Absent is normal.
const K_TRASHED_BY: &str = "trashed_by";
/// The same, for whoever restored it. Cleared on the next trash, so exactly one
/// of the two is ever live.
const K_RESTORED_BY: &str = "restored_by";
/// Where a canvas object sits: **one whole `LoroValue::Map`** `{x, y}` of
/// `I64`s, never two keys. A map *value* (not a container) is last-writer-wins
/// as a whole, so two devices moving one card at once converge on one of the
/// two positions rather than on one's `x` and the other's `y` — the reason
/// this does not follow [`K_IMG_W`]/[`K_IMG_H`]. The first structured values
/// in node meta; the pattern is `koru-store-core`'s personal attributes.
/// Absent on every page node, so no schema bump (the `prose_language`
/// precedent).
const K_AT: &str = "at";
/// How large a canvas object is drawn: one whole map `{w[, h]}`, separate from
/// [`K_AT`] so a move and a width change made at once both survive.
const K_EXTENT: &str = "extent";
// Fields inside the two canvas values. A reader requires `x` and `y` (and
// `w`) and ignores any field it does not know, so a later build can add one
// without this build reading the whole value as malformed.
const F_X: &str = "x";
const F_Y: &str = "y";
const F_W: &str = "w";
const F_H: &str = "h";

// Keys inside one attribute's record.
const K_ATTR_ROLE: &str = "role";
const K_ATTR_TYPE: &str = "type";
const K_ATTR_VALUE: &str = "value";
const K_ATTR_HOME: &str = "home";
const K_ATTR_TARGET: &str = "target";
const K_ATTR_LABEL: &str = "label";
/// The IANA timezone beside a `datetime` value's instant. Absent when the
/// value carries no timezone semantics; never an offset (D3, ADR-176).
const K_ATTR_TZ: &str = "tz";

// Workspace meta keys.
pub(crate) const K_NAME: &str = "name";
pub(crate) const K_SCHEMA: &str = "schema_version";
/// **The document this `LoroDoc` *is*** — the id the store's manifest lists it
/// under, stamped in by whoever opened it ([`WorkspaceDoc::stamp_document_id`]).
///
/// Beside [`K_ID`], which is the workspace, and for the same reason: a document
/// the store made arrives empty, so nothing inside it knows either id until the
/// side that asked for it says so. The node-level `doc_id` is a different key on
/// a different map — that one is a *document node*'s own id, and for anything
/// below a root it is derived rather than stored.
const K_DOCUMENT: &str = "document";

pub const SCHEMA_VERSION: i64 = 5;

/// What a document carrying no marker *is*.
///
/// Absence is a version, not an absence of one: 1 is all there has ever been
/// below the marker, so a reader that has to answer "which version is this"
/// answers 1 rather than null. [`Compatibility::of`] already reads it that way;
/// this is the same rule for anything that reports the number instead of
/// gating a write on it. See [`WorkspaceDoc::effective_schema_version`].
pub const LEGACY_SCHEMA_VERSION: i64 = 1;

/// Whether this build understands the document it has just opened.
///
/// The field has been written since the beginning and read by nothing, which
/// was survivable while one program wrote it. It stops being survivable once a
/// released build and a build from this tree take turns over one directory: an
/// older build opening a newer document sees the containers it knows about, is
/// perfectly happy, and then **re-exports the whole document** on the next
/// save. Whatever the newer build added is not lost by Loro — it is lost by the
/// older build having no idea it was there.
///
/// So the older build does not write. Read-only is a smaller failure than a
/// silent downgrade, and it is one the user can see and act on.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Compatibility {
    /// Same version, or a legacy document with no version recorded.
    Writable,
    /// Written by a build newer than this one. Open it, do not save it.
    TooNew {
        /// What the document says it is.
        found: i64,
        /// What this build understands.
        understood: i64,
    },
}

impl Compatibility {
    /// The general form: what the document says against what this build knows.
    ///
    /// Takes `understood` rather than reading [`SCHEMA_VERSION`] because there
    /// is more than one document type now, and each versions itself
    /// independently — a build that understands workspaces at version 2 and
    /// profiles at version 1 is a real, safe state, and one shared constant
    /// could not express it.
    pub fn against(found: Option<i64>, understood: i64) -> Self {
        match found {
            Some(v) if v > understood => Self::TooNew {
                found: v,
                understood,
            },
            // A version *older* than this build is where a migration will go.
            // There has only ever been one, so there is nothing to migrate yet
            // and nothing to refuse.
            _ => Self::Writable,
        }
    }

    /// The workspace document's gate. `None` means the field predates itself —
    /// a version 1 document from before the key was written, not an unknown
    /// future one.
    pub fn of(found: Option<i64>) -> Self {
        Self::against(found, SCHEMA_VERSION)
    }

    pub fn writable(self) -> bool {
        matches!(self, Self::Writable)
    }

    /// What to tell the user, naming the kind of document this is about.
    ///
    /// The kind is a parameter because the sentence is shown for a workspace
    /// and for a profile, and "this workspace" in a dialog about a profile is
    /// the kind of wrongness that sends somebody looking in the wrong place.
    pub fn describe(&self, what: &str) -> String {
        match self {
            Self::Writable => "writable".into(),
            Self::TooNew { found, understood } => format!(
                "this {what} was written by a newer version of Koru \
                 (schema {found}; this build understands {understood})"
            ),
        }
    }
}

impl std::fmt::Display for Compatibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.describe("workspace"))
    }
}

#[derive(Debug)]
pub enum StoreError {
    Loro(LoroError),
    /// `export` has its own error type, distinct from the rest of the API.
    Encode(loro::LoroEncodeError),
    /// `LoroText::update` diffs old against new and can time out on pathological
    /// input; it is not a normal `LoroError`.
    TextUpdate(loro::UpdateTimeoutError),
    Shape(ShapeError),
    /// The node exists in the tree but its meta is missing or malformed.
    Malformed(&'static str),
    /// No node with that id in this document.
    NoSuchNode(NodeId),
    /// Nothing in this workspace carries that [`DocumentId`].
    ///
    /// Distinct from [`NoSuchNode`](Self::NoSuchNode) because the two mean
    /// different things to a caller: a missing node is usually a stale id from
    /// before a delete, where a missing *document* is an Explore entry that no
    /// longer names anything.
    NoSuchDocument(DocumentId),
    /// A trash operation was asked for on something that is not a top-level
    /// trash item — a live node, or something *inside* a trashed subtree.
    ///
    /// It exists so that `delete_permanently` cannot be mis-wired into a plain
    /// delete: a button that skipped the trash would be the one unrecoverable
    /// bug this whole feature is organised against.
    NotInTrash(NodeId),
    /// [`WorkspaceDoc::trash`] was asked to bin something already in the bin, or
    /// the trash root itself.
    AlreadyInTrash(NodeId),
    /// [`WorkspaceDoc::set_attribute`] was asked for a key that is not a
    /// system attribute. A custom key goes through
    /// [`WorkspaceDoc::set_attribute_defined`] with its definition in hand.
    UnknownAttribute(String),
    /// The value offered under a key does not fit its definition — wrong
    /// type, a token outside a select's options, or a personal-scope key
    /// offered to shared node storage. `why` phrases the reason and never
    /// quotes the value, which may be user content.
    AttributeRejected {
        key: String,
        why: String,
    },
    /// A canonical task command was pointed at something it cannot act on —
    /// a kind that cannot carry task state, or a node that is not a task and
    /// that this command does not create one on.
    ///
    /// A **token**, not prose ([ADR-286]): MCP maps it and a test asserts on
    /// it, neither of which can be done with a sentence.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    TaskRejected {
        node: NodeId,
        why: crate::tasks::TaskRefusal,
    },
    /// A canvas operation was pointed at something it cannot act on — an
    /// arrow from a card to itself, an agent setting on a plain card, an
    /// image handed to the card constructor. A **token**, not prose, for the
    /// same reason as [`TaskRejected`](Self::TaskRejected): a host maps it and
    /// a test asserts on it. See [`crate::canvas`].
    CanvasRejected {
        node: NodeId,
        why: crate::canvas::CanvasRefusal,
    },
}

impl From<LoroError> for StoreError {
    fn from(e: LoroError) -> Self {
        Self::Loro(e)
    }
}

impl From<loro::LoroEncodeError> for StoreError {
    fn from(e: loro::LoroEncodeError) -> Self {
        Self::Encode(e)
    }
}

impl From<loro::UpdateTimeoutError> for StoreError {
    fn from(e: loro::UpdateTimeoutError) -> Self {
        Self::TextUpdate(e)
    }
}

impl From<ShapeError> for StoreError {
    fn from(e: ShapeError) -> Self {
        Self::Shape(e)
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Loro(e) => write!(f, "loro: {e}"),
            Self::Encode(e) => write!(f, "loro export: {e}"),
            Self::TextUpdate(e) => write!(f, "text update: {e}"),
            Self::Shape(e) => write!(f, "{e}"),
            Self::Malformed(what) => write!(f, "malformed node: {what}"),
            Self::NoSuchNode(id) => write!(f, "no such node {id:?}"),
            Self::NoSuchDocument(id) => write!(f, "no such document {id:?}"),
            Self::NotInTrash(id) => write!(f, "{id:?} is not an item in the trash"),
            Self::AlreadyInTrash(id) => write!(f, "{id:?} is already in the trash"),
            Self::UnknownAttribute(key) => write!(f, "{key} is not a system attribute"),
            Self::AttributeRejected { key, why } => write!(f, "{key} {why}"),
            Self::TaskRejected { node, why } => {
                write!(f, "task command refused on {node:?}: {}", why.as_str())
            }
            Self::CanvasRejected { node, why } => {
                write!(f, "canvas operation refused on {node:?}: {}", why.as_str())
            }
        }
    }
}

impl std::error::Error for StoreError {}

pub type Result<T> = std::result::Result<T, StoreError>;

/// A position inside one text node that survives intervening CRDT edits.
///
/// Koru's editor and Parley address text in UTF-8 bytes; Loro cursors address
/// Unicode scalar positions.  The conversion stays behind `WorkspaceDoc` so a
/// UI cannot accidentally persist or resolve a bare byte offset after a peer
/// has edited earlier text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StableTextPosition {
    node: NodeId,
    cursor: loro::cursor::Cursor,
}

impl StableTextPosition {
    pub fn node(&self) -> NodeId {
        self.node
    }
}

/// The directed endpoints of a text selection.
///
/// `anchor` and `focus` are deliberately not normalised: a backwards
/// selection must still be backwards when an overlay returns focus to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StableTextRange {
    pub anchor: StableTextPosition,
    pub focus: StableTextPosition,
}

/// Where to put a new node.
///
/// Two roots became one. `Root` was the outline's and `Library` was the
/// workspace's; the outline is deleted and the workspace's list of documents
/// lives in the store's manifest now, so what is left is the top of *this*
/// document — and `Root` is the name that survived, because 185 call sites used
/// it against 59 for the other.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Parent {
    /// The top of this document.
    ///
    /// **Its children are trash-filtered**, a rule inherited from the library
    /// root along with the position: the bin is a real `Kind::Trash` child that
    /// [`WorkspaceDoc::children`] hides, and that one exclusion is what keeps it
    /// out of every walk in the tree ([ADR-69](../../../docs/decisions/README.md)).
    Root,
    Node(NodeId),
}

/// One item in the trash: something a person binned, and what the trash screen
/// needs to describe it.
///
/// **Only top-level items.** What went *with* an item stays inside it and earns
/// no row of its own — restoring the parent brings the whole subtree back, and
/// offering to restore a child on its own would promise a placement the model
/// does not have.
///
/// Built by [`WorkspaceDoc::trashed`] rather than by a `read` per row, because
/// `read` materialises a node's text and attributes and a listing does not need
/// them.
#[derive(Clone, PartialEq, Debug)]
pub struct TrashEntry {
    pub node: NodeId,
    pub kind: Kind,
    /// The node's own text, or `meta.name` for kinds that carry none. May be
    /// empty — the UI substitutes "Untitled" the way `name_of` does, because
    /// what an empty title should read as is presentation.
    pub name: String,
    /// Unix ms, as recorded when it was trashed. `None` for an item binned by a
    /// build that did not record it — a state, not an error.
    pub trashed_at: Option<i64>,
    /// A self-declared display label for whoever binned it. `None` is normal;
    /// see [`NodeMeta::trashed_by`](crate::node::NodeMeta::trashed_by).
    pub trashed_by: Option<String>,
    /// Where a restore puts it back. `None` is the top of the library — which is
    /// also what a stale origin resolves to.
    pub origin: Option<NodeId>,
    /// Documents inside it. What the confirm and the toast quote, and what the
    /// row's `· N nested` suffix says.
    pub nested: usize,
}

/// A subtree lifted out of a document, ready to be planted in one.
///
/// Made by [`WorkspaceDoc::cutting`] and used by [`WorkspaceDoc::plant`].
/// A normal planting mints fresh identities, so one cutting can be planted more
/// than once. Identity-preserving paths instead use the captured node and
/// document identities so references, local undo, and legacy manifests stay
/// stable.
///
/// **A reference inside it still points at what it always pointed at.** A pill
/// is a pointer to a node ([ADR-40](../../../docs/decisions/README.md)), so a copy of a
/// page that links a contact links the *same* contact. The case this does not
/// remap is a pill pointing at a node inside the cutting itself — that comes
/// through naming the original rather than the copy, which is what `duplicate`
/// has always done and is worth knowing before relying on either.
pub struct Cutting {
    id: NodeId,
    document_id: Option<DocumentId>,
    kind: Kind,
    content: Content,
    meta: crate::node::NodeMeta,
    /// Inline formatting, as the ranges `runs` reported.
    runs: Vec<TextRun>,
    children: Vec<Cutting>,
}

impl Cutting {
    /// Every blob reference carried by this subtree.
    ///
    /// The bytes are workspace-scoped even though their hashes are content
    /// addresses. A caller planting this cutting in another workspace uses
    /// this list to copy the bytes before it publishes the reference there.
    pub fn blob_hashes(&self) -> Vec<BlobHash> {
        let mut hashes = Vec::new();
        self.collect_blob_hashes(&mut hashes);
        hashes.sort_by(|left, right| left.0.cmp(&right.0));
        hashes.dedup_by(|left, right| left.0 == right.0);
        hashes
    }

    fn collect_blob_hashes(&self, hashes: &mut Vec<BlobHash>) {
        if let Content::Blob { hash, .. } = &self.content {
            hashes.push(hash.clone());
        }
        for child in &self.children {
            child.collect_blob_hashes(hashes);
        }
    }
}

#[cfg(test)]
mod cutting_blob_tests {
    use super::*;

    #[test]
    fn a_cutting_names_nested_blob_bytes_once() {
        let mut doc = WorkspaceDoc::create(WorkspaceId::new(), "source").unwrap();
        let section = doc
            .insert(Parent::Root, Node::paragraph("Pictures").unwrap())
            .unwrap();
        for hash in ["b3:bbb", "b3:aaa", "b3:bbb"] {
            doc.insert(
                Parent::Node(section),
                Node::image(BlobHash(hash.into()), "image/png", None, None).unwrap(),
            )
            .unwrap();
        }

        let cutting = doc.cutting(section).unwrap();
        assert_eq!(
            cutting
                .blob_hashes()
                .into_iter()
                .map(|hash| hash.0)
                .collect::<Vec<_>>(),
            vec!["b3:aaa".to_string(), "b3:bbb".to_string()]
        );
    }
}

/// A workspace: its outline of tasks and its workspace-scoped library.
pub struct WorkspaceDoc {
    doc: LoroDoc,
    /// `NodeId` is the public identity; `TreeID` is Loro's. This maps between
    /// them, and carries the little that is worth caching beside it. Rebuilt
    /// from the tree after any import.
    index: HashMap<NodeId, NodeLocator>,
    /// Nodes whose locators carry a blob hash. Kept separately so servicing
    /// the display cache is O(blob nodes), not O(every paragraph).
    blob_nodes: Vec<NodeId>,
    /// Undo history, once [`enable_undo`] has been called. Off by default so a
    /// headless consumer — a query, a migration — pays nothing for it.
    ///
    /// [`enable_undo`]: WorkspaceDoc::enable_undo
    undo: Option<loro::UndoManager>,
    /// Caret destination waiting to be attached to the next undo item.
    ///
    /// Loro calls the undo manager's `on_push` hook synchronously when that
    /// item is recorded. Keeping this channel beside the manager makes the
    /// destination part of the exact history step, rather than a parallel
    /// stack that would drift as soon as another kind of edit occurred.
    pending_undo_caret: Arc<Mutex<Option<StableTextPosition>>>,
    /// The caret metadata popped by the most recent successful undo, after
    /// Loro has transformed it against the restored text and remote edits.
    popped_undo_caret: Arc<Mutex<Option<StableTextPosition>>>,
    /// Whether an undo group is currently open — see [`begin_group`].
    ///
    /// [`begin_group`]: WorkspaceDoc::begin_group
    grouping: bool,
}

/// Where a node is, and the one thing worth remembering about it.
///
/// **It used to carry a `Realm` too**, because a workspace was two trees and a
/// `TreeID` alone did not say which. One Loro document is one document's nodes
/// now ([ADR-90]) so there is only the one tree, and the field went with the
/// realm.
///
/// `blob_hash` is a *cache of the meta map*, and the point of it: without it,
/// answering "which blobs does this document reference" walks every node and
/// reads each one, which cost 26% of a core on an idle window. The CRDT stays
/// authoritative — this is rebuilt by `reindex` and maintained beside every
/// write that could change it.
///
/// [ADR-90]: ../../../docs/decisions/ADR-076-100.md#adr-90-onetree--one-loro-document-is-one-documents-nodes-and-the-realm-is-gone
#[derive(Clone)]
struct NodeLocator {
    tree: TreeID,
    blob_hash: Option<String>,
}

/// Expansion decides what happens to a mark when text is typed at its
/// boundary. Formatting should grow — type at the end of a bold word and
/// the new letters are bold — but a pill anchor must never grow, or typing
/// beside a pill would swallow the text into it.
///
/// This is document configuration rather than persisted data, so it has to
/// be applied every time a document is opened, not just when it is created.
///
/// A free function rather than a method because the profile document's library
/// holds text nodes too, and it must be configured the same way — one copy of
/// the table, so the two cannot drift into disagreeing about what a pill does.
pub(crate) fn configure_styles(doc: &LoroDoc) {
    let mut styles = loro::StyleConfigMap::new();
    for mark in Mark::ALL {
        styles.insert(
            mark.key().into(),
            loro::StyleConfig {
                expand: loro::ExpandType::After,
            },
        );
    }
    // Value marks expand the same way flags do: type after a highlighted
    // word and the new text is highlighted too, which is what a person
    // expects and what makes turning it off a deliberate act.
    for key in VALUE_KEYS {
        styles.insert(
            key.into(),
            loro::StyleConfig {
                expand: loro::ExpandType::After,
            },
        );
    }
    // Inline code is the one flag that does *not* expand: typing after a
    // `` `code` `` span should leave it, the way markdown closing backticks do,
    // rather than continuing the span. Every other flag inherits rightwards.
    styles.insert(
        Mark::Code.key().into(),
        loro::StyleConfig {
            expand: loro::ExpandType::None,
        },
    );
    styles.insert(
        PILL_KEY.into(),
        loro::StyleConfig {
            expand: loro::ExpandType::None,
        },
    );
    doc.config_text_style(styles);
}

impl WorkspaceDoc {
    pub fn create(id: WorkspaceId, name: &str) -> Result<Self> {
        let doc = LoroDoc::new();
        configure_styles(&doc);
        let meta = doc.get_map(C_META);
        meta.insert(K_ID, id.to_storage())?;
        meta.insert(K_NAME, name)?;
        meta.insert(K_SCHEMA, SCHEMA_VERSION)?;
        doc.commit();
        Ok(Self {
            doc,
            index: HashMap::new(),
            blob_nodes: Vec::new(),
            undo: None,
            pending_undo_caret: Default::default(),
            popped_undo_caret: Default::default(),
            grouping: false,
        })
    }

    /// Rebuild from a snapshot. Also reindexes, since `TreeID`s are only
    /// meaningful relative to the document they came from.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        let decode = crate::timing::Span::start("snapshot decode");
        let doc = LoroDoc::new();
        configure_styles(&doc);
        doc.import(bytes)?;
        decode.stop_with(|| format!("{} KB", bytes.len() / 1024));
        let mut ws = Self {
            doc,
            index: HashMap::new(),
            blob_nodes: Vec::new(),
            undo: None,
            pending_undo_caret: Default::default(),
            popped_undo_caret: Default::default(),
            grouping: false,
        };
        ws.reindex()?;
        Ok(ws)
    }

    /// Adopt a `LoroDoc` somebody else is holding — a fork, most usefully.
    ///
    /// **The read-only counterpart of [`from_snapshot`](Self::from_snapshot),
    /// without the round trip.** Exporting a document and importing it back to
    /// get a `WorkspaceDoc` costs a full serialise and a full parse; a
    /// `LoroDoc::fork` is a structural share, so a reader that already has one
    /// should not pay to turn it into bytes and back. The store's projection
    /// layer is the caller this exists for.
    ///
    /// Undo is off, as it is on every fresh document — a walker has no history
    /// to keep and `enable_undo` is what asks for one.
    pub fn adopt(doc: LoroDoc) -> Result<Self> {
        // Not persisted, so it does not survive a fork and has to be re-applied
        // — the same rule `from_snapshot` follows. It governs how marks expand
        // on edits rather than what a read sees, so a reader that skipped it
        // would look fine until the day something wrote through one.
        configure_styles(&doc);
        let mut ws = Self {
            doc,
            index: HashMap::new(),
            blob_nodes: Vec::new(),
            undo: None,
            pending_undo_caret: Default::default(),
            popped_undo_caret: Default::default(),
            grouping: false,
        };
        ws.reindex()?;
        Ok(ws)
    }

    pub fn name(&self) -> Option<String> {
        as_string(self.doc.get_map(C_META).get(K_NAME)?)
    }

    pub fn id(&self) -> Option<WorkspaceId> {
        WorkspaceId::parse(&as_string(self.doc.get_map(C_META).get(K_ID)?)?)
    }

    /// Rename the workspace.
    ///
    /// The profile's entry for this workspace carries a name too, and a rename
    /// has to write both — the entry is a cache that lets the switcher list
    /// workspaces without opening every snapshot, and **this** is the truth. If
    /// only the entry were written, opening the workspace would show the old
    /// name; if only this were, the switcher would.
    pub fn rename(&mut self, name: &str) -> Result<()> {
        self.doc.get_map(C_META).insert(K_NAME, name)?;
        self.doc.commit();
        Ok(())
    }

    /// What version of the schema wrote this document, **as recorded**.
    ///
    /// `None` has two causes and one meaning. The original is a document
    /// written before the field existed. The other arrived with the store: a
    /// document the Koru Store made is an *empty* `LoroDoc` that the app seeds
    /// nodes into, so [`create`](Self::create) — the only place this key was
    /// ever written — never ran for it, and every document created between the
    /// cutover and [`stamp_schema_version`](Self::stamp_schema_version) has no
    /// marker at all. Both mean version 1: it is what the document holds, since
    /// anything above 1 is written on demand by `record_schema_for_kind`.
    ///
    /// Use [`effective_schema_version`](Self::effective_schema_version) to
    /// *report* the version and [`compatibility`](Self::compatibility) to gate
    /// a write on it. Reach for this one only to tell "recorded" from "not
    /// recorded" — which is what `stamp_schema_version` does.
    pub fn schema_version(&self) -> Option<i64> {
        as_i64(self.doc.get_map(C_META).get(K_SCHEMA)?)
    }

    /// The version to answer with — [`LEGACY_SCHEMA_VERSION`] when none is
    /// recorded.
    ///
    /// A reader outside this process cannot make that substitution for itself
    /// without knowing the rule, so it is made here. MCP publishes the result
    /// as a required integer: `read_document` returned `null` for every
    /// store-created document and strict clients rejected the whole response.
    pub fn effective_schema_version(&self) -> i64 {
        self.schema_version().unwrap_or(LEGACY_SCHEMA_VERSION)
    }

    /// Record this build's schema version on a document that has none.
    ///
    /// **Only ever at creation.** [`create`](Self::create) stamps the document
    /// it makes; this stamps the one the *store* made, which arrives as an
    /// empty `LoroDoc` and is seeded by the app instead. Calling it on a
    /// document merely being *opened* would claim this build wrote content it
    /// only read — precisely the silent downgrade [`Compatibility`] exists to
    /// prevent — so `schema_version().is_none()` is not on its own a licence
    /// to call this.
    ///
    /// A document that already carries a marker keeps it, including one from a
    /// build newer than this one.
    pub fn stamp_schema_version(&mut self) -> Result<()> {
        if self.schema_version().is_none() {
            self.doc.get_map(C_META).insert(K_SCHEMA, SCHEMA_VERSION)?;
            self.doc.commit();
        }
        Ok(())
    }

    /// Record the workspace this document belongs to, on a document that
    /// carries no id yet.
    ///
    /// **The counterpart of [`stamp_schema_version`](Self::stamp_schema_version),
    /// for the other key [`create`](Self::create) writes and the store path
    /// never does.** A document the Koru Store made arrives as an empty
    /// `LoroDoc` the app seeds nodes into, so `create` — the only writer of
    /// [`id`](Self::id) — never ran for it, and *every* document opened from
    /// the store answers `None`. That is not cosmetic: it is step 0 of every
    /// canonical task command's preflight, so a `/due` on real content refused
    /// with `Malformed("the document carries no workspace id")` while every
    /// suite built on `create` passed.
    ///
    /// Unlike the schema version, stamping this at **open** claims nothing
    /// about who wrote the content. The id is a fact about where the document
    /// lives that the store already knows and the caller was handed; it is
    /// never derived from a path, a name or a peer, per DATAMODEL §Identity.
    ///
    /// **CRDT-safe by being idempotent and convergent.** `C_META` is a
    /// last-writer-wins map, and two peers opening the same document stamp the
    /// *same* value — whichever write wins, both replicas read that id. A
    /// document that already carries one keeps it, so a document moved between
    /// workspaces is never restamped by whoever opens it next: the move owns
    /// that key, not the open.
    pub fn stamp_workspace_id(&mut self, id: WorkspaceId) -> Result<()> {
        if self.id().is_none() {
            self.doc.get_map(C_META).insert(K_ID, id.to_storage())?;
            self.doc.commit();
        }
        Ok(())
    }

    /// **The document this `WorkspaceDoc` is** — the id the manifest lists it
    /// under, and the middle segment of every reference minted inside it.
    ///
    /// Two answers, in this order, and the order is the whole of it:
    ///
    /// 1. the id [`stamp_document_id`](Self::stamp_document_id) recorded, which
    ///    is the store's own answer for what this document is;
    /// 2. failing that, the id carried by its **root document node** — a
    ///    document node stores an id of its own ([ADR-91]), and for a tree that
    ///    reached here without a store (a legacy workspace, an importer building
    ///    a document before it is registered) that node *is* the document.
    ///
    /// `None` when there is neither: an in-memory document nothing registered,
    /// which is what a test builds. It is deliberately **not**
    /// [`document_id`](Self::document_id) of the node being addressed — that
    /// answers *this node's* id and derives one per node for anything that is
    /// not a document root, which is how two tasks on one page came to carry two
    /// different document segments ([TECHDEBT 72], now closed).
    ///
    /// One `LoroDoc` is one document ([ADR-90]), so the first `Kind::Document`
    /// child of the root is the whole of step 2 — the same rule
    /// `koru_workspace::document_root` follows, restated here because the
    /// domain may not depend on that crate.
    ///
    /// [ADR-90]: ../../../docs/decisions/ADR-076-100.md#adr-90-onetree--one-loro-document-is-one-documents-nodes-and-the-realm-is-gone
    /// [ADR-91]: ../../../docs/decisions/ADR-076-100.md#adr-91-perdocument--a-document-keeps-a-root-node-for-its-title-and-the-manifest-mirrors-it
    /// [TECHDEBT 57]: ../../../docs/contributing/techdebt/README.md
    pub fn own_document_id(&self) -> Option<DocumentId> {
        let stamped = self
            .doc
            .get_map(C_META)
            .get(K_DOCUMENT)
            .and_then(as_string)
            .as_deref()
            .and_then(DocumentId::parse);
        if stamped.is_some() {
            return stamped;
        }
        self.children(Parent::Root)
            .ok()?
            .into_iter()
            .find(|&id| self.kind_of(id) == Some(Kind::Document))
            .map(|root| self.document_id(root))
    }

    /// Record which document this is, on one that does not say.
    ///
    /// [`stamp_workspace_id`](Self::stamp_workspace_id)'s twin, called from the
    /// same seams and for the same reason: a document the Koru Store made
    /// arrives as an empty `LoroDoc`, so the only side that knows its id is the
    /// one that just asked the store for it by that id. Without this the
    /// document segment of every `task:` reference minted in it would have to be
    /// derived from the node — which is exactly the wart this closes.
    ///
    /// **CRDT-safe the same way**: `C_META` is last-writer-wins, two peers
    /// opening the same document stamp the same value, and a document that
    /// already carries an id keeps it — a document *moved* between workspaces
    /// keeps its identity, and an open is never allowed to rewrite it.
    ///
    /// Answers **whether it wrote**, which
    /// [`stamp_workspace_id`](Self::stamp_workspace_id) does not have to: a
    /// caller that must persist the stamp cannot ask
    /// [`own_document_id`](Self::own_document_id) whether one is needed, because
    /// that question is answered by a *seeded root node* on a document the store
    /// nonetheless knows by another id. The stored key is the only thing that
    /// decides, and this is the one place that reads it.
    pub fn stamp_document_id(&mut self, id: DocumentId) -> Result<bool> {
        if self.doc.get_map(C_META).get(K_DOCUMENT).is_some() {
            return Ok(false);
        }
        self.doc
            .get_map(C_META)
            .insert(K_DOCUMENT, id.to_storage())?;
        self.doc.commit();
        Ok(true)
    }

    /// Whether this build may write to the document it just opened.
    pub fn compatibility(&self) -> Compatibility {
        Compatibility::of(self.schema_version())
    }

    pub fn doc(&self) -> &LoroDoc {
        &self.doc
    }

    /// Freeze the current causal state for read-only background projection.
    /// The fork shares no future mutations or undo manager with this editor,
    /// while retaining stable tree ids and the already-built node index.
    pub fn fork_for_read(&self) -> Self {
        Self {
            doc: self.doc.fork(),
            index: self.index.clone(),
            blob_nodes: self.blob_nodes.clone(),
            undo: None,
            pending_undo_caret: Default::default(),
            popped_undo_caret: Default::default(),
            grouping: false,
        }
    }

    /// Restore a durable snapshot by creating new CRDT operations.
    ///
    /// The live document is never replaced or checked out. When its oplog
    /// still contains the target frontier, Loro can revert directly. A shallow
    /// replica first imports the target snapshot to make that frontier known,
    /// then performs the same forward-moving revert. The returned update is
    /// everything a peer needs to converge on the restored state.
    pub fn restore_snapshot_forward(&mut self, snapshot: &[u8]) -> Result<Vec<u8>> {
        let target = WorkspaceDoc::from_snapshot(snapshot)?;
        if !target.compatibility().writable() {
            return Err(StoreError::Malformed("restore snapshot schema is newer"));
        }
        let target_frontiers = target.doc.oplog_frontiers();
        let target_version = target.doc.oplog_vv();
        let before = self.version();
        self.atomic(|this| {
            if !this.version().includes_vv(&target_version) {
                // A snapshot carries the target's operations and state. Import
                // makes an otherwise compacted frontier addressable; revert_to
                // then writes the actual restoration as this replica's ops.
                this.doc.import(snapshot)?;
            }
            this.doc.revert_to(&target_frontiers)?;
            this.doc.commit();
            this.reindex()
        })?;
        self.export_updates_since(&before)
    }

    /// Capture a UTF-8 byte offset as a Loro cursor stable across peer edits.
    pub fn capture_text_position(&self, target: NodeId, byte: usize) -> Result<StableTextPosition> {
        self.capture_text_position_with_side(target, byte, loro::cursor::Side::Middle)
    }

    fn capture_text_position_with_side(
        &self,
        target: NodeId,
        byte: usize,
        side: loro::cursor::Side,
    ) -> Result<StableTextPosition> {
        let text = self.text_handle(target)?;
        let unicode = text
            .convert_pos(
                byte,
                loro::cursor::PosType::Bytes,
                loro::cursor::PosType::Unicode,
            )
            .ok_or(StoreError::Malformed("byte offset outside the text"))?;
        let cursor = text
            .get_cursor(unicode, side)
            .ok_or(StoreError::Malformed("cannot capture text position"))?;
        Ok(StableTextPosition {
            node: target,
            cursor,
        })
    }

    /// Resolve a stable cursor back to the editor's UTF-8 byte coordinates.
    pub fn resolve_text_position(&self, position: &StableTextPosition) -> Result<usize> {
        let unicode = self
            .doc
            .get_cursor_pos(&position.cursor)
            .map_err(|_| StoreError::Malformed("cannot resolve text position"))?
            .current
            .pos;
        self.text_handle(position.node)?
            .convert_pos(
                unicode,
                loro::cursor::PosType::Unicode,
                loro::cursor::PosType::Bytes,
            )
            .ok_or(StoreError::Malformed("resolved position outside the text"))
    }

    pub fn capture_text_range(
        &self,
        target: NodeId,
        anchor: usize,
        focus: usize,
    ) -> Result<StableTextRange> {
        Ok(StableTextRange {
            anchor: self.capture_text_position(target, anchor)?,
            focus: self.capture_text_position(target, focus)?,
        })
    }

    /// Resolve a directed range without discarding selection direction.
    pub fn resolve_text_range(&self, range: &StableTextRange) -> Result<(usize, usize)> {
        if range.anchor.node != range.focus.node {
            return Err(StoreError::Malformed("text range crosses nodes"));
        }
        Ok((
            self.resolve_text_position(&range.anchor)?,
            self.resolve_text_position(&range.focus)?,
        ))
    }

    fn nodes(&self) -> loro::LoroTree {
        self.doc.get_tree(C_NODES)
    }

    /// Build the one-tree read model used by the legacy workspace importer.
    ///
    /// Workspaces written before ADR-90 have no `nodes` tree: their task rows
    /// live in `outline` and their documents and trash live in `library`.
    /// `LoroDoc::get_tree("nodes")` quite correctly returns an empty tree for
    /// such a snapshot, which is indistinguishable from an empty current
    /// document unless the old roots are checked explicitly.
    ///
    /// The source document is never changed. When legacy roots are present,
    /// this returns a new in-memory document containing the old outline roots
    /// followed by the old library roots, preserving node UUIDs, content,
    /// metadata, formatting and the hidden trash subtree. The caller can then
    /// use the ordinary current-tree read API for preview and conversion.
    pub fn normalized_for_legacy_import(self) -> Result<Self> {
        if tree_has_live_nodes(&self.nodes()) {
            return Ok(self);
        }

        let outline = self.doc.get_tree(C_OUTLINE);
        let library = self.doc.get_tree(C_LIBRARY);
        if !tree_has_live_nodes(&outline) && !tree_has_live_nodes(&library) {
            return Ok(self);
        }

        let id = self
            .id()
            .ok_or(StoreError::Malformed("legacy workspace has no identity"))?;
        let name = self.name().unwrap_or_else(|| "Imported workspace".into());
        let mut normalized = Self::create(id, &name)?;
        for tree in [outline, library] {
            for tid in tree.children(TreeParentId::Root).unwrap_or_default() {
                let cutting = cutting_from_tree(&tree, tid)?;
                normalized.plant_import(&cutting, Parent::Root)?;
            }
        }
        Ok(normalized)
    }

    // ------------------------------------------------------------ sync

    pub fn export_snapshot(&self) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::snapshot())?)
    }

    /// Everything this peer knows — the simplest correct thing to hand another
    /// replica. `export_updates_since` is the incremental form.
    pub fn export_all_updates(&self) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::all_updates())?)
    }

    pub fn export_updates_since(&self, from: &loro::VersionVector) -> Result<Vec<u8>> {
        Ok(self.doc.export(ExportMode::updates(from))?)
    }

    /// This replica's Loro peer id.
    ///
    /// **Identifies a replica, never a person and never a machine.** Loro mints
    /// it randomly per `LoroDoc`, so two windows on one workspace hold two — and
    /// that is precisely what makes it useful for telling them apart while
    /// testing. Nothing is keyed by it and nothing persists it.
    pub fn peer_id(&self) -> u64 {
        self.doc.peer_id()
    }

    pub fn version(&self) -> loro::VersionVector {
        self.doc.oplog_vv()
    }

    /// Merge another replica's changes. Idempotent — Loro's version vectors
    /// discard anything already seen, so re-importing is harmless.
    ///
    /// **Importing many frames? Use [`import_frames`].** This reindexes, and
    /// reindexing is O(the whole document) — so a loop around this is quadratic
    /// in a way nothing at the call site hints at. That loop cost 7.0 of the 9.4
    /// seconds it took to open a real workspace.
    ///
    /// [`import_frames`]: Self::import_frames
    pub fn import(&mut self, bytes: &[u8]) -> Result<()> {
        self.doc.import(bytes)?;
        self.reindex()
    }

    /// Merge many frames, reindexing **once** at the end.
    ///
    /// `import_batch` also lets loro resolve the frames against each other
    /// before applying, so they may arrive in any order — which is what makes
    /// this safe to use over a whole peer log rather than frame by frame.
    ///
    /// An empty slice is a no-op, and deliberately does not reindex: `sync_in`
    /// calls this on every poll, and most polls have nothing.
    pub fn import_frames(&mut self, frames: &[Vec<u8>]) -> Result<()> {
        if frames.is_empty() {
            return Ok(());
        }
        // **One at a time rather than `import_batch`.** Not because the loop is
        // faster — on the reconnect path both are catastrophic and within 10% of
        // each other, 174 frames totalling 20 KB taking 8.9 s batched and 9.8 s
        // here — but because `import_batch` resolves the frames against each
        // other and so cannot say which one was bad, and this shape is no worse.
        //
        // **That per-frame merge cost is an open bug**, not something this
        // function solves: see `plans/FREEZE_DEBUG.md`. What *is* solved here is
        // the reindex, below.
        for frame in frames {
            self.doc.import(frame)?;
        }
        // The saving that made this function worth having is here, not there:
        // `import` reindexes, and reindexing is O(the whole document), so the
        // loop this replaced cost one whole-document walk per frame.
        self.reindex()
    }

    /// Whether this document already contains everything in `frame`, judged
    /// from the frame's **header** rather than by applying it.
    ///
    /// `decode_import_blob_meta` reads the version range an update covers
    /// without materialising its ops, so a log the snapshot already subsumes
    /// costs a header parse per frame instead of an import and a reindex. Every
    /// peer file is written by a replica that also wrote to `snapshot.loro`, so
    /// in the ordinary single-user case that is *all* of them.
    ///
    /// A frame whose header will not parse answers `false` — unreadable is not
    /// the same as known, and the import path is where damage is diagnosed and
    /// reported. Checksums are skipped: this is a fast pre-filter, and
    /// `import_frames` validates anything it actually applies.
    pub fn frame_is_known(&self, frame: &[u8]) -> bool {
        match LoroDoc::decode_import_blob_meta(frame, false) {
            Ok(meta) => self.version().includes_vv(&meta.partial_end_vv),
            Err(_) => false,
        }
    }

    // ------------------------------------------------------------ change tracking

    /// Commit any pending local write. Loro auto-commits on read, so this is
    /// nearly always a no-op; it exists so a caller about to read what changed
    /// ([`ChangeTracker::drain`]) can be certain nothing is left uncommitted.
    pub fn commit(&self) {
        self.doc.commit();
    }

    /// Resolve a tree node to its persistent id by reading `id` from its meta
    /// map. `None` if the node is gone or its meta is malformed.
    ///
    /// Used by [`ChangeTracker::drain`] to turn the tree ids a subscription
    /// records into [`NodeId`]s a view can patch.
    pub fn node_id_of(&self, tid: TreeID) -> Option<NodeId> {
        let meta = self.nodes().get_meta(tid).ok()?;
        read_node_id(&meta)
    }

    /// Register a per-view change tracker backed by a Loro event subscription.
    ///
    /// A view keeps the returned [`ChangeTracker`] for its lifetime; dropping it
    /// unsubscribes. Each view gets its **own** tracker — the subscription fires
    /// into it alone — so one view's `drain` cannot steal another's changes.
    ///
    /// See [`ChangeTracker`] for what the subscription records and how `drain`
    /// resolves it.
    pub fn track_changes(&self) -> ChangeTracker {
        ChangeTracker::new(
            &self.doc,
            self.index
                .iter()
                .map(|(id, locator)| (locator.tree, *id))
                .collect(),
        )
    }

    // ------------------------------------------------------------ undo

    /// Start recording an undo history.
    ///
    /// Undo is *local-only*: it reverts this peer's own operations and never
    /// another's, which is the only definition that makes sense once two people
    /// are editing at once.
    ///
    /// Grouping is explicit rather than time-based. Loro's `merge_interval`
    /// merges by wall-clock alone and `record_new_checkpoint` cannot break out
    /// of a merge window, so an interval would fold a structural change into
    /// whatever the user happened to type a moment earlier — one Ctrl+Z
    /// undoing both. [`begin_group`] and [`checkpoint`] mark the boundaries
    /// instead, and the editor knows exactly where they are.
    ///
    /// [`begin_group`]: Self::begin_group
    /// [`checkpoint`]: Self::checkpoint
    pub fn enable_undo(&mut self) {
        self.clear_next_undo_caret();
        self.popped_undo_caret
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let pending = Arc::clone(&self.pending_undo_caret);
        let popped = Arc::clone(&self.popped_undo_caret);
        let transferred = Arc::new(Mutex::new(None));
        let transferred_on_push = Arc::clone(&transferred);
        let mut undo = loro::UndoManager::new(&self.doc);
        undo.set_on_push(Some(Box::new(move |direction, _, _| {
            if let Some(meta) = transferred_on_push
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take()
            {
                return meta;
            }
            let mut meta = loro::UndoItemMeta::new();
            if direction == loro::UndoOrRedo::Undo {
                let caret = pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                if let Some(position) = caret {
                    meta.set_value(format!("koru:caret:{}", position.node.to_storage()).into());
                    meta.add_cursor(&position.cursor);
                }
            }
            meta
        })));
        undo.set_on_pop(Some(Box::new(move |direction, _, meta| {
            if direction == loro::UndoOrRedo::Undo
                && let LoroValue::String(value) = &meta.value
                && let Some(encoded) = value.strip_prefix("koru:caret:")
                && let Some(node) = NodeId::parse(encoded)
                && let Some(cursor) = meta.cursors.first().map(|item| item.cursor.clone())
            {
                *popped
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some(StableTextPosition { node, cursor });
            }
            *transferred
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(meta);
        })));
        self.undo = Some(undo);
    }

    /// Attach a caret destination to the next local undo item.
    ///
    /// Must be called immediately before the edit. The metadata is consumed by
    /// Loro's synchronous push callback and stays with that exact history step
    /// across unrelated edits and remote transformations.
    pub fn set_next_undo_caret(&mut self, node: NodeId, at: usize) -> Result<()> {
        // `Left` keeps this on the leading edge when undo resurrects text with
        // new CRDT ids. `Middle` follows the resurrected run to its far edge,
        // which would put the caret after the word rather than before it.
        let position = self.capture_text_position_with_side(node, at, loro::cursor::Side::Left)?;
        *self
            .pending_undo_caret
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(position);
        Ok(())
    }

    /// Drop a destination when the edit it was prepared for did not commit.
    pub fn clear_next_undo_caret(&mut self) {
        self.pending_undo_caret
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
    }

    /// Take the destination from the undo that just completed.
    ///
    /// Resolution happens now, after Loro transformed the cursor while
    /// restoring the deleted run. Resolving the top item before `undo()` would
    /// point to the far side of the still-present replacement instead.
    pub fn take_popped_undo_caret(&self) -> Option<(NodeId, usize)> {
        let position = self
            .popped_undo_caret
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()?;
        Some((position.node, self.resolve_text_position(&position).ok()?))
    }

    pub fn can_undo(&self) -> bool {
        self.undo.as_ref().is_some_and(|u| u.can_undo())
    }

    pub fn can_redo(&self) -> bool {
        self.undo.as_ref().is_some_and(|u| u.can_redo())
    }

    /// Begin collecting edits into one undo step. Idempotent, so the caller can
    /// simply say this before every keystroke: a burst of typing becomes a
    /// single Ctrl+Z rather than one per character.
    ///
    /// Returns whether a group is open afterwards (`true` if already grouping
    /// or `group_start` succeeded). `false` means undo is off or Loro refused
    /// `group_start` — callers that care should log that (see `KORU_UNDO_DEBUG`).
    pub fn begin_group(&mut self) -> bool {
        if self.grouping {
            return true;
        }
        if let Some(u) = &mut self.undo {
            match u.group_start() {
                Ok(()) => {
                    self.grouping = true;
                    true
                }
                Err(_) => false,
            }
        } else {
            false
        }
    }

    /// Whether an undo group is open — whether the next edit will join the last
    /// one rather than begin a step of its own.
    ///
    /// Asked by a caller that has to record something *once per step* rather
    /// than once per edit. The editor remembers where the caret was so undo can
    /// put it back, and a run of Ctrl+Backspace presses is one step: the
    /// position to come back to is the one from before the first of them.
    pub fn in_group(&self) -> bool {
        self.grouping
    }

    /// Close the current group, so the next edit starts a fresh undo step.
    /// Called when the caret moves, a word ends, or the structure changes.
    pub fn checkpoint(&mut self) {
        if !self.grouping {
            return;
        }
        if let Some(u) = &mut self.undo {
            u.group_end();
        }
        self.grouping = false;
    }

    /// Run a compound edit as a single undo step.
    ///
    /// A split is a create, a copy and a delete; without this, one Ctrl+Z would
    /// take back only the delete and leave the document in a state the user
    /// never asked for. Nesting is handled: an inner call joins the outer group
    /// rather than closing it early.
    pub(crate) fn atomic<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        let nested = self.grouping;
        if !nested {
            self.begin_group();
        }
        let out = f(self);
        if !nested {
            self.checkpoint();
        }
        out
    }

    /// [`atomic`](Self::atomic) for callers outside this crate: run a
    /// compound edit as **one undo step**. What the editor uses to make
    /// "delete the `/tag` trigger text and set the attribute" a single
    /// Ctrl+Z. Nested calls join the outer group rather than closing it
    /// early. Callers should [`checkpoint`](Self::checkpoint) first if the
    /// user has been typing, or the group folds into the keystrokes.
    pub fn edit_group<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.atomic(f)
    }

    pub fn undo(&mut self) -> Result<bool> {
        self.step(true)
    }

    pub fn redo(&mut self) -> Result<bool> {
        self.step(false)
    }

    fn step(&mut self, back: bool) -> Result<bool> {
        // An open group would otherwise be undone half-formed.
        self.checkpoint();
        if back {
            self.popped_undo_caret
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .take();
        }
        let Some(undo) = &mut self.undo else {
            return Ok(false);
        };
        let moved = if back { undo.undo()? } else { undo.redo()? };
        if moved {
            // An undo can resurrect or remove a node, so the id map has to be
            // rebuilt — the same reason an import does.
            self.reindex()?;
        }
        Ok(moved)
    }

    // ------------------------------------------------------------ identity

    /// Walk the tree and rebuild `NodeId -> TreeID`.
    ///
    /// O(all live nodes), and it clears rather than patches — so
    /// anything that calls it in a loop is quietly quadratic. That is why
    /// [`import_frames`] exists beside [`import`]: a peer log holding a thousand
    /// frames must cost one of these, not a thousand.
    ///
    /// [`import_frames`]: Self::import_frames
    /// [`import`]: Self::import
    fn reindex(&mut self) -> Result<()> {
        let span = crate::timing::Span::start("reindex");
        self.index.clear();
        self.blob_nodes.clear();
        let tree = self.nodes();
        for tid in tree.nodes() {
            if tree.is_node_deleted(&tid).unwrap_or(false) {
                continue;
            }
            let meta = tree.get_meta(tid)?;
            if let Some(id) = read_node_id(&meta) {
                let blob_hash = meta.get(K_BLOB).and_then(as_string);
                if blob_hash.is_some() {
                    self.blob_nodes.push(id);
                }
                self.index.insert(
                    id,
                    NodeLocator {
                        tree: tid,
                        blob_hash,
                    },
                );
            }
        }
        let n = self.index.len();
        span.stop_with(|| format!("{n} nodes"));
        Ok(())
    }

    fn tree_id(&self, id: NodeId) -> Result<TreeID> {
        self.index
            .get(&id)
            .map(|locator| locator.tree)
            .ok_or(StoreError::NoSuchNode(id))
    }

    /// Every node holding a reference to `target`.
    ///
    /// **Derived, never stored.** The forward reference is already the truth; a
    /// `linked_from` list beside it would be a second one that can come to
    /// disagree — two peers concurrently deleting a task and linking it would
    /// converge on a list naming a node that no longer exists. Walking cannot
    /// go stale.
    ///
    /// It finds references *within this document*. A task in another workspace
    /// cannot be found by scanning, because that workspace is not open — that
    /// is what the profile document's `locations` index is for. See
    /// `docs/deferred/`.
    pub fn backlinks(&self, target: NodeId) -> Result<Vec<NodeId>> {
        let mut out = Vec::new();
        for (id, _) in self.walk()? {
            let Ok(node) = self.read(id) else { continue };
            let links = node
                .content
                .attrs()
                .iter()
                .filter_map(|a| a.as_ref_target())
                .any(|r| r.node == target);
            if links {
                out.push(id);
            }
        }
        Ok(out)
    }

    /// The Loro parent a [`Parent`] names.
    ///
    /// One tree now, so this no longer answers *which* tree — the question that
    /// made a node's realm part of its identity, and the reason `move_node`
    /// used to refuse a cross-realm move.
    fn resolve_parent(&self, parent: Parent) -> Result<TreeParentId> {
        Ok(match parent {
            Parent::Root => TreeParentId::Root,
            Parent::Node(id) => TreeParentId::Node(self.tree_id(id)?),
        })
    }

    // ------------------------------------------------------------ writes

    /// Insert `node` under `parent`. Rejects the insert if the parent cannot
    /// take children — an image and a table are leaves.
    pub fn insert(&mut self, parent: Parent, node: Node) -> Result<NodeId> {
        if let Parent::Node(pid) = parent {
            self.read(pid)?.may_adopt()?;
        }
        let tree = self.nodes();
        let under = self.resolve_parent(parent)?;
        let tid = tree.create(under)?;
        let meta = tree.get_meta(tid)?;
        write_node(&meta, &node)?;
        self.record_schema_for_kind(node.kind)?;
        mint_document_id(&meta, node.kind)?;
        self.doc.commit();
        let blob_hash = meta.get(K_BLOB).and_then(as_string);
        if blob_hash.is_some() {
            self.blob_nodes.push(node.id);
        }
        self.index.insert(
            node.id,
            NodeLocator {
                tree: tid,
                blob_hash,
            },
        );
        Ok(node.id)
    }

    /// Insert `node` as the next sibling of `after` — the operation a paste or
    /// a drop into the middle performs, which [`insert`] (an append) does not
    /// express. `after` keeps its identity; the new node takes the slot directly
    /// below it, pushing the rest down.
    ///
    /// Same realm as `after` by construction, since a sibling shares its
    /// parent's tree. Built from [`insert`] + [`move_node`], the same shape
    /// `paste_rows` inlines.
    ///
    /// [`insert`]: Self::insert
    /// [`move_node`]: Self::move_node
    pub fn insert_after(&mut self, after: NodeId, node: Node) -> Result<NodeId> {
        let parent = self.parent(after)?;
        let index = self.sibling_index(after)? + 1;
        let id = self.insert(parent, node)?;
        self.move_node(id, parent, index)?;
        Ok(id)
    }

    /// Move a node — with its whole subtree — under a new parent.
    ///
    /// This is Alt+↑/↓, drag-reorder, "wrap these into a group" and "move phase
    /// one's tasks into phase two", all of which are the same operation.
    ///
    /// `index` counts the **visible** children of `parent`, which at
    /// [`Parent::Root`] is not what Loro counts — the trash root is a real child
    /// that [`children`] hides. It is translated here rather than at the call
    /// sites, so that the one list callers can see is the one list they
    /// address.
    ///
    /// [`children`]: Self::children
    pub fn move_node(&mut self, target: NodeId, parent: Parent, index: usize) -> Result<()> {
        if let Parent::Node(pid) = parent {
            self.read(pid)?.may_adopt()?;
        }
        let tid = self.tree_id(target)?;
        let under = self.resolve_parent(parent)?;
        let index = if parent == Parent::Root {
            self.raw_root_index(index)
        } else {
            index
        };
        self.nodes().mov_to(tid, under, index)?;
        self.doc.commit();
        Ok(())
    }

    /// Turn an index into the visible root into the index `mov_to` wants.
    ///
    /// The visible list is the raw list minus the trash roots, so a visible
    /// index shifts by however many of them sort before it. Landing *past* the
    /// end means the end of the raw list — which is where the trash root also
    /// lives if it happens to sort last, and that is fine: nothing renders it,
    /// so its position among its siblings is not a fact anybody can see.
    fn raw_root_index(&self, visible_index: usize) -> usize {
        let Ok(raw) = self.root_children_raw() else {
            return visible_index;
        };
        let visible: Vec<NodeId> = self.children(Parent::Root).unwrap_or_else(|_| raw.clone());
        match visible.get(visible_index) {
            Some(at) => raw.iter().position(|id| id == at).unwrap_or(visible_index),
            None => raw.len(),
        }
    }

    /// Wrap `target` in a new [`Kind::ListBlock`] container. The one-row form
    /// delegates to [`wrap_in_list_block_many`](Self::wrap_in_list_block_many)
    /// so typed markers, slash commands, toolbar buttons and selection
    /// shortcuts all use the same structural operation.
    ///
    /// `target` keeps its kind and content — it becomes the first item. A
    /// paragraph becomes item 1 of the list; the ListBlock itself renders no
    /// row (it is the container).
    pub fn wrap_in_list_block(&mut self, target: NodeId, style: ListStyle) -> Result<NodeId> {
        self.wrap_in_list_block_many(&[target], style)
    }

    /// Wrap contiguous siblings in one new [`Kind::ListBlock`]. The block is
    /// created at the first target's position and the targets keep their order
    /// as its children. This is the multi-row counterpart of
    /// [`wrap_in_list_block`](Self::wrap_in_list_block).
    ///
    /// Validating the complete run before moving anything matters: a malformed
    /// selection must not leave half its rows inside a list. [`atomic`](Self::atomic)
    /// supplies the second half of that guarantee if a Loro operation fails.
    pub fn wrap_in_list_block_many(
        &mut self,
        targets: &[NodeId],
        style: ListStyle,
    ) -> Result<NodeId> {
        self.atomic(|ws| {
            let first = *targets
                .first()
                .ok_or(StoreError::Malformed("empty list selection"))?;
            let parent = ws.parent(first)?;
            let index = ws.sibling_index(first)?;
            for (offset, &target) in targets.iter().enumerate() {
                if ws.parent(target)? != parent || ws.sibling_index(target)? != index + offset {
                    return Err(StoreError::Malformed(
                        "list selection must be contiguous siblings",
                    ));
                }
            }
            // Create the container at the item's exact slot, then move the item
            // under it. `insert` appends, so insert then `move_node` to `index`.
            let block = ws.insert(parent, Node::list_block()?)?;
            ws.move_node(block, parent, index)?;
            for (at, &target) in targets.iter().enumerate() {
                ws.move_node(target, Parent::Node(block), at)?;
            }
            ws.set_list_style(block, Some(style))?;

            ws.coalesce_list_block(block)
        })
    }

    /// Put contiguous siblings into one new [`Kind::Callout`] box, created at
    /// the first target's position with the targets as its children in order
    /// — the callout counterpart of [`wrap_in_list_block_many`](Self::wrap_in_list_block_many),
    /// with the same all-or-nothing validation. The box starts with the
    /// default look; [`set_icon`](Self::set_icon) and
    /// [`set_color`](Self::set_color) change it.
    pub fn wrap_in_callout_many(&mut self, targets: &[NodeId]) -> Result<NodeId> {
        self.atomic(|ws| {
            let first = *targets
                .first()
                .ok_or(StoreError::Malformed("empty callout selection"))?;
            let parent = ws.parent(first)?;
            let index = ws.sibling_index(first)?;
            for (offset, &target) in targets.iter().enumerate() {
                if ws.parent(target)? != parent || ws.sibling_index(target)? != index + offset {
                    return Err(StoreError::Malformed(
                        "callout selection must be contiguous siblings",
                    ));
                }
            }
            let callout = ws.insert(parent, Node::callout(None, None)?)?;
            ws.move_node(callout, parent, index)?;
            for (at, &target) in targets.iter().enumerate() {
                ws.move_node(target, Parent::Node(callout), at)?;
            }
            Ok(callout)
        })
    }

    /// Take a [`Kind::Callout`] box away and keep its lines: the children
    /// move into the box's own slot, in order, and the empty box is deleted.
    /// Returns the lines, now siblings where the box was.
    pub fn dissolve_callout(&mut self, callout: NodeId) -> Result<Vec<NodeId>> {
        self.atomic(|ws| {
            if ws.kind_of(callout) != Some(Kind::Callout) {
                return Err(StoreError::Malformed("not a callout"));
            }
            let parent = ws.parent(callout)?;
            let index = ws.sibling_index(callout)?;
            let children = ws.children(Parent::Node(callout))?;
            for (offset, &child) in children.iter().enumerate() {
                ws.move_node(child, parent, index + 1 + offset)?;
            }
            ws.delete(callout)?;
            Ok(children)
        })
    }

    /// Merge a ListBlock with compatible ListBlock siblings immediately before
    /// and after it. The surviving container keeps every item in visible order.
    ///
    /// This is shared by both ways a compatible list can appear between two
    /// fragments: wrapping extracted paragraphs and re-styling an existing
    /// ListBlock. Keeping the coalescing in the model prevents toolbar and typed
    /// list operations from growing different definitions of "one list".
    pub fn coalesce_list_block(&mut self, block: NodeId) -> Result<NodeId> {
        self.atomic(|ws| {
            let node = ws.read(block)?;
            if node.kind != Kind::ListBlock {
                return Err(StoreError::Malformed("node is not a list block"));
            }
            let style = node.meta.list_style.unwrap_or(ListStyle::Bullet);
            let parent = ws.parent(block)?;
            let mut survivor = block;
            let siblings = ws.children(parent)?;
            let at = siblings
                .iter()
                .position(|id| *id == survivor)
                .ok_or(StoreError::NoSuchNode(survivor))?;
            if let Some(&before) = at.checked_sub(1).and_then(|i| siblings.get(i))
                && ws.read(before).is_ok_and(|node| {
                    node.kind == Kind::ListBlock
                        && node.meta.list_style.unwrap_or(ListStyle::Bullet) == style
                })
            {
                let append_at = ws.children(Parent::Node(before))?.len();
                let items = ws.children(Parent::Node(survivor))?;
                for (offset, item) in items.into_iter().enumerate() {
                    ws.move_node(item, Parent::Node(before), append_at + offset)?;
                }
                ws.delete(survivor)?;
                survivor = before;
            }
            let siblings = ws.children(parent)?;
            let at = siblings
                .iter()
                .position(|id| *id == survivor)
                .ok_or(StoreError::NoSuchNode(survivor))?;
            if let Some(&after) = siblings.get(at + 1)
                && ws.read(after).is_ok_and(|node| {
                    node.kind == Kind::ListBlock
                        && node.meta.list_style.unwrap_or(ListStyle::Bullet) == style
                })
            {
                let append_at = ws.children(Parent::Node(survivor))?.len();
                let items = ws.children(Parent::Node(after))?;
                for (offset, item) in items.into_iter().enumerate() {
                    ws.move_node(item, Parent::Node(survivor), append_at + offset)?;
                }
                ws.delete(after)?;
            }
            Ok(survivor)
        })
    }

    /// Turn one direct ListBlock child into a paragraph sibling without moving
    /// it visually. An interior item splits the list into two containers; a
    /// first or last item simply leaves one side absent. Both containers keep
    /// the original style, so a numbered suffix restarts at 1.
    pub fn extract_list_item(&mut self, item: NodeId) -> Result<bool> {
        self.atomic(|ws| {
            let Parent::Node(block) = ws.parent(item)? else {
                return Ok(false);
            };
            let node = ws.read(block)?;
            if node.kind != Kind::ListBlock {
                return Ok(false);
            }
            let style = node.meta.list_style.unwrap_or(ListStyle::Bullet);
            let children = ws.children(Parent::Node(block))?;
            let Some(item_at) = children.iter().position(|id| *id == item) else {
                return Err(StoreError::NoSuchNode(item));
            };
            let trailing = children[item_at + 1..].to_vec();
            let parent = ws.parent(block)?;
            let block_at = ws.sibling_index(block)?;

            if item_at == 0 {
                ws.move_node(item, parent, block_at)?;
            } else {
                ws.move_node(item, parent, block_at + 1)?;
            }

            if item_at > 0 && !trailing.is_empty() {
                let suffix = ws.insert(parent, Node::list_block()?)?;
                ws.move_node(suffix, parent, block_at + 2)?;
                ws.set_list_style(suffix, Some(style))?;
                for (offset, child) in trailing.into_iter().enumerate() {
                    ws.move_node(child, Parent::Node(suffix), offset)?;
                }
            }
            if ws.children(Parent::Node(block))?.is_empty() {
                ws.delete(block)?;
            }
            Ok(true)
        })
    }

    /// Move contiguous direct children of one [`Kind::ListBlock`] into a list
    /// owned by `destination`, preserving their marker style and order.
    ///
    /// This is the structural counterpart of moving an ordinary row beneath a
    /// task: list items need their invisible container to travel with them or
    /// they become plain paragraphs. When `include_following` is set, the
    /// source suffix moves too; that keeps document order when lifting a list
    /// fragment out from beneath its parent.
    pub fn reparent_list_fragment(
        &mut self,
        items: &[NodeId],
        destination: Parent,
        index: usize,
        include_following: bool,
    ) -> Result<bool> {
        self.atomic(|ws| {
            let Some(&first) = items.first() else {
                return Ok(false);
            };
            let Parent::Node(source) = ws.parent(first)? else {
                return Ok(false);
            };
            let source_node = ws.read(source)?;
            if source_node.kind != Kind::ListBlock {
                return Ok(false);
            }
            let children = ws.children(Parent::Node(source))?;
            let Some(start) = children.iter().position(|&id| id == first) else {
                return Err(StoreError::NoSuchNode(first));
            };
            for (offset, &item) in items.iter().enumerate() {
                if children.get(start + offset) != Some(&item) {
                    return Err(StoreError::Malformed(
                        "list fragment must be contiguous direct items",
                    ));
                }
            }

            let end = if include_following {
                children.len()
            } else {
                start + items.len()
            };
            let moving = children[start..end].to_vec();
            let style = source_node.meta.list_style.unwrap_or(ListStyle::Bullet);

            let destination_block = ws.insert(destination, Node::list_block()?)?;
            ws.move_node(destination_block, destination, index)?;
            ws.set_list_style(destination_block, Some(style))?;
            for (at, item) in moving.into_iter().enumerate() {
                ws.move_node(item, Parent::Node(destination_block), at)?;
            }
            if ws.children(Parent::Node(source))?.is_empty() {
                ws.delete(source)?;
            }
            ws.coalesce_list_block(destination_block)?;
            Ok(true)
        })
    }

    /// Remove a [`Kind::ListBlock`] while keeping its items in place. Its
    /// children become siblings at the container's former position, in the
    /// same order, and the empty container is deleted. The inverse structural
    /// operation of [`wrap_in_list_block_many`](Self::wrap_in_list_block_many).
    pub fn unwrap_list_block(&mut self, block: NodeId) -> Result<Vec<NodeId>> {
        self.atomic(|ws| {
            if ws.read(block)?.kind != Kind::ListBlock {
                return Err(StoreError::Malformed("node is not a list block"));
            }
            let parent = ws.parent(block)?;
            let index = ws.sibling_index(block)?;
            let children = ws.children(Parent::Node(block))?;
            for (offset, &child) in children.iter().enumerate() {
                ws.move_node(child, parent, index + offset)?;
            }
            ws.delete(block)?;
            Ok(children)
        })
    }

    /// Remove a node and its whole subtree, for good.
    ///
    /// **The hard delete.** Everything a person can reach goes through the trash
    /// instead; this is what the trash itself is built on, plus the internal
    /// callers that remove a node the user never sees (a merged row, an emptied
    /// list container, a cut). See [`trash`] and [`delete_permanently`].
    ///
    /// [`trash`]: Self::trash
    /// [`delete_permanently`]: Self::delete_permanently
    pub fn delete(&mut self, target: NodeId) -> Result<()> {
        let mut removed = vec![target];
        let mut at = 0;
        while at < removed.len() {
            removed.extend(self.children(Parent::Node(removed[at]))?);
            at += 1;
        }
        let tid = self.tree_id(target)?;
        self.nodes().delete(tid)?;
        self.doc.commit();
        for id in &removed {
            self.index.remove(id);
        }
        self.blob_nodes.retain(|id| self.index.contains_key(id));
        Ok(())
    }

    /// Delete `target`, moving its direct children up into its place.
    ///
    /// The inverse of [`delete`](Self::delete): instead of taking the whole
    /// subtree down, the children are promoted one level — each keeping its own
    /// subtree — and only `target` itself is removed. Deleting a bullet should
    /// outdent the bullets nested under it, not delete them with it.
    ///
    /// One undo step. Returns the promoted children, in order.
    pub fn delete_promoting_children(&mut self, target: NodeId) -> Result<Vec<NodeId>> {
        self.atomic(|ws| {
            let parent = ws.parent(target)?;
            let index = ws.sibling_index(target)?;
            let children = ws.children(Parent::Node(target))?;
            for (offset, &child) in children.iter().enumerate() {
                ws.move_node(child, parent, index + offset)?;
            }
            ws.delete(target)?;
            Ok(children)
        })
    }

    // ------------------------------------------------------------- the trash
    //
    // The trash is a *place*: a hidden `Kind::Trash` node at the library root,
    // created the first time something is binned. Trashing is therefore an
    // ordinary `move_node` inside one realm — the node keeps its `NodeId`, its
    // `LoroText`, its marks and its pills, so a peer typing inside a document
    // while it is trashed still converges, and restoring is the same operation
    // backwards. See ADR-69 for the alternatives this beat.
    //
    // Concurrency is handled by a tolerant reader rather than by a mergeable
    // create, because Loro has no mergeable `LoroTree::create`: two peers can
    // each mint a root, and `ensure_trash_root` unions them behind the lowest
    // `NodeId`, which every peer agrees on without talking.

    /// Move `target` — with its whole subtree — into this workspace's trash.
    ///
    /// `at_ms` is unix milliseconds and `by` is a display label for whoever did
    /// it; **both are passed in**, because this crate has neither a clock nor an
    /// identity, deliberately. The caller's are `koru_workspace::open::now_ms`
    /// and `Workspace::actor_label`. `by` may be `None` — every message built
    /// from it has a form with no name in it.
    ///
    /// One undo step. **`checkpoint()` first if the user has been typing**, or
    /// this folds into the last keystroke and one Ctrl+Z takes back both.
    ///
    /// Refuses a trash root, and anything already in the trash.
    pub fn trash(&mut self, target: NodeId, at_ms: i64, by: Option<&str>) -> Result<()> {
        if self.kind_of(target) == Some(Kind::Trash) || self.in_trash(target) {
            return Err(StoreError::AlreadyInTrash(target));
        }
        self.atomic(|ws| {
            // Read the origin *before* the move, obviously — but note it is the
            // parent node, never a sibling index. An index recorded now is wrong
            // the moment any sibling moves; a restore appends instead.
            let origin = match ws.parent(target)? {
                Parent::Node(p) => Some(p),
                _ => None,
            };
            let root = ws.ensure_trash_root()?;
            let end = ws.children(Parent::Node(root))?.len();
            ws.move_node(target, Parent::Node(root), end)?;
            ws.mark_trashed(target, at_ms, origin, by)
        })
    }

    /// Put a trashed item back where it came from.
    ///
    /// Falls back to the top of the library whenever the recorded origin can no
    /// longer take it — see [`restore_target`]. Restoring **appends**; no
    /// sibling index was stored, on purpose.
    ///
    /// One undo step. Refuses anything that is not a top-level trash item: what
    /// went *with* an item stays inside it, and restoring a child on its own
    /// would promise a placement the model does not have.
    ///
    /// [`restore_target`]: Self::restore_target
    pub fn restore(&mut self, target: NodeId, by: Option<&str>) -> Result<()> {
        if !self.is_trashed(target) {
            return Err(StoreError::NotInTrash(target));
        }
        self.atomic(|ws| {
            let (parent, index) = ws.restore_target(target)?;
            ws.move_node(target, parent, index)?;
            ws.clear_trash_marks(target, by)
        })
    }

    /// Really remove one trashed item.
    ///
    /// The hard [`delete`], refused for anything not in the trash — so a
    /// mis-wired button cannot skip the bin. That refusal is the whole reason
    /// this exists rather than the call site reaching for `delete`.
    ///
    /// [`delete`]: Self::delete
    pub fn delete_permanently(&mut self, target: NodeId) -> Result<()> {
        if !self.is_trashed(target) {
            return Err(StoreError::NotInTrash(target));
        }
        self.delete(target)
    }

    /// Really remove everything in the trash. Returns how many items went, so
    /// the confirm and the toast can say it.
    ///
    /// One undo step — but do not promise the user one. Since `loro-internal`
    /// 1.13.9 undo does survive most concurrent remote changes, but it still
    /// declines outright on a node another peer deleted, and this is a hard
    /// delete of many nodes at once. **Never called on a timer**: nothing in
    /// this app purges the trash by itself (ADR-69).
    pub fn empty_trash(&mut self) -> Result<usize> {
        let items = self.trashed_ids()?;
        if items.is_empty() {
            return Ok(0);
        }
        self.atomic(|ws| {
            for id in &items {
                ws.delete(*id)?;
            }
            Ok(items.len())
        })
    }

    /// Where a restore will actually land, and why it may not be where it came
    /// from.
    ///
    /// Public so the trash screen can *show* the destination before the user
    /// commits to it. Five ways an origin stops being usable, all of which fall
    /// back to the top of the library rather than erroring — "it went back
    /// somewhere sensible" beats "the button did nothing".
    pub fn restore_target(&self, item: NodeId) -> Result<(Parent, usize)> {
        let end = |p: Parent| -> Result<(Parent, usize)> { Ok((p, self.children(p)?.len())) };
        // 1. Nothing was recorded: it came from the top of the library, which is
        //    also where every other case lands. The two agree by construction.
        let Some(origin) = self.read(item)?.meta.origin else {
            return end(Parent::Root);
        };
        // 2. The origin is gone — hard-deleted here, or by a peer.
        if self.index.get(&origin).is_none() {
            return end(Parent::Root);
        }
        // 3. The origin is itself in the trash. Restoring into it would leave the
        //    item somewhere still invisible, which reads as Restore doing nothing.
        if self.in_trash(origin) {
            return end(Parent::Root);
        }
        // 4. The origin is the item, or moved inside it while it was binned — a
        //    peer can do that. `mov_to` would refuse the cycle anyway; landing
        //    somewhere sane is better than an error the user cannot act on.
        if origin == item || self.ancestors(origin)?.contains(&item) {
            return end(Parent::Root);
        }
        // 5. The origin stopped being able to adopt: it became an image, a table
        //    or a divider while the item was away.
        if self.read(origin)?.may_adopt().is_err() {
            return end(Parent::Root);
        }
        end(Parent::Node(origin))
    }

    /// What is in the trash — newest first.
    ///
    /// **Not a walk.** What went with an item stays inside it and earns no row;
    /// this is the list of things a person chose to bin. Built without a `read`
    /// per row where it can be, because `read` materialises a node's text.
    pub fn trashed(&self) -> Result<Vec<TrashEntry>> {
        let mut out = Vec::new();
        for id in self.trashed_ids()? {
            let Ok(node) = self.read(id) else { continue };
            let name = node
                .content
                .as_text()
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
                .or_else(|| node.meta.name.clone())
                .unwrap_or_default();
            out.push(TrashEntry {
                node: id,
                kind: node.kind,
                name,
                trashed_at: node.meta.trashed_at,
                trashed_by: node.meta.trashed_by.clone(),
                origin: node.meta.origin,
                nested: self.nested_documents(id),
            });
        }
        // Newest first. An item with no timestamp sorts oldest — it was binned
        // by a build that did not record one, so it is the least recent thing we
        // can say anything about.
        out.sort_by_key(|e| std::cmp::Reverse(e.trashed_at.unwrap_or(i64::MIN)));
        Ok(out)
    }

    /// How many items are in the trash, without building the listing.
    ///
    /// What the pinned row at the foot of the explore column shows. One
    /// `children` call, so it is cheap enough to ask once per rebuild — but it
    /// is still not free, and nothing should ask it per frame.
    pub fn trash_len(&self) -> usize {
        self.trashed_ids().map(|v| v.len()).unwrap_or(0)
    }

    /// Whether `id` is a trashed item **or lives inside one**.
    ///
    /// The ancestor test, and the only trash question a view outside the trash
    /// screen should ask: a tab showing a paragraph of a binned document is
    /// showing something in the bin.
    pub fn in_trash(&self, id: NodeId) -> bool {
        if self.kind_of(id) == Some(Kind::Trash) {
            return true;
        }
        let Ok(ancestors) = self.ancestors(id) else {
            return false;
        };
        ancestors
            .iter()
            .any(|a| self.kind_of(*a) == Some(Kind::Trash))
    }

    /// Whether `id` is a **top-level** trash item — one a person binned, rather
    /// than something that came along inside it.
    pub fn is_trashed(&self, id: NodeId) -> bool {
        self.kind_of(id) != Some(Kind::Trash)
            && matches!(self.parent(id), Ok(Parent::Node(p)) if self.kind_of(p) == Some(Kind::Trash))
    }

    /// A self-declared label for whoever binned `id`, if one was recorded.
    ///
    /// Never inferred: see [`NodeMeta::trashed_by`](crate::node::NodeMeta::trashed_by).
    pub fn trashed_by(&self, id: NodeId) -> Option<String> {
        self.read(id).ok()?.meta.trashed_by
    }

    /// A self-declared label for whoever last restored `id`, on the same terms.
    pub fn restored_by(&self, id: NodeId) -> Option<String> {
        self.read(id).ok()?.meta.restored_by
    }

    /// How many [`Kind::Document`] nodes sit under `id`, not counting `id`.
    ///
    /// **The same rule the explore column uses to earn a row**, so "and 3 items
    /// inside" counts what a person can actually see. Counting every descendant
    /// would count paragraphs, and then every page with a few lines in it would
    /// trip the confirmation.
    pub fn nested_documents(&self, id: NodeId) -> usize {
        self.walk_under(id)
            .map(|nodes| {
                nodes
                    .into_iter()
                    .filter(|(n, _)| self.kind_of(*n) == Some(Kind::Document))
                    .count()
            })
            .unwrap_or(0)
    }

    /// Copy `target` and everything under it to a new place, with new ids.
    ///
    /// **A copy, not a move**: the original is untouched, and every node in the
    /// copy is a *different* node. That is the whole point — it is what lets
    /// somebody keep working on a document after it has been thrown away,
    /// without arguing with the peer who threw it away about whether it is in
    /// the bin.
    ///
    /// Carries what a person would say the document *is*: its kind, its meta,
    /// its text, its inline formatting, its attributes and its whole subtree.
    /// It deliberately does **not** carry the trash marks — a copy of something
    /// binned is not itself binned.
    ///
    /// One undo step.
    pub fn duplicate(&mut self, target: NodeId, parent: Parent, title: &str) -> Result<NodeId> {
        let cutting = self.cutting(target)?;
        self.plant(&cutting, parent, title)
    }

    /// Lift `target` and everything under it out of this document, as a value.
    ///
    /// **The middle of a copy, and it is a value because a copy now crosses two
    /// documents.** Reading and writing in one call worked while a workspace was
    /// one `LoroDoc` holding everything; with one per document
    /// ([ADR-90](../../../docs/decisions/ADR-076-100.md#adr-90-onetree--one-loro-document-is-one-documents-nodes-and-the-realm-is-gone)),
    /// *Save a copy* is a read here and a write in a document that does not
    /// exist yet. Neither side can borrow the other, so nothing does.
    ///
    /// Carries what a person would say the subtree *is*: its kinds, its meta,
    /// its text, its inline formatting, its attributes and its whole shape.
    pub fn cutting(&self, target: NodeId) -> Result<Cutting> {
        let source = self.read(target)?;
        // Only a text node has runs to ask for, and asking anyway is a wasted
        // traversal per image and divider.
        let runs = if source.content.as_text().is_some() {
            self.runs(target)?
        } else {
            Vec::new()
        };
        let mut children = Vec::new();
        for child in self.children(Parent::Node(target))? {
            children.push(self.cutting(child)?);
        }
        Ok(Cutting {
            id: source.id,
            document_id: (source.kind == Kind::Document).then(|| self.document_id(source.id)),
            kind: source.kind,
            content: source.content,
            meta: source.meta,
            runs,
            children,
        })
    }

    /// Lift a subtree for migration into another document, preserving every
    /// node identity and stopping before nested document roots. A nested page
    /// becomes its own target Loro document and must not also be copied into
    /// its parent's snapshot.
    pub fn cutting_for_import(&self, target: NodeId) -> Result<Cutting> {
        self.cutting_for_import_inner(target)
    }

    fn cutting_for_import_inner(&self, target: NodeId) -> Result<Cutting> {
        let source = self.read(target)?;
        let runs = if source.content.as_text().is_some() {
            self.runs(target)?
        } else {
            Vec::new()
        };
        let mut children = Vec::new();
        for child in self.children(Parent::Node(target))? {
            if self.kind_of(child) == Some(Kind::Document) {
                continue;
            }
            children.push(self.cutting_for_import_inner(child)?);
        }
        Ok(Cutting {
            id: source.id,
            document_id: (source.kind == Kind::Document).then(|| self.document_id(source.id)),
            kind: source.kind,
            content: source.content,
            meta: source.meta,
            runs,
            children,
        })
    }

    /// Plant a cutting under `parent`, with a **new `NodeId` for every node in
    /// it**, and rename its root when `title` is not empty.
    ///
    /// A copy, not a move: the document it was cut from is untouched, and
    /// nothing in the copy is a second name for anything in it. One undo step.
    pub fn plant(&mut self, cutting: &Cutting, parent: Parent, title: &str) -> Result<NodeId> {
        self.atomic(|ws| {
            let root = ws.place(cutting, parent)?;
            if !title.is_empty() {
                ws.set_text(root, title)?;
            }
            Ok(root)
        })
    }

    /// Plant migration content with its original node UUIDs. References name
    /// node UUIDs, so reminting here would turn every internal link dangling.
    pub fn plant_import(&mut self, cutting: &Cutting, parent: Parent) -> Result<NodeId> {
        self.atomic(|ws| ws.place_import(cutting, parent))
    }

    /// Restore a captured subtree with its original identities and position.
    ///
    /// This is the compensating half of a local compound edit whose tree
    /// insertion Loro cannot undo reliably. The caller must only restore a
    /// cutting after those identities have left the document.
    pub fn restore_cutting(
        &mut self,
        cutting: &Cutting,
        parent: Parent,
        index: usize,
    ) -> Result<NodeId> {
        self.atomic(|ws| {
            let id = ws.place_import(cutting, parent)?;
            ws.move_node(id, parent, index)?;
            Ok(id)
        })
    }

    /// Plant a migrated top-level item directly in the hidden trash while
    /// retaining its original deletion metadata.
    pub fn plant_import_in_trash(&mut self, cutting: &Cutting) -> Result<NodeId> {
        self.atomic(|ws| {
            let trash = ws.ensure_trash_root()?;
            ws.place_import(cutting, Parent::Node(trash))
        })
    }

    fn place_import(&mut self, cutting: &Cutting, parent: Parent) -> Result<NodeId> {
        let mut node = Node::new(cutting.kind, cutting.content.clone())?.with_id(cutting.id);
        node.meta = cutting.meta.clone();
        let id = self.insert(parent, node)?;
        if let Some(document) = cutting.document_id {
            self.nodes()
                .get_meta(self.tree_id(id)?)?
                .insert(K_DOC_ID, document.to_storage())?;
            self.doc.commit();
        }
        self.replay_cutting_runs(id, &cutting.runs)?;
        for child in &cutting.children {
            self.place_import(child, Parent::Node(id))?;
        }
        Ok(id)
    }

    /// One cutting and its children, depth first and in order, under `parent`.
    fn place(&mut self, cutting: &Cutting, parent: Parent) -> Result<NodeId> {
        // A fresh `NodeId`: `Node::new` mints one, and that is what makes this a
        // copy rather than a second name for the same node.
        let mut node = Node::new(cutting.kind, cutting.content.clone())?;
        node.meta = cutting.meta.clone();
        // A copy of something in the bin is not itself in the bin.
        node.meta.trashed_at = None;
        node.meta.origin = None;
        node.meta.trashed_by = None;
        node.meta.restored_by = None;
        let id = self.insert(parent, node)?;

        self.replay_cutting_runs(id, &cutting.runs)?;

        for child in &cutting.children {
            self.place(child, Parent::Node(id))?;
        }
        Ok(id)
    }

    fn replay_cutting_runs(&mut self, id: NodeId, runs: &[TextRun]) -> Result<()> {
        // Inline formatting, replayed from the ranges `runs` already reported
        // rather than traversed a second time in the mark store.
        for run in runs {
            let range = run.range();
            if range.is_empty() {
                continue;
            }
            for mark in [
                Mark::Bold,
                Mark::Italic,
                Mark::Underline,
                Mark::Strike,
                Mark::Code,
            ] {
                if run.has(mark) {
                    self.set_mark(id, range.clone(), mark, true)?;
                }
            }
            if let Some(script) = run.style.script {
                self.set_script(id, range.clone(), Some(script))?;
            }
            if let Some(size) = run.style.size {
                self.set_size(id, range.clone(), Some(size))?;
            }
            if let Some(colour) = run.style.highlight.as_deref() {
                self.set_highlight(id, range.clone(), Some(colour))?;
            }
            if let Some(attr) = run.pill {
                self.text_handle(id)?
                    .mark_utf8(range.clone(), PILL_KEY, attr.to_storage())?;
                self.doc.commit();
            }
        }

        Ok(())
    }

    // ---- the trash, privately

    /// The ids in the trash, across every root, in tree order.
    fn trashed_ids(&self) -> Result<Vec<NodeId>> {
        let mut out = Vec::new();
        for root in self.trash_roots()? {
            out.extend(self.children(Parent::Node(root))?);
        }
        Ok(out)
    }

    /// Every trash root, lowest `NodeId` first.
    ///
    /// Plural because two peers can each create one before they meet, and there
    /// is no mergeable `LoroTree::create` to prevent it. Readers tolerate the
    /// duplicates; [`ensure_trash_root`] unions them the next time anyone
    /// writes.
    ///
    /// [`ensure_trash_root`]: Self::ensure_trash_root
    fn trash_roots(&self) -> Result<Vec<NodeId>> {
        let mut roots: Vec<NodeId> = self
            .root_children_raw()?
            .into_iter()
            .filter(|id| self.kind_of(*id) == Some(Kind::Trash))
            .collect();
        // Lowest id wins, so every peer picks the same one without talking.
        // `NodeId` is `Ord` for exactly this kind of tie-break.
        roots.sort_unstable();
        Ok(roots)
    }

    /// The trash root to write into, creating it if this workspace has never had
    /// one and folding away any duplicates it finds on the way.
    ///
    /// **Lazily, never in [`create`]**: a workspace that has never binned
    /// anything has an empty library, which is what every test and every
    /// first-run screen expects.
    ///
    /// [`create`]: Self::create
    fn ensure_trash_root(&mut self) -> Result<NodeId> {
        let roots = self.trash_roots()?;
        let Some((&canonical, duplicates)) = roots.split_first() else {
            return self.insert(Parent::Root, Node::trash()?);
        };
        // A union made of ordinary ops, so it converges like anything else.
        for &dup in duplicates {
            for child in self.children(Parent::Node(dup))? {
                let end = self.children(Parent::Node(canonical))?.len();
                self.move_node(child, Parent::Node(canonical), end)?;
            }
            self.delete(dup)?;
        }
        Ok(canonical)
    }

    fn mark_trashed(
        &mut self,
        id: NodeId,
        at_ms: i64,
        origin: Option<NodeId>,
        by: Option<&str>,
    ) -> Result<()> {
        let tid = self.tree_id(id)?;
        let meta = self.nodes().get_meta(tid)?;
        meta.insert(K_TRASHED_AT, at_ms)?;
        write_or_clear(&meta, K_ORIGIN, origin.map(|o| o.to_storage()))?;
        write_or_clear(&meta, K_TRASHED_BY, by.map(str::to_owned))?;
        // Exactly one of the two is ever live, so the strip and the hand-off
        // toast cannot both claim to be current.
        write_or_clear(&meta, K_RESTORED_BY, None)?;
        self.doc.commit();
        Ok(())
    }

    fn clear_trash_marks(&mut self, id: NodeId, by: Option<&str>) -> Result<()> {
        let tid = self.tree_id(id)?;
        let meta = self.nodes().get_meta(tid)?;
        write_or_clear(&meta, K_TRASHED_AT, None)?;
        write_or_clear(&meta, K_ORIGIN, None)?;
        write_or_clear(&meta, K_TRASHED_BY, None)?;
        write_or_clear(&meta, K_RESTORED_BY, by.map(str::to_owned))?;
        self.doc.commit();
        Ok(())
    }

    /// Replace a text node's line. Uses `update` so Loro records a minimal
    /// diff rather than a delete-all-then-insert, which is what lets two peers
    /// edit different parts of the same line and both survive.
    pub fn set_text(&mut self, target: NodeId, text: &str) -> Result<()> {
        if text.contains('\n')
            && !self
                .kind_of(target)
                .map(|k| k.allows_newlines())
                .unwrap_or(false)
        {
            return Err(ShapeError::NewlineInText.into());
        }
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        let t = meta.ensure_mergeable_text(K_TEXT)?;
        t.update(text, Default::default())?;
        self.doc.commit();
        Ok(())
    }

    /// Set the display name of content that has no text of its own.
    ///
    /// Kept in node metadata so renaming an image never changes its blob
    /// content, dimensions, or identity. `None` removes the explicit name and
    /// lets a view fall back to its generated label.
    pub fn set_name(&mut self, target: NodeId, name: Option<&str>) -> Result<()> {
        if name.is_some_and(|name| name.contains('\n')) {
            return Err(ShapeError::NewlineInText.into());
        }
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match name {
            Some(name) => meta.insert(K_NAME, name)?,
            None if meta.get(K_NAME).is_some() => meta.delete(K_NAME)?,
            None => {}
        }
        self.doc.commit();
        Ok(())
    }

    /// Set a node's optional Unicode icon. Standard emoji are stored directly
    /// as text metadata so every Loro peer sees the same portable value.
    pub fn set_icon(&mut self, target: NodeId, icon: Option<&str>) -> Result<()> {
        if icon.is_some_and(|icon| icon.contains('\n')) {
            return Err(ShapeError::NewlineInText.into());
        }
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match icon {
            Some(icon) if !icon.is_empty() => meta.insert(K_ICON, icon)?,
            Some(_) | None if meta.get(K_ICON).is_some() => meta.delete(K_ICON)?,
            Some(_) | None => {}
        }
        self.doc.commit();
        Ok(())
    }

    /// Turn `target` into an image: its kind becomes `Image` and its content the
    /// given blob, replacing any text. The node keeps its identity and place in
    /// the tree, so the slash command turns the line you typed on into the image
    /// rather than shuffling nodes. The bytes themselves never enter the CRDT —
    /// only the hash, mime and dimensions do ([ADR-12](../../../docs/decisions/README.md)).
    pub fn set_image(
        &mut self,
        target: NodeId,
        hash: BlobHash,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) -> Result<()> {
        Node::check(
            Kind::Image,
            &Content::blob(hash.clone(), mime, width, height),
        )?;
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        // The blob fields take over; drop the text container and its attributes
        // so `read_node` cannot see two content shapes at once.
        if meta.get(K_TEXT).is_some() {
            meta.delete(K_TEXT)?;
        }
        if meta.get(K_ATTRS).is_some() {
            meta.delete(K_ATTRS)?;
        }
        meta.insert(K_KIND, Kind::Image.as_str())?;
        meta.insert(K_BLOB, hash.0.as_str())?;
        meta.insert(K_MIME, mime)?;
        match width {
            Some(w) => meta.insert(K_WIDTH, w as i64)?,
            None => {
                if meta.get(K_WIDTH).is_some() {
                    meta.delete(K_WIDTH)?;
                }
            }
        }
        match height {
            Some(h) => meta.insert(K_HEIGHT, h as i64)?,
            None => {
                if meta.get(K_HEIGHT).is_some() {
                    meta.delete(K_HEIGHT)?;
                }
            }
        }
        self.doc.commit();
        let locator = self
            .index
            .get_mut(&target)
            .ok_or(StoreError::NoSuchNode(target))?;
        locator.blob_hash = Some(hash.0);
        if !self.blob_nodes.contains(&target) {
            self.blob_nodes.push(target);
        }
        Ok(())
    }

    /// Rewrite only the address of migrated blob content. Unlike `set_image`,
    /// this preserves the node kind (notably Canvas), dimensions and MIME type.
    pub fn rewrite_blob_hash(&mut self, target: NodeId, hash: BlobHash) -> Result<()> {
        let node = self.read(target)?;
        if !matches!(node.content, Content::Blob { .. }) {
            return Err(StoreError::Shape(ShapeError::WrongContent {
                kind: node.kind,
                expected: node.kind.shape(),
            }));
        }
        let meta = self.nodes().get_meta(self.tree_id(target)?)?;
        meta.insert(K_BLOB, hash.0.as_str())?;
        self.doc.commit();
        if let Some(locator) = self.index.get_mut(&target) {
            locator.blob_hash = Some(hash.0);
        }
        Ok(())
    }

    /// Relabel a node's kind in place, keeping its identity, tree position,
    /// text and attributes. Validates that the existing content is legal for
    /// the new kind: a Text→Code or Code→Paragraph relabel is fine; relabelling
    /// to a blob kind is refused (use [`set_image`](Self::set_image), which
    /// swaps the content too). This is the primitive behind `/code` and the
    /// markdown fence.
    pub fn set_kind(&mut self, target: NodeId, kind: Kind) -> Result<()> {
        self.atomic(|ws| {
            let node = ws.read(target)?;
            Node::check(kind, &node.content)?;
            // A kind that cannot hold children must not be left holding any:
            // relabelling a parent task into a code block (or a table, an image,
            // a quote) with its sub-tasks still under it would strand them under
            // a node the tree model says cannot adopt. Outdent them into the
            // node's place first — the same promotion a delete does — so the text
            // keeps its identity and nothing is lost.
            if !kind.accepts_children() {
                let parent = ws.parent(target)?;
                let index = ws.sibling_index(target)?;
                let children = ws.children(Parent::Node(target))?;
                for (offset, &child) in children.iter().enumerate() {
                    ws.move_node(child, parent, index + offset)?;
                }
            }
            let tid = ws.tree_id(target)?;
            let meta = ws.nodes().get_meta(tid)?;
            meta.insert(K_KIND, kind.as_str())?;
            ws.record_schema_for_kind(kind)?;
            // A paragraph relabelled into a document becomes a thing Explore
            // lists, so it needs an id from that moment. `mint_document_id` is a
            // no-op when one is already there, which is what makes relabelling a
            // document back and forth keep the id it started with.
            mint_document_id(&meta, kind)?;
            ws.doc.commit();
            Ok(())
        })
    }

    /// A document's own id.
    ///
    /// Minted when the node becomes a document and stored beside it, so it is
    /// stable across restarts, moves and renames — the properties a `NodeId`
    /// already has and a title never did.
    ///
    /// **Falls back to deriving one from the `NodeId` when the key is absent.**
    /// That is the legacy path: a workspace written before documents had ids of
    /// their own. The derivation is deterministic, so two readers of the same
    /// old snapshot agree without talking, and it is *not* a way of minting ids
    /// in general — a fresh document gets a fresh UUID like everything else
    /// ([ADR-39](../../../docs/decisions/ADR-026-050.md#adr-39-identity--identity-is-client-minted-document-scoped-and-cloud-ready)). It exists so an old
    /// workspace lists rather than showing an empty Explore, and it goes when
    /// the import does.
    pub fn document_id(&self, target: NodeId) -> DocumentId {
        self.tree_id(target)
            .ok()
            .and_then(|tid| self.nodes().get_meta(tid).ok())
            .and_then(|meta| read_document_id(&meta))
            .unwrap_or_else(|| DocumentId::from_uuid(target.as_uuid()))
    }

    /// Raise an older workspace's marker only when a write introduces content
    /// that the older schema cannot understand. Ordinary edits keep their
    /// existing marker; adding a Divider, Quote or Callout makes the
    /// compatibility gate tell the corresponding older build to open read-only.
    fn record_schema_for_kind(&self, kind: Kind) -> Result<()> {
        let required = match kind {
            Kind::Divider => 2,
            Kind::Quote => 3,
            Kind::Callout => 5,
            _ => return Ok(()),
        };
        if self.effective_schema_version() < required {
            self.doc.get_map(C_META).insert(K_SCHEMA, required)?;
        }
        Ok(())
    }

    /// Move the checkbox, and nothing else.
    ///
    /// **The ordinary checkbox door** ([ADR-286]). It never creates a task,
    /// never touches the canonical lifecycle, and is a guarded no-op when the
    /// value is already what was asked for — answers whether anything moved.
    /// `checkpoint()` then one `atomic`, so a click is its own undo step.
    ///
    /// This is the plain-checklist path, which Tasks does not touch, **and**
    /// the rich-task box, which SPEC §5 says is only the box. What a checkbox
    /// click *means* is decided in the domain by
    /// [`tasks::checkbox_completes`](crate::tasks::checkbox_completes): true,
    /// and the caller runs `complete_task`/`reopen_task`; false, and it runs
    /// this.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub fn set_checkbox(&mut self, target: NodeId, done: bool) -> Result<bool> {
        if self.read(target)?.meta.done == done {
            return Ok(false);
        }
        self.checkpoint();
        self.atomic(|ws| ws.set_done(target, done))?;
        Ok(true)
    }

    /// Write `NodeMeta::done` raw — no guard, no undo group, no lifecycle.
    ///
    /// **Owed a narrowing to `pub(crate)`** ([ADR-286]): the checkbox, paste
    /// and MCP land on [`set_checkbox`](Self::set_checkbox) or on
    /// `complete_task`/`reopen_task`, and the importers land on
    /// [`tasks::faithful_import::restore_done`](crate::tasks::faithful_import::restore_done).
    /// Those rewires belong to the plans that own their call sites (F-4a, F-8,
    /// D-2); until they land this stays public so the tree compiles, and it is
    /// **not** the door for anything new.
    ///
    /// It is also one of the four operations ADR-272's validate-first proof
    /// allows in a command's mutation phase: `tree_id` → `get_meta` → one
    /// `insert`, reading nothing and parsing nothing.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub fn set_done(&mut self, target: NodeId, done: bool) -> Result<()> {
        let tid = self.tree_id(target)?;
        self.nodes().get_meta(tid)?.insert(K_DONE, done)?;
        self.doc.commit();
        Ok(())
    }

    /// Set a node's presentation color — an `AccentColor` token, never RGB.
    /// A callout's tint is the one reader today ([`crate::callout`]). `None`
    /// (or empty) removes it, which draws the default.
    pub fn set_color(&mut self, target: NodeId, color: Option<&str>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match color {
            Some(color) if !color.is_empty() => meta.insert(K_COLOUR, color)?,
            Some(_) | None if meta.get(K_COLOUR).is_some() => meta.delete(K_COLOUR)?,
            Some(_) | None => {}
        }
        self.doc.commit();
        Ok(())
    }

    pub fn set_level_name(&mut self, target: NodeId, level: Option<&str>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match level {
            Some(l) => meta.insert(K_LEVEL, l)?,
            None => meta.delete(K_LEVEL)?,
        }
        self.doc.commit();
        Ok(())
    }

    /// How a block's lines sit in their column. `None` restores the default.
    ///
    /// A block property rather than a mark: alignment applies to the whole
    /// node, and marking half a line "centred" means nothing.
    pub fn set_align(&mut self, target: NodeId, align: Option<Align>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match align {
            Some(a) => meta.insert(K_ALIGN, a.as_str())?,
            None => meta.delete(K_ALIGN)?,
        }
        self.doc.commit();
        Ok(())
    }

    /// This block's heading level. `None` returns it to body text.
    ///
    /// Stored as the *number* rather than a name, unlike [`set_align`]: a
    /// heading level is ordinal — a table of contents asks for "shallower than
    /// three" — and a level from a newer peer clamps to something this build can
    /// draw, where an unknown name could only be dropped.
    ///
    /// [`set_align`]: Self::set_align
    pub fn set_heading(&mut self, target: NodeId, heading: Option<Heading>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match heading {
            Some(h) => meta.insert(K_HEADING, h.level() as i64)?,
            // Guarded the way `set_image_size` is: deleting a key that is not
            // there is still an op, and therefore still an undo step — and
            // setting body text on body text should not leave one behind.
            None if meta.get(K_HEADING).is_some() => meta.delete(K_HEADING)?,
            None => {}
        }
        self.doc.commit();
        Ok(())
    }

    /// The list style of a [`Kind::ListBlock`] container — how its item
    /// children render. `None` clears it. Meaningful only on a ListBlock; on
    /// any other kind this is a no-op rather than writing a key nothing reads.
    pub fn set_list_style(&mut self, target: NodeId, style: Option<ListStyle>) -> Result<()> {
        if !matches!(self.read(target).map(|n| n.kind), Ok(Kind::ListBlock)) {
            return Ok(());
        }
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match style {
            Some(s) => meta.insert(K_LIST_STYLE, s.as_str())?,
            None if meta.get(K_LIST_STYLE).is_some() => meta.delete(K_LIST_STYLE)?,
            None => {}
        }
        self.doc.commit();
        Ok(())
    }

    /// The grammar a code block highlights as (`"rust"`, `"py"`, …). `None`
    /// restores auto-detect. Stored as *absent* when `None`, so a document
    /// written before code blocks existed reads back unchanged. The highlight
    /// colour itself is never stored — it is derived from the text and this
    /// language at layout time.
    pub fn set_language(&mut self, target: NodeId, language: Option<&str>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match language {
            Some(l) => meta.insert(K_LANG, l)?,
            None if meta.get(K_LANG).is_some() => meta.delete(K_LANG)?,
            None => {}
        }
        self.doc.commit();
        Ok(())
    }

    /// The language a document's prose is written in (BCP 47) — the shared
    /// spelling override, on the document's root node. `None` restores "the
    /// reader's own default" and is stored as absence. A `cutting`/`plant`
    /// copy carries it. One caveat the ADR carries: a value-level copy made by
    /// a build older than the canvas model (P1.1) rebuilds the meta without
    /// this key and drops it — `write_node` did not write it until then — while
    /// in-place edits and saves preserve it on every build.
    pub fn set_prose_language(&mut self, target: NodeId, language: Option<&str>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match language {
            Some(l) => meta.insert(K_PROSE_LANGUAGE, l)?,
            None if meta.get(K_PROSE_LANGUAGE).is_some() => meta.delete(K_PROSE_LANGUAGE)?,
            None => {}
        }
        self.doc.commit();
        Ok(())
    }

    /// Set a table's synced width policy. Compact is stored as absence so old
    /// tables need no migration. Calling this for another kind is a no-op.
    pub fn set_table_layout(&mut self, target: NodeId, layout: TableLayout) -> Result<()> {
        if !matches!(self.read(target).map(|n| n.kind), Ok(Kind::Table)) {
            return Ok(());
        }
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match layout {
            TableLayout::Extended => meta.insert(K_TABLE_LAYOUT, layout.as_str())?,
            TableLayout::Compact if meta.get(K_TABLE_LAYOUT).is_some() => {
                meta.delete(K_TABLE_LAYOUT)?;
            }
            TableLayout::Compact => {}
        }
        self.doc.commit();
        Ok(())
    }

    /// The size an image is drawn at, in logical pixels. `None` restores the
    /// natural size (fill the column at the blob's aspect ratio).
    pub fn set_image_size(&mut self, target: NodeId, size: Option<(u32, u32)>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match size {
            Some((w, h)) => {
                meta.insert(K_IMG_W, w as i64)?;
                meta.insert(K_IMG_H, h as i64)?;
            }
            None => {
                if meta.get(K_IMG_W).is_some() {
                    meta.delete(K_IMG_W)?;
                }
                if meta.get(K_IMG_H).is_some() {
                    meta.delete(K_IMG_H)?;
                }
            }
        }
        self.doc.commit();
        Ok(())
    }

    /// Where this node sits on a canvas, as **one whole value**; `None` takes
    /// it off the canvas (it becomes an arrival). Clamped to
    /// ±[`CANVAS_LIMIT`]. Writing the position it already has is no op and no
    /// undo step — Loro skips a map insert of an equal value.
    ///
    /// The raw door. [`crate::canvas`]'s writers are what a surface calls.
    pub fn set_at(&mut self, target: NodeId, at: Option<CanvasPoint>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match at {
            Some(at) => meta.insert(K_AT, at_to_loro(at))?,
            None if meta.get(K_AT).is_some() => meta.delete(K_AT)?,
            None => {}
        }
        self.doc.commit();
        Ok(())
    }

    /// How large this node is drawn on a canvas, as one whole value; `None`
    /// removes it. Each side is clamped to `1..=`[`CANVAS_LIMIT`].
    pub fn set_extent(&mut self, target: NodeId, extent: Option<CanvasExtent>) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        match extent {
            Some(extent) => meta.insert(K_EXTENT, extent_to_loro(extent))?,
            None if meta.get(K_EXTENT).is_some() => meta.delete(K_EXTENT)?,
            None => {}
        }
        self.doc.commit();
        Ok(())
    }

    // ------------------------------------------------------- outline shape

    /// Where a node hangs. The inverse of the `parent` argument to [`insert`].
    ///
    /// [`insert`]: Self::insert
    pub fn parent(&self, id: NodeId) -> Result<Parent> {
        let tree = self.nodes();
        Ok(match tree.parent(self.tree_id(id)?) {
            Some(TreeParentId::Node(p)) => match read_node_id(&tree.get_meta(p)?) {
                Some(nid) => Parent::Node(nid),
                // A tree parent whose meta carries no `NodeId` is not a node
                // this model minted — so the honest answer is the top of the
                // document rather than a parent nobody can name.
                None => Parent::Root,
            },
            _ => Parent::Root,
        })
    }

    /// A node's position among its siblings.
    pub fn sibling_index(&self, id: NodeId) -> Result<usize> {
        self.children(self.parent(id)?)?
            .iter()
            .position(|&c| c == id)
            .ok_or(StoreError::NoSuchNode(id))
    }

    /// Move a node up or down among its siblings. Returns false at either end,
    /// where there is nowhere to go — Alt+↑ on the first row is not an error.
    pub fn shift(&mut self, id: NodeId, delta: i32) -> Result<bool> {
        let parent = self.parent(id)?;
        let count = self.children(parent)?.len();
        let from = self.sibling_index(id)?;
        let Some(to) = from
            .checked_add_signed(delta as isize)
            .filter(|&t| t < count)
        else {
            return Ok(false);
        };
        self.move_node(id, parent, to)?;
        Ok(true)
    }

    /// Indent: become the last child of the sibling above.
    ///
    /// This is why the old `indent <= parent + 1` repair pass is gone. There is
    /// no sibling above at position zero, so the illegal state cannot be
    /// reached — depth is structural rather than a number to be validated.
    pub fn indent(&mut self, id: NodeId) -> Result<bool> {
        let parent = self.parent(id)?;
        let siblings = self.children(parent)?;
        let Some(index) = siblings.iter().position(|&c| c == id) else {
            return Err(StoreError::NoSuchNode(id));
        };
        let Some(&above) = index.checked_sub(1).and_then(|i| siblings.get(i)) else {
            return Ok(false);
        };
        if !self.read(above)?.kind.accepts_children() {
            return Ok(false);
        }
        let at = self.children(Parent::Node(above))?.len();
        self.move_node(id, Parent::Node(above), at)?;
        Ok(true)
    }

    /// Outdent: become the next sibling of the current parent.
    pub fn outdent(&mut self, id: NodeId) -> Result<bool> {
        let Parent::Node(parent) = self.parent(id)? else {
            return Ok(false);
        };
        let grandparent = self.parent(parent)?;
        let at = self.sibling_index(parent)? + 1;
        self.move_node(id, grandparent, at)?;
        Ok(true)
    }

    /// Outdent while keeping document order.
    ///
    /// Plain [`outdent`](Self::outdent) moves only `id`, so following siblings
    /// that were under the old parent stay there and **draw above** the moved
    /// row (they are still walked as children of the old parent first). List
    /// editing wants the opposite: the row stays above those siblings, and they
    /// become its children — Enter/Shift+Tab on an empty nested bullet with a
    /// peer below it must not jump past that peer.
    ///
    /// One undo step. Returns `false` at the root (nothing to outdent under).
    pub fn outdent_keeping_order(&mut self, id: NodeId) -> Result<bool> {
        self.atomic(|ws| {
            let Parent::Node(parent) = ws.parent(id)? else {
                return Ok(false);
            };
            let siblings = ws.children(Parent::Node(parent))?;
            let Some(index) = siblings.iter().position(|&c| c == id) else {
                return Err(StoreError::NoSuchNode(id));
            };
            let following: Vec<NodeId> = siblings[index + 1..].to_vec();
            let grandparent = ws.parent(parent)?;
            let at = ws.sibling_index(parent)? + 1;
            ws.move_node(id, grandparent, at)?;
            let mut child_at = ws.children(Parent::Node(id))?.len();
            for child in following {
                ws.move_node(child, Parent::Node(id), child_at)?;
                child_at += 1;
            }
            Ok(true)
        })
    }

    /// Outdent, but never above `boundary`.
    ///
    /// A document's list items live under the document node, so the outermost
    /// list level is that node's *direct children* — outdenting one of those
    /// with plain [`outdent`](Self::outdent) would move the item out of the
    /// document (to be a sibling of the document in the library). This refuses
    /// instead, returning `false`, which is what lets Shift+Tab "stop at level
    /// 1" and Enter-on-empty "leave the list" at the outermost level. Returns
    /// `false` when `id`'s parent is already `boundary` (or root).
    pub fn outdent_within(&mut self, id: NodeId, boundary: NodeId) -> Result<bool> {
        if self.parent(id)? == Parent::Node(boundary) {
            return Ok(false);
        }
        self.outdent(id)
    }

    /// [`outdent_keeping_order`](Self::outdent_keeping_order), never above
    /// `boundary`. Used for nested list items; outermost ListBlock children use
    /// [`extract_list_item`](Self::extract_list_item) instead.
    pub fn outdent_within_keeping_order(&mut self, id: NodeId, boundary: NodeId) -> Result<bool> {
        if self.parent(id)? == Parent::Node(boundary) {
            return Ok(false);
        }
        self.outdent_keeping_order(id)
    }

    /// The text's length in bytes.
    pub fn text_len(&self, target: NodeId) -> Result<usize> {
        Ok(self.text_handle(target)?.len_utf8())
    }

    /// Split a line at a byte offset, the tail becoming a new node.
    ///
    /// A node with children takes the tail as its *first child* rather than as
    /// a following sibling, because a sibling would appear below the children
    /// and Enter at the end of a heading would jump the caret past its subtree.
    pub fn split(&mut self, target: NodeId, at: usize) -> Result<NodeId> {
        self.split_with_id(target, at, NodeId::new())
    }

    /// The same split with a caller-owned id, for idempotent native commands.
    pub fn split_with_id(&mut self, target: NodeId, at: usize, tail_id: NodeId) -> Result<NodeId> {
        if self.kind_of(tail_id).is_some() {
            return Err(StoreError::Malformed("split target already exists"));
        }
        self.atomic(|ws| {
            let node = ws.read(target)?;
            let len = ws.text_len(target)?;
            let at = at.min(len);

            let has_children = !ws.children(Parent::Node(target))?.is_empty();
            let (parent, index) = if has_children && node.kind.accepts_children() {
                (Parent::Node(target), 0)
            } else {
                (ws.parent(target)?, ws.sibling_index(target)? + 1)
            };

            let mut tail_node = Node::new(node.kind, Content::text(""))?;
            tail_node.id = tail_id;
            let tail = ws.insert(parent, tail_node)?;
            ws.move_node(tail, parent, index)?;
            // A heading carries to the tail only when the split leaves text
            // behind it: breaking a title in two makes two titles, but Enter at
            // the *end* of one means "now write under it", and a second empty
            // heading is never what was meant. Alignment carries not at all —
            // there is no equivalent of "at the end" for it.
            if at < len {
                if let Some(heading) = node.meta.heading {
                    ws.set_heading(tail, Some(heading))?;
                }
            }
            ws.transplant(target, at..len, tail, 0)?;
            ws.delete_text(target, at..len)?;
            Ok(tail)
        })
    }

    /// Append `source` to `target` and delete it. Returns the seam — the byte
    /// offset where the two joined, which is where the caret belongs.
    ///
    /// `source`'s children come along, so backspacing a group's heading into the
    /// line above does not strand its subtree.
    pub fn merge_into(&mut self, target: NodeId, source: NodeId) -> Result<usize> {
        self.atomic(|ws| {
            let seam = ws.text_len(target)?;
            let len = ws.text_len(source)?;
            ws.transplant(source, 0..len, target, seam)?;

            let mut at = ws.children(Parent::Node(target))?.len();
            for child in ws.children(Parent::Node(source))? {
                ws.move_node(child, Parent::Node(target), at)?;
                at += 1;
            }
            ws.delete(source)?;
            Ok(seam)
        })
    }

    /// Copy a byte range of one node's text into another, marks and pills
    /// intact. Returns how many bytes were written.
    ///
    /// Every mark is written explicitly, including the ones being turned *off*:
    /// formatting marks expand rightwards (see `configure_styles`), so plain
    /// text inserted after a bold word would otherwise arrive bold.
    fn transplant(
        &mut self,
        from: NodeId,
        range: std::ops::Range<usize>,
        to: NodeId,
        at: usize,
    ) -> Result<usize> {
        let runs = self.runs(from)?;
        let attrs = self.read(from)?.content.attrs().to_vec();
        let mut cursor = at;

        for run in runs {
            let start = run.start.get().max(range.start);
            let end = run.end.get().min(range.end);
            if start >= end {
                continue;
            }

            if let Some(id) = run.pill {
                // A pill only travels whole; a half-copied anchor is not a pill.
                if run.start.get() < range.start || run.end.get() > range.end {
                    continue;
                }
                if let Some(attr) = attrs.iter().find(|a| a.id == id) {
                    self.insert_pill(to, cursor, attr)?;
                    cursor += ANCHOR_LEN;
                }
                continue;
            }

            let slice = &run.text[start - run.start.get()..end - run.start.get()];
            let slice = slice.to_string();
            self.insert_text(to, cursor, &slice)?;
            let span = cursor..cursor + slice.len();
            for mark in Mark::ALL {
                self.set_mark(to, span.clone(), mark, run.has(mark))?;
            }
            // Values too, or a split would drop a highlight or a size.
            self.set_script(to, span.clone(), run.style.script)?;
            self.set_size(to, span.clone(), run.style.size)?;
            self.set_highlight(to, span.clone(), run.style.highlight.as_deref())?;
            cursor += slice.len();
        }
        Ok(cursor - at)
    }

    // ------------------------------------------------------------ queries

    /// Answer a [`TaskQuery`].
    ///
    /// Walks the outline once and filters. That is O(n) per call, which is
    /// correct and fast enough for a workspace held in memory; when it stops
    /// being enough the same signature is served by a SQLite index and callers
    /// do not change. See `docs/deferred/`.
    pub fn query(&self, q: &TaskQuery) -> Result<Vec<TaskSummary>> {
        // Restricting to a subtree is a prefix test on the ancestor chain, so
        // resolve the scope once rather than per node.
        let scope = q.under;
        let mut out = Vec::new();

        for (id, depth) in self.walk()? {
            if let Some(root) = scope {
                if id == root || !self.ancestors(id)?.contains(&root) {
                    continue;
                }
            }
            let node = self.read(id)?;
            let children = self.children(Parent::Node(id))?;
            if !matches_node(q, &node, !children.is_empty()) {
                continue;
            }

            let mut done_children = 0u32;
            for child in &children {
                if self.read(*child).map(|c| c.meta.done).unwrap_or(false) {
                    done_children += 1;
                }
            }

            out.push(TaskSummary {
                id,
                text: node.content.as_text().unwrap_or_default().to_string(),
                done: node.meta.done,
                due: node.due(),
                assignees: node.assignees().map(|r| self.resolve(r)).collect(),
                meta: node.meta.clone(),
                depth,
                progress: (done_children, children.len() as u32),
            });
        }
        Ok(out)
    }

    /// Look up a reference.
    ///
    /// Only this document is searched for now, so a target in a profile library
    /// or another profile reports as dangling. That is the correct *shape* of
    /// the answer — a reference whose target is not reachable is a normal state
    /// that renders as a placeholder — and the locations index that widens the
    /// search is in `docs/deferred/`.
    pub fn resolve(&self, r: Ref) -> ResolvedRef {
        match self.read(r.node) {
            Ok(node) => ResolvedRef::Found {
                id: r.node,
                label: label_of(&node),
                icon: node.meta.icon.clone(),
            },
            Err(_) => ResolvedRef::Dangling { id: r.node },
        }
    }

    // ------------------------------------------------------------ text

    /// The node's text split into runs of uniform formatting.
    ///
    /// This is what feeds the renderer: walk the runs, push a style per mark,
    /// and place an inline box wherever `pill` is set. It replaces parsing
    /// `**bold**` out of a string.
    pub fn runs(&self, target: NodeId) -> Result<Vec<TextRun>> {
        Ok(runs_from_text(&self.text_handle(target)?))
    }

    /// The marks covering every run overlapping `range` — what a toolbar shows,
    /// and what decides whether Ctrl+B sets or clears.
    pub fn marks_in(&self, target: NodeId, range: std::ops::Range<usize>) -> Result<MarkSet> {
        let runs = self.runs(target)?;
        let touching: Vec<_> = runs
            .iter()
            .filter(|r| r.start.get() < range.end && range.start < r.end.get())
            .collect();
        Ok(MarkSet::covering(touching))
    }

    /// The marks text inserted at `at` would come out wearing.
    ///
    /// Distinct from [`marks_in`] with an empty range, which touches no run at
    /// all. Formatting marks expand rightwards (see `configure_styles`), so an
    /// insertion inherits from the run *ending* at `at` — which is what makes
    /// "type at the end of a bold word and it stays bold" true, and what the
    /// editor has to know before it can offer to turn that off.
    ///
    /// [`marks_in`]: Self::marks_in
    pub fn marks_at(&self, target: NodeId, at: usize) -> Result<MarkSet> {
        let runs = self.runs(target)?;
        Ok(runs
            .iter()
            // A pill anchor never expands, so it lends its marks to nothing.
            .filter(|r| !r.is_pill())
            .find(|r| r.start.get() < at && at <= r.end.get())
            .map(|r| MarkSet {
                bold: r.bold,
                italic: r.italic,
                underline: r.underline,
                strike: r.strike,
                code: r.code,
            })
            .unwrap_or_default())
    }

    /// The value marks covering a byte range, with anything that differs
    /// between runs reported as `None` — see [`TextStyle::covering`].
    pub fn style_in(&self, target: NodeId, range: std::ops::Range<usize>) -> Result<TextStyle> {
        let runs = self.runs(target)?;
        let touching: Vec<_> = runs
            .iter()
            .filter(|r| r.start.get() < range.end && range.start < r.end.get())
            .collect();
        Ok(TextStyle::covering(touching))
    }

    /// The value marks text inserted at `at` would come out wearing — the
    /// value counterpart of [`marks_at`].
    ///
    /// [`marks_at`]: Self::marks_at
    pub fn style_at(&self, target: NodeId, at: usize) -> Result<TextStyle> {
        let runs = self.runs(target)?;
        Ok(runs
            .iter()
            .filter(|r| !r.is_pill())
            .find(|r| r.start.get() < at && at <= r.end.get())
            .map(|r| r.style.clone())
            .unwrap_or_default())
    }

    /// Superscript, subscript, or neither.
    pub fn set_script(
        &mut self,
        target: NodeId,
        range: std::ops::Range<usize>,
        script: Option<Script>,
    ) -> Result<()> {
        match script {
            Some(s) => self.mark_value(target, range, SCRIPT_KEY, s.as_str()),
            None => self.clear_mark(target, range, SCRIPT_KEY),
        }
    }

    /// An explicit font size in points, or `None` to follow the theme.
    pub fn set_size(
        &mut self,
        target: NodeId,
        range: std::ops::Range<usize>,
        size: Option<f32>,
    ) -> Result<()> {
        match size {
            Some(v) => self.mark_value(target, range, SIZE_KEY, v as f64),
            None => self.clear_mark(target, range, SIZE_KEY),
        }
    }

    /// A highlight colour by *name*, resolved by the theme so it stays legible
    /// in both light and dark. Storing RGB would freeze it to one theme.
    pub fn set_highlight(
        &mut self,
        target: NodeId,
        range: std::ops::Range<usize>,
        colour: Option<&str>,
    ) -> Result<()> {
        match colour {
            Some(c) => self.mark_value(target, range, HIGHLIGHT_KEY, c),
            None => self.clear_mark(target, range, HIGHLIGHT_KEY),
        }
    }

    fn mark_value(
        &mut self,
        target: NodeId,
        range: std::ops::Range<usize>,
        key: &str,
        value: impl Into<LoroValue>,
    ) -> Result<()> {
        if range.is_empty() {
            return Ok(());
        }
        self.text_handle(target)?.mark_utf8(range, key, value)?;
        self.doc.commit();
        Ok(())
    }

    /// `unmark_utf8` does not exist, so the byte range is converted through
    /// Loro rather than by counting characters — that keeps the two in
    /// agreement about what a position is.
    fn clear_mark(
        &mut self,
        target: NodeId,
        range: std::ops::Range<usize>,
        key: &str,
    ) -> Result<()> {
        if range.is_empty() {
            return Ok(());
        }
        let text = self.text_handle(target)?;
        let to_unicode = |b: usize| {
            text.convert_pos(
                b,
                loro::cursor::PosType::Bytes,
                loro::cursor::PosType::Unicode,
            )
            .ok_or(StoreError::Malformed("byte offset outside the text"))
        };
        text.unmark(to_unicode(range.start)?..to_unicode(range.end)?, key)?;
        self.doc.commit();
        Ok(())
    }

    /// Apply or clear one mark over a byte range.
    pub fn set_mark(
        &mut self,
        target: NodeId,
        range: std::ops::Range<usize>,
        mark: Mark,
        on: bool,
    ) -> Result<()> {
        if range.is_empty() {
            return Ok(());
        }
        let text = self.text_handle(target)?;
        if on {
            text.mark_utf8(range, mark.key(), true)?;
        } else {
            // `mark_utf8` exists but `unmark_utf8` does not, so the range has
            // to be converted. Doing it through Loro rather than by counting
            // chars keeps the two in agreement about what a position is.
            let to_unicode = |b: usize| {
                text.convert_pos(
                    b,
                    loro::cursor::PosType::Bytes,
                    loro::cursor::PosType::Unicode,
                )
                .ok_or(StoreError::Malformed("byte offset outside the text"))
            };
            text.unmark(to_unicode(range.start)?..to_unicode(range.end)?, mark.key())?;
        }
        self.doc.commit();
        Ok(())
    }

    /// Insert text at a byte offset, leaving surrounding marks and pills alone.
    pub fn insert_text(&mut self, target: NodeId, at: usize, text: &str) -> Result<()> {
        if text.contains('\n')
            && !self
                .kind_of(target)
                .map(|k| k.allows_newlines())
                .unwrap_or(false)
        {
            return Err(ShapeError::NewlineInText.into());
        }
        self.text_handle(target)?.insert_utf8(at, text)?;
        self.doc.commit();
        Ok(())
    }

    /// Replace a byte range as one local undo step.
    ///
    /// The entire range is validated before the first write. This is the
    /// commit path for a picker that captured its target earlier and resolved
    /// it after possible peer edits.
    pub fn replace_text(
        &mut self,
        target: NodeId,
        range: std::ops::Range<usize>,
        replacement: &str,
    ) -> Result<usize> {
        if replacement.contains('\n') {
            return Err(ShapeError::NewlineInText.into());
        }
        let current = self
            .read(target)?
            .content
            .as_text()
            .ok_or(StoreError::Malformed("node has no text"))?
            .to_owned();
        if range.start > range.end
            || range.end > current.len()
            || !current.is_char_boundary(range.start)
            || !current.is_char_boundary(range.end)
        {
            return Err(StoreError::Malformed("byte range outside the text"));
        }
        let caret = range.start + replacement.len();
        self.atomic(|ws| {
            if !range.is_empty() {
                ws.delete_text(target, range.clone())?;
            }
            if !replacement.is_empty() {
                ws.insert_text(target, range.start, replacement)?;
            }
            Ok(())
        })?;
        Ok(caret)
    }

    /// Delete a byte range, dropping any attribute whose anchor it removes.
    ///
    /// This is what makes backspacing over a pill delete the whole thing: the
    /// anchor is one cluster, and losing it takes the attribute with it.
    pub fn delete_text(&mut self, target: NodeId, range: std::ops::Range<usize>) -> Result<()> {
        if range.is_empty() {
            return Ok(());
        }
        let orphaned: Vec<AttrId> = self
            .runs(target)?
            .iter()
            .filter(|r| r.is_pill() && r.start.get() >= range.start && r.end.get() <= range.end)
            .filter_map(|r| r.pill)
            .collect();

        self.text_handle(target)?
            .delete_utf8(range.start, range.end - range.start)?;
        for attr in orphaned {
            self.remove_attr(target, attr)?;
        }
        self.doc.commit();
        Ok(())
    }

    /// Insert a pill: the anchor character, marked with the attribute it draws.
    ///
    /// The mark uses `ExpandType::None` so typing either side of a pill does not
    /// inherit its mark and turn ordinary text into part of the pill.
    pub fn insert_pill(&mut self, target: NodeId, at: usize, attr: &Attr) -> Result<AttrId> {
        self.set_attr(target, attr)?;
        // What the anchor is about to inherit, read *before* it exists. Every
        // mark but `pill` expands rightwards, so an anchor inserted at the end
        // of a bold or highlighted range picks that range up.
        let inherited_marks = self.marks_at(target, at)?;
        let inherited_style = self.style_at(target, at)?;

        let text = self.text_handle(target)?;
        text.insert_utf8(at, ANCHOR_STR)?;
        // Expansion is configured per style key on the document (see
        // `configure_styles`): `pill` never expands, so typing next to a pill
        // cannot absorb the new text into it.
        text.mark_utf8(at..at + ANCHOR_LEN, PILL_KEY, attr.id.to_storage())?;
        self.doc.commit();

        // Strip what it inherited, so the anchor's formatting depends on
        // nothing but the pill. Harmless for bold, since a pill is drawn over
        // its anchor — but a highlight paints a band, which would stripe it.
        //
        // Only what is *actually* set: `unmark` for a style that is not present
        // does not leave the text alone. It disturbs the neighbouring `pill`
        // span, and the anchor stops being one run of its own — which is what
        // `typing_beside_a_pill_does_not_join_it` catches.
        let span = at..at + ANCHOR_LEN;
        for mark in Mark::ALL {
            if inherited_marks.has(mark) {
                self.set_mark(target, span.clone(), mark, false)?;
            }
        }
        if inherited_style.script.is_some() {
            self.clear_mark(target, span.clone(), SCRIPT_KEY)?;
        }
        if inherited_style.size.is_some() {
            self.clear_mark(target, span.clone(), SIZE_KEY)?;
        }
        if inherited_style.highlight.is_some() {
            self.clear_mark(target, span.clone(), HIGHLIGHT_KEY)?;
        }
        Ok(attr.id)
    }

    /// Replace one complete body subtree with a paragraph containing a pill.
    ///
    /// This is the source-side half of extracting a block into another
    /// document. The replacement keeps the old subtree's parent and ordinal,
    /// but deliberately receives a fresh [`NodeId`]: the old identity moved
    /// with the content conceptually, while this paragraph is a new reference
    /// to that content. The whole replacement is one rollback-safe operation.
    ///
    /// A document title is the root of its document, not a replaceable body
    /// block, so root children are refused here rather than relying on every
    /// caller to remember that invariant.
    pub fn replace_subtree_with_pill(&mut self, target: NodeId, attr: &Attr) -> Result<NodeId> {
        self.atomic(|ws| {
            let parent = ws.parent(target)?;
            if parent == Parent::Root {
                return Err(StoreError::Malformed(
                    "a document root cannot be replaced with an inline reference",
                ));
            }
            let shape = ws.replaced_block_shape(target);
            let ordinal = ws.sibling_index(target)?;
            ws.delete(target)?;
            let replacement = ws.insert(parent, Node::paragraph("")?)?;
            ws.move_node(replacement, parent, ordinal)?;
            ws.insert_pill(replacement, 0, attr)?;
            ws.apply_replaced_block_shape(replacement, shape)?;
            Ok(replacement)
        })
    }

    /// What the replacement paragraph keeps of the block it stands in for: a
    /// text-shaped kind (a task keeps its checkbox, a quote its rule) and a
    /// heading level. Read before the delete; a blob- or container-shaped
    /// source contributes nothing — the pill needs a text row to sit in.
    fn replaced_block_shape(&self, target: NodeId) -> (Option<Kind>, Option<crate::Heading>) {
        let Ok(node) = self.read(target) else {
            return (None, None);
        };
        let kind = matches!(node.kind, Kind::Task | Kind::Quote).then_some(node.kind);
        (kind, node.meta.heading)
    }

    fn apply_replaced_block_shape(
        &mut self,
        replacement: NodeId,
        (kind, heading): (Option<Kind>, Option<crate::Heading>),
    ) -> Result<()> {
        if let Some(kind) = kind {
            self.set_kind(replacement, kind)?;
        }
        if heading.is_some() {
            self.set_heading(replacement, heading)?;
        }
        Ok(())
    }

    /// Replace several body subtrees with **one** paragraph containing a pill.
    ///
    /// The multi-root form of [`replace_subtree_with_pill`], for a selection
    /// that spans blocks: every target is deleted and a single replacement
    /// paragraph takes the first target's parent and ordinal. Targets must be
    /// selection roots — no target an ancestor of another — which is exactly
    /// what `selection_roots` hands out; a nested target would be deleted
    /// twice. The first target's place is read **before** anything is lifted
    /// out, because a delete shifts every ordinal after it.
    ///
    /// [`replace_subtree_with_pill`]: Self::replace_subtree_with_pill
    pub fn replace_subtrees_with_pill(
        &mut self,
        targets: &[NodeId],
        attr: &Attr,
    ) -> Result<NodeId> {
        let [first, rest @ ..] = targets else {
            return Err(StoreError::Malformed(
                "replacing nothing with a pill would only insert a stray reference",
            ));
        };
        self.atomic(|ws| {
            let parent = ws.parent(*first)?;
            if parent == Parent::Root {
                return Err(StoreError::Malformed(
                    "a document root cannot be replaced with an inline reference",
                ));
            }
            for target in rest {
                if ws.parent(*target)? == Parent::Root {
                    return Err(StoreError::Malformed(
                        "a document root cannot be replaced with an inline reference",
                    ));
                }
            }
            // The trail pill stands where the *first* block stood, so it
            // keeps that block's shape: a heading stays a heading, a task a
            // task. (A bullet keeps its bullet by position — the replacement
            // sits under the same `ListBlock`.)
            let shape = ws.replaced_block_shape(*first);
            let ordinal = ws.sibling_index(*first)?;
            for target in targets {
                ws.delete(*target)?;
            }
            let replacement = ws.insert(parent, Node::paragraph("")?)?;
            ws.move_node(replacement, parent, ordinal)?;
            ws.insert_pill(replacement, 0, attr)?;
            ws.apply_replaced_block_shape(replacement, shape)?;
            Ok(replacement)
        })
    }

    /// Pour runs — text and pills — into a node at byte `at`, and report where
    /// they end.
    ///
    /// A pill is re-created through [`insert_pill`] rather than by copying its
    /// anchor character: the anchor is only a pill because it carries an
    /// `AttrId`, and the attribute behind it has to exist in *this* document
    /// first. A pill whose attribute is not in `attrs` is dropped rather than
    /// written as an invisible character. Every mark is written on **or off**
    /// per run — formatting expands rightwards by design, so a run that said
    /// nothing about bold would inherit its left neighbour's.
    ///
    /// Tolerant by design: a run that fails to write is skipped, because this
    /// is the pasting/moving path and half an insertion beats none.
    ///
    /// [`insert_pill`]: Self::insert_pill
    pub fn insert_runs(
        &mut self,
        target: NodeId,
        mut at: usize,
        runs: &[TextRun],
        attrs: &[Attr],
    ) -> usize {
        for run in runs {
            if let Some(pill) = run.pill {
                if let Some(attr) = attrs.iter().find(|a| a.id == pill) {
                    if self.insert_pill(target, at, attr).is_ok() {
                        at += ANCHOR_LEN;
                    }
                }
                continue;
            }
            if run.text.is_empty() || self.insert_text(target, at, &run.text).is_err() {
                continue;
            }
            let range = at..at + run.text.len();
            for mark in Mark::ALL {
                let on = match mark {
                    Mark::Bold => run.bold,
                    Mark::Italic => run.italic,
                    Mark::Underline => run.underline,
                    Mark::Strike => run.strike,
                    Mark::Code => run.code,
                };
                let _ = self.set_mark(target, range.clone(), mark, on);
            }
            let _ = self.set_script(target, range.clone(), run.style.script);
            let _ = self.set_size(target, range.clone(), run.style.size);
            let _ = self.set_highlight(target, range.clone(), run.style.highlight.as_deref());
            at = range.end;
        }
        at
    }

    fn text_handle(&self, target: NodeId) -> Result<loro::LoroText> {
        let tid = self.tree_id(target)?;
        Ok(self.nodes().get_meta(tid)?.ensure_mergeable_text(K_TEXT)?)
    }

    // ------------------------------------------------------------ attributes

    /// Attach an attribute — a due date, an assignee, a link. This is what a
    /// pill *is*: the pill in the text renders the attribute, and a query reads
    /// the attribute without ever touching the text.
    pub fn set_attr(&mut self, target: NodeId, attr: &Attr) -> Result<AttrId> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        // Mergeable, so two peers attaching an attribute to the same node
        // converge instead of one clobbering the other's `attrs` container.
        let attrs = meta.ensure_mergeable_map(K_ATTRS)?;
        let record = attrs.ensure_mergeable_map(&attr.id.to_storage())?;
        record.insert(K_ATTR_ROLE, attr.role.as_str())?;
        if let Some(label) = &attr.label {
            record.insert(K_ATTR_LABEL, label.as_str())?;
        }
        write_attr_value(&record, &attr.value)?;
        self.doc.commit();
        Ok(attr.id)
    }

    pub fn remove_attr(&mut self, target: NodeId, attr: AttrId) -> Result<()> {
        let tid = self.tree_id(target)?;
        let meta = self.nodes().get_meta(tid)?;
        if let Some(ValueOrContainer::Container(c)) = meta.get(K_ATTRS) {
            if let Ok(attrs) = c.into_map() {
                attrs.delete(&attr.to_storage())?;
                self.doc.commit();
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ reads

    pub fn read(&self, id: NodeId) -> Result<Node> {
        let tid = self.tree_id(id)?;
        read_node(&self.nodes().get_meta(tid)?)
    }

    /// A node's children, in order.
    ///
    /// **The library root's list omits the trash.** That single omission is what
    /// makes the trash invisible everywhere else: `walk_from` is built on this,
    /// and so is every enumerating consumer — the explore column, `backlinks`,
    /// `walk_all`, `prune_empty_containers`, find-or-create, the document
    /// count. One filter here rather than a check at each of them, because a
    /// check at each of them is a check somebody will forget to add to the
    /// eighth.
    ///
    /// The consequence to know about: **indices into `Parent::Root` are in
    /// this filtered coordinate system**, and [`move_node`] translates them back
    /// before handing them to Loro. The visible list is the only list the public
    /// API speaks.
    ///
    /// [`move_node`]: Self::move_node
    pub fn children(&self, parent: Parent) -> Result<Vec<NodeId>> {
        self.children_inner(parent, parent == Parent::Root)
    }

    /// [`children`], including the trash root. Private, and the only door to it:
    /// the trash operations need the real list, nothing else does.
    ///
    /// [`children`]: Self::children
    fn root_children_raw(&self) -> Result<Vec<NodeId>> {
        self.children_inner(Parent::Root, false)
    }

    /// [`children`], with nothing filtered out — the list Loro's own indices
    /// address.
    ///
    /// `pub(crate)` for [`crate::explore`], which places a document among its
    /// siblings and so has to speak the tree's coordinates rather than the
    /// visible ones. Everything outside this crate should use [`children`],
    /// whose one omission is what keeps the bin out of every walk.
    ///
    /// [`children`]: Self::children
    pub(crate) fn children_raw(&self, parent: Parent) -> Result<Vec<NodeId>> {
        self.children_inner(parent, false)
    }

    /// Move a node to a **raw** sibling index — Loro's own coordinates, with
    /// nothing filtered and the index read after the node has been lifted out.
    ///
    /// [`move_node`] is the one to reach for: it speaks the visible list, which
    /// is the list a caller can see. This exists for [`crate::explore`], which
    /// has already done a translation of its own and must not have a second
    /// applied on top of it.
    ///
    /// [`move_node`]: Self::move_node
    pub(crate) fn move_node_raw(
        &mut self,
        target: NodeId,
        parent: Parent,
        raw: usize,
    ) -> Result<()> {
        if let Parent::Node(pid) = parent {
            self.read(pid)?.may_adopt()?;
        }
        let tid = self.tree_id(target)?;
        let under = self.resolve_parent(parent)?;
        self.nodes().mov_to(tid, under, raw)?;
        self.doc.commit();
        Ok(())
    }

    fn children_inner(&self, parent: Parent, hide_trash: bool) -> Result<Vec<NodeId>> {
        let tree = self.nodes();
        let under = self.resolve_parent(parent)?;
        let kids = tree.children(under).unwrap_or_default();
        Ok(kids
            .into_iter()
            .filter_map(|tid| {
                let meta = tree.get_meta(tid).ok()?;
                if hide_trash && is_trash(&meta) {
                    return None;
                }
                read_node_id(&meta)
            })
            .collect())
    }

    /// Ancestors nearest-first — the breadcrumb, reversed.
    pub fn ancestors(&self, id: NodeId) -> Result<Vec<NodeId>> {
        let tree = self.nodes();
        let mut out = Vec::new();
        let mut cur = self.tree_id(id)?;
        while let Some(TreeParentId::Node(p)) = tree.parent(cur) {
            if let Some(nid) = read_node_id(&tree.get_meta(p)?) {
                out.push(nid);
            }
            cur = p;
        }
        Ok(out)
    }

    pub fn depth(&self, id: NodeId) -> Result<usize> {
        Ok(self.ancestors(id)?.len())
    }

    /// Every live node in the document, depth-first in document order.
    ///
    /// Trash-filtered, because [`children`] is — see [`Parent::Root`].
    ///
    /// [`children`]: Self::children
    pub fn walk(&self) -> Result<Vec<(NodeId, usize)>> {
        let mut out = Vec::new();
        self.walk_from(Parent::Root, 0, &mut HashSet::new(), &mut out)?;
        Ok(out)
    }

    /// Everything beneath one node, depth-first, **excluding the node itself**
    /// and with depth counted from it — so its children are at depth 0.
    ///
    /// This is what a zoomed view and a document view both walk.
    pub fn walk_under(&self, root: NodeId) -> Result<Vec<(NodeId, usize)>> {
        let mut out = Vec::new();
        // `root` is already spoken for, so a tree that made it its own
        // descendant cannot walk into it a second time.
        let mut seen = HashSet::from([root]);
        self.walk_from(Parent::Node(root), 0, &mut seen, &mut out)?;
        Ok(out)
    }

    /// Every live node in the document, depth-first.
    ///
    /// **The same thing [`walk`] does, now that there is one tree.** It kept its
    /// name because the callers that ask for *everything* — the blob sweep, the
    /// empty-list-block prune — say something about their intent that `walk`
    /// does not, and renaming them would lose it.
    ///
    /// [`walk`]: Self::walk
    pub fn walk_all(&self) -> Result<Vec<(NodeId, usize)>> {
        self.walk()
    }

    /// Every node in the document, depth-first, **including the trash** — the
    /// bin's own root and everything sitting inside it.
    ///
    /// [`walk`] deliberately does not, and that omission is the load-bearing
    /// one: a single filter in [`children`] is what keeps the bin out of the
    /// explore column, out of `backlinks`, out of the document count and out of
    /// every other enumerating consumer, rather than a check each of them has
    /// to remember. Anything that *draws* the document wants [`walk`].
    ///
    /// This is the door for the one kind of reader whose job is the other
    /// answer: a projection that has to index binned content so a search can
    /// find it and offer to put it back. A row it builds from here is flagged,
    /// never hidden — the bin being visible to the index is what makes
    /// search-and-restore possible at all.
    ///
    /// Depths are counted from the document root, so the trash root is at depth
    /// 0 and a binned document is at depth 1 — one deeper than it stood before
    /// it was binned, because the trash is a place rather than a flag.
    ///
    /// [`walk`]: Self::walk
    /// [`children`]: Self::children
    pub fn walk_with_trash(&self) -> Result<Vec<(NodeId, usize)>> {
        Ok(self.walk_with_trash_checked()?.0)
    }

    /// [`walk_with_trash`], and the node ids it had to drop to keep its promise.
    ///
    /// **For the one caller that wants to name the damage** rather than only
    /// survive it. The walk drops a repeated node silently because this crate
    /// is the domain model and links no logger; a caller that has one can say
    /// which node, in which document, and that sentence is the whole diagnosis
    /// of a shape no healthy tree produces.
    ///
    /// The second value is empty for every healthy document, and costs nothing
    /// to produce: the walk was already deciding it.
    ///
    /// [`walk_with_trash`]: Self::walk_with_trash
    pub fn walk_with_trash_checked(&self) -> Result<(Vec<(NodeId, usize)>, Vec<NodeId>)> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut repeats = Vec::new();
        for child in self.root_children_raw()? {
            if !seen.insert(child) {
                repeats.push(child);
                continue;
            }
            out.push((child, 0));
            self.walk_from_reporting(Parent::Node(child), 1, &mut seen, &mut out, &mut repeats)?;
        }
        Ok((out, repeats))
    }

    /// Delete `parent` when it is a [`Kind::Callout`] left with no children —
    /// a box with nothing inside has no row to reach it from — and the box
    /// around it when that is now empty too. The edits that can empty a box
    /// (joining its last line into the row above, deleting its rows) call
    /// this with the parent the rows had. Returns how many boxes went.
    pub fn prune_empty_callout(&mut self, parent: Parent) -> Result<usize> {
        let mut pruned = 0;
        let mut at = parent;
        while let Parent::Node(id) = at {
            if self.kind_of(id) != Some(Kind::Callout) || !self.children(at)?.is_empty() {
                break;
            }
            let above = self.parent(id)?;
            self.delete(id)?;
            pruned += 1;
            at = above;
        }
        Ok(pruned)
    }

    /// Delete every container — a `Kind::ListBlock` or a `Kind::Callout` —
    /// that has no children: a cleanup pass for lists left empty when their
    /// last item exited and boxes left empty by an older build. Called on
    /// document open so stale containers from a previous session do not
    /// accumulate in explore.
    pub fn prune_empty_containers(&mut self) -> Result<()> {
        let empty: Vec<NodeId> = self
            .walk_all()?
            .into_iter()
            .filter_map(|(id, _)| {
                let node = self.read(id).ok()?;
                (matches!(node.kind, Kind::ListBlock | Kind::Callout)
                    && self
                        .children(Parent::Node(id))
                        .unwrap_or_default()
                        .is_empty())
                .then_some(id)
            })
            .collect();
        for id in empty {
            let _ = self.delete(id);
        }
        Ok(())
    }

    /// **One document, one `Kind::Document` root.** Fold any extras away.
    ///
    /// A store-registered document starts empty (ADR-80) and is seeded by
    /// whoever opens it first — the desktop's `seed_empty_document`, a phone's
    /// `seed_note_document` (ADR-277-filenote). The manifest entry and the
    /// document sync separately, so two devices can each find the note empty
    /// and each seed it, and there is no mergeable `LoroTree::create` to stop
    /// them: the merge keeps both seeds. Every reader answers with the *first*
    /// root in document order, so a second root's content is invisible — this
    /// is the repair, run from the same open path that seeds.
    ///
    /// The first root in document order wins, which is the root every reader
    /// was already showing; each duplicate's children move to the end of the
    /// winner, in order, and the emptied duplicate is deleted. **Nothing is
    /// dropped**: the fold is moves plus a delete of a root whose only
    /// remaining content is a title the winner carries too, because both were
    /// seeded from the same manifest entry.
    ///
    /// Convergence is the trash union's argument exactly (`ensure_trash_root`):
    /// the rule is a deterministic function of the merged state, so two
    /// replicas repairing the same pair issue the same moves and the same
    /// delete, and ordinary ops converge like anything else. Returns whether
    /// anything was folded, so the caller can publish.
    pub fn unify_document_roots(&mut self) -> Result<bool> {
        let roots: Vec<NodeId> = self
            .children(Parent::Root)?
            .into_iter()
            .filter(|&id| self.kind_of(id) == Some(Kind::Document))
            .collect();
        let Some((&canonical, duplicates)) = roots.split_first() else {
            return Ok(false);
        };
        if duplicates.is_empty() {
            return Ok(false);
        }
        // A union made of ordinary ops, so it converges like anything else.
        for &dup in duplicates {
            for child in self.children(Parent::Node(dup))? {
                let end = self.children(Parent::Node(canonical))?.len();
                self.move_node(child, Parent::Node(canonical), end)?;
            }
            self.delete(dup)?;
        }
        Ok(true)
    }

    /// **Every walk yields a node once**, and `seen` is what makes that true
    /// rather than assumed.
    ///
    /// A tree should not be able to hand the same child back twice, so this
    /// defends a shape no healthy document has. It earns its place because
    /// every consumer above already relies on the promise and none of them
    /// checks: the projection writes rows keyed by `(document, node)` and a
    /// repeat failed the insert, taking the whole document out of the read
    /// model and out of search for good; the phone keys its document rows by
    /// node id and a repeat is a hard `IllegalArgumentException` mid-scroll.
    /// Both were the same damaged document arriving at two surfaces, and
    /// neither surface was the right place to learn the lesson.
    ///
    /// It also bounds the recursion. Nodes move, and a move-based tree that
    /// ever made a node its own ancestor would otherwise descend until the
    /// stack ran out; skipping the second sighting turns that into a short
    /// walk instead of a crash.
    ///
    /// **Silently, because this crate is the domain model and links no
    /// logger.** A caller that wants to *name* the damage rather than only
    /// survive it takes [`walk_with_trash_checked`], which hands back what was
    /// dropped and costs nothing extra to produce.
    ///
    /// [`walk_with_trash_checked`]: Self::walk_with_trash_checked
    fn walk_from(
        &self,
        parent: Parent,
        depth: usize,
        seen: &mut HashSet<NodeId>,
        out: &mut Vec<(NodeId, usize)>,
    ) -> Result<()> {
        self.walk_from_reporting(parent, depth, seen, out, &mut Vec::new())
    }

    /// [`walk_from`](Self::walk_from), collecting what it skipped.
    fn walk_from_reporting(
        &self,
        parent: Parent,
        depth: usize,
        seen: &mut HashSet<NodeId>,
        out: &mut Vec<(NodeId, usize)>,
        repeats: &mut Vec<NodeId>,
    ) -> Result<()> {
        for child in self.children(parent)? {
            if !seen.insert(child) {
                repeats.push(child);
                continue;
            }
            out.push((child, depth));
            self.walk_from_reporting(Parent::Node(child), depth + 1, seen, out, repeats)?;
        }
        Ok(())
    }

    /// What kind of node `id` is, without reading the rest of it.
    ///
    /// One key off the meta map. [`read`] would materialise the node's text into
    /// a `String` on the way to the same answer, which is the wrong price for a
    /// question asked about every node in the document — the explore column asks
    /// it of every paragraph in every document to decide that a paragraph earns
    /// no row.
    ///
    /// `None` for a node that is not there, or whose kind this build does not
    /// know. Both mean "nothing to show", which is what every caller does with
    /// it, and it is the same answer `read` gives by failing.
    ///
    /// [`read`]: Self::read
    pub fn kind_of(&self, id: NodeId) -> Option<Kind> {
        let tid = self.index.get(&id)?.tree;
        self.nodes()
            .get_meta(tid)
            .ok()?
            .get(K_KIND)
            .and_then(as_string)
            .and_then(|s| Kind::parse(&s))
    }

    /// Every blob hash this document references.
    ///
    /// **Reads only derived blob locators, not the document.** Reindex records
    /// the optional hash while it is already visiting each node, and local
    /// mutations keep `blob_nodes` current. The shell asks this question
    /// whenever the document changes, so its cost scales with referenced blobs
    /// rather than paragraphs.
    ///
    /// Order is the walk's, and duplicates are kept: an image referenced twice
    /// is two entries. The caller is looking things up in a cache, not counting.
    pub fn blob_hashes(&self) -> Result<Vec<String>> {
        Ok(self
            .blob_nodes
            .iter()
            .filter_map(|id| self.index.get(id)?.blob_hash.clone())
            .collect())
    }
}

fn tree_has_live_nodes(tree: &loro::LoroTree) -> bool {
    tree.nodes().into_iter().any(|tid| {
        !tree.is_node_deleted(&tid).unwrap_or(false)
            && tree
                .get_meta(tid)
                .ok()
                .and_then(|meta| read_node_id(&meta))
                .is_some()
    })
}

fn cutting_from_tree(tree: &loro::LoroTree, tid: TreeID) -> Result<Cutting> {
    let meta = tree.get_meta(tid)?;
    let source = read_node(&meta)?;
    let runs = if source.content.as_text().is_some() {
        runs_from_text(&meta.ensure_mergeable_text(K_TEXT)?)
    } else {
        Vec::new()
    };
    let mut children = Vec::new();
    for child in tree.children(TreeParentId::Node(tid)).unwrap_or_default() {
        children.push(cutting_from_tree(tree, child)?);
    }
    Ok(Cutting {
        id: source.id,
        document_id: (source.kind == Kind::Document).then(|| {
            read_document_id(&meta).unwrap_or_else(|| DocumentId::from_uuid(source.id.as_uuid()))
        }),
        kind: source.kind,
        content: source.content,
        meta: source.meta,
        runs,
        children,
    })
}

fn runs_from_text(text: &loro::LoroText) -> Vec<TextRun> {
    let mut out = Vec::new();
    let mut at = 0usize;

    for delta in text.to_delta() {
        let loro::TextDelta::Insert { insert, attributes } = delta else {
            // `to_delta` on a document (rather than a diff) yields inserts
            // only; retains and deletes belong to change events.
            continue;
        };
        let len = insert.len();
        let attrs = attributes.unwrap_or_default();
        let truthy = |k: &str| -> bool { matches!(attrs.get(k), Some(LoroValue::Bool(true))) };
        let string = |k: &str| -> Option<String> {
            match attrs.get(k) {
                Some(LoroValue::String(v)) => Some(v.to_string()),
                _ => None,
            }
        };

        out.push(TextRun {
            start: Utf8Pos(at),
            end: Utf8Pos(at + len),
            bold: truthy(Mark::Bold.key()),
            italic: truthy(Mark::Italic.key()),
            underline: truthy(Mark::Underline.key()),
            strike: truthy(Mark::Strike.key()),
            code: truthy(Mark::Code.key()),
            style: TextStyle {
                script: string(SCRIPT_KEY).as_deref().and_then(Script::parse),
                size: match attrs.get(SIZE_KEY) {
                    Some(LoroValue::Double(v)) => Some(*v as f32),
                    Some(LoroValue::I64(v)) => Some(*v as f32),
                    _ => None,
                },
                highlight: string(HIGHLIGHT_KEY),
            },
            pill: attrs.get(PILL_KEY).and_then(|v| match v {
                LoroValue::String(s) => AttrId::parse(s),
                _ => None,
            }),
            text: insert,
        });
        at += len;
    }
    normalise(out)
}

// ---------------------------------------------------------------- encoding
//
// `pub(crate)` from here down: the profile document holds nodes in its library
// with exactly the same envelope, and a second encoder would be a second thing
// to keep in step with `docs/architecture/DATAMODEL.md`. One reader, one writer, two trees
// in two documents.

pub(crate) fn write_node(meta: &LoroMap, node: &Node) -> Result<()> {
    meta.insert(K_ID, node.id.to_storage())?;
    meta.insert(K_KIND, node.kind.as_str())?;
    meta.insert(K_DONE, node.meta.done)?;
    if let Some(icon) = &node.meta.icon {
        meta.insert(K_ICON, icon.as_str())?;
    }
    if let Some(name) = &node.meta.name {
        meta.insert(K_NAME, name.as_str())?;
    }
    if let Some(level) = &node.meta.level_name {
        meta.insert(K_LEVEL, level.as_str())?;
    }
    if let Some(colour) = &node.meta.colour {
        meta.insert(K_COLOUR, colour.as_str())?;
    }
    if let Some(align) = node.meta.align {
        meta.insert(K_ALIGN, align.as_str())?;
    }
    if let Some(heading) = node.meta.heading {
        meta.insert(K_HEADING, heading.level() as i64)?;
    }
    if let Some(style) = node.meta.list_style {
        meta.insert(K_LIST_STYLE, style.as_str())?;
    }
    if let Some((w, h)) = node.meta.image_size {
        meta.insert(K_IMG_W, w as i64)?;
        meta.insert(K_IMG_H, h as i64)?;
    }
    if let Some(lang) = &node.meta.language {
        meta.insert(K_LANG, lang.as_str())?;
    }
    // Written by `set_prose_language` and read by `read_node` from the start,
    // but missing here — so every `cutting`/`plant` copy of a page, on every
    // build, dropped its spelling language, not only an older build's copy.
    if let Some(lang) = &node.meta.prose_language {
        meta.insert(K_PROSE_LANGUAGE, lang.as_str())?;
    }
    if let Some(at) = node.meta.at {
        meta.insert(K_AT, at_to_loro(at))?;
    }
    if let Some(extent) = node.meta.extent {
        meta.insert(K_EXTENT, extent_to_loro(extent))?;
    }
    if node.meta.table_layout == Some(TableLayout::Extended) {
        meta.insert(K_TABLE_LAYOUT, TableLayout::Extended.as_str())?;
    }
    if let Some(at) = node.meta.trashed_at {
        meta.insert(K_TRASHED_AT, at)?;
    }
    if let Some(origin) = node.meta.origin {
        meta.insert(K_ORIGIN, origin.to_storage())?;
    }
    if let Some(by) = &node.meta.trashed_by {
        meta.insert(K_TRASHED_BY, by.as_str())?;
    }
    if let Some(by) = &node.meta.restored_by {
        meta.insert(K_RESTORED_BY, by.as_str())?;
    }

    match &node.content {
        Content::Text { text, attrs } => {
            // `ensure_mergeable_text`, not `insert_container` or the deprecated
            // `get_or_create_container`: those create op-id children, so two
            // peers creating this node concurrently would end up with two text
            // containers and one would win. The mergeable form converges.
            let t = meta.ensure_mergeable_text(K_TEXT)?;
            t.update(text, Default::default())?;
            write_attrs(meta, attrs)?;
        }
        Content::Blob {
            hash,
            mime,
            width,
            height,
            attrs,
        } => {
            meta.insert(K_BLOB, hash.0.as_str())?;
            meta.insert(K_MIME, mime.as_str())?;
            if let Some(w) = width {
                meta.insert(K_WIDTH, *w as i64)?;
            }
            if let Some(h) = height {
                meta.insert(K_HEIGHT, *h as i64)?;
            }
            // A blob carries whole-node attributes too — a due, a tag, a task
            // marker — so a copied image is still the task it was.
            write_attrs(meta, attrs)?;
        }
        Content::Embed { target } => {
            meta.insert(K_EMBED, target.node.to_storage())?;
        }
    }
    Ok(())
}

/// The attrs half of [`write_node`], shared by both content shapes so a blob's
/// attributes and a paragraph's are written the same way.
fn write_attrs(meta: &LoroMap, attrs: &[Attr]) -> Result<()> {
    for attr in attrs {
        let map = meta.ensure_mergeable_map(K_ATTRS)?;
        let record = map.ensure_mergeable_map(&attr.id.to_storage())?;
        record.insert(K_ATTR_ROLE, attr.role.as_str())?;
        if let Some(label) = &attr.label {
            record.insert(K_ATTR_LABEL, label.as_str())?;
        }
        write_attr_value(&record, &attr.value)?;
    }
    Ok(())
}

pub(crate) fn read_node(meta: &LoroMap) -> Result<Node> {
    let id = read_node_id(meta).ok_or(StoreError::Malformed("missing or invalid id"))?;
    let kind = meta
        .get(K_KIND)
        .and_then(as_string)
        .and_then(|s| Kind::parse(&s))
        .ok_or(StoreError::Malformed("missing or unknown kind"))?;

    let node_meta = NodeMeta {
        icon: meta.get(K_ICON).and_then(as_string),
        name: meta.get(K_NAME).and_then(as_string),
        level_name: meta.get(K_LEVEL).and_then(as_string),
        colour: meta.get(K_COLOUR).and_then(as_string),
        done: meta.get(K_DONE).and_then(as_bool).unwrap_or(false),
        align: meta
            .get(K_ALIGN)
            .and_then(as_string)
            .as_deref()
            .and_then(Align::parse),
        heading: meta
            .get(K_HEADING)
            .and_then(as_i64)
            .and_then(Heading::from_level),
        list_style: meta
            .get(K_LIST_STYLE)
            .and_then(as_string)
            .as_deref()
            .and_then(ListStyle::parse),
        image_size: match (
            meta.get(K_IMG_W).and_then(as_i64),
            meta.get(K_IMG_H).and_then(as_i64),
        ) {
            (Some(w), Some(h)) => Some((w.max(0) as u32, h.max(0) as u32)),
            _ => None,
        },
        language: meta.get(K_LANG).and_then(as_string),
        prose_language: meta.get(K_PROSE_LANGUAGE).and_then(as_string),
        // Malformed reads as `None`, never a failed node — the `origin`
        // precedent. On a canvas that shows the object among the arrivals
        // rather than hiding it.
        at: meta
            .get(K_AT)
            .as_ref()
            .and_then(ValueOrContainer::as_value)
            .and_then(at_from_loro),
        extent: meta
            .get(K_EXTENT)
            .as_ref()
            .and_then(ValueOrContainer::as_value)
            .and_then(extent_from_loro),
        table_layout: meta
            .get(K_TABLE_LAYOUT)
            .and_then(as_string)
            .as_deref()
            .and_then(TableLayout::parse)
            .filter(|layout| *layout != TableLayout::Compact),
        trashed_at: meta.get(K_TRASHED_AT).and_then(as_i64),
        // A malformed origin reads back as `None` rather than failing the node:
        // "restore to the top of the library" is a working answer, and refusing
        // to read the node at all would hide it from the trash screen that is
        // the only place it can be recovered from.
        origin: meta
            .get(K_ORIGIN)
            .and_then(as_string)
            .as_deref()
            .and_then(NodeId::parse),
        trashed_by: meta.get(K_TRASHED_BY).and_then(as_string),
        restored_by: meta.get(K_RESTORED_BY).and_then(as_string),
    };

    let content = if let Some(target) = meta.get(K_EMBED).and_then(as_string) {
        let node = NodeId::parse(&target).ok_or(StoreError::Malformed("bad embed target"))?;
        Content::Embed {
            target: crate::ids::Ref::new(node),
        }
    } else if let Some(hash) = meta.get(K_BLOB).and_then(as_string) {
        Content::Blob {
            hash: BlobHash(hash),
            mime: meta.get(K_MIME).and_then(as_string).unwrap_or_default(),
            width: meta.get(K_WIDTH).and_then(as_i64).map(|v| v as u32),
            height: meta.get(K_HEIGHT).and_then(as_i64).map(|v| v as u32),
            attrs: read_attrs(meta),
        }
    } else {
        let text = match meta.get(K_TEXT) {
            Some(ValueOrContainer::Container(c)) => {
                c.into_text().map(|t| t.to_string()).unwrap_or_default()
            }
            _ => String::new(),
        };
        Content::Text {
            text,
            attrs: read_attrs(meta),
        }
    };

    Ok(Node {
        id,
        kind,
        meta: node_meta,
        content,
    })
}

/// Drop a zone left by an earlier value. Guarded — deleting an absent key is
/// still an op, and therefore a phantom `Ctrl+Z`.
fn clear_zone(record: &LoroMap) -> Result<()> {
    if record.get(K_ATTR_TZ).is_some() {
        record.delete(K_ATTR_TZ)?;
    }
    Ok(())
}

fn write_attr_value(record: &LoroMap, value: &AttrValue) -> Result<()> {
    record.insert(K_ATTR_TYPE, value.key())?;
    match value {
        // The three temporal types, each under its own token
        // (`crate::time::TemporalKind`). Only the zoned one writes a zone, and
        // the other two **clear** a stale one — rewriting a due from a timed
        // deadline to a calendar day must not leave a zone lying behind it for
        // a reader to misread. Guarded, like `write_or_clear`: only if it is
        // there, or the delete is a phantom undo step.
        AttrValue::Date(d) => {
            record.insert(K_ATTR_VALUE, d.to_storage())?;
            clear_zone(record)?;
        }
        AttrValue::FloatingDateTime(f) => {
            record.insert(K_ATTR_VALUE, f.to_storage())?;
            clear_zone(record)?;
        }
        AttrValue::ZonedDateTime(z) => {
            record.insert(K_ATTR_VALUE, z.instant_ms())?;
            record.insert(K_ATTR_TZ, z.zone().as_str())?;
        }
        AttrValue::Number(n) => record.insert(K_ATTR_VALUE, *n)?,
        AttrValue::Bool(b) => record.insert(K_ATTR_VALUE, *b)?,
        AttrValue::Url(u) => record.insert(K_ATTR_VALUE, u.as_str())?,
        AttrValue::Text(t) => record.insert(K_ATTR_VALUE, t.as_str())?,
        AttrValue::Ref(r) => {
            record.insert(K_ATTR_VALUE, r.node.to_storage())?;
            // `home` is a cache: it makes the common lookup one hop, and
            // resolution falls back to the locations index when it is stale
            // because the target was promoted to another document.
            match r.home {
                Some(DocRef::Profile(p)) => {
                    record.insert(K_ATTR_HOME, format!("profile:{}", p.to_storage()))?
                }
                Some(DocRef::Workspace(w)) => {
                    record.insert(K_ATTR_HOME, format!("workspace:{}", w.to_storage()))?
                }
                Some(DocRef::Document(d)) => {
                    record.insert(K_ATTR_HOME, format!("document:{}", d.to_storage()))?
                }
                Some(DocRef::DocumentAt {
                    profile,
                    workspace,
                    document,
                }) => record.insert(
                    K_ATTR_HOME,
                    format!(
                        "document-at:{}/{}/{}",
                        profile.to_storage(),
                        workspace.to_storage(),
                        document.to_storage()
                    ),
                )?,
                None => {}
            }
        }
        AttrValue::Document(reference) => {
            // `value` remains a UUID-shaped node target for older readers;
            // `home=document:` lets a new reader recover the typed document
            // identity, and `target` exists only for a genuine deep link.
            record.insert(K_ATTR_VALUE, reference.document.to_storage())?;
            record.insert(
                K_ATTR_HOME,
                format!("document:{}", reference.document.to_storage()),
            )?;
            match reference.node {
                Some(node) => record.insert(K_ATTR_TARGET, node.to_storage())?,
                None if record.get(K_ATTR_TARGET).is_some() => record.delete(K_ATTR_TARGET)?,
                None => {}
            }
        }
    }
    Ok(())
}

fn read_attrs(meta: &LoroMap) -> Vec<Attr> {
    let Some(ValueOrContainer::Container(c)) = meta.get(K_ATTRS) else {
        return Vec::new();
    };
    let Ok(attrs) = c.into_map() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    let keys: Vec<String> = attrs.keys().map(|k| k.to_string()).collect();
    for key in keys {
        let Some(id) = AttrId::parse(&key) else {
            continue;
        };
        let Some(ValueOrContainer::Container(rc)) = attrs.get(&key) else {
            continue;
        };
        let Ok(record) = rc.into_map() else { continue };
        let Some(role) = record.get(K_ATTR_ROLE).and_then(as_string) else {
            continue;
        };
        let Some(value) = read_attr_value(&record, &role) else {
            continue;
        };
        let label = record.get(K_ATTR_LABEL).and_then(as_string);
        out.push(Attr {
            id,
            role,
            value,
            label,
        });
    }
    // Map iteration order is not defined; sort so reads are reproducible.
    out.sort_by(|a, b| a.role.cmp(&b.role).then(a.id.cmp(&b.id)));
    out
}

/// One attribute value off its record, or `None` to skip the attribute.
///
/// **An unknown type token reads as absent, never as an error** — that is the
/// whole compatibility story for new value types: an older build walks past an
/// attribute it cannot type rather than refusing the node, and re-exporting
/// preserves the record it did not understand because Loro keeps what was not
/// touched. Pinned by `an_unknown_attr_type_token_reads_as_absent`.
fn read_attr_value(record: &LoroMap, role: &str) -> Option<AttrValue> {
    let token = record.get(K_ATTR_TYPE).and_then(as_string)?;
    // The typed scalars first: their value is not a string, so the shared
    // string read below would drop them.
    match token.as_str() {
        // A zoned value **must** carry a valid zone. A record missing one, or
        // naming an offset, is not read as "an instant with no zone" — that
        // shape does not exist here — it reads as absent, the same as any
        // token this build cannot type. The retired `datetime` token, which
        // did mean instant-plus-optional-zone, falls through to the same
        // absence.
        t if t == crate::time::TemporalKind::Zoned.token() => {
            let instant_ms = record.get(K_ATTR_VALUE).and_then(as_i64)?;
            let zone = record
                .get(K_ATTR_TZ)
                .and_then(as_string)
                .and_then(|z| crate::time::TimeZoneName::parse(&z))?;
            return Some(AttrValue::ZonedDateTime(crate::time::ZonedDateTime::new(
                instant_ms, zone,
            )));
        }
        "number" => {
            return Some(AttrValue::Number(
                record.get(K_ATTR_VALUE).and_then(as_f64)?,
            ));
        }
        "bool" => {
            return Some(AttrValue::Bool(record.get(K_ATTR_VALUE).and_then(as_bool)?));
        }
        _ => {}
    }
    let raw = record.get(K_ATTR_VALUE).and_then(as_string)?;
    Some(match token.as_str() {
        // `date` is unchanged from before the three-type split, which is what
        // keeps every document already written byte-identical.
        t if t == crate::time::TemporalKind::Date.token() => {
            AttrValue::Date(crate::time::DateOnly::parse(&raw)?)
        }
        t if t == crate::time::TemporalKind::Floating.token() => {
            AttrValue::FloatingDateTime(crate::time::FloatingDateTime::parse(&raw)?)
        }
        "url" => AttrValue::Url(raw),
        "text" => AttrValue::Text(raw),
        "ref" => {
            let node = NodeId::parse(&raw)?;
            let home = record.get(K_ATTR_HOME).and_then(as_string).and_then(|h| {
                let (kind, id) = h.split_once(':')?;
                match kind {
                    "profile" => ProfileId::parse(id).map(DocRef::Profile),
                    "workspace" => WorkspaceId::parse(id).map(DocRef::Workspace),
                    "document" => DocumentId::parse(id).map(DocRef::Document),
                    "document-at" => {
                        let mut ids = id.split('/');
                        let profile = ProfileId::parse(ids.next()?)?;
                        let workspace = WorkspaceId::parse(ids.next()?)?;
                        let document = DocumentId::parse(ids.next()?)?;
                        ids.next().is_none().then_some(DocRef::DocumentAt {
                            profile,
                            workspace,
                            document,
                        })
                    }
                    // An unknown prefix drops the *cache*, not the
                    // reference: `home` is a hint, so an older build reading
                    // a newer home still resolves through the index rather
                    // than reporting a dangling pill.
                    _ => None,
                }
            });
            // **Which roles mean "a document"** — `document`, and the
            // delegated run's `koru.run` (`attributes::DOCUMENT_REFERENCE_ROLES`).
            // The record is byte-identical either way; this decides only what
            // the reader hands back, which is why widening the set is a code
            // change and not a schema change. A build that knows only
            // `document` reads a run pill as a `Ref` homed at the run,
            // preserves it and re-exports it untouched.
            if crate::attributes::is_document_reference_role(role) {
                match home {
                    Some(DocRef::Document(document)) => {
                        let target = record
                            .get(K_ATTR_TARGET)
                            .and_then(as_string)
                            .as_deref()
                            .and_then(NodeId::parse)
                            .or_else(|| (node.as_uuid() != document.as_uuid()).then_some(node));
                        AttrValue::Document(crate::ids::DocumentReference {
                            document,
                            node: target,
                        })
                    }
                    Some(DocRef::DocumentAt { document, .. }) => {
                        let target = record
                            .get(K_ATTR_TARGET)
                            .and_then(as_string)
                            .as_deref()
                            .and_then(NodeId::parse)
                            .or_else(|| (node.as_uuid() != document.as_uuid()).then_some(node));
                        AttrValue::Document(crate::ids::DocumentReference {
                            document,
                            node: target,
                        })
                    }
                    _ => AttrValue::Ref(Ref { node, home }),
                }
            } else {
                AttrValue::Ref(Ref { node, home })
            }
        }
        _ => return None,
    })
}

/// Write a string key, or remove it — **guarded**, so clearing a key that is
/// already absent writes no op.
///
/// Deleting a key that is not there is still an op and therefore still an undo
/// step, which is how a restore ends up needing two Ctrl+Z. Same reasoning as
/// `set_heading`'s guard, hoisted here because the trash clears four keys.
fn write_or_clear(meta: &LoroMap, key: &str, value: Option<String>) -> Result<()> {
    match value {
        Some(v) => meta.insert(key, v.as_str())?,
        None if meta.get(key).is_some() => meta.delete(key)?,
        None => {}
    }
    Ok(())
}

/// Whether a node's meta says it is a trash root, without reading the node.
///
/// One key, the way `kind_of` does it — this is asked of every child of the
/// library root on every `children` call, which the explore column makes once
/// per structural change.
fn is_trash(meta: &LoroMap) -> bool {
    meta.get(K_KIND).and_then(as_string).as_deref() == Some(Kind::Trash.as_str())
}

pub(crate) fn read_node_id(meta: &LoroMap) -> Option<NodeId> {
    NodeId::parse(&as_string(meta.get(K_ID)?)?)
}

fn read_document_id(meta: &LoroMap) -> Option<DocumentId> {
    DocumentId::parse(&as_string(meta.get(K_DOC_ID)?)?)
}

/// Give a document node an id of its own, if it is a document and has none.
///
/// **Idempotent on purpose.** It runs on every insert and every `set_kind`, and
/// a document that already has an id must keep it — minting a second one would
/// silently rehome every reference pointing at the first.
fn mint_document_id(meta: &LoroMap, kind: Kind) -> Result<()> {
    if kind != Kind::Document || read_document_id(meta).is_some() {
        return Ok(());
    }
    meta.insert(K_DOC_ID, DocumentId::new().to_storage())?;
    Ok(())
}

pub(crate) fn as_string(v: ValueOrContainer) -> Option<String> {
    match v.as_value()? {
        LoroValue::String(s) => Some(s.to_string()),
        _ => None,
    }
}

pub(crate) fn as_bool(v: ValueOrContainer) -> Option<bool> {
    match v.as_value()? {
        LoroValue::Bool(b) => Some(*b),
        _ => None,
    }
}

pub(crate) fn as_i64(v: ValueOrContainer) -> Option<i64> {
    match v.as_value()? {
        LoroValue::I64(i) => Some(*i),
        _ => None,
    }
}

pub(crate) fn as_f64(v: ValueOrContainer) -> Option<f64> {
    match v.as_value()? {
        LoroValue::Double(d) => Some(*d),
        // A whole number may have been written by a tool as an integer; a
        // number attribute still reads it.
        LoroValue::I64(i) => Some(*i as f64),
        _ => None,
    }
}

// ------------------------------------------------------------ canvas values
//
// The first structured values in node meta. Each is **one whole map value**,
// written and replaced atomically, never a container and never split across
// keys: a map *value* is last-writer-wins as a whole, which is what makes two
// concurrent moves resolve to one position.

/// The stored form of a canvas position, clamped to ±[`CANVAS_LIMIT`].
pub(crate) fn at_to_loro(at: CanvasPoint) -> LoroValue {
    let at = at.clamped();
    LoroValue::from(HashMap::from([
        (F_X.to_string(), LoroValue::I64(at.x.into())),
        (F_Y.to_string(), LoroValue::I64(at.y.into())),
    ]))
}

/// The stored form of a canvas extent. `h` is written only when there is one,
/// so a card's extent is `{w}` and nothing reads a height that was never set.
pub(crate) fn extent_to_loro(extent: CanvasExtent) -> LoroValue {
    let extent = extent.clamped();
    let mut map = HashMap::from([(F_W.to_string(), LoroValue::I64(extent.w.into()))]);
    if let Some(h) = extent.h {
        map.insert(F_H.to_string(), LoroValue::I64(h.into()));
    }
    LoroValue::from(map)
}

/// A stored canvas position, **leniently**: `x` and `y` must both be whole
/// numbers inside ±[`CANVAS_LIMIT`]; anything else — another type, a missing
/// field, a float, a value outside `i32` or outside the limit — is `None`, and
/// fields this build does not know are ignored. Never an error: a position
/// nobody can read makes an arrival, not a node that fails to load.
pub(crate) fn at_from_loro(value: &LoroValue) -> Option<CanvasPoint> {
    let LoroValue::Map(map) = value else {
        return None;
    };
    Some(CanvasPoint {
        x: canvas_field(map, F_X, -CANVAS_LIMIT)?,
        y: canvas_field(map, F_Y, -CANVAS_LIMIT)?,
    })
}

/// A stored canvas extent, leniently: `w` must be a whole number in
/// `1..=`[`CANVAS_LIMIT`]. `h` is optional, and a malformed `h` reads as no
/// height rather than as no extent — a derived height is always a workable
/// answer, where losing the width would lose what a person set.
pub(crate) fn extent_from_loro(value: &LoroValue) -> Option<CanvasExtent> {
    let LoroValue::Map(map) = value else {
        return None;
    };
    Some(CanvasExtent {
        w: canvas_field(map, F_W, 1)?,
        h: canvas_field(map, F_H, 1),
    })
}

/// One whole-pixel field of a canvas value: an `I64` that fits `i32` and lies
/// in `min..=CANVAS_LIMIT`. Decoded with `i32::try_from`, never a cast, so an
/// out-of-range value is malformed rather than silently wrapped.
fn canvas_field(map: &loro::LoroMapValue, key: &str, min: i32) -> Option<i32> {
    match map.get(key)? {
        LoroValue::I64(v) => i32::try_from(*v)
            .ok()
            .filter(|v| (min..=CANVAS_LIMIT).contains(v)),
        _ => None,
    }
}

#[cfg(test)]
mod attr_storage_tests {
    use super::*;
    use crate::node::AttrValue;

    fn one_node() -> (WorkspaceDoc, NodeId) {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "attrs").unwrap();
        let node = ws
            .insert(Parent::Root, Node::task("Reward flow").unwrap())
            .unwrap();
        (ws, node)
    }

    fn zoned(instant_ms: i64, zone: &str) -> AttrValue {
        AttrValue::ZonedDateTime(crate::time::ZonedDateTime::new(
            instant_ms,
            crate::time::TimeZoneName::parse(zone).unwrap(),
        ))
    }

    fn floating(text: &str) -> AttrValue {
        AttrValue::FloatingDateTime(crate::time::FloatingDateTime::parse(text).unwrap())
    }

    /// All three temporal types survive storage as themselves — and each under
    /// its own token, so nothing downstream has to guess which of them a
    /// `datetime` was.
    #[test]
    fn the_three_temporal_types_round_trip_separately() {
        let (mut ws, node) = one_node();
        let effort = Attr::new("effort", AttrValue::Number(2.5));
        let flag = Attr::new("flag", AttrValue::Bool(true));
        let day = Attr::new(
            "start",
            AttrValue::Date(crate::time::DateOnly::parse("2026-08-30").unwrap()),
        );
        let timed = Attr::new("due", zoned(1_790_000_000_000, "America/New_York"));
        let wall = Attr::new("plan", floating("2026-08-30T09:00:00"));
        for attr in [&effort, &flag, &day, &timed, &wall] {
            ws.set_attr(node, attr).unwrap();
        }

        // Through a snapshot, so what is proved is storage, not memory.
        let back = WorkspaceDoc::from_snapshot(&ws.export_snapshot().unwrap()).unwrap();
        let read = back.read(node).unwrap();
        for attr in [&effort, &flag, &day, &timed, &wall] {
            let found = read.content.attrs().iter().find(|a| a.id == attr.id);
            assert_eq!(found, Some(attr), "{} survived storage", attr.role);
        }
        assert_eq!(day.value.key(), "date");
        assert_eq!(wall.value.key(), "datetime_floating");
        assert_eq!(timed.value.key(), "datetime_zoned");
    }

    /// Rewriting a zoned deadline as a floating or date-only value clears the
    /// zone that was there — a stale zone left beside a zone-free value is
    /// exactly the ambiguity the three types exist to remove.
    #[test]
    fn rewriting_away_from_a_zoned_value_clears_the_stale_zone() {
        let (mut ws, node) = one_node();
        let mut attr = Attr::new("due", zoned(1000, "Asia/Tokyo"));
        ws.set_attr(node, &attr).unwrap();
        attr.value = floating("2026-08-30T09:00:00");
        ws.set_attr(node, &attr).unwrap();
        let read = ws.read(node).unwrap();
        assert_eq!(
            read.content.attrs().iter().find(|a| a.id == attr.id),
            Some(&attr),
            "the Tokyo zone is gone with the instant it belonged to"
        );

        attr.value = AttrValue::Date(crate::time::DateOnly::parse("2026-09-01").unwrap());
        ws.set_attr(node, &attr).unwrap();
        assert_eq!(
            ws.read(node)
                .unwrap()
                .content
                .attrs()
                .iter()
                .find(|a| a.id == attr.id),
            Some(&attr)
        );
    }

    /// A zoned record whose zone did not survive is **absent**, never an
    /// instant standing on its own: "an instant with no zone" is the shape the
    /// three types abolish, so it must not be reachable by editing storage.
    #[test]
    fn a_zoned_record_without_a_valid_zone_reads_as_absent() {
        for zone in [None, Some("+02:00")] {
            let (mut ws, node) = one_node();
            let attr = Attr::new("due", zoned(1000, "Asia/Tokyo"));
            ws.set_attr(node, &attr).unwrap();
            let record = attr_record(&ws, node, &attr);
            match zone {
                None => record.delete(K_ATTR_TZ).unwrap(),
                Some(bad) => record.insert(K_ATTR_TZ, bad).unwrap(),
            }
            ws.doc.commit();
            assert!(
                ws.read(node).unwrap().content.attrs().is_empty(),
                "a zoned record with {zone:?} for a zone is skipped, not half-read"
            );
        }
    }

    /// The R0 token `datetime` meant *instant plus optional zone* — the
    /// ambiguous shape. It is retired rather than migrated (R0 never shipped),
    /// and this build must not resurrect it by reading one.
    #[test]
    fn the_retired_datetime_token_reads_as_absent() {
        let (mut ws, node) = one_node();
        let attr = Attr::new("due", zoned(1000, "Asia/Tokyo"));
        ws.set_attr(node, &attr).unwrap();
        attr_record(&ws, node, &attr)
            .insert(K_ATTR_TYPE, "datetime")
            .unwrap();
        ws.doc.commit();
        assert!(ws.read(node).unwrap().content.attrs().is_empty());
    }

    /// The stored record behind one attribute, for tests that corrupt it.
    fn attr_record(ws: &WorkspaceDoc, node: NodeId, attr: &Attr) -> LoroMap {
        let tid = ws.tree_id(node).unwrap();
        let meta = ws.nodes().get_meta(tid).unwrap();
        let Some(ValueOrContainer::Container(c)) = meta.get(K_ATTRS) else {
            panic!("attrs map exists");
        };
        let Some(ValueOrContainer::Container(rc)) =
            c.into_map().unwrap().get(&attr.id.to_storage())
        else {
            panic!("record exists");
        };
        rc.into_map().unwrap()
    }

    /// The compatibility story for every new value type: an older build reads
    /// a token it does not know as *absent* — the attribute is skipped, the
    /// node reads fine, and nothing errors. A build that added `duration`
    /// tomorrow costs this build nothing today.
    #[test]
    fn an_unknown_attr_type_token_reads_as_absent() {
        let (mut ws, node) = one_node();
        let known = Attr::new("due", AttrValue::Text("soon".into()));
        ws.set_attr(node, &known).unwrap();
        let stranger = Attr::new("effort", AttrValue::Text("placeholder".into()));
        ws.set_attr(node, &stranger).unwrap();

        // Overwrite the second record's type token with one from the future.
        let tid = ws.tree_id(node).unwrap();
        let meta = ws.nodes().get_meta(tid).unwrap();
        let Some(ValueOrContainer::Container(c)) = meta.get(K_ATTRS) else {
            panic!("attrs map exists");
        };
        let attrs = c.into_map().unwrap();
        let Some(ValueOrContainer::Container(rc)) = attrs.get(&stranger.id.to_storage()) else {
            panic!("record exists");
        };
        rc.into_map()
            .unwrap()
            .insert(K_ATTR_TYPE, "duration")
            .unwrap();
        ws.doc.commit();

        let read = ws.read(node).unwrap();
        let roles: Vec<&str> = read
            .content
            .attrs()
            .iter()
            .map(|a| a.role.as_str())
            .collect();
        assert_eq!(roles, vec!["due"], "the stranger is skipped, not an error");
    }

    /// A document written before the new value types existed reads back
    /// byte-identical: opening and reading performs no writes.
    #[test]
    fn an_old_document_reads_back_byte_identical() {
        let (mut ws, node) = one_node();
        ws.set_attr(
            node,
            &Attr::new(
                "due",
                AttrValue::Date(crate::time::DateOnly::parse("2026-08-30").unwrap()),
            ),
        )
        .unwrap();
        ws.set_attr(
            node,
            &Attr::new("note", AttrValue::Text("old shape".into())),
        )
        .unwrap();
        let before = ws.export_snapshot().unwrap();

        let reopened = WorkspaceDoc::from_snapshot(&before).unwrap();
        // Read everything the new code paths read.
        let read = reopened.read(node).unwrap();
        assert_eq!(read.content.attrs().len(), 2);
        assert_eq!(reopened.export_snapshot().unwrap(), before);
    }
}

#[cfg(test)]
mod schema_marker_tests {
    use super::*;

    #[test]
    fn introducing_a_quote_raises_an_older_workspaces_schema_marker() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "old").unwrap();
        ws.doc.get_map(C_META).insert(K_SCHEMA, 1).unwrap();
        ws.doc.commit();

        let paragraph = ws
            .insert(Parent::Root, Node::paragraph("quoted").unwrap())
            .unwrap();
        assert_eq!(ws.schema_version(), Some(1), "ordinary prose is still v1");

        ws.set_kind(paragraph, Kind::Quote).unwrap();
        assert_eq!(ws.schema_version(), Some(3));

        ws.doc.get_map(C_META).insert(K_SCHEMA, 1).unwrap();
        ws.doc.commit();
        ws.insert(Parent::Root, Node::quote("pasted").unwrap())
            .unwrap();
        assert_eq!(ws.schema_version(), Some(3));
    }

    #[test]
    fn legacy_outline_and_library_become_one_import_tree() {
        let old = LoroDoc::new();
        configure_styles(&old);
        let workspace = WorkspaceId::new();
        old.get_map(C_META)
            .insert(K_ID, workspace.to_storage())
            .unwrap();
        old.get_map(C_META).insert(K_NAME, "Old").unwrap();

        let outline = old.get_tree(C_OUTLINE);
        let task_id = outline.create(TreeParentId::Root).unwrap();
        let task = Node::task("From outline").unwrap();
        write_node(&outline.get_meta(task_id).unwrap(), &task).unwrap();

        let library = old.get_tree(C_LIBRARY);
        let page_id = library.create(TreeParentId::Root).unwrap();
        let page = Node::document("Page").unwrap();
        let page_node = page.id;
        write_node(&library.get_meta(page_id).unwrap(), &page).unwrap();
        let paragraph_id = library.create(TreeParentId::Node(page_id)).unwrap();
        let paragraph = Node::paragraph("bold body").unwrap();
        let paragraph_node = paragraph.id;
        let paragraph_meta = library.get_meta(paragraph_id).unwrap();
        write_node(&paragraph_meta, &paragraph).unwrap();
        paragraph_meta
            .ensure_mergeable_text(K_TEXT)
            .unwrap()
            .mark_utf8(0..4, Mark::Bold.key(), true)
            .unwrap();

        let trash_id = library.create(TreeParentId::Root).unwrap();
        write_node(
            &library.get_meta(trash_id).unwrap(),
            &Node::trash().unwrap(),
        )
        .unwrap();
        let deleted_id = library.create(TreeParentId::Node(trash_id)).unwrap();
        let mut deleted = Node::paragraph("Deleted note").unwrap();
        deleted.meta.trashed_at = Some(1234);
        let deleted_node = deleted.id;
        write_node(&library.get_meta(deleted_id).unwrap(), &deleted).unwrap();
        old.commit();

        let legacy = WorkspaceDoc::adopt(old).unwrap();
        assert!(
            legacy.explore().unwrap().is_empty(),
            "the current tree is absent"
        );
        let normalized = legacy.normalized_for_legacy_import().unwrap();

        assert_eq!(normalized.id(), Some(workspace));
        assert_eq!(normalized.explore().unwrap().len(), 1);
        assert_eq!(
            normalized.document_id(page_node),
            DocumentId::from_uuid(page_node.as_uuid()),
            "a pre-document-id page gets the stable legacy fallback"
        );
        assert_eq!(
            normalized.read(task.id).unwrap().content.as_text(),
            Some("From outline")
        );
        assert!(
            normalized
                .runs(paragraph_node)
                .unwrap()
                .iter()
                .any(|run| run.bold && run.text == "bold")
        );
        assert!(normalized.is_trashed(deleted_node));
        assert_eq!(
            normalized.read(deleted_node).unwrap().meta.trashed_at,
            Some(1234)
        );
    }

    #[test]
    fn restore_is_forward_moving_and_converges_on_another_peer() {
        let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "History").unwrap();
        let line = ws
            .insert(Parent::Root, Node::paragraph("before").unwrap())
            .unwrap();
        let target = ws.export_snapshot().unwrap();
        ws.set_text(line, "after").unwrap();
        let later = ws.export_snapshot().unwrap();
        let mut peer = WorkspaceDoc::from_snapshot(&later).unwrap();

        let update = ws.restore_snapshot_forward(&target).unwrap();
        peer.import(&update).unwrap();

        assert_eq!(ws.read(line).unwrap().content.as_text(), Some("before"));
        assert_eq!(peer.read(line).unwrap().content.as_text(), Some("before"));
        assert_eq!(ws.version(), peer.version());
    }

    #[test]
    fn restore_can_recover_a_frontier_missing_from_a_shallow_replica() {
        let mut original = WorkspaceDoc::create(WorkspaceId::new(), "History").unwrap();
        let line = original
            .insert(Parent::Root, Node::paragraph("old").unwrap())
            .unwrap();
        let target = original.export_snapshot().unwrap();
        original.set_text(line, "new").unwrap();
        // Loading a snapshot is the compact/shallow case: it has state but not
        // necessarily every reversible change retained by the original.
        let mut shallow =
            WorkspaceDoc::from_snapshot(&original.export_snapshot().unwrap()).unwrap();
        shallow.restore_snapshot_forward(&target).unwrap();
        assert_eq!(shallow.read(line).unwrap().content.as_text(), Some("old"));
    }
}
