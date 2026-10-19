// SPDX-License-Identifier: MPL-2.0

//! The Koru domain model.
//!
//! Everything here is headless: no parley, no vello, no windowing. It is the
//! contract that views, tools and (later) third parties build against, and it
//! is where the invariants live.
//!
//! See `docs/architecture/DATAMODEL.md` for the model and the reasoning behind it.

pub mod attributes;
pub mod canvas;
pub mod change;
pub mod explore;
pub mod ids;
pub mod markdown;
pub mod node;
pub mod profile;
pub mod query;
pub mod schema;
pub mod system;
pub mod table;
pub mod tags;
pub mod tasks;
pub mod text;
pub mod time;
pub mod timing;

pub use attributes::{
    AccentColor, AttributeDefinition, AttributeScope, Cardinality, Lifecycle, Priority, TaskReason,
    ValueType,
};
pub use ids::{
    AttrId, DocRef, DocumentId, DocumentReference, EntityRef, NodeId, ProfileId, Ref, TagId,
    WorkspaceId,
};
pub use node::{
    Align, Attr, AttrValue, BlobHash, CANVAS_LIMIT, CanvasExtent, CanvasPoint, Content, Heading,
    Kind, ListStyle, Node, NodeMeta, Shape, ShapeError,
};
pub use profile::{PROFILE_SCHEMA_VERSION, ProfileDoc, ProfileSummary, WorkspaceEntry};
pub use query::{DateRange, ResolvedRef, TaskQuery, TaskSummary};
pub use schema::{Parent, StableTextPosition, StableTextRange, StoreError, WorkspaceDoc};
pub use system::{SystemDoc, TagEntry};
pub use table::TableLayout;
pub use tags::{TagName, TagPathError};
pub use text::{ANCHOR, Mark, MarkSet, Script, TextRun, TextStyle, Utf8Pos};
pub use time::{
    DateOnly, FloatingDateTime, Temporal, TemporalKind, TimeZoneName, ZoneRules, ZonedDateTime,
};
