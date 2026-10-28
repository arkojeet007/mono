// SPDX-License-Identifier: MPL-2.0

//! Koru's MCP surface.
//!
//! The handler depends on [`Backend`], not local discovery. The desktop
//! adapter below supplies one local principal; a future Koru Cloud adapter can
//! supply tenant, authenticated principal and scopes per request without
//! changing a tool or exposing a cloud listener from this crate.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use rmcp::{
    ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        Annotations, CallToolResult, ErrorData, ListResourcesResult, ReadResourceRequestParams,
        ReadResourceResponse, ReadResourceResult, Resource, ResourceContents, Role,
        ServerCapabilities, ServerInfo,
    },
    schemars, tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use koru_api_client::Client;
use koru_core::{
    Attr, AttrValue, AttributeDefinition, Content, DocRef, DocumentId as CoreDocumentId,
    DocumentReference, Kind, Node, NodeId, Parent, ProfileId as CoreProfileId, Ref, SystemDoc,
    TagId, WorkspaceDoc, WorkspaceId as CoreWorkspaceId,
    time::{DateOnly, FloatingDateTime, TimeZoneName, ZonedDateTime},
};
use koru_store_core::ids::{
    DocumentId as StoreDocumentId, HistoryEntryId, WorkspaceId as StoreWorkspaceId,
};
use koru_store_core::model::{Blob, DocumentEntry, Scope, Workspace};
use koru_store_wire::attributes::DueFilter;
use koru_store_wire::search::{
    AttrFilter, AttrOp, LifecycleFilter, SearchQuery, SearchRequest, SearchSort,
};
use koru_store_wire::tasks::{
    LifecycleFilter as TaskLifecycleFilter, MAX_LIMIT as TASK_MAX_LIMIT, TaskQuery,
    TaskQueryResponse, TaskSort,
};
use koru_store_wire::{CaptureHistory, HistoryEntry, HistoryPage, HistoryQuery, Status};
use koru_workspace::store::{StoreSession, Synced};

mod chat_tools;
pub use chat_tools::CHAT_TOOL_NAMES;
use chat_tools::{ListChatsToolRequest, ReadChatToolRequest, SearchChatsToolRequest};

mod published_schema;
pub mod tasks;
mod tool_pages;

pub const MODEL_RESOURCE_URI: &str = "koru://docs/model";
/// Where a release installs the model contract, under its data directory.
/// `koru_update::RELEASE_DOCUMENTS` names the same path; it is append-only,
/// so this never moves.
const MODEL_DOCUMENT_PATH: &str = "docs/mcp/README.md";
/// Where the contract's source lives in the repository, relative to the
/// workspace root: beside this crate, so the tree builds and tests without
/// the `docs/` checkout. The release packager copies it to
/// [`MODEL_DOCUMENT_PATH`].
const MODEL_DOCUMENT_SOURCE: &str = "crates/koru-mcp-server/MCP.md";
const MODEL_DOCUMENT_MIME: &str = "text/markdown";

/// Locate the public MCP contract without making it part of the executable.
///
/// A release carries `docs/mcp/README.md` beside the executables. During development,
/// walking up from `target/{profile}` reaches the repository's source,
/// `crates/koru-mcp-server/MCP.md`, so editing the guide takes effect on the
/// next resource read without rebuilding.
fn model_document_path() -> Result<PathBuf, ToolError> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        // The first candidate is the released layout: docs/ beside the exes
        // on Windows, and the bundle's Resources/ on macOS, where codesign's
        // seal keeps data out of Contents/MacOS.
        candidates.push(parent.join(MODEL_DOCUMENT_PATH));
        if let Some(contents) = parent.parent() {
            candidates.push(contents.join("Resources").join(MODEL_DOCUMENT_PATH));
        }
        candidates.extend(
            parent
                .ancestors()
                .take(6)
                .filter(|root| root.join("crates/koru-mcp-server/Cargo.toml").is_file())
                .map(|root| root.join(MODEL_DOCUMENT_SOURCE)),
        );
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.extend(
            cwd.ancestors()
                .take(6)
                .filter(|root| root.join("crates/koru-mcp-server/Cargo.toml").is_file())
                .map(|root| root.join(MODEL_DOCUMENT_SOURCE)),
        );
    }
    candidates.dedup();
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| ToolError {
            code: "documentation_missing".into(),
            message: format!(
                "{MODEL_DOCUMENT_PATH} is missing; reinstall Koru or rebuild the release package"
            ),
            details: Some(json!({ "uri": MODEL_RESOURCE_URI })),
        })
}

fn read_model_document() -> Result<String, ToolError> {
    let path = model_document_path()?;
    std::fs::read_to_string(&path).map_err(|error| ToolError {
        code: "documentation_failed".into(),
        message: format!("could not read {}: {error}", path.display()),
        details: Some(json!({ "uri": MODEL_RESOURCE_URI })),
    })
}

/// Fail a host startup early when a release omitted its public MCP contract.
pub fn verify_model_documentation() -> Result<(), ToolError> {
    read_model_document().map(|_| ())
}

fn model_resource() -> Resource {
    Resource::new(MODEL_RESOURCE_URI, "koru-model")
        .with_title("Koru document model and editing contract")
        .with_description(
            "Read before the first mutation. Defines Koru's typed tree, identities, ordering, inline objects, preconditions, history, and cross-document semantics.",
        )
        .with_mime_type(MODEL_DOCUMENT_MIME)
        .with_annotations(
            Annotations::default()
                .with_audience(vec![Role::Assistant])
                .with_priority(1.0),
        )
}

#[derive(Clone, Debug)]
pub struct RequestContext {
    pub tenant: String,
    pub principal: String,
    pub scopes: Vec<String>,
    pub workspace_access: WorkspaceAccess,
    pub chat_tools: ChatToolAccess,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ChatToolAccess {
    Denied,
    Bridge,
}

/// Which workspaces one tool caller may observe or change.
///
/// `All` is reserved for the existing trusted local MCP bridge. Product
/// callers such as Koru AI use `Selected`, where an empty set deliberately
/// means no access. Keeping that distinction explicit prevents an empty AI
/// grant from inheriting Store search's ordinary "all workspaces" meaning.
#[derive(Clone, Debug)]
pub enum WorkspaceAccess {
    All,
    Selected(Arc<HashSet<StoreWorkspaceId>>),
}

impl WorkspaceAccess {
    pub fn selected(workspaces: impl IntoIterator<Item = StoreWorkspaceId>) -> Self {
        Self::Selected(Arc::new(workspaces.into_iter().collect()))
    }

    fn allows(&self, workspace: StoreWorkspaceId) -> bool {
        match self {
            Self::All => true,
            Self::Selected(workspaces) => workspaces.contains(&workspace),
        }
    }

    fn selected_ids(&self) -> Option<&HashSet<StoreWorkspaceId>> {
        match self {
            Self::All => None,
            Self::Selected(workspaces) => Some(workspaces),
        }
    }
}

impl RequestContext {
    pub fn local() -> Self {
        Self {
            tenant: "local".into(),
            principal: "desktop-user".into(),
            scopes: vec!["read_documents".into(), "edit_documents".into()],
            workspace_access: WorkspaceAccess::All,
            chat_tools: ChatToolAccess::Bridge,
        }
    }

    /// Build a context whose workspace authority is explicit and deny-by-default.
    pub fn selected(
        tenant: impl Into<String>,
        principal: impl Into<String>,
        scopes: impl IntoIterator<Item = Scope>,
        workspaces: impl IntoIterator<Item = StoreWorkspaceId>,
    ) -> Self {
        Self {
            tenant: tenant.into(),
            principal: principal.into(),
            scopes: scopes
                .into_iter()
                .map(|scope| scope.as_str().to_owned())
                .collect(),
            workspace_access: WorkspaceAccess::selected(workspaces),
            chat_tools: ChatToolAccess::Denied,
        }
    }

    fn has_scope(&self, required: Scope) -> bool {
        self.scopes.iter().any(|scope| scope == required.as_str())
    }
}

/// The cloud seam: handlers carry explicit identity and authorization context,
/// while this release installs only [`LocalBackend`].
pub trait Backend: Send + Sync {
    fn invoke(
        &self,
        context: &RequestContext,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ToolError>;
    fn doctor(&self) -> Result<Value, ToolError>;
}

/// Typed, transport-free Store operations needed by the shared workspace-tool
/// implementation. The desktop MCP bridge uses its existing local client;
/// embedded product hosts implement this port over their bounded Store worker.
pub trait EmbeddedStore: Send + Sync {
    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError>;
    fn documents(&self, workspace: StoreWorkspaceId) -> Result<Vec<DocumentEntry>, ToolError>;
    fn search(
        &self,
        request: SearchRequest,
    ) -> Result<koru_store_wire::search::SearchResponse, ToolError>;
    fn snapshot(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<Vec<u8>, ToolError>;
    fn create_document(
        &self,
        workspace: StoreWorkspaceId,
        title: String,
        kind: String,
    ) -> Result<StoreDocumentId, ToolError>;
    /// The one note a file carries, creating it when it has none.
    ///
    /// Idempotent, like the store operation it ports: the boolean answers
    /// whether this call created the note or the file already had one.
    fn ensure_file_note(
        &self,
        workspace: StoreWorkspaceId,
        file: StoreDocumentId,
    ) -> Result<(StoreDocumentId, bool), ToolError>;
    fn seed_document(
        &self,
        request: EmbeddedSeedDocument,
    ) -> Result<EmbeddedSeededDocument, ToolError>;
    fn persist_update(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        update: Vec<u8>,
    ) -> Result<(), ToolError>;
    fn capture_history(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        request: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError>;
    fn history(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        query: HistoryQuery,
    ) -> Result<HistoryPage, ToolError>;
    fn history_snapshot(
        &self,
        workspace: StoreWorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<Vec<u8>, ToolError>;
    fn blobs(&self, workspace: StoreWorkspaceId) -> Result<Vec<Blob>, ToolError>;
    fn get_blob(
        &self,
        workspace: StoreWorkspaceId,
        hash: koru_store_core::model::BlobHash,
    ) -> Result<Vec<u8>, ToolError>;
    fn status(&self) -> Result<Status, ToolError>;
}

pub struct EmbeddedSeedDocument {
    pub workspace: StoreWorkspaceId,
    pub title: String,
    pub kind: String,
    pub parent: Option<StoreDocumentId>,
    pub index: Option<usize>,
    pub idempotency_key: String,
    pub request_hash: String,
    pub initial_update: Vec<u8>,
}

pub struct EmbeddedSeededDocument {
    pub id: StoreDocumentId,
    pub initial_update: Vec<u8>,
    pub replayed: bool,
}

/// Provider-neutral safety metadata published by the shared workspace catalog.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolSafety {
    pub read_only: bool,
    pub destructive: bool,
    pub idempotent: bool,
    pub open_world: bool,
}

/// One transport-neutral tool contract.
///
/// MCP converts these values to its protocol objects; Koru AI sends the same
/// names and schemas to providers without exposing rmcp or provider SDK types
/// to its core model.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceTool {
    pub name: String,
    pub title: String,
    pub description: String,
    pub input_schema: Value,
    pub output_schema: Value,
    pub safety: ToolSafety,
}

#[derive(Copy, Clone)]
enum WorkspaceTarget {
    None,
    List,
    One,
    /// The tool names `workspace_ids` and the executor intersects that list with
    /// what this principal may reach.
    ///
    /// **The empty answer travels with the target**, as the function that builds
    /// one. When the intersection is empty the executor answers without calling
    /// the backend at all, and that answer has to be *this tool's* shape — an
    /// empty search page is not an empty task page, and a client handed the
    /// wrong envelope reads a missing field rather than an empty list. Carrying
    /// the builder here makes adding a third scoped tool a compile error until
    /// somebody says what its empty page looks like.
    Scoped(fn() -> Value),
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum ToolDomain {
    Workspace,
    Chat,
}

#[derive(Copy, Clone)]
struct ToolPolicy {
    scope: Option<Scope>,
    workspace: WorkspaceTarget,
    domain: ToolDomain,
}

fn tool_policy(name: &str) -> Option<ToolPolicy> {
    let read = Some(Scope::ReadDocuments);
    let edit = Some(Scope::EditDocuments);
    Some(match name {
        "get_documentation" => ToolPolicy {
            scope: None,
            workspace: WorkspaceTarget::None,
            domain: ToolDomain::Workspace,
        },
        "list_chats" | "search_chats" | "read_chat" => ToolPolicy {
            scope: None,
            workspace: WorkspaceTarget::None,
            domain: ToolDomain::Chat,
        },
        "list_workspaces" => ToolPolicy {
            scope: read,
            workspace: WorkspaceTarget::List,
            domain: ToolDomain::Workspace,
        },
        "search_documents" => ToolPolicy {
            scope: read,
            workspace: WorkspaceTarget::Scoped(empty_search_result),
            domain: ToolDomain::Workspace,
        },
        // **`list_tasks` reads under the ordinary document-read boundary.**
        // `Scope::ReadTasks` exists and is deliberately not required here: this
        // release has one trusted user with peered devices, so a second scope
        // in front of a list of that user's own tasks would gate nothing and
        // would have to be granted by every host that already grants reads
        // (roadmap rev6 §1, SPEC §10).
        "list_tasks" => ToolPolicy {
            scope: read,
            workspace: WorkspaceTarget::Scoped(empty_task_page),
            domain: ToolDomain::Workspace,
        },
        "list_documents"
        | "list_files"
        | "search_files"
        | "get_file"
        | "read_file"
        | "read_document"
        | "list_document_versions"
        | "compare_document_versions" => ToolPolicy {
            scope: read,
            workspace: WorkspaceTarget::One,
            domain: ToolDomain::Workspace,
        },
        "create_document"
        | "edit_document"
        | "ensure_file_note"
        | "extract_to_document"
        | "save_document_version"
        | "restore_document_version" => ToolPolicy {
            scope: edit,
            workspace: WorkspaceTarget::One,
            domain: ToolDomain::Workspace,
        },
        _ => return None,
    })
}

/// The shared authorization and execution boundary used by MCP and Koru AI.
#[derive(Clone)]
pub struct WorkspaceToolExecutor {
    backend: Arc<dyn Backend>,
}

impl WorkspaceToolExecutor {
    pub fn new(backend: Arc<dyn Backend>) -> Self {
        Self { backend }
    }

    pub fn invoke(
        &self,
        context: &RequestContext,
        tool: &str,
        mut arguments: Value,
    ) -> Result<Value, ToolError> {
        let policy = tool_policy(tool).ok_or_else(|| ToolError::invalid("unknown tool"))?;
        if policy.domain == ToolDomain::Chat && context.chat_tools == ChatToolAccess::Denied {
            return Err(ToolError::permission_denied(
                "this principal is not allowed to access chat tools",
            )
            .with_details(json!({ "tool": tool })));
        }
        if let Some(required) = policy.scope
            && !context.has_scope(required)
        {
            return Err(ToolError::permission_denied(
                "this principal does not have the required Koru scope",
            )
            .with_details(json!({
                "tool": tool,
                "required_scope": required.as_str(),
            })));
        }

        match policy.workspace {
            WorkspaceTarget::None => {}
            WorkspaceTarget::List => {}
            WorkspaceTarget::One => {
                let raw = arguments
                    .get("workspace_id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::invalid("workspace_id is required"))?;
                let workspace = workspace(raw)?;
                if !context.workspace_access.allows(workspace) {
                    return Err(ToolError::permission_denied(
                        "this chat is not allowed to access that workspace",
                    )
                    .with_details(json!({ "tool": tool, "workspace_id": raw })));
                }
            }
            WorkspaceTarget::Scoped(_) => {
                let requested = arguments
                    .get("workspace_ids")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|value| {
                        value
                            .as_str()
                            .ok_or_else(|| {
                                ToolError::invalid("every workspace_id must be a UUID string")
                            })
                            .and_then(workspace)
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                if let Some(selected) = context.workspace_access.selected_ids() {
                    let authorized = if requested.is_empty() {
                        selected.iter().copied().collect::<Vec<_>>()
                    } else {
                        requested
                            .into_iter()
                            .filter(|workspace| selected.contains(workspace))
                            .collect::<Vec<_>>()
                    };
                    if authorized.is_empty() {
                        let WorkspaceTarget::Scoped(empty) = policy.workspace else {
                            unreachable!("this arm is only reached for a scoped target")
                        };
                        return Ok(empty());
                    }
                    let object = arguments
                        .as_object_mut()
                        .ok_or_else(|| ToolError::invalid("tool arguments must be an object"))?;
                    object.insert(
                        "workspace_ids".into(),
                        json!(
                            authorized
                                .into_iter()
                                .map(|workspace| workspace.to_storage())
                                .collect::<Vec<_>>()
                        ),
                    );
                }
            }
        }

        let result = if tool == "get_documentation" {
            let request: DocumentationRequest = from(arguments)?;
            if request.uri != MODEL_RESOURCE_URI {
                return Err(ToolError {
                    code: "documentation_not_found".into(),
                    message: format!("unknown Koru documentation URI: {}", request.uri),
                    details: Some(json!({ "uri": request.uri })),
                });
            }
            json!({
                "uri": MODEL_RESOURCE_URI,
                "mime_type": MODEL_DOCUMENT_MIME,
                "text": read_model_document()?,
            })
        } else {
            self.backend.invoke(context, tool, arguments)?
        };

        if matches!(policy.workspace, WorkspaceTarget::List) {
            filter_workspaces(result, &context.workspace_access)
        } else {
            Ok(result)
        }
    }

    pub fn doctor(&self) -> Result<Value, ToolError> {
        self.backend.doctor()
    }
}

fn filter_workspaces(mut result: Value, access: &WorkspaceAccess) -> Result<Value, ToolError> {
    let Some(selected) = access.selected_ids() else {
        return Ok(result);
    };
    let workspaces = result
        .get_mut("workspaces")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| ToolError::invalid("list_workspaces returned an invalid result"))?;
    workspaces.retain(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .and_then(StoreWorkspaceId::parse)
            .is_some_and(|workspace| selected.contains(&workspace))
    });
    Ok(result)
}

fn empty_search_result() -> Value {
    json!({
        "hits": [],
        "next_offset": null,
        "updating_workspaces": [],
        "corrected_query": null,
    })
}

/// What `list_tasks` answers when this principal may reach no workspace at all.
///
/// **`restarted` is `false` and not absent.** A caller paging with a cursor
/// branches on it, and a missing boolean is a third state nobody wrote a branch
/// for.
fn empty_task_page() -> Value {
    json!({
        "tasks": [],
        "updating_workspaces": [],
        "next_after": null,
        "restarted": false,
    })
}

#[derive(Debug)]
pub struct ToolError {
    pub code: String,
    pub message: String,
    pub details: Option<Value>,
}

impl ToolError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_request".into(),
            message: message.into(),
            details: None,
        }
    }

    fn permission_denied(message: impl Into<String>) -> Self {
        Self {
            code: "permission_denied".into(),
            message: message.into(),
            details: None,
        }
    }

    fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for ToolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for ToolError {}
impl From<koru_store_core::errors::Error> for ToolError {
    fn from(e: koru_store_core::errors::Error) -> Self {
        Self {
            code: e.code().as_str().into(),
            message: e.message().into(),
            details: None,
        }
    }
}
impl From<koru_core::StoreError> for ToolError {
    fn from(e: koru_core::StoreError) -> Self {
        match e {
            koru_core::StoreError::NoSuchNode(id) => {
                let node_id = id.to_storage();
                Self {
                    code: "stale_node".into(),
                    message: format!(
                        "node {node_id} no longer exists; reread the document and retry with its current node ids"
                    ),
                    details: Some(json!({"node_id": node_id, "retry":"read_document"})),
                }
            }
            // **The model is the validator, and its refusals reach the wire
            // with their own codes.** `koru_core::attributes` already decides
            // whether a value fits a key's definition — the temporal three do
            // not substitute for one another, a select token must be in its
            // options, a personal key never lands on a shared node. Folding
            // those into `document_error` would leave an agent unable to tell
            // "that key does not exist" from "that value is the wrong shape",
            // which are two different things to do next.
            koru_core::StoreError::UnknownAttribute(key) => Self {
                code: "unknown_attribute".into(),
                message: format!(
                    "{key} is not an attribute this workspace defines; read_document reports the keys a node carries"
                ),
                details: Some(json!({"key": key})),
            },
            koru_core::StoreError::AttributeRejected { key, why } => Self {
                code: "attribute_rejected".into(),
                message: format!("{key} {why}"),
                details: Some(json!({"key": key, "why": why})),
            },
            // A canonical task command pointed at something it cannot act on:
            // a kind SPEC §4 excludes, or a node that is not a task and this
            // command does not create one. The `why` is a **token**, not
            // prose, so a client can branch on it and a test can assert on it
            // ([ADR-286]).
            //
            // [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
            koru_core::StoreError::TaskRejected { node, why } => {
                let node_id = node.to_storage();
                Self {
                    code: "task_rejected".into(),
                    message: format!(
                        "node {node_id} cannot carry task state: {}",
                        match why {
                            koru_core::tasks::TaskRefusal::KindNotCapable =>
                                "its kind is not one a task can live on",
                            koru_core::tasks::TaskRefusal::NotATask =>
                                "it is not a task, and this operation does not make one",
                        }
                    ),
                    details: Some(json!({"node_id": node_id, "why": why.as_str()})),
                }
            }
            other => Self {
                code: "document_error".into(),
                message: other.to_string(),
                details: None,
            },
        }
    }
}
impl From<koru_core::ShapeError> for ToolError {
    fn from(e: koru_core::ShapeError) -> Self {
        Self {
            code: "invalid_node_shape".into(),
            message: e.to_string(),
            details: None,
        }
    }
}
impl From<koru_history::Error> for ToolError {
    fn from(e: koru_history::Error) -> Self {
        Self {
            code: "history_error".into(),
            message: e.to_string(),
            details: None,
        }
    }
}

/// Whether this failure means the connection is spent rather than the request
/// being wrong.
///
/// Bearer renewal is handled by `koru-store-client` before an MCP request gets
/// here. The remaining recovery is a Store process that has disappeared. A
/// `storage_failed` is deliberately **not** here: that store answered, and
/// dialling it again reaches the same store and the same disk.
fn spent(error: &koru_store_core::errors::Error) -> bool {
    use koru_store_core::errors::Code;
    matches!(error.code(), Code::StoreUnreachable)
}

fn spent_ai(error: &koru_ai_wire::Failure) -> bool {
    matches!(
        error.code.as_str(),
        "gateway_unavailable" | "unauthenticated"
    )
}

#[derive(Clone)]
pub struct LocalBackend {
    segment: String,
    root: Option<PathBuf>,
    /// The store this bridge is talking to, once it has found one.
    ///
    /// **A connection outlives nothing.** The rendezvous hands back a port the
    /// store happened to bind and a session credential minted *for that
    /// connection*, held in that process's memory — so both die with the
    /// process and a cached `Client` is worth exactly as long as the store it
    /// dialled. `koru-mcp` is a bridge an MCP host starts once and leaves
    /// running for a day; a store restarted underneath it (a rebuild, a
    /// `stop-all`, a crash) turned every remaining tool call into
    /// `store_unreachable` until somebody restarted the *bridge*, which is a
    /// thing a person has to notice and do.
    ///
    /// So it is dialled on demand and dropped the moment it is spent. Every
    /// other store client here already worked this way without anyone deciding
    /// it: `StoreSession::join` re-discovers on each call, which is why
    /// reading a document recovered while listing workspaces did not.
    client: Arc<Mutex<Option<Client>>>,
    dialer: Option<koru_workspace::store::ClientFactory>,
    embedded: Option<Arc<dyn EmbeddedStore>>,
}

enum DocumentSession {
    Remote(StoreSession),
    Embedded {
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        /// The version this session opened at, recorded by whoever decoded the
        /// snapshot.
        ///
        /// **`None` until [`DocumentSession::record_base`] is called**, because
        /// `join_raw` hands back bytes it deliberately has not decoded — two
        /// kinds of document arrive on that route and only the caller knows
        /// which type to open them as. `persist` refuses on `None` rather than
        /// exporting everything, which would look like it worked.
        before: Option<loro::VersionVector>,
    },
}

impl DocumentSession {
    /// Record the version an embedded session opened at, once the caller has
    /// decoded the snapshot. A no-op for a remote session, which tracks its own.
    fn record_base<T: Synced + ?Sized>(&mut self, doc: &T) {
        if let DocumentSession::Embedded { before, .. } = self {
            *before = Some(doc.version());
        }
    }

    /// Leave the room. An embedded session never joined one.
    fn leave(&self) {
        if let DocumentSession::Remote(session) = self {
            session.leave();
        }
    }
}

impl LocalBackend {
    /// Prepare to talk to a store, dialling one now if it is there.
    ///
    /// **Not reaching a store is not an error here.** It was, and it made the
    /// bridge exit at startup: an MCP host then reports a server that failed to
    /// launch, and the user restarts it by hand once the store is up. A bridge
    /// that starts, says `store_unreachable` while there is no store, and
    /// begins working the moment there is one asks nothing of anybody. The
    /// failure is still reported — through the first call, and through
    /// `doctor`, which is the command whose whole job is to say.
    pub fn connect(segment: impl Into<String>, root: Option<&Path>) -> Result<Self, ToolError> {
        let backend = Self {
            segment: segment.into(),
            root: root.map(Path::to_path_buf),
            client: Arc::new(Mutex::new(None)),
            dialer: None,
            embedded: None,
        };
        if let Err(error) = backend.connected() {
            tracing::warn!(
                code = %error.code,
                error = %error.message,
                "no store yet; it will be dialled again on the first call that needs one"
            );
        }
        Ok(backend)
    }

    /// Explicit internal transport injection for hermetic server tests.
    pub fn with_client_factory(dialer: koru_workspace::store::ClientFactory) -> Self {
        Self {
            segment: String::new(),
            root: None,
            client: Arc::new(Mutex::new(None)),
            dialer: Some(dialer),
            embedded: None,
        }
    }

    /// Use the same semantic backend over an in-process bounded Store worker.
    pub fn with_embedded(store: Arc<dyn EmbeddedStore>) -> Self {
        Self {
            segment: String::new(),
            root: None,
            client: Arc::new(Mutex::new(None)),
            dialer: None,
            embedded: Some(store),
        }
    }

    /// Find a store, starting one where that is this build's business.
    ///
    /// With an explicit `--root` it is a developer's or a test's store and this
    /// process must not guess how it should have been started; without one it
    /// is the ordinary desktop case, and `ensure_running` is what every other
    /// Koru client does — including its refusal to start a channel this binary
    /// does not serve ([ADR-112](../../../docs/decisions/ADR-101-125.md#adr-112-sixbinaries--a-release-is-six-binaries-and-the-launcher-starts-none-of-them)).
    fn dial(&self) -> Result<Client, ToolError> {
        if let Some(dialer) = &self.dialer {
            return dialer().map_err(ToolError::from);
        }
        match self.root.as_deref() {
            Some(root) => Ok(Client::open_at(&self.segment, root)?),
            None => Ok(Client::ensure(&self.segment)?),
        }
    }

    fn held(&self) -> std::sync::MutexGuard<'_, Option<Client>> {
        // Poisoning is not a reason to stop: what this guards is a handle, and
        // a panic cannot have left one half-written.
        self.client.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current client, dialling one if there is none.
    fn connected(&self) -> Result<Client, ToolError> {
        let mut held = self.held();
        if let Some(client) = held.as_ref() {
            return Ok(client.clone());
        }
        let client = self.dial()?;
        *held = Some(client.clone());
        Ok(client)
    }

    /// Run a management call, re-dialling **once** if the store it was made
    /// against has gone.
    ///
    /// The lock is never held across the call itself: it guards the handle, not
    /// the request. `store_unreachable` means the request went nowhere and
    /// `unauthenticated` means the credential belonged to a store that is no
    /// longer there; neither is fixed by sending the same thing again to the
    /// same address, and both are fixed by finding out where the store is now.
    ///
    /// **A retried mutation may duplicate rather than fail.** `seed_document`
    /// carries an idempotency key and replays by construction; a repeated
    /// `capture_history` can leave a second history marker, and extraction's
    /// bare `create_document` a second empty document, in the narrow window
    /// where a request was applied and its answer lost. That is the trade this
    /// tree makes everywhere — duplication is recoverable, lost content is not
    /// — and the alternative is failing a call whose store is simply the next
    /// process along.
    fn store<T>(
        &self,
        call: impl Fn(&Client) -> Result<T, koru_store_core::errors::Error>,
    ) -> Result<T, ToolError> {
        let client = self.connected()?;
        match call(&client) {
            Err(error) if spent(&error) => {
                tracing::info!(code = %error.code(), "the store went away; dialling again");
                self.held().take();
                let fresh = self.connected()?;
                call(&fresh).map_err(ToolError::from)
            }
            other => other.map_err(ToolError::from),
        }
    }

    /// Run a read-only AI-facet call over the same reconnecting gateway client.
    fn ai<T>(
        &self,
        call: impl Fn(&Client) -> koru_api_client::AiResult<T>,
    ) -> Result<T, ToolError> {
        if self.embedded.is_some() {
            return Err(ToolError {
                code: "chat_tools_unavailable".into(),
                message: "This embedded Koru host does not provide chat tools.".into(),
                details: None,
            });
        }
        let client = self.connected()?;
        match call(&client) {
            Err(error) if spent_ai(&error) => {
                tracing::info!(code = %error.code, "the gateway went away; dialling again");
                self.held().take();
                let fresh = self.connected()?;
                call(&fresh).map_err(ai_tool_error)
            }
            other => other.map_err(ai_tool_error),
        }
    }

    fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
        match &self.embedded {
            Some(store) => store.workspaces(),
            None => self.store(|client| client.workspaces()),
        }
    }

    fn documents(&self, workspace: StoreWorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
        match &self.embedded {
            Some(store) => store.documents(workspace),
            None => self.store(|client| client.documents(workspace)),
        }
    }

    fn search(
        &self,
        request: SearchRequest,
    ) -> Result<koru_store_wire::search::SearchResponse, ToolError> {
        match &self.embedded {
            Some(store) => store.search(request),
            None => self.store(|client| client.store().search(&request)),
        }
    }

    /// What is outstanding, from the store's read model.
    ///
    /// **The embedded host has no answer to this and says so.** `EmbeddedStore`
    /// is a port over a bounded in-process Store *worker*, and the tasks query
    /// lives beside the daemon's HTTP surface rather than in that worker — so
    /// widening the port would mean teaching a second host to build a page it
    /// has no statement for. It refuses in the shape `ai()` already refuses in,
    /// because a capability a host does not have is better named than
    /// simulated with an empty list that reads as *you have nothing to do*.
    fn query_tasks(&self, query: &TaskQuery) -> Result<TaskQueryResponse, ToolError> {
        if self.embedded.is_some() {
            return Err(ToolError {
                code: "tasks_unavailable".into(),
                message: "This embedded Koru host does not provide the tasks query.".into(),
                details: None,
            });
        }
        self.store(|client| client.store().query_tasks(query))
    }

    fn snapshot(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<Vec<u8>, ToolError> {
        match &self.embedded {
            Some(store) => store.snapshot(workspace, document),
            None => self.store(|client| client.snapshot(workspace, document)),
        }
    }

    fn create_empty_document(
        &self,
        workspace: StoreWorkspaceId,
        title: &str,
        kind: &str,
    ) -> Result<StoreDocumentId, ToolError> {
        match &self.embedded {
            Some(store) => store.create_document(workspace, title.to_owned(), kind.to_owned()),
            None => self.store(|client| client.create_document(workspace, title, kind)),
        }
    }

    /// The one note a file carries, creating it when it has none.
    ///
    /// Idempotent in both modes: the store operation the embedded port wraps is
    /// idempotent by construction and reports `created` itself, and the HTTP
    /// route's `created` flag is the same fact passed through.
    fn ensure_file_note(
        &self,
        workspace: StoreWorkspaceId,
        file: StoreDocumentId,
    ) -> Result<(StoreDocumentId, bool), ToolError> {
        match &self.embedded {
            Some(store) => store.ensure_file_note(workspace, file),
            None => self.store(|client| {
                client
                    .ensure_file_note(workspace, file)
                    .map(|ensured| (ensured.id, ensured.created))
            }),
        }
    }

    fn seed_document(
        &self,
        request: EmbeddedSeedDocument,
    ) -> Result<EmbeddedSeededDocument, ToolError> {
        match &self.embedded {
            Some(store) => store.seed_document(request),
            None => self
                .store(|client| {
                    client.seed_document(
                        request.workspace,
                        &request.title,
                        &request.kind,
                        request.parent,
                        request.index,
                        &request.idempotency_key,
                        &request.request_hash,
                        request.initial_update.clone(),
                    )
                })
                .map(|seeded| EmbeddedSeededDocument {
                    id: seeded.id,
                    initial_update: seeded.initial_update,
                    replayed: seeded.replayed,
                }),
        }
    }

    fn capture_history(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        request: CaptureHistory,
    ) -> Result<HistoryEntry, ToolError> {
        match &self.embedded {
            Some(store) => store.capture_history(workspace, document, request),
            None => self.store(|client| client.capture_history(workspace, document, &request)),
        }
    }

    fn history(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        query: HistoryQuery,
    ) -> Result<HistoryPage, ToolError> {
        match &self.embedded {
            Some(store) => store.history(workspace, document, query),
            None => self.store(|client| client.history(workspace, document, &query)),
        }
    }

    fn history_snapshot(
        &self,
        workspace: StoreWorkspaceId,
        entry: HistoryEntryId,
    ) -> Result<Vec<u8>, ToolError> {
        match &self.embedded {
            Some(store) => store.history_snapshot(workspace, entry),
            None => self.store(|client| client.history_snapshot(workspace, entry)),
        }
    }

    fn blobs(&self, workspace: StoreWorkspaceId) -> Result<Vec<Blob>, ToolError> {
        match &self.embedded {
            Some(store) => store.blobs(workspace),
            None => self.store(|client| client.blobs(workspace)),
        }
    }

    fn get_blob(
        &self,
        workspace: StoreWorkspaceId,
        hash: &koru_store_core::model::BlobHash,
    ) -> Result<Vec<u8>, ToolError> {
        match &self.embedded {
            Some(store) => store.get_blob(workspace, hash.clone()),
            None => self.store(|client| client.get_blob(workspace, hash)),
        }
    }

    fn status(&self) -> Result<Status, ToolError> {
        match &self.embedded {
            Some(store) => store.status(),
            None => self.store(|client| client.status()),
        }
    }

    fn join(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<(DocumentSession, WorkspaceDoc), ToolError> {
        let (mut session, snapshot) = self.join_raw(workspace, document)?;
        let doc = WorkspaceDoc::from_snapshot(&snapshot)?;
        session.record_base(&doc);
        Ok((session, doc))
    }

    /// Join a document this call is about to **write**, and record the
    /// workspace it lives in if nothing ever did.
    ///
    /// **The bridge's own half of the store-path gap.**
    /// `WorkspaceDoc::create` is the only writer of the `C_META` workspace id
    /// and nothing on the store path calls it — the store hands out an empty
    /// `LoroDoc` that whoever opens it seeds nodes into. `koru-workspace`
    /// stamps at its two open seams, but a window is not involved here: this
    /// process reaches documents no window has ever had. Step 0 of every
    /// canonical task command's preflight is that read, so a `koru.due`
    /// through `edit_document` refused with
    /// `Malformed("the document carries no workspace id")` on exactly the
    /// documents an agent is most likely to meet — including the ones
    /// [`extract_to_document`](Self::extract_to_document) creates, which are
    /// made through the store and filled through this seam.
    ///
    /// `workspace` is the id the *request* addressed and the store confirmed;
    /// nothing here derives or invents one, per DATAMODEL §Identity. The stamp
    /// is idempotent, so a document that already carries an id keeps it.
    ///
    /// **Why the write paths and not [`join`](Self::join).** A stamp is a CRDT
    /// op, so it advances the document's version — and `read_document`
    /// publishes that version as `revision` without persisting a thing. Stamping
    /// on the read path would hand a caller a revision minted by a local peer
    /// nobody keeps, and the `edit_document` it then sent would be refused as
    /// `revision_conflict` against a document that had never heard of it. On a
    /// write path there is no such problem: `validate_base` asks whether the
    /// caller's base is *included*, and a stamp only ever adds.
    ///
    /// **It is committed here rather than carried by the edit, and that is not
    /// tidiness.** Every write path captures a `*_before` history entry, and
    /// [`capture`](Self::capture) hands the store this document's *frontiers* to
    /// fork at. A frontier naming an op the store has never seen is
    /// `version_unavailable`, so an uncommitted stamp turns the first agent edit
    /// of an unstamped document into a failure one step later than the one it
    /// was fixing. Committing it makes it a fact about the document rather than
    /// part of somebody's edit, which is also what it is: a refused edit still
    /// leaves the document knowing where it lives.
    ///
    /// The commit is paid **only when the stamp happens**. A document that
    /// already carries an id takes this route unchanged — which matters because
    /// a `publish` with nothing local to say still exports everything since the
    /// empty version vector and waits for the acknowledgement.
    ///
    /// Undo does not come into it — the bridge never calls `enable_undo`, so
    /// there is no history for the stamp to become the first step of. That is
    /// the one thing the desktop seam has to order around and this one does not.
    fn join_for_edit(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<(DocumentSession, WorkspaceDoc), ToolError> {
        let (mut session, mut doc) = self.join(workspace, document)?;
        // **Both ids, for one reason.** The workspace is step 0 of every task
        // command's preflight; the document is the middle segment of every
        // reference minted here, and this is the only side that knows it — the
        // caller asked the store for this document *by* this id. Each stamp is
        // idempotent, and the document one answers whether it wrote, so a
        // document already carrying both takes the route below unchanged.
        let owed = doc.id().is_none();
        if owed {
            doc.stamp_workspace_id(CoreWorkspaceId::from_uuid(workspace.as_uuid()))?;
        }
        let stamped_document =
            doc.stamp_document_id(CoreDocumentId::from_uuid(document.as_uuid()))?;
        if owed || stamped_document {
            // After `join`'s `record_base`, deliberately: an embedded session
            // exports what changed since the base, and a stamp inside the base
            // is a stamp that never reaches the disk.
            self.persist(&mut session, &mut doc)?;
            // And the base moves past it, so the edit that follows publishes
            // itself rather than the stamp a second time.
            session.record_base(&doc);
        }
        Ok((session, doc))
    }

    /// Join a room and hand back its snapshot bytes undecoded.
    ///
    /// Two kinds of document arrive on this route now — a page, and the hidden
    /// `koru-system` document behind [`Self::system_document`] — and only the
    /// caller knows which type to open them as. The transport choice (an
    /// injected factory for a hermetic host, discovery otherwise) is the part
    /// they share, and it stays in one place.
    fn join_raw(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
    ) -> Result<(DocumentSession, Vec<u8>), ToolError> {
        // The embedded store answers without a socket, and has no room to
        // join. It cannot record the base version here — these bytes are
        // deliberately not decoded — so the caller does it after decoding,
        // and `persist` refuses rather than guesses if nobody did.
        if let Some(embedded) = &self.embedded {
            let snapshot = embedded.snapshot(workspace, document)?;
            return Ok((
                DocumentSession::Embedded {
                    workspace,
                    document,
                    before: None,
                },
                snapshot,
            ));
        }
        match &self.dialer {
            Some(dialer) => {
                let (session, snapshot) =
                    StoreSession::join_with_client_factory(dialer.clone(), workspace, document)?;
                Ok((DocumentSession::Remote(session), snapshot))
            }
            None => {
                let (session, snapshot) =
                    StoreSession::join(&self.segment, self.root.as_deref(), workspace, document)?;
                Ok((DocumentSession::Remote(session), snapshot))
            }
        }
    }

    /// The workspace's system document — the tag table and the custom
    /// attribute definitions — opened through the store as a client.
    ///
    /// **This is the third implementation of "find and open it" in the tree**,
    /// and the two that exist cannot be reached from here: `koru-workspace`'s
    /// is a private method on a `Workspace` this process has none of, and the
    /// daemon's `open_system_document` locks an in-process `AppState`. What
    /// stays shared is the *rule*, not a copy of it —
    /// [`SystemDoc::canonical_document`] picks the survivor and
    /// [`SystemDoc::absorb`] unions the rest, exactly as they do.
    ///
    /// It differs from the app's in one deliberate way: a duplicate is
    /// unioned and **left in the manifest**. Dropping it is a manifest write,
    /// and the manifest is the app's to keep tidy; a bridge that pruned rows
    /// as a side effect of tagging a task would be doing something nobody
    /// asked it to.
    fn system_document(
        &self,
        workspace: StoreWorkspaceId,
        create: bool,
    ) -> Result<Option<(DocumentSession, SystemDoc)>, ToolError> {
        let mut ids: Vec<CoreDocumentId> = self
            .store(|client| client.documents(workspace))?
            .into_iter()
            .filter(|entry| entry.kind == koru_core::explore::KIND_SYSTEM)
            .map(|entry| koru_workspace::explore::koru_document(entry.id))
            .collect();
        ids.sort();
        ids.dedup();

        let Some(canonical) = SystemDoc::canonical_document(&ids) else {
            if !create {
                return Ok(None);
            }
            // Registered with an empty title, the way the app registers it:
            // `explore::listed` filters the kind out of every listing, so a
            // name here would be a name nobody ever sees.
            let id = self.store(|client| {
                client.create_document_at(
                    workspace,
                    "",
                    koru_core::explore::KIND_SYSTEM,
                    None,
                    None,
                )
            })?;
            let (mut session, snapshot) = self.join_raw(workspace, id)?;
            let mut doc = SystemDoc::create()?;
            // Based on the *empty* document, before the seed is absorbed, so an
            // embedded session exports the whole of what it then writes.
            session.record_base(&doc);
            // The union is the seed plus whatever a racing peer already wrote.
            doc.absorb(&snapshot)?;
            self.persist(&mut session, &mut doc)?;
            return Ok(Some((session, doc)));
        };

        let canonical = koru_workspace::explore::store_document(canonical);
        let (mut session, snapshot) = self.join_raw(workspace, canonical)?;
        let mut doc = SystemDoc::open_or_create(Some(&snapshot))?;
        session.record_base(&doc);
        for duplicate in ids
            .into_iter()
            .map(koru_workspace::explore::store_document)
            .filter(|id| *id != canonical)
        {
            if let Ok((extra, bytes)) = self.join_raw(workspace, duplicate) {
                if let Err(error) = doc.absorb(&bytes) {
                    tracing::debug!(%duplicate, "a duplicate system document did not absorb: {error}");
                }
                extra.leave();
            }
        }
        Ok(Some((session, doc)))
    }

    fn persist<T: Synced + ?Sized>(
        &self,
        session: &mut DocumentSession,
        doc: &mut T,
    ) -> Result<(), ToolError> {
        match session {
            DocumentSession::Remote(session) => {
                let Some(batch) = session.publish(doc)? else {
                    return Ok(());
                };
                let deadline = Instant::now() + Duration::from_secs(10);
                while !session.is_durable(batch) {
                    session.drain(doc)?;
                    if Instant::now() >= deadline {
                        return Err(ToolError {
                            code: "durability_timeout".into(),
                            message: "the Store did not acknowledge the edit within ten seconds"
                                .into(),
                            details: None,
                        });
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Ok(())
            }
            DocumentSession::Embedded {
                workspace,
                document,
                before,
            } => {
                // **Refuse rather than guess.** `join_raw` cannot record the
                // base version — it hands back bytes it has not decoded — so
                // the caller does, right after decoding. Defaulting to "export
                // everything" here would look like it worked and would be the
                // one failure nobody notices.
                let Some(before) = before.as_ref() else {
                    return Err(ToolError {
                        code: "session_not_based".into(),
                        message: "an embedded session was not given its base version before saving"
                            .into(),
                        details: None,
                    });
                };
                let update = doc
                    .export_updates_since(before)
                    .map_err(ToolError::invalid)?;
                if update.is_empty() {
                    return Ok(());
                }
                self.embedded
                    .as_ref()
                    .expect("embedded document session has an embedded Store")
                    .persist_update(*workspace, *document, update)
            }
        }
    }

    fn file_json(
        &self,
        workspace: StoreWorkspaceId,
        entry: DocumentEntry,
    ) -> Result<Value, ToolError> {
        let snapshot = self.snapshot(workspace, entry.id)?;
        let doc = WorkspaceDoc::from_snapshot(&snapshot)?;
        let root = doc
            .children(Parent::Root)?
            .into_iter()
            .find(|id| doc.kind_of(*id) == Some(Kind::File))
            .ok_or_else(|| ToolError::invalid("file entry has no file root node"))?;
        let node = doc.read(root)?;
        let Content::Blob { hash, mime, .. } = node.content else {
            return Err(ToolError::invalid("file root has no blob content"));
        };
        let size = self
            .blobs(workspace)?
            .into_iter()
            .find(|blob| blob.hash.as_str() == hash.0)
            .map(|blob| blob.size);
        Ok(json!({
            "file_id": entry.id.to_storage(),
            "name": entry.title,
            "mime": mime,
            "size": size,
            "hash": hash.0,
            "root_node_id": root.to_storage(),
            // The file's one note as a document id, so `list_files`,
            // `search_files` and `get_file` reveal the link; null when none.
            "note_id": entry.note.map(|note| note.to_storage()),
        }))
    }

    fn capture(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        doc: &WorkspaceDoc,
        reason: &str,
        boundary: Option<&str>,
    ) -> Result<Value, ToolError> {
        let request = CaptureHistory {
            reason: reason.into(),
            frontiers: Some(doc.doc().oplog_frontiers().encode()),
            label: None,
            message: None,
            boundary: boundary.map(str::to_owned),
        };
        let entry = self.capture_history(workspace, document, request)?;
        serde_json::to_value(entry).map_err(|e| ToolError::invalid(e.to_string()))
    }

    /// Refuse a document the read tools would not open, in `read_document`'s
    /// own words, naming the field that carried it.
    ///
    /// **Every write and history tool asks this before it joins, stamps,
    /// captures or creates anything.** They used to join by id with no kind
    /// check at all, so a client could write into a run, a schedule or the
    /// system document — kinds it can neither list, search nor read here.
    /// Asked first, a refusal leaves nothing behind:
    /// [`join_for_edit`](Self::join_for_edit) alone would already have
    /// persisted a stamp.
    fn ensure_readable(
        &self,
        workspace: StoreWorkspaceId,
        document: StoreDocumentId,
        field: &str,
    ) -> Result<(), ToolError> {
        if self
            .documents(workspace)?
            .into_iter()
            .any(|entry| entry.id == document && readable_kind(&entry.kind))
        {
            return Ok(());
        }
        Err(ToolError::invalid(format!(
            "{field} is not a page or a note in this workspace; use get_file for files"
        )))
    }

    fn inline_context(&self, workspace: StoreWorkspaceId) -> Result<InlineContext, ToolError> {
        let profile = self
            .workspaces()?
            .into_iter()
            .find(|candidate| candidate.id == workspace)
            .map(|workspace| CoreProfileId::from_uuid(workspace.owner.as_uuid()))
            .ok_or_else(|| ToolError::invalid("workspace_id is not available"))?;
        let entries = self.documents(workspace)?;
        let documents = entries
            .iter()
            .filter(|entry| entry.kind == koru_core::explore::KIND_DOCUMENT)
            .map(|entry| (entry.id, entry.title.clone()))
            .collect();
        let files = entries
            .into_iter()
            .filter(|entry| entry.kind == koru_core::explore::KIND_FILE)
            .map(|entry| (entry.id, entry.title))
            .collect();
        Ok(InlineContext {
            profile,
            workspace: CoreWorkspaceId::from_uuid(workspace.as_uuid()),
            documents,
            files,
            attrs: AttrContext::default(),
        })
    }

    /// Answer this batch's tag names and custom keys out of the workspace
    /// system document — **on a fork, so nothing is created yet**.
    ///
    /// The answers are enough to validate a batch with: a tag that already
    /// exists resolves to the id it really has, and one that has to be created
    /// gets an id minted on the fork. What comes back beside the context is
    /// what the batch still *owes* the table, held until the batch is known
    /// good — [`PendingAttrs::commit`] is the only door to a real write and it
    /// opens after validation, so a rejected batch leaves the tag table
    /// exactly as it was (Q41, reversing ADR-190's fourth point).
    ///
    /// The fork is a snapshot round trip per edit that names a tag, and only
    /// then: a batch that asks the table nothing never opens a system document
    /// at all, and one whose names all exist settles without a fork surviving
    /// this call.
    fn attr_context<'a>(
        &self,
        workspace: StoreWorkspaceId,
        operations: &'a [EditOperation],
    ) -> Result<(AttrContext, PendingAttrs<'a>), ToolError> {
        let asked = AttrRequests::read(operations)?;
        if asked.is_empty() {
            return Ok((AttrContext::default(), PendingAttrs::Settled));
        }
        let now = koru_workspace::open::now_ms();
        // Opened read-only, even for a batch that will need a system document:
        // registering one is a manifest write, and a batch that fails
        // validation must not leave that behind either. `commit` creates it.
        let Some((session, mut doc)) = self.system_document(workspace, false)? else {
            // No system document. Every custom key is therefore undefined and
            // every named tag names nothing — which an empty table answers
            // exactly, refusal included. This one is a throwaway, so it *is*
            // the fork; the real table is registered by `commit`, if the batch
            // ever gets that far.
            let mut empty = SystemDoc::create()?;
            let (context, creates) = asked.resolve(&mut empty, now)?;
            let pending = match creates {
                true => PendingAttrs::Absent { workspace, asked },
                false => PendingAttrs::Settled,
            };
            return Ok((context, pending));
        };
        let session = SessionGuard(session);
        let (context, creates) = match asked.creates() {
            // A batch that only *reads* the table — a removal, a custom key —
            // cannot write, so the real document answers it and no snapshot is
            // taken. `remove_tag_by_an_unknown_name_creates_nothing` is what
            // holds that claim up.
            false => asked.resolve(&mut doc, now)?,
            true => asked.resolve_on_fork(&doc, now)?,
        };
        if !creates {
            // Every answer came off a record that already exists, so it is the
            // real table's answer too and there is nothing left to write.
            return Ok((context, PendingAttrs::Settled));
        }
        Ok((
            context,
            PendingAttrs::Held {
                session,
                doc,
                asked,
            },
        ))
    }

    fn extract_to_document(&self, a: ExtractToDocumentRequest) -> Result<Value, ToolError> {
        let workspace = workspace(&a.workspace_id)?;
        let source_document = document(&a.source_document_id)?;
        let context = self.inline_context(workspace)?;

        // Exactly one destination: a title to create, or a document to join.
        let (new_title, existing_target) = match (&a.new_document_title, &a.target_document_id) {
            (Some(title), None) => (Some(title.as_str()), None),
            (None, Some(target)) => (None, Some(document(target)?)),
            _ => {
                return Err(ToolError::invalid(
                    "pass exactly one of new_document_title and target_document_id",
                ));
            }
        };
        if existing_target == Some(source_document) {
            return Err(ToolError::invalid(
                "target_document_id must differ from source_document_id; \
                 use edit operations to move within one document",
            ));
        }
        // Both ends, before either is joined or a destination is created: a
        // refused move must leave no stamp and no empty page behind.
        self.ensure_readable(workspace, source_document, "source_document_id")?;
        if let Some(target) = existing_target {
            self.ensure_readable(workspace, target, "target_document_id")?;
        }
        let under = a
            .under_node_id
            .as_deref()
            .map(|raw| node(raw))
            .transpose()?;

        let mut source_nodes = vec![node(&a.source_node_id)?];
        for raw in &a.extra_source_node_ids {
            source_nodes.push(node(raw)?);
        }
        let (mut source_session, mut source) = self.join_for_edit(workspace, source_document)?;
        validate_base(&source, &a.base_revision)?;

        for (i, &source_node) in source_nodes.iter().enumerate() {
            let value = source.read(source_node)?;
            if value.kind == Kind::Document || source.parent(source_node)? == Parent::Root {
                return Err(ToolError::invalid(
                    "every source node must name a document-body block, not a document root",
                ));
            }
            if i == 0 && value.content.as_text() != Some(a.expected.as_str()) {
                return Err(ToolError {
                    code: "precondition_failed".into(),
                    message: "extract_to_document expected value does not match".into(),
                    details: None,
                });
            }
        }
        let cuttings = source_nodes
            .iter()
            .map(|&source_node| source.cutting(source_node))
            .collect::<Result<Vec<_>, _>>()?;

        // Prove the complete source-side replacement before touching any
        // destination. Validation uses an otherwise meaningless route; only
        // shape, position and rollback behaviour matter on this fork.
        let mut validation = source.fork_for_read();
        let validation_attr = context.routed_document_attr(StoreDocumentId::new(), "validation");
        validation.replace_subtrees_with_pill(&source_nodes, &validation_attr)?;

        // The trail pill's caption: the created title, or what the workspace
        // calls the existing destination.
        let label = match (new_title, existing_target) {
            (Some(title), _) => title.to_owned(),
            (None, Some(target)) => context
                .documents
                .get(&target)
                .cloned()
                .unwrap_or_else(|| "Untitled".into()),
            (None, None) => unreachable!("one destination was proven above"),
        };

        let (target_document, created) = match existing_target {
            Some(target) => (target, false),
            None => (
                self.create_empty_document(workspace, new_title.unwrap_or_default(), "page")?,
                true,
            ),
        };
        let failure_details = |source_changed: bool| {
            json!({
                "created_document_id": created.then(|| target_document.to_storage()),
                "target_document_id": target_document.to_storage(),
                "source_document_id": source_document.to_storage(),
                "source_unchanged": !source_changed,
                "recovery_required": true,
            })
        };

        let (mut target_session, mut target) = self
            .join_for_edit(workspace, target_document)
            .map_err(|error| error.with_details(failure_details(false)))?;
        let target_boundary = uuid::Uuid::new_v4().to_string();
        self.capture(
            workspace,
            target_document,
            &target,
            "mcp_before",
            Some(&target_boundary),
        )
        .map_err(|error| error.with_details(failure_details(false)))?;
        // Where the content goes. A created document gets its root here; an
        // existing one already has one, and `under` — when it still resolves —
        // names a block to land first beneath instead of the top.
        let anchor = if created {
            target
                .insert(Parent::Root, Node::document(new_title.unwrap_or_default())?)
                .map_err(ToolError::from)
                .map_err(|error| error.with_details(failure_details(false)))?
        } else {
            let root = target
                .children(Parent::Root)
                .ok()
                .into_iter()
                .flatten()
                .find(|&id| target.kind_of(id) == Some(Kind::Document))
                .ok_or_else(|| {
                    ToolError::invalid("the destination document has no content root")
                        .with_details(failure_details(false))
                })?;
            under
                .filter(|&node| target.read(node).is_ok())
                .unwrap_or(root)
        };
        // Planting appends; an existing destination wants the moved content
        // first, in its original order.
        let mut index = 0usize;
        for cutting in &cuttings {
            let planted = target
                .plant(cutting, Parent::Node(anchor), "")
                .map_err(ToolError::from)
                .map_err(|error| error.with_details(failure_details(false)))?;
            if !created {
                target
                    .move_node(planted, Parent::Node(anchor), index)
                    .map_err(ToolError::from)
                    .map_err(|error| error.with_details(failure_details(false)))?;
            }
            index += 1;
        }
        self.persist(&mut target_session, &mut target)
            .map_err(|error| error.with_details(failure_details(false)))?;
        let target_history = self
            .capture(
                workspace,
                target_document,
                &target,
                "mcp_after",
                Some(&target_boundary),
            )
            .map_err(|error| error.with_details(failure_details(false)))?;

        let source_boundary = uuid::Uuid::new_v4().to_string();
        self.capture(
            workspace,
            source_document,
            &source,
            "mcp_before",
            Some(&source_boundary),
        )
        .map_err(|error| error.with_details(failure_details(false)))?;
        // Deep-link the pill to the heading the content went under, when one
        // was chosen; the anchor of a created document is its root, which the
        // document-level reference already means.
        let link_node = (!created)
            .then_some(under)
            .flatten()
            .filter(|&node| target.read(node).is_ok());
        let attr = context.document_reference_attr(target_document, link_node, &label);
        let replacement = source
            .replace_subtrees_with_pill(&source_nodes, &attr)
            .map_err(ToolError::from)
            .map_err(|error| error.with_details(failure_details(false)))?;
        self.persist(&mut source_session, &mut source)
            .map_err(|error| error.with_details(failure_details(false)))?;
        let source_history = self
            .capture(
                workspace,
                source_document,
                &source,
                "mcp_after",
                Some(&source_boundary),
            )
            .map_err(|error| error.with_details(failure_details(true)))?;

        Ok(json!({
            "document_id": target_document.to_storage(),
            "document_revision": revision(&target),
            "source_revision": revision(&source),
            "source_link_node_id": replacement.to_storage(),
            "history": {
                "document": target_history,
                "source": source_history,
            }
        }))
    }
}

/// Whether a manifest kind is one the read tools open: a page, or the note
/// attached to a file (ADR-277).
///
/// `read_document` and `search_documents` admit exactly this set, and every
/// write and history tool admits no more — **a client may only change what it
/// can read**, so a kind added here becomes readable and writable together.
/// `list_documents` and inline links stay narrower on purpose: a note is
/// reached through its file, never listed or linked as a page.
fn readable_kind(kind: &str) -> bool {
    kind == koru_core::explore::KIND_DOCUMENT || kind == koru_core::explore::KIND_NOTE
}

impl Backend for LocalBackend {
    fn invoke(
        &self,
        _context: &RequestContext,
        tool: &str,
        arguments: Value,
    ) -> Result<Value, ToolError> {
        match tool {
            "list_chats" => {
                let request: ListChatsToolRequest = from(arguments)?;
                serde_json::to_value(self.ai(|client| client.ai().tool_list_chats(&request))?)
                    .map_err(|error| ToolError::invalid(error.to_string()))
            }
            "search_chats" => {
                let request: SearchChatsToolRequest = from(arguments)?;
                serde_json::to_value(self.ai(|client| client.ai().tool_search_chats(&request))?)
                    .map_err(|error| ToolError::invalid(error.to_string()))
            }
            "read_chat" => {
                let request: ReadChatToolRequest = from(arguments)?;
                serde_json::to_value(self.ai(|client| client.ai().tool_read_chat(&request))?)
                    .map_err(|error| ToolError::invalid(error.to_string()))
            }
            "list_workspaces" => Ok(json!({
                "workspaces": self.workspaces()?
            })),
            "list_documents" => {
                let a: WorkspaceRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                Ok(json!({
                    "documents": self.documents(ws)?
                        .into_iter().filter(|entry| entry.kind == koru_core::explore::KIND_DOCUMENT)
                        .collect::<Vec<_>>()
                }))
            }
            "search_documents" => {
                let a: SearchDocumentsRequest = from(arguments)?;
                let workspaces = a
                    .workspace_ids
                    .iter()
                    .map(|raw| workspace(raw))
                    .collect::<Result<Vec<_>, _>>()?;
                let request = SearchRequest {
                    query: SearchQuery {
                        text: a.query,
                        workspaces,
                        excluded_workspaces: Vec::new(),
                        kinds: a.kinds,
                        sort: a.sort,
                        lifecycle: a.lifecycle,
                        page_size: a.page_size.unwrap_or(50).clamp(1, 200),
                        offset: a.offset.unwrap_or(0),
                        request_seq: 0,
                        tags: a.tags,
                        attributes: attribute_filters(&a.attributes)?,
                    },
                    include_facets: false,
                    recent_queries: None,
                };
                let response = self.search(request)?;
                let hits = response
                    .page
                    .hits
                    .into_iter()
                    .filter(|hit| {
                        // Pages and the notes attached to files — both ordinary
                        // documents. File hits stay out: their bytes are read
                        // through read_file, not this surface.
                        hit.document.is_some() && readable_kind(&hit.document_kind)
                    })
                    .collect::<Vec<_>>();
                Ok(json!({
                    "hits": hits,
                    "next_offset": response.page.next_offset,
                    "updating_workspaces": response.page.updating_workspaces,
                    "corrected_query": response.page.corrected_query,
                }))
            }
            "list_tasks" => {
                let a: ListTasksRequest = from(arguments)?;
                let workspaces = a
                    .workspace_ids
                    .iter()
                    .map(|raw| workspace(raw))
                    .collect::<Result<Vec<_>, _>>()?;
                let query = TaskQuery {
                    workspaces,
                    lifecycle: a.lifecycle,
                    due: a.due,
                    min_priority: a.min_priority,
                    list: a.list,
                    include_trashed: a.include_trashed,
                    sort: a.sort,
                    // Clamped rather than refused, the store's own rule: a
                    // number too large is a client being hopeful.
                    limit: a
                        .limit
                        .unwrap_or(koru_store_wire::tasks::DEFAULT_LIMIT)
                        .clamp(1, TASK_MAX_LIMIT),
                    after: a.cursor,
                    ..TaskQuery::default()
                };
                let response = self.query_tasks(&query)?;
                Ok(task_page_json(response))
            }
            "list_files" => {
                let a: WorkspaceRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let files = self
                    .documents(ws)?
                    .into_iter()
                    .filter(|entry| entry.kind == koru_core::explore::KIND_FILE)
                    .map(|entry| self.file_json(ws, entry))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(json!({"files": files}))
            }
            "search_files" => {
                let a: SearchFilesRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let query = a.query.to_lowercase();
                let files = self
                    .documents(ws)?
                    .into_iter()
                    .filter(|entry| {
                        entry.kind == koru_core::explore::KIND_FILE
                            && entry.title.to_lowercase().contains(&query)
                    })
                    .take(a.limit.unwrap_or(50).min(200))
                    .map(|entry| self.file_json(ws, entry))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(json!({"files": files}))
            }
            "get_file" => {
                let a: FileRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let id = document(&a.file_id)?;
                let entry = self
                    .documents(ws)?
                    .into_iter()
                    .find(|entry| entry.id == id && entry.kind == koru_core::explore::KIND_FILE)
                    .ok_or_else(|| ToolError::invalid("file_id is not a file in this workspace"))?;
                self.file_json(ws, entry)
            }
            "read_file" => {
                use base64::Engine as _;
                let a: ReadFileRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let id = document(&a.file_id)?;
                let entry = self
                    .documents(ws)?
                    .into_iter()
                    .find(|entry| entry.id == id && entry.kind == koru_core::explore::KIND_FILE)
                    .ok_or_else(|| ToolError::invalid("file_id is not a file in this workspace"))?;
                let metadata = self.file_json(ws, entry)?;
                let hash = metadata
                    .get("hash")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::invalid("file has no blob hash"))?;
                let hash = koru_store_core::model::BlobHash::parse(hash)?;
                let bytes = self.get_blob(ws, &hash)?;
                let offset = a.offset.unwrap_or(0).min(bytes.len());
                let length = a.length.unwrap_or(1024 * 1024).min(1024 * 1024);
                let end = offset.saturating_add(length).min(bytes.len());
                Ok(json!({
                    "file": metadata,
                    "offset": offset,
                    "length": end - offset,
                    "total_bytes": bytes.len(),
                    "eof": end == bytes.len(),
                    "base64": base64::engine::general_purpose::STANDARD.encode(&bytes[offset..end]),
                }))
            }
            "read_document" => {
                let a: DocumentRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let doc_id = document(&a.document_id)?;
                // A file's note is an ordinary document and reads through the
                // same projection; the note's id comes from the file's
                // `note_id`, never from a listing.
                self.ensure_readable(ws, doc_id, "document_id")?;
                let (_, doc) = self.join(ws, doc_id)?;
                Ok(document_json(doc_id, &doc)?)
            }
            "create_document" => {
                let a: CreateDocumentRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                if a.kind.as_deref().is_some_and(|kind| kind != "page") {
                    return Err(ToolError::invalid(
                        "create_document supports manifest kind page only",
                    ));
                }
                let idempotency_key = match a.idempotency_key.as_deref() {
                    Some(key) => uuid::Uuid::parse_str(key)
                        .map_err(|_| ToolError::invalid("idempotency_key must be a UUID"))?
                        .to_string(),
                    None => uuid::Uuid::new_v4().to_string(),
                };
                let request_hash = create_request_hash(&a, ws)?;
                if a.initial_operations.is_some() && a.root_node_id.is_none() {
                    return Err(ToolError::invalid(
                        "root_node_id is required when initial_operations is present",
                    ));
                }
                let root_id = match a.root_node_id.as_deref() {
                    Some(value) => node(value)?,
                    None => NodeId::new(),
                };
                let inline_context = self.inline_context(ws)?;
                let mut doc =
                    WorkspaceDoc::create(CoreWorkspaceId::from_uuid(ws.as_uuid()), &a.title)?;
                let mut root_value = Node::document(&a.title)?;
                root_value.id = root_id;
                let root = doc.insert(Parent::Root, root_value)?;
                let before = doc.fork_for_read();
                match a.initial_operations.as_deref() {
                    Some(operations) => {
                        apply_initial_ops(&mut doc, root, operations, &inline_context)?
                    }
                    None => {
                        doc.insert(Parent::Node(root), Node::paragraph("")?)?;
                    }
                }
                let initial_update = doc.export_all_updates()?;
                // Its own message rather than `document`'s: an error naming
                // `document_id` when `parent_document_id` is what was wrong
                // sends somebody to check the field that is fine.
                let parent = a
                    .parent_document_id
                    .as_deref()
                    .map(|value| {
                        StoreDocumentId::parse(value)
                            .ok_or_else(|| ToolError::invalid("parent_document_id is not a UUID"))
                    })
                    .transpose()?;
                let kind = a.kind.as_deref().unwrap_or("page");
                // The one call here that is safe to repeat by construction: the
                // idempotency key and request hash make a second attempt a
                // replay of the first, whichever store answers it.
                let seeded = self.seed_document(EmbeddedSeedDocument {
                    workspace: ws,
                    title: a.title.clone(),
                    kind: kind.to_owned(),
                    parent,
                    index: None,
                    idempotency_key,
                    request_hash,
                    initial_update,
                })?;
                let returned = WorkspaceDoc::from_snapshot(&seeded.initial_update)?;
                if !seeded.replayed {
                    let boundary = uuid::Uuid::new_v4().to_string();
                    let _ = self.capture(ws, seeded.id, &before, "mcp_before", Some(&boundary))?;
                    let _ = self.capture(ws, seeded.id, &returned, "mcp_after", Some(&boundary))?;
                }
                Ok(document_json(seeded.id, &returned)?)
            }
            "ensure_file_note" => {
                let a: FileRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let file = document(&a.file_id)?;
                // The store refuses anything that is not a file; that refusal
                // is the tool's error, not a separate guard here.
                let (note, created) = self.ensure_file_note(ws, file)?;
                Ok(json!({
                    "note_id": note.to_storage(),
                    "created": created,
                }))
            }
            "edit_document" => {
                let a: EditDocumentRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let doc_id = document(&a.document_id)?;
                self.ensure_readable(ws, doc_id, "document_id")?;
                let mut inline_context = self.inline_context(ws)?;
                // Provisional answers, minted on a fork of the system
                // document: enough to validate with, and nothing written. Any
                // creation the batch implies is held in `pending`, which
                // writes nothing if this function leaves before committing it.
                let (provisional, mut pending) = self.attr_context(ws, &a.operations)?;
                inline_context.attrs = provisional;
                let (mut session, mut doc) = self.join_for_edit(ws, doc_id)?;
                validate_base(&doc, &a.base_revision)?;
                let mut validation = doc.fork_for_read();
                apply_ops(&mut validation, &a.operations, &inline_context)?;
                // Proved, so the tags this batch names may now exist. The real
                // pass runs on the ids that were persisted, never the fork's.
                if let Some(attrs) = pending.commit(self)? {
                    inline_context.attrs = attrs;
                }
                let boundary = uuid::Uuid::new_v4().to_string();
                let _ = self.capture(ws, doc_id, &doc, "mcp_before", Some(&boundary))?;
                let tasks = apply_ops(&mut doc, &a.operations, &inline_context)?;
                self.persist(&mut session, &mut doc)?;
                // **After the durability wait**, best effort, one attempt. A
                // failed append keeps the task change and is never retried and
                // never fabricated later (SPEC §9), and the batch is the burst:
                // one delivery per `edit_document` call.
                tasks.deliver(&mut tasks::LogSink::default());
                let after = self.capture(ws, doc_id, &doc, "mcp_after", Some(&boundary))?;
                Ok(json!({"revision": revision(&doc), "history": after}))
            }
            "extract_to_document" => {
                let a: ExtractToDocumentRequest = from(arguments)?;
                self.extract_to_document(a)
            }
            "save_document_version" => {
                let a: SaveVersionRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let doc_id = document(&a.document_id)?;
                self.ensure_readable(ws, doc_id, "document_id")?;
                let (_, doc) = self.join(ws, doc_id)?;
                let request = CaptureHistory {
                    reason: "manual".into(),
                    frontiers: Some(doc.doc().oplog_frontiers().encode()),
                    label: a.label,
                    message: a.message,
                    boundary: None,
                };
                let entry = self.capture_history(ws, doc_id, request)?;
                Ok(serde_json::to_value(entry).unwrap_or(Value::Null))
            }
            "list_document_versions" => {
                let a: DocumentRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let doc_id = document(&a.document_id)?;
                self.ensure_readable(ws, doc_id, "document_id")?;
                let query = HistoryQuery {
                    limit: Some(200),
                    ..Default::default()
                };
                let page = self.history(ws, doc_id, query)?;
                Ok(serde_json::to_value(page).unwrap_or(Value::Null))
            }
            "compare_document_versions" => {
                // **The one history tool not gated by kind, because it cannot
                // be here.** It names two history entries and no document,
                // and the store's history-snapshot route answers bytes alone,
                // so this bridge cannot tell whose entries they are. Every
                // tool that hands an entry id to a client is gated, which is
                // what keeps one of an unreadable kind out of an MCP client's
                // hands; closing it outright is docs/mcp/BACKLOG.md's.
                let a: CompareRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let left_id = history(&a.left_entry_id)?;
                let right_id = history(&a.right_entry_id)?;
                let left = self.history_snapshot(ws, left_id)?;
                let right = self.history_snapshot(ws, right_id)?;
                let diff = koru_history::compare(
                    &koru_history::SemanticDocument::from_snapshot(&left)?,
                    &koru_history::SemanticDocument::from_snapshot(&right)?,
                );
                Ok(diff_json(&diff))
            }
            "restore_document_version" => {
                let a: RestoreRequest = from(arguments)?;
                let ws = workspace(&a.workspace_id)?;
                let doc_id = document(&a.document_id)?;
                self.ensure_readable(ws, doc_id, "document_id")?;
                let (mut session, mut doc) = self.join_for_edit(ws, doc_id)?;
                validate_base(&doc, &a.base_revision)?;
                let entry_id = history(&a.entry_id)?;
                let target = self.history_snapshot(ws, entry_id)?;
                let boundary = uuid::Uuid::new_v4().to_string();
                let _ = self.capture(ws, doc_id, &doc, "restore_before", Some(&boundary))?;
                doc.restore_snapshot_forward(&target)?;
                self.persist(&mut session, &mut doc)?;
                let after = self.capture(ws, doc_id, &doc, "restore_after", Some(&boundary))?;
                Ok(json!({"revision": revision(&doc), "history": after}))
            }
            _ => Err(ToolError::invalid("unknown tool")),
        }
    }

    fn doctor(&self) -> Result<Value, ToolError> {
        let status = self.status()?;
        Ok(
            json!({"ok": true, "api_version": status.api_version, "capabilities": status.capabilities,
            "history": status.capabilities.iter().any(|c| c == "document-history-v1")}),
        )
    }
}

fn ai_tool_error(error: koru_ai_wire::Failure) -> ToolError {
    ToolError {
        code: error.code,
        message: error.message,
        details: (!error.details.is_empty()).then(|| json!(error.details)),
    }
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorkspaceRequest {
    /// UUID of the workspace returned by `list_workspaces`.
    workspace_id: String,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentationRequest {
    /// Koru documentation URI. Currently only `koru://docs/model` is available.
    uri: String,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DocumentRequest {
    /// UUID of the workspace returned by `list_workspaces`.
    workspace_id: String,
    /// UUID of the document returned by `list_documents`.
    document_id: String,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FileRequest {
    /// UUID of the workspace returned by `list_workspaces`.
    workspace_id: String,
    /// UUID returned by `list_files` or `search_files`.
    file_id: String,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchFilesRequest {
    workspace_id: String,
    /// Case-insensitive filename fragment.
    query: String,
    /// At most 200. Defaults to 50.
    limit: Option<usize>,
}
/// One typed attribute filter, in the shape `read_document` already reports
/// values in.
///
/// **`kinds` is required and is not a hint.** A stored value is compared only
/// against a filter that names its kind, which is what keeps the three
/// temporal types apart: a `date` due and a `datetime_zoned` due are different
/// questions, and a window naming one does not answer the other. Name every
/// kind the question covers — a due window usually means both.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AttrFilterArg {
    /// `koru.due`, `koru.priority`, `koru.color`, or a workspace's own key.
    key: String,
    /// `is`, `one_of`, `before`, `after`, `between`, `exists`, `missing`.
    op: String,
    /// Empty for `exists`/`missing`, one for `is`/`before`/`after`, two for
    /// `between`, any number for `one_of`.
    #[serde(default)]
    values: Vec<String>,
    /// The same operands again in **epoch milliseconds**, for the kinds that
    /// compare as numbers. Same count and same order as `values`.
    ///
    /// Give this whenever `kinds` names `datetime_zoned` beside a text kind: a
    /// calendar day and an instant are compared through different columns, so
    /// `values: ["2026-09-10"]` alone answers for all-day deadlines and
    /// silently omits every timed one.
    #[serde(default)]
    numeric_values: Vec<String>,
    /// `date`, `datetime_floating`, `datetime_zoned`, `number`, `bool`, `ref`,
    /// `url`, `text`, `select`.
    kinds: Vec<String>,
}

/// Turn the tool's filter arguments into wire filters, refusing what cannot
/// mean anything.
///
/// **A filter naming no kinds is refused rather than answered.** It would
/// match nothing, and an empty page reads as a fact about the workspace when
/// it is really a fact about the question — an error naming the kinds it could
/// have used is something an agent can act on; a silence is not.
fn attribute_filters(args: &[AttrFilterArg]) -> Result<Vec<AttrFilter>, ToolError> {
    args.iter()
        .map(|filter| {
            if filter.kinds.is_empty() {
                return Err(ToolError::invalid(format!(
                    "attribute filter on {} names no kinds; say which of date, \
                     datetime_floating, datetime_zoned, number, bool, ref, url, text \
                     or select it applies to",
                    filter.key,
                )));
            }
            let op = match filter.op.as_str() {
                "is" => AttrOp::Is,
                "one_of" => AttrOp::OneOf,
                "before" => AttrOp::Before,
                "after" => AttrOp::After,
                "between" => AttrOp::Between,
                "exists" => AttrOp::Exists,
                "missing" => AttrOp::Missing,
                other => {
                    return Err(ToolError::invalid(format!(
                        "unknown attribute operator {other}; use is, one_of, before, \
                         after, between, exists or missing"
                    )));
                }
            };
            Ok(AttrFilter {
                key: filter.key.clone(),
                op,
                values: filter.values.clone(),
                numeric_values: filter.numeric_values.clone(),
                kinds: filter.kinds.clone(),
            })
        })
        .collect()
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SearchDocumentsRequest {
    /// Text to find in document titles, blocks, attributes, and breadcrumbs.
    query: String,
    /// Workspaces to search. Empty means every workspace authorized for this caller.
    #[serde(default)]
    workspace_ids: Vec<String>,
    /// Optional Koru block-kind filters such as paragraph, task, or code.
    #[serde(default)]
    kinds: Vec<String>,
    /// Live documents by default; trash or all may be requested explicitly.
    #[serde(default)]
    lifecycle: LifecycleFilter,
    /// Relevance by default; observed_modified or last_used_workspace are also available.
    #[serde(default)]
    sort: SearchSort,
    /// Number of indexed candidates to inspect, clamped to 1..=200.
    page_size: Option<usize>,
    /// Candidate offset returned by the previous page.
    offset: Option<usize>,
    /// Tag **names**, not ids. A name matches its descendants: `fun` matches
    /// `fun/home`.
    #[serde(default)]
    tags: Vec<String>,
    /// Attribute filters. Filter on a definition's key; do not guess one.
    #[serde(default)]
    attributes: Vec<AttrFilterArg>,
}
/// What `list_tasks` accepts — the useful subset of the store's `TaskQuery`.
///
/// **A subset, deliberately.** The wire query also carries `has`, `missing`,
/// `tags`, an exact `priority`, `group_by` and `overdue`; those exist for a
/// Tasks *view* drawing rails and counts, and an agent asking *what is
/// outstanding* has `search_documents` for the tag and key questions it can
/// already ask. Every field here is optional and absence means no constraint.
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListTasksRequest {
    /// Workspaces to list from. Empty means every workspace authorized for this
    /// caller.
    #[serde(default)]
    workspace_ids: Vec<String>,
    /// `open` (the default), `completed`, or `all`. A task carrying no stored
    /// state is open, so a due date alone is an open task.
    #[serde(default)]
    lifecycle: TaskLifecycleFilter,
    /// Which deadlines match, stated **per temporal type**: `dates` compares
    /// calendar days and floating wall clocks, `instants` compares zoned
    /// instants in epoch milliseconds. Naming only one leaves the other kind
    /// out, exactly as it does in a `search_documents` attribute filter.
    #[serde(default)]
    due: Option<DueFilter>,
    /// Only tasks whose `koru.priority` is this token or more important, in the
    /// semantic order `low < medium < high < urgent` — never the alphabetic one.
    min_priority: Option<String>,
    /// Only tasks in this list, as the `koru.list` id a row reports. Ids, never
    /// names: renaming a list must not change what a saved query matches.
    list: Option<String>,
    /// Also return tasks sitting in their own document's bin, each flagged with
    /// `in_trash`. Off by default. It does not govern binned *documents*: those
    /// rows always come back, flagged with `document_trashed`.
    #[serde(default)]
    include_trashed: bool,
    /// `due` (the default, soonest first and undated last), `priority`, or
    /// `modified`.
    #[serde(default)]
    sort: TaskSort,
    /// Rows per page, clamped to 1..=500.
    limit: Option<usize>,
    /// The `task:` reference the previous page ended on, from `next_after`.
    /// Keyset paging, never an offset — see `restarted`.
    cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadFileRequest {
    workspace_id: String,
    file_id: String,
    /// Byte offset. Defaults to zero.
    offset: Option<usize>,
    /// Number of bytes, capped at 1 MiB per call.
    length: Option<usize>,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CreateDocumentRequest {
    /// UUID of the workspace returned by `list_workspaces`.
    workspace_id: String,
    /// Text of the document-title root.
    title: String,
    /// Manifest kind. Defaults to `page`.
    kind: Option<String>,
    /// UUID of the document to nest this one under, from `list_documents`.
    /// Absent means the workspace root. Nesting is a manifest relationship
    /// between two documents, not a body edit: a document is never a node
    /// inside another document's tree.
    parent_document_id: Option<String>,
    /// Stable retry key. Reusing it with the same request replays the original
    /// success for seven days; reusing it with different input is an error.
    idempotency_key: Option<String>,
    /// Fresh client-minted UUID for the document-title root. Required when
    /// `initial_operations` is present so every parent reference is explicit.
    root_node_id: Option<String>,
    /// A non-empty atomic seed batch. When present, Koru does not add its
    /// ordinary blank paragraph.
    initial_operations: Option<Vec<InitialOperation>>,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SaveVersionRequest {
    workspace_id: String,
    document_id: String,
    label: Option<String>,
    message: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CompareRequest {
    workspace_id: String,
    left_entry_id: String,
    right_entry_id: String,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RestoreRequest {
    workspace_id: String,
    document_id: String,
    entry_id: String,
    base_revision: String,
}
#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditDocumentRequest {
    /// UUID of the workspace returned by `list_workspaces`.
    workspace_id: String,
    /// UUID of the document returned by `list_documents`.
    document_id: String,
    /// Opaque revision returned by the latest `read_document` call.
    base_revision: String,
    /// Semantic edits applied atomically in this order.
    operations: Vec<EditOperation>,
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ExtractToDocumentRequest {
    /// UUID of the workspace containing both source and destination.
    workspace_id: String,
    /// UUID of the source document returned by `list_documents`.
    source_document_id: String,
    /// Opaque revision returned by the latest source `read_document` call.
    base_revision: String,
    /// UUID of the complete source block subtree to extract.
    source_node_id: String,
    /// UUIDs of further complete subtrees moved with it, in order. One trail
    /// pill replaces the whole set, at `source_node_id`'s position. None may
    /// be an ancestor of another or of `source_node_id`.
    #[serde(default)]
    extra_source_node_ids: Vec<String>,
    /// Exact current text of the `source_node_id` node.
    expected: String,
    /// Title of a **new** top-level Store document to create and move into.
    /// Exactly one of this and `target_document_id`.
    #[serde(default)]
    new_document_title: Option<String>,
    /// UUID of an **existing** destination document; the content lands at its
    /// top. Must differ from `source_document_id`.
    #[serde(default)]
    target_document_id: Option<String>,
    /// UUID of a heading (or any block) in the destination; the content lands
    /// first under it instead of at the top, and the trail pill deep-links to
    /// it. Only meaningful with `target_document_id`; silently falls back to
    /// the top when it no longer resolves.
    #[serde(default)]
    under_node_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum InlineValue {
    /// A `/document` pill pointing at a real Store document.
    Document {
        /// UUID returned by `list_documents` or `create_document`.
        document_id: String,
        /// Optional UUID of one block inside that document, to open it at.
        ///
        /// **Caller-asserted, and deliberately not validated.** Checking that
        /// the node lives in that document would mean loading the document's
        /// CRDT state, which this context does not have; and an unresolvable
        /// node degrades to opening the document, which is what a stale
        /// reference already promises.
        node_id: Option<String>,
        /// Optional fallback caption used while the target is not loaded.
        label: Option<String>,
    },
    /// A routed attachment pill pointing at a first-class workspace file.
    Attachment {
        /// UUID returned by `list_files` or `search_files`.
        file_id: String,
        /// Optional fallback caption; defaults to the stored filename.
        label: Option<String>,
    },
}

/// A typed attribute value, in **exactly the shapes `read_document` emits**.
///
/// That symmetry is the contract, not a coincidence: a tool that reads a due
/// date off one task can write the same JSON onto another without knowing what
/// a `ZonedDateTime` is. Every variant here is one of `attr_json`'s, spelled
/// the same way, and `attribute_round_trips_through_read_and_write` holds the
/// two functions against each other.
///
/// The three temporal shapes are three types and never one with an optional
/// zone — see [`docs/architecture/TIME.md`](../../../docs/architecture/TIME.md) and ADR-183. The two
/// shapes `read_document` emits that are **not** here, `document` and `ref`,
/// are inline objects: they are written with `insert_inline`, which builds the
/// anchor and the mark that make them one semantic thing.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum AttributeValue {
    /// A calendar date, strictly `YYYY-MM-DD`. Never an instant: `2026-08-30`
    /// is August 30 in every timezone.
    Date {
        value: String,
    },
    /// A wall clock with no zone, `YYYY-MM-DDTHH:MM:SS`. What a **personal**
    /// timed value takes — it follows the person rather than a place.
    DatetimeFloating {
        wall: String,
    },
    /// An instant plus the zone it was meant in. What a **shared** timed value
    /// such as `koru.due` takes, so collaborators agree on one moment.
    DatetimeZoned {
        /// Epoch milliseconds, UTC.
        instant_ms: i64,
        /// An IANA zone name (`Europe/Zurich`), never a `+02:00` offset: a
        /// stored offset is a wrong answer after the next daylight change.
        timezone: String,
    },
    /// Free text — and also the form a `select` token (`high`, `teal`) and a
    /// `koru.tag` value (a tag id) take.
    Text {
        value: String,
    },
    Number {
        value: f64,
    },
    Bool {
        value: bool,
    },
    Url {
        value: String,
    },
}

impl AttributeValue {
    fn to_core(&self) -> Result<AttrValue, ToolError> {
        Ok(match self {
            Self::Date { value } => AttrValue::Date(
                DateOnly::parse(value)
                    .ok_or_else(|| ToolError::invalid("a date value must be exactly YYYY-MM-DD"))?,
            ),
            Self::DatetimeFloating { wall } => {
                AttrValue::FloatingDateTime(FloatingDateTime::parse(wall).ok_or_else(|| {
                    ToolError::invalid("a datetime_floating wall must be YYYY-MM-DDTHH:MM:SS")
                })?)
            }
            Self::DatetimeZoned {
                instant_ms,
                timezone,
            } => AttrValue::ZonedDateTime(ZonedDateTime::new(
                *instant_ms,
                TimeZoneName::parse(timezone).ok_or_else(|| {
                    ToolError::invalid(
                        "a datetime_zoned timezone must be an IANA zone name, never an offset",
                    )
                })?,
            )),
            Self::Text { value } => AttrValue::Text(value.clone()),
            Self::Number { value } => AttrValue::Number(*value),
            Self::Bool { value } => AttrValue::Bool(*value),
            Self::Url { value } => AttrValue::Url(value.clone()),
        })
    }
}

/// A field that is **absent**, **null** or **a value**, which `Option<T>` alone
/// cannot tell apart: serde reads absent and null both as `None`. On a field
/// that also carries `#[serde(default)]`, absent stays `None` and anything
/// present is wrapped: `Some(None)` for `null`, `Some(Some(value))` otherwise.
/// Used where leaving a field alone and restoring its default are different
/// requests, as they are for a callout's icon and color.
fn set_or_clear<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum EditOperation {
    /// Replace all text in one existing node. `expected` protects against
    /// overwriting a concurrent edit even within the requested base revision.
    ReplaceText {
        /// UUID of the existing node to edit.
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
        /// Complete replacement text.
        text: String,
    },
    /// Insert a semantic inline object into an existing text node. Koru builds
    /// the CRDT anchor, mark and attribute; callers never send those internals.
    InsertInline {
        /// UUID of the existing text node that will contain the inline object.
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
        /// UTF-8 byte boundary in `expected` at which to insert the object.
        at_utf8: usize,
        /// The semantic inline value. Document references are supported first.
        inline: InlineValue,
    },
    /// Insert one new text-shaped node as a child of an existing node.
    Insert {
        /// Fresh client-minted UUID for the new node.
        node_id: String,
        /// UUID of the existing parent node. This is required: the absolute
        /// CRDT root is not a document body. For body paragraphs, use the
        /// document-title node's `node_id` from `read_document`.
        #[schemars(
            description = "Required UUID of the existing parent node. The absolute CRDT root is not a document body; for body paragraphs, use the document-title node_id returned by read_document."
        )]
        parent_id: String,
        /// UUID of an existing sibling under `parent_id`. The new node is
        /// inserted immediately after it. Omit or use null to prepend. Numeric
        /// ordinals are deliberately unsupported because concurrent edits
        /// make remote positions unstable.
        #[schemars(
            description = "Existing sibling UUID under parent_id; insert immediately after it. Omit or use null to prepend. Numeric ordinals are deliberately unsupported because concurrent edits make remote positions unstable."
        )]
        after_node_id: Option<String>,
        /// Supported document-body node kind. The JSON schema enumerates every
        /// accepted spelling so clients never have to probe with bad values.
        kind: InsertKind,
        /// Initial complete text of the new node.
        text: String,
    },
    /// Move one existing node beneath another existing node.
    Move {
        /// UUID of the node to move.
        node_id: String,
        /// Exact current text returned by `read_document`. This prevents a
        /// concurrent split from leaving the UUID alive on the wrong half and
        /// moving content other than the block the caller read.
        expected: String,
        /// UUID of the destination parent. Required; moving content to the
        /// absolute CRDT root is not a valid document edit.
        #[schemars(
            description = "Required UUID of the destination parent. Moving content to the absolute CRDT root is not a valid document edit."
        )]
        parent_id: String,
        /// UUID of an existing sibling under `parent_id`; place the moved node
        /// immediately after it. Omit or use null to make it the first child.
        #[schemars(
            description = "Existing sibling UUID under parent_id; move immediately after it. Omit or use null to make the node the first child. Numeric ordinals are unsupported."
        )]
        after_node_id: Option<String>,
    },
    /// Remove one existing non-title node and its descendants.
    Remove {
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
    },
    /// Set the completion state of an existing task-like node.
    SetDone {
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
        done: bool,
    },
    /// Set the grammar a code block highlights as (`"rust"`, `"py"`, …).
    /// `null` restores auto-detect. The target must be a code block.
    /// `"mermaid"` makes the block a diagram, drawn from its text; a read then
    /// carries `diagram`, whether it draws (`MCP.md`, Diagrams).
    SetLanguage {
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
        language: Option<String>,
    },
    /// Set a callout box's look: the emoji beside its first line and the
    /// accent color of its tint. Both live on the box, so `node_id` is the node
    /// `read_document` reports as `"kind": "callout"`, never one of its lines,
    /// and `expected` is its text, which is empty.
    ///
    /// Each field stands alone: leave it out to leave it as it is, send `null`
    /// to restore the default, send a value to set it. At least one is
    /// required. `icon` is one emoji. `color` is `red`, `orange`, `yellow`,
    /// `green`, `teal`, `blue`, `purple`, `pink` or `gray` — what `read_document`
    /// reports as `metadata.colour` — and `colour` is accepted as its spelling.
    /// A target that is not a callout, a value that is not one emoji and a
    /// color that is not one of the nine are `invalid_request`, and nothing is
    /// written.
    SetCalloutLook {
        node_id: String,
        /// Exact current text returned by `read_document`: empty for a callout.
        expected: String,
        /// One emoji; `null` restores the default icon.
        #[serde(
            default,
            deserialize_with = "set_or_clear",
            skip_serializing_if = "Option::is_none"
        )]
        icon: Option<Option<String>>,
        /// An accent token; `null` restores the default tint.
        #[serde(
            default,
            alias = "colour",
            deserialize_with = "set_or_clear",
            skip_serializing_if = "Option::is_none"
        )]
        color: Option<Option<String>>,
    },
    /// Set one typed attribute on an existing node — a due date, a priority,
    /// an accent color, or a custom key this workspace defines.
    ///
    /// The value shapes are the ones `read_document` reports, so a value read
    /// from one node can be written to another unchanged. The key's definition
    /// decides what fits: `koru.due` is shared and takes a date or a zoned
    /// datetime, `koru.priority` and `koru.color` take one of their tokens, and
    /// a custom key takes whatever its definition declares. A value that does
    /// not fit is `attribute_rejected`; a key nothing defines is
    /// `unknown_attribute`. Use `add_tag` rather than setting `koru.tag` by
    /// hand — a tag is named, not spelled as a UUID.
    SetAttribute {
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
        /// The attribute key — the `role` a `read_document` attribute carries.
        /// `koru.due`, `koru.priority`, `koru.color`, `koru.tag`, or a custom
        /// key this workspace has defined.
        key: String,
        value: AttributeValue,
    },
    /// Remove every value under one attribute key from an existing node.
    ///
    /// Clearing a key the node does not carry changes nothing and leaves no
    /// undo step. The key must still be one the workspace defines: a
    /// misspelling is an error, never a successful no-op.
    ClearAttribute {
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
        key: String,
    },
    /// Tag an existing node, **by name**.
    ///
    /// A name is resolved case-insensitively against the workspace tag table
    /// and created when nothing answers — `fun/home` creates `fun` too, so the
    /// parent is a real tag in its own right. Supply `tag_id` instead when a
    /// `koru.tag` value read from another node is already in hand. Exactly one
    /// of the two.
    AddTag {
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
        /// The tag's name; `parent/child` for a path, up to five deep.
        #[serde(default)]
        tag: Option<String>,
        /// The tag's id, as a `koru.tag` attribute value spells it.
        #[serde(default)]
        tag_id: Option<String>,
    },
    /// Untag an existing node. Exactly one of `tag` and `tag_id`, as
    /// `add_tag`.
    ///
    /// A name nothing answers to removes nothing and **creates nothing** — the
    /// tag table is not written on the way to a removal. Only the named tag
    /// goes; the node's other tags are untouched.
    RemoveTag {
        node_id: String,
        /// Exact current text returned by `read_document`.
        expected: String,
        /// The tag's name; `parent/child` for a path.
        #[serde(default)]
        tag: Option<String>,
        /// The tag's id, as a `koru.tag` attribute value spells it.
        #[serde(default)]
        tag_id: Option<String>,
    },
}

/// Which tag an operation names, once the two optional fields have been read.
///
/// **A name, not only a UUID**, because an agent reading a document sees tag
/// *ids* on nodes and has no way to invent one for a tag it wants to apply.
/// The id form is kept because it costs one branch and is the exact thing a
/// `read_document` value hands back.
enum TagSpec<'a> {
    Named(&'a str),
    Id(TagId),
}

impl<'a> TagSpec<'a> {
    fn read(
        op: &'static str,
        tag: Option<&'a String>,
        id: Option<&'a String>,
    ) -> Result<Self, ToolError> {
        match (tag, id) {
            (Some(_), Some(_)) => Err(ToolError::invalid(format!(
                "{op} takes tag or tag_id, never both"
            ))),
            (None, None) => Err(ToolError::invalid(format!(
                "{op} needs a tag name in tag, or a tag id in tag_id"
            ))),
            (Some(name), None) if koru_core::tags::fold_key(name).is_empty() => Err(
                ToolError::invalid(format!("{op} tag is empty once separators are removed")),
            ),
            (Some(name), None) => Ok(Self::Named(name.as_str())),
            (None, Some(id)) => TagId::parse(id)
                .map(Self::Id)
                .ok_or_else(|| ToolError::invalid(format!("{op} tag_id is not a UUID"))),
        }
    }
}

/// Operations meaningful while constructing a previously nonexistent body.
/// Their wire shapes match the corresponding ordinary edit variants exactly.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
enum InitialOperation {
    /// Insert a fresh text-shaped block. `parent_id` names the supplied
    /// document root or a node inserted earlier in this batch.
    Insert {
        node_id: String,
        parent_id: String,
        /// A sibling inserted earlier in this batch; null means first.
        after_node_id: Option<String>,
        kind: InsertKind,
        text: String,
    },
    /// Insert a semantic inline object into a node inserted earlier in this
    /// batch.
    InsertInline {
        node_id: String,
        expected: String,
        at_utf8: usize,
        inline: InlineValue,
    },
    /// Set completion on a task-like node inserted earlier in this batch.
    SetDone {
        node_id: String,
        expected: String,
        done: bool,
    },
    /// Set the grammar of a code block inserted earlier in this batch.
    /// `null` restores auto-detect; `"mermaid"` makes the block a diagram.
    SetLanguage {
        node_id: String,
        expected: String,
        language: Option<String>,
    },
    /// Set the look of a callout inserted earlier in this batch, exactly as
    /// the edit operation of the same name does.
    SetCalloutLook {
        node_id: String,
        expected: String,
        #[serde(
            default,
            deserialize_with = "set_or_clear",
            skip_serializing_if = "Option::is_none"
        )]
        icon: Option<Option<String>>,
        #[serde(
            default,
            alias = "colour",
            deserialize_with = "set_or_clear",
            skip_serializing_if = "Option::is_none"
        )]
        color: Option<Option<String>>,
    },
}

#[derive(Debug, Copy, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(inline)]
#[serde(rename_all = "snake_case")]
enum InsertKind {
    Task,
    Paragraph,
    Quote,
    #[serde(rename = "listblock")]
    ListBlock,
    Table,
    Code,
    Contact,
    Divider,
    /// A callout box: a container with no text of its own. The inserted node
    /// is the box; a non-empty `text` becomes a first paragraph inside it,
    /// and later inserts under it are the rest of the box's lines. It
    /// arrives with the default icon and tint; no operation here sets either
    /// yet (see MCP.md).
    Callout,
}

impl From<InsertKind> for Kind {
    fn from(value: InsertKind) -> Self {
        match value {
            InsertKind::Task => Self::Task,
            InsertKind::Paragraph => Self::Paragraph,
            InsertKind::Quote => Self::Quote,
            InsertKind::ListBlock => Self::ListBlock,
            InsertKind::Table => Self::Table,
            InsertKind::Code => Self::Code,
            InsertKind::Contact => Self::Contact,
            InsertKind::Divider => Self::Divider,
            InsertKind::Callout => Self::Callout,
        }
    }
}

/// What an operation needs that the content document does not hold, resolved
/// once by the adapter before anything is applied.
///
/// Documents and files come from the manifest; tags and custom attribute
/// definitions come from the workspace system document. All of it is read
/// before the validation fork, because `apply_ops` runs twice — once against a
/// fork to validate, once for real — and a resolution that ran inside it would
/// run twice too.
///
/// `attrs` is therefore the one field that can be *replaced* between the two
/// passes: the validation pass sees ids minted on a fork of the system
/// document, and once the batch has been proved [`PendingAttrs::commit`]
/// answers the ids that were actually persisted (Q41).
struct InlineContext {
    profile: CoreProfileId,
    workspace: CoreWorkspaceId,
    documents: HashMap<StoreDocumentId, String>,
    files: HashMap<StoreDocumentId, String>,
    attrs: AttrContext,
}

/// The workspace system document's answers to this batch's questions.
///
/// Empty is the ordinary state: a batch that names no tag and no custom key
/// never opens the system document at all, which is what keeps an ordinary
/// text edit one store round trip.
#[derive(Default, Debug)]
struct AttrContext {
    /// Tag fold key → the record it names. A name absent here named no live
    /// tag, which is a no-op for `remove_tag` and cannot happen for `add_tag`
    /// (which creates).
    tags: HashMap<String, TagId>,
    /// Custom attribute key → its definition. System keys are not in here:
    /// their definitions are this build's, in code.
    definitions: HashMap<String, AttributeDefinition>,
}

impl AttrContext {
    /// The definition behind `key` — the system registry first, because its
    /// semantics are never the workspace's to override.
    fn definition(&self, key: &str) -> Option<AttributeDefinition> {
        koru_core::attributes::system_definition(key).or_else(|| self.definitions.get(key).cloned())
    }

    fn unknown(key: &str) -> ToolError {
        ToolError::from(koru_core::StoreError::UnknownAttribute(key.to_owned()))
    }

    /// The id behind a `TagSpec`. `None` only for a name nothing answers to,
    /// which is a `remove_tag` of a tag that was never there.
    fn tag(&self, spec: &TagSpec<'_>) -> Option<TagId> {
        match spec {
            TagSpec::Id(id) => Some(*id),
            TagSpec::Named(name) => self.tags.get(&koru_core::tags::fold_key(name)).copied(),
        }
    }
}

/// A store session left on the way out, however the way out is reached.
///
/// The system document's session used to be opened and left inside one
/// function; it now spans the validation of a whole batch, so the leave can no
/// longer sit at the end of a body. A guard is what keeps it from being missed
/// on an error path in the middle.
struct SessionGuard(DocumentSession);

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.0.leave();
    }
}

/// What one batch still owes the workspace system document, held until the
/// batch is known good.
///
/// **This is the whole of the Q41 fix.** Tag names have to be resolved before
/// validation, because validation needs the ids; they do not have to be
/// *created* before it. Resolution happens twice — once on a fork whose ids go
/// nowhere, once here — and only the second one reaches a table anybody else
/// can see. A batch refused in between drops this value, and dropping it
/// writes nothing.
enum PendingAttrs<'a> {
    /// Nothing to write: the batch asked the table nothing, or every name it
    /// asked about already had a record. The context handed back beside this
    /// is final.
    Settled,
    /// The workspace has a system document, open and untouched.
    Held {
        session: SessionGuard,
        doc: SystemDoc,
        asked: AttrRequests<'a>,
    },
    /// The workspace has no system document and this batch needs one.
    /// Registering one is a manifest write, so it waits with the tags.
    Absent {
        workspace: StoreWorkspaceId,
        asked: AttrRequests<'a>,
    },
}

impl PendingAttrs<'_> {
    /// Write what the batch asked for, now that the batch has been proved.
    ///
    /// Answers the context the *real* pass must use, because the ids in it are
    /// the ids that were persisted rather than the fork's. `None` means the
    /// context from [`LocalBackend::attr_context`] was already final and
    /// nothing was written here.
    ///
    /// It resolves rather than replaying the fork's ids deliberately:
    /// `create_tag` reuses on a fold-key match, so a tag some other peer
    /// created while this batch was being checked is *answered* here instead
    /// of duplicated.
    fn commit(&mut self, backend: &LocalBackend) -> Result<Option<AttrContext>, ToolError> {
        let (mut session, mut doc, asked) = match std::mem::replace(self, Self::Settled) {
            Self::Settled => return Ok(None),
            Self::Held {
                session,
                doc,
                asked,
            } => (session, doc, asked),
            Self::Absent { workspace, asked } => {
                let (session, doc) =
                    backend.system_document(workspace, true)?.ok_or_else(|| {
                        ToolError::invalid("the workspace system document is missing")
                    })?;
                (SessionGuard(session), doc, asked)
            }
        };
        let (context, created) = asked.resolve(&mut doc, koru_workspace::open::now_ms())?;
        if created {
            backend.persist(&mut session.0, &mut doc)?;
        }
        Ok(Some(context))
    }
}

/// What one batch asks of the workspace system document — read from the
/// operations before the document is opened, so a batch that asks nothing
/// never opens one.
///
/// Split out from [`LocalBackend::attr_context`] so the two decisions that
/// matter — *what does this batch need* and *what does the table answer* —
/// are testable against an in-memory [`SystemDoc`], with no store anywhere
/// near them.
#[derive(Default, Debug, PartialEq)]
struct AttrRequests<'a> {
    /// Named tags, in first-seen order, each paired with whether some
    /// operation needs it to exist afterwards. Folded by tag fold key, so a
    /// batch naming `#Home` and `#home` asks once.
    tags: Vec<(&'a str, bool)>,
    /// Keys the system registry does not answer — the workspace's own.
    keys: Vec<&'a str>,
}

impl<'a> AttrRequests<'a> {
    fn read(operations: &'a [EditOperation]) -> Result<Self, ToolError> {
        let mut asked = Self::default();
        for operation in operations {
            let (op, tag, tag_id, create) = match operation {
                EditOperation::AddTag { tag, tag_id, .. } => ("add_tag", tag, tag_id, true),
                EditOperation::RemoveTag { tag, tag_id, .. } => ("remove_tag", tag, tag_id, false),
                EditOperation::SetAttribute { key, .. }
                | EditOperation::ClearAttribute { key, .. } => {
                    if koru_core::attributes::system_definition(key).is_none()
                        && !asked.keys.contains(&key.as_str())
                    {
                        asked.keys.push(key);
                    }
                    continue;
                }
                _ => continue,
            };
            // Read here as well as in `apply_ops` so a malformed pair is
            // refused before anything is created rather than after.
            if let TagSpec::Named(name) = TagSpec::read(op, tag.as_ref(), tag_id.as_ref())? {
                let fold = koru_core::tags::fold_key(name);
                match asked
                    .tags
                    .iter_mut()
                    .find(|(held, _)| koru_core::tags::fold_key(held) == fold)
                {
                    Some(entry) => entry.1 |= create,
                    None => asked.tags.push((name, create)),
                }
            }
        }
        Ok(asked)
    }

    fn is_empty(&self) -> bool {
        self.tags.is_empty() && self.keys.is_empty()
    }

    fn creates(&self) -> bool {
        self.tags.iter().any(|(_, create)| *create)
    }

    /// Ask a **copy** of the table, so a batch that is only being checked
    /// creates nothing.
    ///
    /// Every answer for a tag that already exists is the real one:
    /// `create_tag` reuses on a fold-key match, and a fork of an
    /// already-folded document folds to itself. An id minted here belongs to
    /// the fork and is thrown away with it — [`Self::resolve`] against the
    /// document the workspace can see is what mints the one that gets stored,
    /// and [`PendingAttrs::commit`] is the only caller of that.
    fn resolve_on_fork(
        &self,
        system: &SystemDoc,
        now_ms: i64,
    ) -> Result<(AttrContext, bool), ToolError> {
        let mut fork = SystemDoc::from_snapshot(&system.export_snapshot()?)?;
        self.resolve(&mut fork, now_ms)
    }

    /// Ask the table. Answers the context and whether anything was written to
    /// the system document — a `false` there is what keeps a batch of pure
    /// lookups from publishing an empty update.
    ///
    /// **This writes**, so on the batch path it is reached only through
    /// [`PendingAttrs::commit`], after validation.
    fn resolve(
        &self,
        system: &mut SystemDoc,
        now_ms: i64,
    ) -> Result<(AttrContext, bool), ToolError> {
        let mut context = AttrContext::default();
        let mut created = false;
        for (name, create) in &self.tags {
            let fold = koru_core::tags::fold_key(name);
            if *create {
                // `create_tag` *is* the lookup: a matching fold key answers the
                // existing record and writes nothing, so reuse and creation are
                // one call and cannot disagree (ADR-185). It also creates the
                // parent records a `/`-path implies.
                let before = system.tags().len();
                let id = system.create_tag(name, None, now_ms)?;
                created |= system.tags().len() != before;
                context.tags.insert(fold, id);
            } else if let Some(entry) = system
                .tags()
                .into_iter()
                .find(|entry| entry.fold_key() == fold)
            {
                context.tags.insert(fold, entry.id);
            }
        }
        for definition in system.definitions() {
            if self.keys.contains(&definition.key.as_str()) {
                context
                    .definitions
                    .insert(definition.key.clone(), definition);
            }
        }
        match self
            .keys
            .iter()
            .find(|key| !context.definitions.contains_key(**key))
        {
            Some(key) => Err(AttrContext::unknown(key)),
            None => Ok((context, created)),
        }
    }
}

impl InlineContext {
    fn routed_document_attr(&self, document: StoreDocumentId, label: &str) -> Attr {
        self.document_reference_attr(document, None, label)
    }

    fn document_reference_attr(
        &self,
        document: StoreDocumentId,
        node: Option<NodeId>,
        label: &str,
    ) -> Attr {
        let core_id = CoreDocumentId::from_uuid(document.as_uuid());
        Attr::labelled(
            "document",
            AttrValue::Document(DocumentReference {
                document: core_id,
                node,
            }),
            label,
        )
    }

    fn document_attr(
        &self,
        document_id: &str,
        node_id: Option<&str>,
        label: Option<&str>,
    ) -> Result<Attr, ToolError> {
        let store_id = document(document_id)?;
        let title = self
            .documents
            .get(&store_id)
            .ok_or_else(|| ToolError::invalid("inline document_id is not in this workspace"))?;
        // Shape is checked even though membership is not: a node id that is not
        // a uuid names nothing at all, and refusing it here is cheaper than a
        // pill that can never resolve.
        let node = node_id
            .map(|raw| {
                NodeId::parse(raw).ok_or_else(|| ToolError::invalid("inline node_id is not a uuid"))
            })
            .transpose()?;
        Ok(self.document_reference_attr(store_id, node, label.unwrap_or(title)))
    }

    fn attachment_attr(&self, file_id: &str, label: Option<&str>) -> Result<Attr, ToolError> {
        let store_id = document(file_id)?;
        let title = self
            .files
            .get(&store_id)
            .ok_or_else(|| ToolError::invalid("inline file_id is not in this workspace"))?;
        let core_id = CoreDocumentId::from_uuid(store_id.as_uuid());
        Ok(Attr::labelled(
            "attachment",
            AttrValue::Ref(Ref::in_doc(
                NodeId::from_uuid(store_id.as_uuid()),
                DocRef::DocumentAt {
                    profile: self.profile,
                    workspace: self.workspace,
                    document: core_id,
                },
            )),
            label.unwrap_or(title),
        ))
    }
}

/// Apply one batch, and answer what task activity it produced.
///
/// The returned [`tasks::Wrapper`] is the batch's accumulated attribution —
/// **MCP's burst is the call** — and it is delivered by the caller *after* the
/// durability wait, never here: an activity line that described bytes the Store
/// had not acknowledged would be a second answer to "did this happen"
/// ([ADR-286]).
///
/// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
/// The node's `koru.priority`, or `None` when it has none — or when the node
/// cannot be read at all, which is the same answer for this purpose: an event
/// is owed only when a value *moved*, and a value nobody can read did not.
fn priority_of(doc: &WorkspaceDoc, id: NodeId) -> Option<koru_core::attributes::Priority> {
    doc.read(id).ok().and_then(|node| node.priority())
}

/// **`task.priority.changed` at the priority door**, when the target is already
/// a task ([ADR-286]'s table, T-0).
///
/// The desktop's identical half is `set_attribute_routed`'s `PriorityOnTask`
/// arm in `koru-editor`; the two read the same before-value, compare it to the
/// same after-value and mint the same three-token `change`, which is what makes
/// this a golden-case comparison rather than a code review.
///
/// Nothing is emitted when the value did not move: the attribute doors are
/// guarded, so re-asserting a priority writes no op, and an event for a write
/// that did not happen is a lie about the document.
///
/// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
fn note_priority_changed(
    wrapper: &mut tasks::Wrapper,
    doc: &WorkspaceDoc,
    id: NodeId,
    before: Option<koru_core::attributes::Priority>,
) {
    let after = priority_of(doc, id);
    if before == after {
        return;
    }
    // A document with no workspace identity cannot be addressed as a task, and
    // a reference missing its first segment is not one — the same read every
    // command's own preflight makes first.
    let Some(task) = koru_core::tasks::refs::task_ref(doc, id) else {
        return;
    };
    wrapper.note_intent(koru_core::tasks::ActivityIntent::PriorityChanged {
        task,
        change: match (before, after) {
            (_, None) => koru_core::tasks::AttrChange::Cleared,
            (None, Some(_)) => koru_core::tasks::AttrChange::Set,
            (Some(_), Some(_)) => koru_core::tasks::AttrChange::Changed,
        },
        priority: after,
    });
}

fn apply_ops(
    doc: &mut WorkspaceDoc,
    operations: &[EditOperation],
    inline_context: &InlineContext,
) -> Result<tasks::Wrapper, ToolError> {
    let mut wrapper = tasks::Wrapper::new();
    for operation in operations {
        match operation {
            EditOperation::ReplaceText {
                node_id,
                expected,
                text,
            } => {
                let id = node(node_id)?;
                if doc.read(id)?.content.as_text() != Some(expected.as_str()) {
                    return Err(ToolError {
                        code: "precondition_failed".into(),
                        message: "replace_text expected value does not match".into(),
                        details: None,
                    });
                }
                doc.set_text(id, text)?;
                // **A task's own text moved** (ADR-273, T-0). Asked only when
                // the text actually differs: re-asserting the value a node
                // already has writes no op, and an event saying somebody was
                // editing when nobody was is worse than none. The wrapper
                // dedupes, so a batch that rewrites one task forty times is
                // still one event — MCP's burst is the call.
                if text != expected {
                    wrapper.note_content_changed(doc, id)?;
                }
            }
            EditOperation::InsertInline {
                node_id,
                expected,
                at_utf8,
                inline,
            } => {
                let id = node(node_id)?;
                let Some(current) = doc.read(id)?.content.as_text().map(str::to_owned) else {
                    return Err(ToolError::invalid(
                        "insert_inline target is not a text-shaped node",
                    ));
                };
                if current != *expected {
                    return Err(ToolError {
                        code: "precondition_failed".into(),
                        message: "insert_inline expected value does not match".into(),
                        details: None,
                    });
                }
                if *at_utf8 > current.len() || !current.is_char_boundary(*at_utf8) {
                    return Err(ToolError::invalid(
                        "at_utf8 must be a UTF-8 byte boundary within expected",
                    ));
                }
                let attr = match inline {
                    InlineValue::Document {
                        document_id,
                        node_id,
                        label,
                    } => inline_context.document_attr(
                        document_id,
                        node_id.as_deref(),
                        label.as_deref(),
                    )?,
                    InlineValue::Attachment { file_id, label } => {
                        inline_context.attachment_attr(file_id, label.as_deref())?
                    }
                };
                doc.insert_pill(id, *at_utf8, &attr)?;
                // A pill is an anchor character in the row's text, so this is
                // that row's own text changing — the same note the
                // `replace_text` arm makes, and unconditional because an insert
                // always changes something.
                wrapper.note_content_changed(doc, id)?;
            }
            EditOperation::Insert {
                node_id,
                parent_id,
                after_node_id,
                kind,
                text,
            } => {
                let kind = Kind::from(*kind);
                // A callout is a container with no text of its own: its lines
                // are its children (MCP.md, "A callout is a box"). One `insert`
                // still makes a one-line callout — the box, and when `text` is
                // non-empty, a first paragraph inside it — so `node_id` names
                // the box, which is what the caller asked for and what later
                // inserts use as `parent_id`.
                let (mut value, first_line) = if kind == Kind::Callout {
                    let line = (!text.is_empty())
                        .then(|| Node::paragraph(text.as_str()))
                        .transpose()?;
                    (Node::callout(None, None)?, line)
                } else {
                    (Node::new(kind, Content::text(text))?, None)
                };
                value.id = node(node_id)?;
                let parent = parent(parent_id)?;
                let index = after_index(doc, parent, after_node_id.as_deref())?;
                let id = doc.insert(parent, value)?;
                doc.move_node(id, parent, index)?;
                if let Some(line) = first_line {
                    doc.insert(Parent::Node(id), line)?;
                }
                // **And what it was created in** (D-2c, SPEC §5(d), ADR-272).
                // D-2b wired the arrival half of `Move` and parked this arm as
                // "a separate question nobody has asked yet"; the answer is the
                // same one, told as a creation rather than a move. An agent
                // inserting an unchecked item into a completed task's checklist
                // reopens it, reason `UNFINISHED_CHILD_ADDED`, exactly as
                // pressing Enter there does — SPEC §2's same-commands
                // invariant, and the sort of gap that is invisible until the
                // two surfaces are compared.
                //
                // `reclassify` rather than `reclassify_moved`: nothing moved, so
                // this is `task.child.added`. There is no departure half — a
                // node that has just been created was nowhere before. Asked
                // after the insert has landed and only when it did, since a
                // refused `insert` or `move_node` leaves this arm by `?`.
                wrapper.run_optional(doc, |ws| koru_core::tasks::reclassify(ws, id))?;
            }
            EditOperation::Move {
                node_id,
                expected,
                parent_id,
                after_node_id,
            } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "move")?;
                if doc.parent(id)? == Parent::Root && doc.read(id)?.kind == Kind::Document {
                    return Err(ToolError::invalid(
                        "the document title root cannot be moved",
                    ));
                }
                let parent = parent(parent_id)?;
                let index = move_after_index(doc, id, parent, after_node_id.as_deref())?;
                // **What this node would leave, asked before it moves** (T-0c,
                // ADR-273's `task.child.removed`). It is the one task question
                // a move cannot answer afterwards: the ancestry that answers it
                // is exactly what the move takes away. A move within one task —
                // a reorder, a nesting deeper inside it — answers `None` below
                // and owes nothing.
                let departing = koru_core::tasks::departing(doc, id)?;
                doc.move_node(id, parent, index)?;
                // **And what it arrived in** (D-2b, SPEC §5, ADR-272). A move
                // is a structural user command wherever it comes from, so
                // putting unfinished work under a completed task reopens it,
                // reason `UNFINISHED_CHILD_ADDED` — the same answer the
                // desktop's drag gets from the same function, which is the
                // whole of SPEC §2's same-commands invariant. Told as a *move*
                // (`task.child.moved`), because the classification cannot know
                // which gesture produced it and guessing would file half the
                // feed under the wrong event.
                //
                // Asked **after** the move has landed — the ancestry a node
                // arrived in is still there to read, which is exactly why the
                // departure above had to be asked before it — and only when the
                // move actually happened, since a refused `move_node` leaves
                // this arm by `?`. `reclassify_moved` answers `None` for every
                // ordinary reorder, so nothing above a document with no
                // completed task costs more than one classification.
                //
                // Through `run_optional` rather than `note_intent`: this is a
                // command, so its intents inherit the causation id the outcome
                // minted, and MCP keeps a chain root per op (ADR-273).
                wrapper.run_optional(doc, |ws| koru_core::tasks::reclassify_moved(ws, id))?;
                if let Some(departing) = departing
                    && let Some(intent) = departing.moved(doc)?
                {
                    wrapper.note_intent(intent);
                }
            }
            EditOperation::Remove { node_id, expected } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "remove")?;
                if doc.parent(id)? == Parent::Root && doc.read(id)?.kind == Kind::Document {
                    return Err(ToolError::invalid(
                        "the document title root cannot be removed",
                    ));
                }
                // The same question, spent the other way: a delete is
                // unconditional, because there is no chain left to re-read.
                // Asked before the node goes, and only about the node named —
                // its subtree loses a task ancestor that is going with it, and
                // an event filed under a task that no longer exists is noise.
                let departing = koru_core::tasks::departing(doc, id)?;
                doc.delete(id)?;
                if let Some(departing) = departing {
                    wrapper.note_intent(departing.deleted());
                }
            }
            EditOperation::SetDone {
                node_id,
                expected,
                done,
            } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "set_done")?;
                // **On a task, `set_done` means the lifecycle** (ADR-272's
                // reconciliation table, row 9). The desktop's checkbox asks
                // `tasks::checkbox_completes`, because a rich task's box is a
                // distinct gesture SPEC §5 gives a smaller meaning to; MCP has
                // no box, only an operation named `set_done`, whose only
                // sensible reading on a task is completion — so it asks
                // `is_task_node` and runs the command whenever the target is
                // one, children or not. Both surfaces run the same command
                // whenever they mean completion, which is what the correctness
                // invariant asks (SPEC §2).
                //
                // Row 10 is the other half: on a plain checklist item this is
                // the ordinary checkbox door, which never creates a task and
                // never touches a lifecycle.
                if koru_core::tasks::is_task_node(doc, id)? {
                    let reason = if *done {
                        koru_core::tasks::LifecycleReason::ExplicitComplete
                    } else {
                        koru_core::tasks::LifecycleReason::ExplicitReopen
                    };
                    let done = *done;
                    wrapper.run(doc, |ws| {
                        if done {
                            koru_core::tasks::complete_task(
                                ws,
                                id,
                                reason,
                                koru_core::time::DateOnly::new(chrono::Local::now().date_naive()),
                            )
                        } else {
                            koru_core::tasks::reopen_task(ws, id, reason)
                        }
                    })?;
                } else {
                    // `set_checkbox` rather than the raw `set_done` it used to
                    // call: a guarded no-op inside one atomic group, so an
                    // agent re-asserting a value leaves no phantom undo step
                    // for the person at the keyboard.
                    doc.set_checkbox(id, *done)?;
                }
            }
            EditOperation::SetLanguage {
                node_id,
                expected,
                language,
            } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "set_language")?;
                if doc.read(id)?.kind != Kind::Code {
                    return Err(ToolError::invalid(
                        "set_language target must be a code block",
                    ));
                }
                doc.set_language(id, language.as_deref())?;
            }
            EditOperation::SetCalloutLook {
                node_id,
                expected,
                icon,
                color,
            } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "set_callout_look")?;
                let current = doc.read(id)?;
                if current.kind != Kind::Callout {
                    return Err(ToolError::invalid(
                        "set_callout_look target must be a callout, the box read_document reports as kind callout",
                    ));
                }
                if icon.is_none() && color.is_none() {
                    return Err(ToolError::invalid(
                        "set_callout_look needs icon, color or both; null restores the default",
                    ));
                }
                // Both values are read before either is written, so a color
                // that is not one of the nine cannot leave the icon changed.
                let icon = icon
                    .as_ref()
                    .map(|icon| callout_icon(icon.as_deref()))
                    .transpose()?;
                let color = color
                    .as_ref()
                    .map(|color| callout_color(color.as_deref()))
                    .transpose()?;
                // A guarded write, like `set_checkbox`: an agent re-asserting
                // the look a box already has leaves no phantom undo step for
                // the person at the keyboard.
                if let Some(icon) = icon
                    && icon != current.meta.icon.as_deref()
                {
                    doc.set_icon(id, icon)?;
                }
                if let Some(color) = color
                    && color != current.meta.colour.as_deref()
                {
                    doc.set_color(id, color)?;
                }
            }
            EditOperation::SetAttribute {
                node_id,
                expected,
                key,
                value,
            } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "set_attribute")?;
                // **The model validates, not this layer.** `set_attribute`
                // and `set_attribute_defined` run the definition's own check
                // — which is where the rule that the three temporal shapes do
                // not substitute for one another actually lives. Re-checking
                // shapes here would be a second authority on the same
                // question, and the two would drift.
                let value = value.to_core()?;
                // The definition is resolved **first**, so a key nothing
                // defines stays a misspelling whatever the routing would have
                // said about it.
                let custom = match koru_core::attributes::system_definition(key) {
                    Some(_) => None,
                    None => Some(
                        inline_context
                            .attrs
                            .definitions
                            .get(key)
                            .ok_or_else(|| AttrContext::unknown(key))?,
                    ),
                };
                // ADR-273's one routing rule, asked in the model rather than
                // restated here. `Refused` — the two command-only keys — falls
                // through to the ordinary door on purpose: that door answers
                // `attribute_rejected` in the model's own words, and refusing
                // again here would be a second authority on one question.
                // Excluded kinds are refused by `route_attribute` itself, as
                // `task_rejected` with `kind_not_capable`.
                let route = koru_core::tasks::route_attribute(doc, id, key)?;
                if matches!(route, koru_core::tasks::AttributeRoute::Due) {
                    // The wire contract is unchanged — this is still
                    // `set_attribute` with `koru.due` — and the routing is
                    // internal: a due *is* a task, so the command writes the
                    // marker in the same atomic group and answers one outcome.
                    let due = due_value(key, value)?;
                    wrapper.run(doc, |ws| koru_core::tasks::set_due(ws, id, due))?;
                } else {
                    // **The priority door** (ADR-273's table: *only when the
                    // target already `is_task`*), which is what
                    // `PriorityOnTask` names. Nothing is routed anywhere — the
                    // write below is the ordinary attribute door, unchanged —
                    // and the only difference is that a task owes an event
                    // afterwards. Priority on a plain node owes nothing, which
                    // is why the route is asked rather than the key.
                    let on_task = matches!(route, koru_core::tasks::AttributeRoute::PriorityOnTask);
                    let before = on_task.then(|| priority_of(doc, id)).flatten();
                    if let Some(def) = custom {
                        doc.set_attribute_defined(id, def, value)?;
                    } else {
                        doc.set_attribute(id, key, value)?;
                    }
                    if on_task {
                        note_priority_changed(&mut wrapper, doc, id, before);
                    }
                }
            }
            EditOperation::ClearAttribute {
                node_id,
                expected,
                key,
            } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "clear_attribute")?;
                // Refused rather than shrugged at: a clear of a key nothing
                // defines is a misspelling, and answering it with the same
                // silence a genuinely absent value gets would hide it.
                if inline_context.attrs.definition(key).is_none() {
                    return Err(AttrContext::unknown(key));
                }
                // The same routing rule, and the same reasons — see the
                // `set_attribute` arm above. `clear_due` **marks first**,
                // inside one atomic group, so taking a deadline away never
                // takes the task with it (SPEC §3).
                let route = koru_core::tasks::route_attribute(doc, id, key)?;
                if matches!(route, koru_core::tasks::AttributeRoute::Due) {
                    wrapper.run(doc, |ws| koru_core::tasks::clear_due(ws, id))?;
                } else {
                    // The priority door's other half — see the `set_attribute`
                    // arm. `before == after` is the guard that matters here:
                    // clearing a priority a task never had writes no op and
                    // owes no event.
                    let on_task = matches!(route, koru_core::tasks::AttributeRoute::PriorityOnTask);
                    let before = on_task.then(|| priority_of(doc, id)).flatten();
                    // `clear_attribute` is already guarded: an absent key
                    // writes no op, so an agent clearing what is not there does
                    // not leave a phantom undo step behind for the person at
                    // the keyboard.
                    doc.clear_attribute(id, key)?;
                    if on_task {
                        note_priority_changed(&mut wrapper, doc, id, before);
                    }
                }
            }
            EditOperation::AddTag {
                node_id,
                expected,
                tag,
                tag_id,
            } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "add_tag")?;
                let spec = TagSpec::read("add_tag", tag.as_ref(), tag_id.as_ref())?;
                let resolved = inline_context.attrs.tag(&spec).ok_or_else(|| ToolError {
                    code: "tag_unavailable".into(),
                    message: "the workspace tag table could not be reached to name that tag".into(),
                    details: None,
                })?;
                doc.add_tag(id, resolved)?;
            }
            EditOperation::RemoveTag {
                node_id,
                expected,
                tag,
                tag_id,
            } => {
                let id = node(node_id)?;
                validate_expected_text(doc, id, expected, "remove_tag")?;
                let spec = TagSpec::read("remove_tag", tag.as_ref(), tag_id.as_ref())?;
                // A name the table has never heard of removes nothing. It is
                // deliberately not an error and deliberately not a create: a
                // removal must not write to the tag table on its way past.
                if let Some(resolved) = inline_context.attrs.tag(&spec) {
                    doc.remove_tag(id, resolved)?;
                }
            }
        }
    }
    Ok(wrapper)
}

fn apply_initial_ops(
    doc: &mut WorkspaceDoc,
    root: NodeId,
    operations: &[InitialOperation],
    inline_context: &InlineContext,
) -> Result<(), ToolError> {
    if operations.is_empty() {
        return Err(ToolError::invalid(
            "initial_operations must be omitted or contain at least one operation",
        ));
    }
    let mut available = HashSet::from([root]);
    let mut inserted = HashSet::new();

    for operation in operations {
        let edit = match operation {
            InitialOperation::Insert {
                node_id,
                parent_id,
                after_node_id,
                kind,
                text,
            } => {
                let id = node(node_id)?;
                if available.contains(&id) {
                    return Err(ToolError::invalid(
                        "initial insert node_id must be fresh within the seed batch",
                    ));
                }
                let parent_id_value = node(parent_id)?;
                if !available.contains(&parent_id_value) {
                    return Err(ToolError::invalid(
                        "initial insert parent_id must name the document root or an earlier insert",
                    ));
                }
                if let Some(after) = after_node_id {
                    let after = node(after)?;
                    if !inserted.contains(&after) {
                        return Err(ToolError::invalid(
                            "initial insert after_node_id must name an earlier insert",
                        ));
                    }
                }
                EditOperation::Insert {
                    node_id: node_id.clone(),
                    parent_id: parent_id.clone(),
                    after_node_id: after_node_id.clone(),
                    kind: *kind,
                    text: text.clone(),
                }
            }
            InitialOperation::InsertInline {
                node_id,
                expected,
                at_utf8,
                inline,
            } => {
                let target = node(node_id)?;
                if !inserted.contains(&target) {
                    return Err(ToolError::invalid(
                        "initial insert_inline node_id must name an earlier insert",
                    ));
                }
                EditOperation::InsertInline {
                    node_id: node_id.clone(),
                    expected: expected.clone(),
                    at_utf8: *at_utf8,
                    inline: inline.clone(),
                }
            }
            InitialOperation::SetDone {
                node_id,
                expected,
                done,
            } => {
                let target = node(node_id)?;
                if !inserted.contains(&target) {
                    return Err(ToolError::invalid(
                        "initial set_done node_id must name an earlier insert",
                    ));
                }
                EditOperation::SetDone {
                    node_id: node_id.clone(),
                    expected: expected.clone(),
                    done: *done,
                }
            }
            InitialOperation::SetLanguage {
                node_id,
                expected,
                language,
            } => {
                let target = node(node_id)?;
                if !inserted.contains(&target) {
                    return Err(ToolError::invalid(
                        "initial set_language node_id must name an earlier insert",
                    ));
                }
                EditOperation::SetLanguage {
                    node_id: node_id.clone(),
                    expected: expected.clone(),
                    language: language.clone(),
                }
            }
            InitialOperation::SetCalloutLook {
                node_id,
                expected,
                icon,
                color,
            } => {
                let target = node(node_id)?;
                if !inserted.contains(&target) {
                    return Err(ToolError::invalid(
                        "initial set_callout_look node_id must name an earlier insert",
                    ));
                }
                EditOperation::SetCalloutLook {
                    node_id: node_id.clone(),
                    expected: expected.clone(),
                    icon: icon.clone(),
                    color: color.clone(),
                }
            }
        };
        // Seeding produces no task activity, and cannot: `InitialOperation`
        // carries no attribute operation at all (there is nothing here that
        // could set a due), and its `set_done` names a node inserted moments
        // ago in this same batch, which is therefore not a task. Should that
        // vocabulary ever grow one, the `create_document` arm has to deliver
        // this wrapper after `seed_document` — the durability point on that
        // path — the way `edit_document` delivers after `persist`.
        let seeded = apply_ops(doc, std::slice::from_ref(&edit), inline_context)?;
        debug_assert!(
            seeded.is_empty(),
            "an initial operation produced task activity nothing delivers",
        );
        if let InitialOperation::Insert { node_id, .. } = operation {
            let id = node(node_id)?;
            available.insert(id);
            inserted.insert(id);
        }
    }
    Ok(())
}

/// Hash the semantic create input, not its incidental JSON spelling or the
/// retry key which indexes the hash. This makes an explicit `kind: "page"`
/// equal to the default and UUID case differences equal to the same identity.
fn create_request_hash(
    request: &CreateDocumentRequest,
    workspace: StoreWorkspaceId,
) -> Result<String, ToolError> {
    let mut value = serde_json::to_value(request).map_err(|error| {
        ToolError::invalid(format!("could not identify create request: {error}"))
    })?;
    let object = value
        .as_object_mut()
        .expect("CreateDocumentRequest serializes as an object");
    object.remove("idempotency_key");
    object.insert("workspace_id".into(), Value::String(workspace.to_storage()));
    object.insert(
        "kind".into(),
        Value::String(request.kind.as_deref().unwrap_or("page").to_owned()),
    );
    normalize_request_uuids(&mut value);
    let bytes = serde_json::to_vec(&value).map_err(|error| {
        ToolError::invalid(format!("could not identify create request: {error}"))
    })?;
    Ok(koru_store_core::checksum(&bytes))
}

fn normalize_request_uuids(value: &mut Value) {
    match value {
        Value::Array(values) => values.iter_mut().for_each(normalize_request_uuids),
        Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(
                    name.as_str(),
                    "workspace_id"
                        | "root_node_id"
                        | "node_id"
                        | "parent_id"
                        | "after_node_id"
                        | "document_id"
                        | "parent_document_id"
                ) && let Value::String(raw) = value
                    && let Ok(id) = uuid::Uuid::parse_str(raw)
                {
                    *raw = id.to_string();
                }
                normalize_request_uuids(value);
            }
        }
        _ => {}
    }
}

/// The deadline a due route hands `set_due`, or the model's own refusal.
///
/// `set_due` takes a `Temporal` because a due **is** one; a value of some other
/// shape never had a chance of being a deadline. Which is not this layer's
/// sentence to compose: the key's definition already decides what fits it — the
/// same authority that refuses a floating wall clock — so ask it, and fall back
/// only if it somehow accepts a value that is not temporal at all.
fn due_value(key: &str, value: AttrValue) -> Result<koru_core::Temporal, ToolError> {
    if let Some(temporal) = value.temporal() {
        return Ok(temporal);
    }
    let why = koru_core::attributes::system_definition(key)
        .and_then(|def| koru_core::attributes::validate(&def, &value).err())
        .unwrap_or_else(|| "must be a date or a zoned date and time".to_owned());
    Err(ToolError::from(koru_core::StoreError::AttributeRejected {
        key: key.to_owned(),
        why,
    }))
}

/// The one emoji a callout's `icon` names, spelled the way Koru stores it, or
/// `None` for the default. Anything else is refused: the box would draw a
/// word beside its first line.
fn callout_icon(icon: Option<&str>) -> Result<Option<&'static str>, ToolError> {
    let Some(icon) = icon else {
        return Ok(None);
    };
    emojis::get(icon.trim())
        .map(|emoji| Some(emoji.as_str()))
        .ok_or_else(|| {
            ToolError::invalid(format!(
                "set_callout_look icon must be one emoji, not {icon:?}; null restores the default"
            ))
        })
}

/// The accent token a callout's `color` names, or `None` for the default tint.
fn callout_color(color: Option<&str>) -> Result<Option<&'static str>, ToolError> {
    let Some(color) = color else {
        return Ok(None);
    };
    koru_core::AccentColor::parse(color)
        .map(|accent| Some(accent.as_str()))
        .ok_or_else(|| {
            let tokens: Vec<_> = koru_core::AccentColor::ALL
                .iter()
                .map(|accent| accent.as_str())
                .collect();
            ToolError::invalid(format!(
                "set_callout_look color must be one of {}, not {color:?}; null restores the default",
                tokens.join(", ")
            ))
        })
}

fn validate_expected_text(
    doc: &WorkspaceDoc,
    id: NodeId,
    expected: &str,
    operation: &str,
) -> Result<(), ToolError> {
    let value = doc.read(id)?;
    let current = value
        .content
        .as_text()
        .ok_or_else(|| ToolError::invalid(format!("{operation} target is not text-shaped")))?;
    if current != expected {
        return Err(ToolError {
            code: "precondition_failed".into(),
            message: format!(
                "{operation} expected value does not match node {}; reread the document and retry",
                id.to_storage()
            ),
            details: Some(json!({"node_id":id.to_storage(), "retry":"read_document"})),
        });
    }
    Ok(())
}

fn after_index(
    doc: &WorkspaceDoc,
    parent: Parent,
    after: Option<&str>,
) -> Result<usize, ToolError> {
    match after {
        None => Ok(0),
        Some(value) => {
            let id = node(value)?;
            if doc.parent(id)? != parent {
                return Err(ToolError::invalid(
                    "after_node_id is not a child of parent_id",
                ));
            }
            Ok(doc.sibling_index(id)? + 1)
        }
    }
}

/// Resolve a sibling anchor in the coordinate system Loro sees *after* the
/// moved node has been lifted out. When a node moves forward among siblings,
/// removing it shifts the anchor left by one; using the pre-move ordinal would
/// place it one row too far down.
fn move_after_index(
    doc: &WorkspaceDoc,
    target: NodeId,
    parent: Parent,
    after: Option<&str>,
) -> Result<usize, ToolError> {
    let Some(value) = after else {
        return Ok(0);
    };
    let anchor = node(value)?;
    if anchor == target {
        return Err(ToolError::invalid(
            "after_node_id cannot name the moved node",
        ));
    }
    if doc.parent(anchor)? != parent {
        return Err(ToolError::invalid(
            "after_node_id is not a child of parent_id",
        ));
    }
    let mut index = doc.sibling_index(anchor)? + 1;
    if doc.parent(target)? == parent && doc.sibling_index(target)? < index {
        index -= 1;
    }
    Ok(index)
}

fn document_json(document: StoreDocumentId, doc: &WorkspaceDoc) -> Result<Value, ToolError> {
    let mut nodes = Vec::new();
    let mut root_node_id = None;
    for (id, depth) in doc.walk_all()? {
        let value = doc.read(id)?;
        let parent = doc.parent(id)?;
        if parent == Parent::Root && value.kind == Kind::Document {
            root_node_id = Some(id.to_storage());
        }
        nodes.push(node_json(
            &value,
            parent,
            doc.sibling_index(id)?,
            depth,
            &doc.runs(id)?,
        ));
    }
    let root_node_id = root_node_id
        .ok_or_else(|| ToolError::invalid("document has no document-title root node"))?;
    Ok(json!({
        "document_id": document.to_storage(),
        "root_node_id": root_node_id,
        "revision":revision(doc),
        // **The effective version, never the recorded one.** A document the
        // store made carries no marker — `WorkspaceDoc::create` is the only
        // thing that wrote one and it never ran for it — so the recorded value
        // is `None`, `null` on the wire, and a required integer in the schema
        // this tool publishes. Strict clients rejected the entire response;
        // permissive ones showed a document whose version was missing. Which
        // documents failed looked random and was not: it was exactly the ones
        // created after the store cutover.
        "schema_version":doc.effective_schema_version(),
        "nodes":nodes
    }))
}

fn diff_json(diff: &koru_history::DocumentDiff) -> Value {
    use koru_history::NodeChange;
    let changes: Vec<Value> = diff.changes.iter().map(|change| match change {
        NodeChange::Added(node) => json!({"type":"added", "after":semantic_node_json(node)}),
        NodeChange::Removed(node) => json!({"type":"removed", "before":semantic_node_json(node)}),
        NodeChange::Moved { before, after } => json!({"type":"moved", "before":semantic_node_json(before), "after":semantic_node_json(after)}),
        NodeChange::Reordered { before, after } => json!({"type":"reordered", "before":semantic_node_json(before), "after":semantic_node_json(after)}),
        NodeChange::Modified(change) => json!({
            "type":"modified",
            "before":semantic_node_json(&change.before),
            "after":semantic_node_json(&change.after),
            "kind_changed":change.kind_changed,
            "content_changed":change.content_changed,
            "metadata_changed":change.metadata_changed,
            "marks_changed":change.marks_changed,
            "text":change.text.iter().map(|span| json!({
                "type":match span.tag { koru_history::DiffTag::Equal => "equal", koru_history::DiffTag::Insert => "insert", koru_history::DiffTag::Delete => "delete" },
                "text":span.text,
            })).collect::<Vec<_>>(),
        }),
    }).collect();
    // `DocumentDiff::unchanged` is a **count of nodes**, and it was published
    // under a name that reads as a yes/no and a schema that declared one. A
    // number arrived where the contract promised a boolean and every comparison
    // was rejected. The count is the useful answer — a diff of four changes over
    // two hundred untouched blocks is a different thing from a diff of four over
    // five — so the name moved to match the value rather than the other way.
    json!({"unchanged_nodes":diff.unchanged, "changes":changes})
}

fn semantic_node_json(node: &koru_history::SemanticNode) -> Value {
    node_json(
        &node.node,
        node.parent,
        node.ordinal,
        node.depth,
        &node.runs,
    )
}

fn node_json(
    node: &Node,
    parent: Parent,
    ordinal: usize,
    depth: usize,
    runs: &[koru_core::TextRun],
) -> Value {
    let content = match &node.content {
        Content::Text { text, attrs } => json!({
            "type":"text",
            "text":text,
            "attributes":attrs.iter().map(attr_json).collect::<Vec<_>>(),
            "runs":runs.iter().map(run_json).collect::<Vec<_>>(),
        }),
        Content::Blob {
            hash,
            mime,
            width,
            height,
            ..
        } => json!({"type":"blob", "hash":&hash.0, "mime":mime, "width":width, "height":height}),
        Content::Embed { target } => json!({"type":"embed", "target":ref_json(target)}),
    };
    let mut value = json!({
        "node_id":node.id.to_storage(),
        "parent_id":match parent { Parent::Root => None, Parent::Node(p) => Some(p.to_storage()) },
        "ordinal":ordinal,
        "depth":depth,
        "kind":node.kind.as_str(),
        "metadata":{
            "icon":node.meta.icon,
            "name":node.meta.name,
            "level_name":node.meta.level_name,
            "colour":node.meta.colour,
            "done":node.meta.done,
            "align":node.meta.align.map(|value| value.as_str()),
            "heading":node.meta.heading.map(|value| value.level()),
            "list_style":node.meta.list_style.map(|value| value.as_str()),
            "image_size":node.meta.image_size.map(|(width, height)| json!({"width":width,"height":height})),
            "language":node.meta.language,
            "table_layout":node.meta.table_layout.map(|value| value.as_str()),
            "trashed_at":node.meta.trashed_at,
            "origin":node.meta.origin.map(|value| value.to_storage()),
            "trashed_by":node.meta.trashed_by,
            "restored_by":node.meta.restored_by,
        },
        "content":content,
    });
    if let Some(diagram) = diagram_json(node) {
        value["diagram"] = diagram;
    }
    value
}

/// The `language` of a code block that is a diagram. Compared exactly, case
/// included, because it is what `set_language` stores and nothing normalises
/// it on the way in.
const MERMAID_LANGUAGE: &str = "mermaid";

/// The `diagram` field of a `mermaid` code block: whether its text draws, in
/// the shape `koru_diagram::Status` serialises to — `draws` with its counts,
/// or `not_parsed` at a line, `kind_not_drawn`, `too_large` or `empty`
/// (`MCP.md`, Diagrams). `None` for every other node, so the field is absent
/// rather than `null` where it does not apply.
///
/// It is a parse and a count against the default limits, never a layout,
/// which is what makes it affordable once per mermaid block on every read.
/// `node_json` serves `read_document`, `create_document`'s result and the
/// nodes of a `compare_document_versions` change, and each of them gets the
/// field for the same reason: an agent that wrote a diagram learns whether it
/// drew from the read it was going to make anyway. Serialising a `Status`
/// cannot fail, so the `ok()` is a type conversion, not a swallowed error.
fn diagram_json(node: &Node) -> Option<Value> {
    if node.kind != Kind::Code || node.meta.language.as_deref() != Some(MERMAID_LANGUAGE) {
        return None;
    }
    let Content::Text { text, .. } = &node.content else {
        return None;
    };
    serde_json::to_value(koru_diagram::inspect(
        text,
        &koru_diagram::Limits::default(),
    ))
    .ok()
}

/// One page of `list_tasks`, from the store's answer.
///
/// **`projected` is not republished, because it is `updating_workspaces` said
/// twice** — the store computes it as `updating.is_empty()`. Two spellings of
/// one fact in one response is the drift the wire's own `tags`/`groups` rule
/// avoids, and a client that wants the boolean can read the list it is derived
/// from.
fn task_page_json(response: TaskQueryResponse) -> Value {
    json!({
        "tasks": response.rows.iter().map(task_row_json).collect::<Vec<_>>(),
        "updating_workspaces": response
            .updating_workspaces
            .iter()
            .map(|workspace| workspace.to_storage())
            .collect::<Vec<_>>(),
        "next_after": response.next_after,
        "restarted": response.restarted,
    })
}

/// One task row.
///
/// **The triple is spelled out beside the `task:` string.** The reference is
/// the row's identity, its pagination cursor and its activity subject; the
/// three ids are what `read_document` and `edit_document` take. Publishing only
/// the reference would make every follow-up call begin by parsing a string this
/// surface documents nowhere else.
fn task_row_json(row: &koru_store_wire::tasks::TaskRow) -> Value {
    let value_of = |key: &str| row.attributes.iter().find(|attr| attr.role == key);
    json!({
        "task": row.task,
        "workspace_id": row.workspace.to_storage(),
        "document_id": row.document.to_storage(),
        "node_id": row.node,
        "node_kind": row.node_kind,
        "title": row.title,
        "breadcrumb": row.breadcrumb,
        "lifecycle": row.lifecycle,
        "due": value_of(koru_core::attributes::KEY_DUE).map(task_due_json),
        "priority": value_of(koru_core::attributes::KEY_PRIORITY).map(|attr| attr.value.clone()),
        "list": value_of(koru_core::attributes::KEY_LIST).map(|attr| attr.value.clone()),
        "document_trashed": row.document_trashed,
        "in_trash": row.in_trash,
    })
}

/// A deadline in **the same three shapes `read_document` publishes and
/// `set_attribute` accepts** (ADR-183), rebuilt from the read model's flat
/// `(kind, value, zone)` triple.
///
/// That equality is the point rather than a nicety: a `due` read here goes
/// straight back into an `edit_document` `set_attribute` with no translation,
/// so an agent that lists a task and moves its deadline never invents a fourth
/// spelling of a date. A kind this function does not know — a `koru.due` that
/// somehow holds text — is published verbatim rather than dropped, because a
/// value nobody can read is still evidence and an absent one is not.
fn task_due_json(attr: &koru_store_wire::attributes::AttributeRowValue) -> Value {
    match attr.kind.as_str() {
        "date" => json!({"type":"date", "value":attr.value}),
        "datetime_floating" => json!({"type":"datetime_floating", "wall":attr.value}),
        "datetime_zoned" => json!({
            "type":"datetime_zoned",
            // The read model stores the instant as decimal epoch-ms text.
            "instant_ms": attr.value.parse::<i64>().ok(),
            "timezone": attr.zone,
        }),
        other => json!({"type":other, "value":attr.value}),
    }
}

fn attr_json(attr: &koru_core::Attr) -> Value {
    let value = match &attr.value {
        koru_core::AttrValue::Date(value) => json!({"type":"date", "value":value.to_string()}),
        koru_core::AttrValue::Document(value) => json!({
            "type":"document",
            "document_id":value.document.to_storage(),
            "node_id":value.node.map(|id| id.to_storage()),
        }),
        koru_core::AttrValue::Ref(value) => json!({"type":"ref", "value":ref_json(value)}),
        koru_core::AttrValue::Url(value) => json!({"type":"url", "value":value}),
        koru_core::AttrValue::Text(value) => json!({"type":"text", "value":value}),
        // **Three temporal types, three JSON shapes** (ADR-183). There is no
        // `datetime` here that might carry a zone or might not: a tool reading
        // `datetime_zoned` knows the zone is there, and one reading
        // `datetime_floating` knows there is no zone to look for.
        koru_core::AttrValue::FloatingDateTime(wall) => {
            json!({"type":"datetime_floating", "wall":wall.to_storage()})
        }
        koru_core::AttrValue::ZonedDateTime(zoned) => {
            json!({"type":"datetime_zoned", "instant_ms":zoned.instant_ms(),
                   "timezone":zoned.zone().as_str()})
        }
        koru_core::AttrValue::Number(value) => json!({"type":"number", "value":value}),
        koru_core::AttrValue::Bool(value) => json!({"type":"bool", "value":value}),
    };
    json!({"attribute_id":attr.id.to_storage(), "role":attr.role, "label":attr.label, "value":value})
}

fn ref_json(value: &koru_core::Ref) -> Value {
    use koru_core::DocRef;
    let home = match value.home {
        None => Value::Null,
        Some(DocRef::Profile(id)) => json!({"type":"profile", "profile_id":id.to_storage()}),
        Some(DocRef::Workspace(id)) => json!({"type":"workspace", "workspace_id":id.to_storage()}),
        Some(DocRef::Document(id)) => json!({"type":"document", "document_id":id.to_storage()}),
        Some(DocRef::DocumentAt {
            profile,
            workspace,
            document,
        }) => json!({
            "type":"document_at", "profile_id":profile.to_storage(),
            "workspace_id":workspace.to_storage(), "document_id":document.to_storage(),
        }),
    };
    json!({"node_id":value.node.to_storage(), "home":home})
}

fn run_json(run: &koru_core::TextRun) -> Value {
    json!({
        "start_utf8":run.start.0,
        "end_utf8":run.end.0,
        "text":run.text,
        "bold":run.bold,
        "italic":run.italic,
        "underline":run.underline,
        "strike":run.strike,
        "code":run.code,
        "script":run.style.script.map(|value| value.as_str()),
        "size":run.style.size,
        "highlight":run.style.highlight,
        "pill_attribute_id":run.pill.map(|value| value.to_storage()),
    })
}
fn revision(doc: &WorkspaceDoc) -> String {
    hex(&doc.version().encode())
}
fn validate_base(doc: &WorkspaceDoc, encoded: &str) -> Result<(), ToolError> {
    let bytes = unhex(encoded)?;
    let base = loro::VersionVector::decode(&bytes)
        .map_err(|e| ToolError::invalid(format!("invalid base_revision: {e}")))?;
    if !doc.version().includes_vv(&base) {
        return Err(ToolError {
            code: "revision_conflict".into(),
            message: "base_revision is not in this document's history".into(),
            details: None,
        });
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn unhex(s: &str) -> Result<Vec<u8>, ToolError> {
    if !s.len().is_multiple_of(2) {
        return Err(ToolError::invalid("revision is not hexadecimal"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| ToolError::invalid("revision is not hexadecimal"))
        })
        .collect()
}
fn workspace(s: &str) -> Result<StoreWorkspaceId, ToolError> {
    StoreWorkspaceId::parse(s).ok_or_else(|| ToolError::invalid("workspace_id is not a UUID"))
}
fn document(s: &str) -> Result<StoreDocumentId, ToolError> {
    StoreDocumentId::parse(s).ok_or_else(|| ToolError::invalid("document_id is not a UUID"))
}
fn history(s: &str) -> Result<HistoryEntryId, ToolError> {
    HistoryEntryId::parse(s).ok_or_else(|| ToolError::invalid("entry_id is not a UUID"))
}
fn node(s: &str) -> Result<NodeId, ToolError> {
    NodeId::parse(s).ok_or_else(|| ToolError::invalid("node_id is not a UUID"))
}
fn parent(s: &str) -> Result<Parent, ToolError> {
    NodeId::parse(s)
        .map(Parent::Node)
        .ok_or_else(|| ToolError::invalid("parent_id is not a UUID"))
}
fn from<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ToolError> {
    serde_json::from_value(value).map_err(|e| ToolError::invalid(e.to_string()))
}

/// Return the canonical provider-neutral tool catalog used by MCP and Koru AI.
pub fn workspace_tool_catalog() -> Vec<WorkspaceTool> {
    let mut router = KoruMcp::tool_router();
    published_schema::compact_router(&mut router);
    router
        .list_all()
        .into_iter()
        .map(|tool| {
            let annotations = tool.annotations.unwrap_or_default();
            WorkspaceTool {
                name: tool.name.to_string(),
                title: tool
                    .title
                    .map(|title| title.to_string())
                    .unwrap_or_default(),
                description: tool
                    .description
                    .map(|description| description.to_string())
                    .unwrap_or_default(),
                input_schema: Value::Object(tool.input_schema.as_ref().clone()),
                // No tool publishes an output schema (ADR-433-compacttools);
                // every result is a JSON object, which is all this says.
                output_schema: json!({ "type": "object" }),
                // A read-only tool publishes no destructive or idempotent
                // hint: MCP defines both as meaningful only when
                // `readOnlyHint` is false. Reading changes nothing, so it is
                // neither destructive nor anything but idempotent.
                safety: {
                    let read_only = annotations.read_only_hint.unwrap_or(false);
                    ToolSafety {
                        read_only,
                        destructive: !read_only && annotations.destructive_hint.unwrap_or(true),
                        idempotent: read_only || annotations.idempotent_hint.unwrap_or(false),
                        open_world: annotations.open_world_hint.unwrap_or(false),
                    }
                },
            }
        })
        .collect()
}

#[derive(Clone)]
pub struct KoruMcp {
    executor: WorkspaceToolExecutor,
    context: RequestContext,
    // Read by the `#[tool_handler(router = self.tool_router)]` expansion, which
    // serves tools from this (compacted) router instead of rebuilding one.
    tool_router: ToolRouter<Self>,
    // Whether `tools/list` answers in pages under `tool_pages::PAGE_BYTES`
    // or in one reply (ADR-433-compacttools).
    paged_tools: bool,
}
impl KoruMcp {
    pub fn new(backend: Arc<dyn Backend>, context: RequestContext) -> Self {
        let mut tool_router = Self::tool_router();
        published_schema::compact_router(&mut tool_router);
        Self {
            executor: WorkspaceToolExecutor::new(backend),
            context,
            tool_router,
            paged_tools: PAGED_TOOLS_BY_DEFAULT,
        }
    }

    /// Serve `tools/list` in pages, or in one reply.
    pub fn with_paged_tools(mut self, paged: bool) -> Self {
        self.paged_tools = paged;
        self
    }
    fn call<T: serde::Serialize>(&self, name: &str, value: T) -> CallToolResult {
        let started = Instant::now();
        tracing::info!(tool = name, "tool started");
        let value = serde_json::to_value(value).map_err(|e| ToolError::invalid(e.to_string()));
        // rmcp dispatches this synchronous handler on its Tokio runtime. The
        // local backend deliberately uses the Store's blocking client, so run
        // it on an ordinary scoped thread rather than nesting Reqwest's private
        // runtime inside the transport runtime. A future async cloud backend
        // can replace this adapter without changing the tool contract.
        let result = value.and_then(|value| {
            std::thread::scope(|scope| {
                scope
                    .spawn(|| self.executor.invoke(&self.context, name, value))
                    .join()
                    .unwrap_or_else(|_| {
                        Err(ToolError {
                            code: "internal_error".into(),
                            message: "the Koru backend worker stopped unexpectedly".into(),
                            details: None,
                        })
                    })
            })
        });
        let elapsed = started.elapsed();
        match result {
            // MCP structuredContent is a JSON object. rmcp accepts any
            // serde_json::Value here, which allowed a Store list to escape as
            // a bare array: permissive clients displayed it, while Claude
            // correctly rejected the tools/call response. List tools expose
            // named envelopes; this fallback keeps every future backend and
            // error path protocol-shaped even if one returns a scalar/array.
            Ok(value @ Value::Object(_)) => {
                tracing::info!(
                    tool = name,
                    elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
                    "tool completed"
                );
                CallToolResult::structured(value)
            }
            Ok(value) => {
                tracing::info!(
                    tool = name,
                    elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
                    "tool completed"
                );
                CallToolResult::structured(json!({ "result": value }))
            }
            Err(e) => {
                tracing::warn!(
                    tool = name,
                    code = %e.code,
                    elapsed_ms = elapsed.as_secs_f64() * 1_000.0,
                    error = %e.message,
                    "tool failed"
                );
                let mut error = json!({"code":e.code,"message":e.message});
                if let (Value::Object(object), Some(details)) = (&mut error, e.details) {
                    object.insert("details".into(), details);
                }
                CallToolResult::structured_error(error)
            }
        }
    }
}

#[tool_router]
impl KoruMcp {
    #[tool(
        title = "Read Koru model documentation",
        description = "Read koru://docs/model, the full contract for these tools, when the host cannot read MCP resources. Read it before the first mutation.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn get_documentation(&self, Parameters(a): Parameters<DocumentationRequest>) -> CallToolResult {
        self.call("get_documentation", a)
    }

    #[tool(
        title = "Search Koru documents",
        description = "Search titles, blocks and attribute values in workspace_ids (empty: all). tags are tag names and a parent matches its children; a #tag in query becomes a tag filter. An attributes filter names a key, an op (is, one_of, before, after, between, exists, missing) and every kind it covers (date, datetime_floating, datetime_zoned, number, bool, ref, url, text, select): a window over date alone misses a timed datetime_zoned deadline, so give numeric_values in epoch ms beside values. page_size is at most 200; pass next_offset back as offset. Read koru://docs/model before building a filter.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn search_documents(
        &self,
        Parameters(a): Parameters<SearchDocumentsRequest>,
    ) -> CallToolResult {
        self.call("search_documents", a)
    }
    #[tool(
        title = "List Koru workspaces",
        description = "List the Koru workspaces this principal can reach.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn list_workspaces(&self) -> CallToolResult {
        self.call("list_workspaces", json!({}))
    }
    #[tool(
        title = "List Koru documents",
        description = "List a workspace's documents without opening their content.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn list_documents(&self, Parameters(a): Parameters<WorkspaceRequest>) -> CallToolResult {
        self.call("list_documents", a)
    }
    #[tool(
        title = "List Koru tasks",
        description = "List tasks (nodes with the task marker or a deadline) in workspace_ids (empty: all) from the store's read model: open by default, soonest due first. A row carries its task: reference, the workspace_id, document_id and node_id to edit it by, title, breadcrumb, lifecycle, due, priority and list. due filters per temporal type: dates {after, before} for days and wall clocks, instants {after_ms, before_ms} for zoned deadlines; naming one leaves the other out. min_priority orders low < medium < high < urgent; list is a list id; limit is at most 500. Page with cursor = next_after; restarted means the cursor's row stopped matching and this is page one again; updating_workspaces may be behind. Change a task with edit_document: set_done, or set_attribute of koru.due.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn list_tasks(&self, Parameters(a): Parameters<ListTasksRequest>) -> CallToolResult {
        self.call("list_tasks", a)
    }
    #[tool(
        title = "List Koru files",
        description = "List workspace files with stable ids, names, MIME types, sizes, blob hashes and note ids.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn list_files(&self, Parameters(a): Parameters<WorkspaceRequest>) -> CallToolResult {
        self.call("list_files", a)
    }
    #[tool(
        title = "Search Koru files",
        description = "Search workspace files by a case-insensitive filename fragment. limit is at most 200 (default 50).",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn search_files(&self, Parameters(a): Parameters<SearchFilesRequest>) -> CallToolResult {
        self.call("search_files", a)
    }
    #[tool(
        title = "Get Koru file metadata",
        description = "Get metadata for one workspace file without reading its bytes.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn get_file(&self, Parameters(a): Parameters<FileRequest>) -> CallToolResult {
        self.call("get_file", a)
    }
    #[tool(
        title = "Read a Koru file",
        description = "Read up to 1 MiB of a workspace file as base64 from offset (default 0). Continue from the next offset until eof.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn read_file(&self, Parameters(a): Parameters<ReadFileRequest>) -> CallToolResult {
        self.call("read_file", a)
    }
    #[tool(
        title = "Read a Koru document",
        description = "Read a document as an exhaustive preorder typed-node tree plus the opaque revision token edits need.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn read_document(&self, Parameters(a): Parameters<DocumentRequest>) -> CallToolResult {
        self.call("read_document", a)
    }
    #[tool(
        title = "Create a Koru document",
        description = "Atomically create, and optionally seed, a complete document before it becomes visible; returns it as read_document does. parent_document_id nests it (absent: the workspace root); kind defaults to page. A UUID idempotency_key replays the same request for seven days. With initial_operations (non-empty; insert, insert_inline, set_done, set_language and set_callout_look only) root_node_id is required, a fresh UUID for the title node; every parent is it or an earlier insert, and no blank paragraph is added. Read koru://docs/model before the first mutation.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn create_document(&self, Parameters(a): Parameters<CreateDocumentRequest>) -> CallToolResult {
        self.call("create_document", a)
    }
    #[tool(
        title = "Ensure a Koru file's note",
        description = "Answer a workspace file's one note, creating it when the file has none; created says which happened. The note is an ordinary document for read_document and edit_document, found through the file's note_id and never listed by list_documents.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn ensure_file_note(&self, Parameters(a): Parameters<FileRequest>) -> CallToolResult {
        self.call("ensure_file_note", a)
    }
    #[tool(
        title = "Edit a Koru document",
        description = "Atomically apply edits, in order, at base_revision from the latest read_document. Read koru://docs/model before the first mutation. Each op on an existing node carries expected, its exact current text; on stale_node or a mismatch, reread and retry. insert mints node_id; parent_id is required and body blocks go under the document-title node, never the absolute CRDT root. after_node_id is a sibling and null means first; numeric ordinals are unsupported. at_utf8 is a UTF-8 byte offset into expected. remove takes the node's whole subtree. set_attribute writes koru.due, koru.priority, koru.color or a workspace's custom key in the value shapes read_document reports: date {value: YYYY-MM-DD}, datetime_zoned {instant_ms, timezone: IANA name}, datetime_floating {wall}. add_tag and remove_tag take a tag name or a tag_id. set_callout_look takes an emoji icon and an accent color token; omit one to keep it, null restores the default. Unknown fields are errors.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn edit_document(&self, Parameters(a): Parameters<EditDocumentRequest>) -> CallToolResult {
        self.call("edit_document", a)
    }
    #[tool(
        title = "Extract blocks to a document",
        description = "Move complete block subtrees (source_node_id, then extra_source_node_ids; none an ancestor of another) into exactly one of a new document (new_document_title) or an existing one (target_document_id, at its top or first under under_node_id), leaving one /document link in their place. expected is source_node_id's exact current text and base_revision the source's. Read koru://docs/model before the first mutation.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn extract_to_document(
        &self,
        Parameters(a): Parameters<ExtractToDocumentRequest>,
    ) -> CallToolResult {
        self.call("extract_to_document", a)
    }
    #[tool(
        title = "Save a document version",
        description = "Save a pinned manual version of the current document state. Read koru://docs/model before the first mutation.",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    fn save_document_version(
        &self,
        Parameters(a): Parameters<SaveVersionRequest>,
    ) -> CallToolResult {
        self.call("save_document_version", a)
    }
    #[tool(
        title = "List document versions",
        description = "List durable document history markers, newest first.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn list_document_versions(&self, Parameters(a): Parameters<DocumentRequest>) -> CallToolResult {
        self.call("list_document_versions", a)
    }
    #[tool(
        title = "Compare document versions",
        description = "Compare two durable versions using Koru's typed node model, not Markdown.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn compare_document_versions(
        &self,
        Parameters(a): Parameters<CompareRequest>,
    ) -> CallToolResult {
        self.call("compare_document_versions", a)
    }
    #[tool(
        title = "Restore a document version",
        description = "Restore a durable version at base_revision by creating forward-moving CRDT operations, with paired history boundaries. Read koru://docs/model before the first mutation.",
        annotations(
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn restore_document_version(
        &self,
        Parameters(a): Parameters<RestoreRequest>,
    ) -> CallToolResult {
        self.call("restore_document_version", a)
    }

    #[tool(
        title = "List Koru AI chats",
        description = "List chats visible to connected assistants, newest first. Private chats are omitted.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn list_chats(&self, Parameters(a): Parameters<ListChatsToolRequest>) -> CallToolResult {
        self.call("list_chats", a)
    }

    #[tool(
        title = "Search Koru AI chats",
        description = "Search finalized responses in chats visible to connected assistants. Private chats are omitted and queries are not logged.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn search_chats(&self, Parameters(a): Parameters<SearchChatsToolRequest>) -> CallToolResult {
        self.call("search_chats", a)
    }

    #[tool(
        title = "Read a Koru AI chat",
        description = "Read a bounded page of terminal messages, newest page first and chronological within each page. Private chats appear not found.",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    fn read_chat(&self, Parameters(a): Parameters<ReadChatToolRequest>) -> CallToolResult {
        self.call("read_chat", a)
    }
}

/// Whether a bridge pages `tools/list` unless told otherwise. Off: Kimi
/// 2.0.0 and Codex 0.156.1 read the first page and never send `nextCursor`
/// back, so paging would hide 17 of 21 tools from both (ADR-433-compacttools).
pub const PAGED_TOOLS_BY_DEFAULT: bool = false;

#[tool_handler(router = self.tool_router)]
impl ServerHandler for KoruMcp {
    async fn list_tools(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, ErrorData> {
        let cursor = request.and_then(|request| request.cursor);
        let tools = self.tool_router.list_all();
        let page = if self.paged_tools {
            tool_pages::page(tools, cursor.as_deref())
        } else if let Some(cursor) = &cursor {
            Err(ErrorData::invalid_params(
                format!("unknown tools/list cursor {cursor:?}; list again without one"),
                None,
            ))
        } else {
            Ok(tool_pages::Page {
                tools,
                next_cursor: None,
            })
        };
        match &page {
            Ok(page) => tracing::info!(
                cursor = cursor.as_deref().unwrap_or(""),
                tools = page.tools.len(),
                next_cursor = page.next_cursor.as_deref().unwrap_or(""),
                "tools/list"
            ),
            Err(_) => tracing::warn!(
                cursor = cursor.as_deref().unwrap_or(""),
                "tools/list refused an unknown cursor"
            ),
        }
        let page = page?;
        // What rmcp's generated `list_tools` sends, beside the page.
        let supports_cache_hints = context
            .protocol_version()
            .is_some_and(|version| version >= rmcp::model::ProtocolVersion::V_2026_07_28);
        let mut result = rmcp::model::ListToolsResult::with_all_items(page.tools);
        result.next_cursor = page.next_cursor;
        result.result_type = Some(rmcp::model::ResultType::COMPLETE);
        result.ttl_ms = supports_cache_hints.then_some(0);
        result.cache_scope = supports_cache_hints.then_some(rmcp::model::CacheScope::Public);
        Ok(result)
    }

    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions("Read koru://docs/model before the first mutation. It is available through standard MCP resources/read or the get_documentation compatibility tool. Read and edit Koru's typed node model, never raw CRDT operations. Always read a fresh revision before mutation; use node ids and after_node_id, never numeric remote positions. On stale_node, reread the document, remap the intended block, and retry. Use insert_inline for /document references and extract_to_document for a cross-document move; never insert kind=document into a body.")
    }

    fn list_resources(
        &self,
        _: Option<rmcp::model::PaginatedRequestParams>,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<ListResourcesResult, ErrorData>> + Send + '_ {
        std::future::ready(Ok(ListResourcesResult::with_all_items(vec![
            model_resource(),
        ])))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<ReadResourceResponse, ErrorData>> + Send + '_ {
        let started = Instant::now();
        tracing::info!(uri = %request.uri, "resource read started");
        let result = if request.uri != MODEL_RESOURCE_URI {
            Err(ErrorData::resource_not_found(
                "Koru documentation resource not found",
                Some(json!({ "uri": request.uri })),
            ))
        } else {
            read_model_document()
                .map(|text| {
                    ReadResourceResult::new(vec![
                        ResourceContents::text(text, MODEL_RESOURCE_URI)
                            .with_mime_type(MODEL_DOCUMENT_MIME),
                    ])
                    .into()
                })
                .map_err(|error| {
                    ErrorData::internal_error(
                        error.message,
                        Some(json!({ "uri": MODEL_RESOURCE_URI })),
                    )
                })
        };
        match &result {
            Ok(_) => tracing::info!(
                uri = %request.uri,
                elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                "resource read completed"
            ),
            Err(error) => tracing::warn!(
                uri = %request.uri,
                code = error.code.0,
                elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                error = %error.message,
                "resource read failed"
            ),
        }
        std::future::ready(result)
    }

    fn on_initialized(
        &self,
        context: rmcp::service::NotificationContext<rmcp::RoleServer>,
    ) -> impl Future<Output = ()> + Send + '_ {
        if let Some(info) = context.peer.peer_info() {
            tracing::info!(
                client = %info.client_info.name,
                version = %info.client_info.version,
                "client connected"
            );
        } else {
            tracing::info!("client connected");
        }
        std::future::ready(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::JsonObject;

    struct Fake;
    impl Backend for Fake {
        fn invoke(&self, _: &RequestContext, tool: &str, _: Value) -> Result<Value, ToolError> {
            Ok(json!({"called": tool}))
        }
        fn doctor(&self) -> Result<Value, ToolError> {
            Ok(json!({"ok":true}))
        }
    }

    struct ArrayFake;
    impl Backend for ArrayFake {
        fn invoke(&self, _: &RequestContext, _: &str, _: Value) -> Result<Value, ToolError> {
            Ok(json!([{"id":"one"}]))
        }
        fn doctor(&self) -> Result<Value, ToolError> {
            Ok(json!({"ok":true}))
        }
    }

    #[derive(Default)]
    struct RecordingFake {
        calls: Mutex<Vec<(String, Value)>>,
    }

    impl Backend for RecordingFake {
        fn invoke(
            &self,
            _: &RequestContext,
            tool: &str,
            arguments: Value,
        ) -> Result<Value, ToolError> {
            self.calls
                .lock()
                .unwrap()
                .push((tool.to_owned(), arguments.clone()));
            Ok(match tool {
                "list_workspaces" => json!({
                    "workspaces": [
                        { "id": "11111111-1111-4111-8111-111111111111" },
                        { "id": "22222222-2222-4222-8222-222222222222" },
                    ]
                }),
                "search_documents" => json!({ "arguments": arguments }),
                _ => json!({ "ok": true }),
            })
        }

        fn doctor(&self) -> Result<Value, ToolError> {
            Ok(json!({ "ok": true }))
        }
    }

    #[test]
    fn selected_workspace_authority_is_enforced_before_the_backend() {
        let allowed = StoreWorkspaceId::parse("11111111-1111-4111-8111-111111111111").unwrap();
        let denied = "22222222-2222-4222-8222-222222222222";
        let backend = Arc::new(RecordingFake::default());
        let executor = WorkspaceToolExecutor::new(backend.clone());
        let context =
            RequestContext::selected("local", "koru-ai", [Scope::ReadDocuments], [allowed]);

        let listed = executor
            .invoke(&context, "list_workspaces", json!({}))
            .unwrap();
        assert_eq!(listed["workspaces"].as_array().unwrap().len(), 1);
        assert_eq!(listed["workspaces"][0]["id"], allowed.to_storage());

        let error = executor
            .invoke(
                &context,
                "list_documents",
                json!({ "workspace_id": denied }),
            )
            .unwrap_err();
        assert_eq!(error.code, "permission_denied");
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn missing_scope_is_denied_before_workspace_or_store_access() {
        let workspace = StoreWorkspaceId::new();
        let backend = Arc::new(RecordingFake::default());
        let executor = WorkspaceToolExecutor::new(backend.clone());
        let context =
            RequestContext::selected("local", "koru-ai", std::iter::empty::<Scope>(), [workspace]);

        let error = executor
            .invoke(
                &context,
                "read_document",
                json!({
                    "workspace_id": workspace.to_storage(),
                    "document_id": StoreDocumentId::new().to_storage(),
                }),
            )
            .unwrap_err();
        assert_eq!(error.code, "permission_denied");
        assert!(backend.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn chat_tools_are_available_only_through_the_local_bridge() {
        let backend = Arc::new(RecordingFake::default());
        let executor = WorkspaceToolExecutor::new(backend.clone());
        let selected = RequestContext::selected(
            "local",
            "koru-ai",
            [Scope::ReadDocuments],
            std::iter::empty::<StoreWorkspaceId>(),
        );
        let denied = executor
            .invoke(&selected, "list_chats", json!({}))
            .unwrap_err();
        assert_eq!(denied.code, "permission_denied");
        assert!(backend.calls.lock().unwrap().is_empty());

        let result = executor
            .invoke(&RequestContext::local(), "list_chats", json!({}))
            .unwrap();
        assert_eq!(result, json!({ "ok": true }));
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn selected_search_intersects_scope_and_empty_grants_never_mean_everywhere() {
        let allowed = StoreWorkspaceId::parse("11111111-1111-4111-8111-111111111111").unwrap();
        let denied = "22222222-2222-4222-8222-222222222222";
        let backend = Arc::new(RecordingFake::default());
        let executor = WorkspaceToolExecutor::new(backend.clone());
        let context =
            RequestContext::selected("local", "koru-ai", [Scope::ReadDocuments], [allowed]);

        let result = executor
            .invoke(
                &context,
                "search_documents",
                json!({
                    "query": "roadmap",
                    "workspace_ids": [allowed.to_storage(), denied],
                }),
            )
            .unwrap();
        assert_eq!(
            result["arguments"]["workspace_ids"],
            json!([allowed.to_storage()])
        );

        let no_access = RequestContext::selected(
            "local",
            "koru-ai",
            [Scope::ReadDocuments],
            std::iter::empty::<StoreWorkspaceId>(),
        );
        let empty = executor
            .invoke(
                &no_access,
                "search_documents",
                json!({ "query": "roadmap" }),
            )
            .unwrap();
        assert_eq!(empty, empty_search_result());
        assert_eq!(backend.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn mutations_require_edit_scope_even_inside_an_allowed_workspace() {
        let workspace = StoreWorkspaceId::new();
        let backend = Arc::new(RecordingFake::default());
        let executor = WorkspaceToolExecutor::new(backend.clone());
        let context =
            RequestContext::selected("local", "koru-ai", [Scope::ReadDocuments], [workspace]);

        let error = executor
            .invoke(
                &context,
                "create_document",
                json!({ "workspace_id": workspace.to_storage(), "title": "Denied" }),
            )
            .unwrap_err();
        assert_eq!(error.code, "permission_denied");
        assert!(backend.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn the_official_sdk_exposes_all_twenty_one_tools() {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let names: std::collections::HashSet<_> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        for expected in [
            "get_documentation",
            "list_workspaces",
            "list_documents",
            "search_documents",
            "list_tasks",
            "list_files",
            "search_files",
            "get_file",
            "read_file",
            "read_document",
            "create_document",
            "ensure_file_note",
            "edit_document",
            "extract_to_document",
            "save_document_version",
            "list_document_versions",
            "compare_document_versions",
            "restore_document_version",
            "list_chats",
            "search_chats",
            "read_chat",
        ] {
            assert!(names.contains(expected), "missing MCP tool {expected}");
        }
        assert_eq!(names.len(), 21);
    }

    fn an_arg(key: &str, op: &str, values: &[&str], kinds: &[&str]) -> AttrFilterArg {
        AttrFilterArg {
            key: key.to_owned(),
            op: op.to_owned(),
            values: values.iter().map(|v| (*v).to_owned()).collect(),
            numeric_values: Vec::new(),
            kinds: kinds.iter().map(|k| (*k).to_owned()).collect(),
        }
    }

    /// **A filter that names no kinds is refused, and the refusal says what to
    /// name.** It would match nothing, and an empty page reads as a fact about
    /// the workspace rather than about the question — which is the one failure
    /// an agent cannot debug from the answer it gets.
    #[test]
    fn a_filter_naming_no_kinds_is_refused_with_the_kinds_it_could_name() {
        let error = attribute_filters(&[an_arg("koru.due", "before", &["2026-09-10"], &[])])
            .expect_err("a filter with no kinds asks nothing");
        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains("koru.due"), "it names the key");
        assert!(
            error.message.contains("datetime_zoned"),
            "and the kinds it could have named: {}",
            error.message
        );
    }

    /// An unknown operator is refused by name, with the set that would work —
    /// rather than being silently read as equality.
    #[test]
    fn an_unknown_operator_is_refused_by_name() {
        let error = attribute_filters(&[an_arg("koru.due", "roughly", &["x"], &["date"])])
            .expect_err("there is no `roughly`");
        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains("roughly"));
        assert!(error.message.contains("between"), "{}", error.message);
    }

    /// Every operator the documentation offers translates, so the contract and
    /// the code cannot drift apart without this failing.
    #[test]
    fn every_documented_operator_translates() {
        let ops = [
            ("is", AttrOp::Is),
            ("one_of", AttrOp::OneOf),
            ("before", AttrOp::Before),
            ("after", AttrOp::After),
            ("between", AttrOp::Between),
            ("exists", AttrOp::Exists),
            ("missing", AttrOp::Missing),
        ];
        for (token, expected) in ops {
            let filters = attribute_filters(&[an_arg("k", token, &["a", "b"], &["text"])])
                .unwrap_or_else(|e| panic!("{token} should translate: {}", e.message));
            assert_eq!(filters[0].op, expected, "{token}");
        }
    }

    /// The key, the operands and the kinds cross unchanged — a filter is
    /// carried, not interpreted.
    #[test]
    fn a_filter_crosses_the_boundary_unchanged() {
        let filters =
            attribute_filters(&[an_arg("user.effort", "between", &["1", "5"], &["number"])])
                .unwrap();
        assert_eq!(filters.len(), 1);
        assert_eq!(filters[0].key, "user.effort");
        assert_eq!(filters[0].values, ["1", "5"]);
        assert_eq!(filters[0].kinds, ["number"]);
    }

    /// **The worked example from the documentation is executed here**, so an
    /// example that stops being true fails the build instead of misleading a
    /// model that trusted it: a due window naming both temporal kinds is one
    /// filter, not two, because two would be ANDed and no value is both a
    /// calendar day and an instant.
    #[test]
    fn the_documented_due_window_is_one_filter_naming_both_kinds() {
        let filters = attribute_filters(&[AttrFilterArg {
            key: "koru.due".to_owned(),
            op: "before".to_owned(),
            values: vec!["2026-09-10".to_owned()],
            numeric_values: vec!["1789000000000".to_owned()],
            kinds: vec!["date".to_owned(), "datetime_zoned".to_owned()],
        }])
        .unwrap();
        assert_eq!(filters.len(), 1, "one filter, so the kinds are an OR");
        assert_eq!(filters[0].kinds.len(), 2);
        assert_eq!(
            filters[0].values,
            ["2026-09-10"],
            "the calendar half compares as ISO text"
        );
        assert_eq!(
            filters[0].numeric_values,
            ["1789000000000"],
            "and the instant half needs its own bound, or it drops out"
        );
    }

    /// Every tool has a display title and explicit safety hints. A read-only
    /// tool publishes only `readOnlyHint` and `openWorldHint`: MCP defines
    /// `destructiveHint` and `idempotentHint` as meaningful only when
    /// `readOnlyHint` is false, and the catalog reads their absence that way.
    /// No tool publishes an `outputSchema` (ADR-433-compacttools).
    #[test]
    fn every_tool_publishes_a_title_and_safety_hints_and_no_output_schema() {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let read_only = [
            "get_documentation",
            "list_workspaces",
            "list_documents",
            "search_documents",
            "list_tasks",
            "list_files",
            "search_files",
            "get_file",
            "read_file",
            "read_document",
            "list_document_versions",
            "compare_document_versions",
            "list_chats",
            "search_chats",
            "read_chat",
        ];
        let destructive = [
            "edit_document",
            "extract_to_document",
            "restore_document_version",
        ];
        let non_idempotent = [
            "create_document",
            "extract_to_document",
            "save_document_version",
        ];

        for tool in server.tool_router.list_all() {
            let name = tool.name.as_ref();
            assert!(tool.title.is_some(), "{name} has no display title");
            assert!(
                tool.output_schema.is_none(),
                "{name} publishes an output schema"
            );
            let annotations = tool
                .annotations
                .as_ref()
                .unwrap_or_else(|| panic!("{name} has no annotations"));
            assert_eq!(annotations.title, None, "{name} repeats its title");
            assert_eq!(annotations.open_world_hint, Some(false), "{name}");
            if read_only.contains(&name) {
                assert_eq!(annotations.read_only_hint, Some(true), "{name}");
                assert_eq!(annotations.destructive_hint, None, "{name}");
                assert_eq!(annotations.idempotent_hint, None, "{name}");
            } else {
                assert_eq!(annotations.read_only_hint, Some(false), "{name}");
                assert_eq!(
                    annotations.destructive_hint,
                    Some(destructive.contains(&name)),
                    "{name} destructive hint"
                );
                assert_eq!(
                    annotations.idempotent_hint,
                    Some(!non_idempotent.contains(&name)),
                    "{name} idempotency hint"
                );
            }
        }

        // The provider-neutral catalog reads the same hints the same way.
        for tool in workspace_tool_catalog() {
            let name = tool.name.as_str();
            assert_eq!(tool.safety.read_only, read_only.contains(&name), "{name}");
            assert_eq!(
                tool.safety.destructive,
                destructive.contains(&name),
                "{name} destructive"
            );
            assert_eq!(
                tool.safety.idempotent,
                !non_idempotent.contains(&name),
                "{name} idempotent"
            );
            assert!(!tool.safety.open_world, "{name}");
        }
    }

    /// The bytes the `tools/list` reply may take, newline excluded. Grok CLI
    /// 1.0.34 can lose an 8,192-byte chunk of any longer line
    /// (ADR-433-compacttools); 21 typed tools cannot fit under that, so this
    /// holds what was reached and makes growth a decision rather than a drift.
    ///
    /// Growth decided so far: 17,500 to 17,700 for `set_callout_look`, one edit
    /// operation and one seed operation (ADR-468-callout). The reply was 17,644
    /// bytes with it.
    const TOOLS_LIST_BUDGET: usize = 17_700;

    /// What crosses the wire is compact: no prose inside a schema except the
    /// generated union summaries, no `$ref`, `$defs`, `$schema`, `title` or
    /// integer width, no output schema, and the whole reply within its budget.
    /// Driven through the real `tools/list` path, because an earlier test read
    /// the router field and passed while the handler served a freshly rebuilt
    /// router.
    #[tokio::test]
    async fn the_published_tool_list_is_compact_and_within_its_budget() {
        use rmcp::ServiceExt;

        fn keywords(value: &Value, parent_is_properties: bool, out: &mut Vec<String>) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        if !parent_is_properties {
                            out.push(key.clone());
                        }
                        keywords(child, key == "properties" && !parent_is_properties, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| keywords(item, false, out)),
                _ => {}
            }
        }

        // The whole list in one reply: the budget is on the list, whether or
        // not it is then served in pages.
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local()).with_paged_tools(false);
        let server_task = tokio::spawn(async move { server.serve(server_transport).await });
        let client = ().serve(client_transport).await.expect("MCP initialize handshake");
        let listed = client
            .peer()
            .list_tools(None)
            .await
            .expect("tools/list over stdio");
        server_task.abort();

        assert_eq!(listed.tools.len(), 21);
        assert!(listed.next_cursor.is_none(), "one page when paging is off");
        let line = serde_json::to_string(&json!({"jsonrpc": "2.0", "id": 2, "result": &listed}))
            .expect("reply JSON");
        assert!(
            line.len() < TOOLS_LIST_BUDGET,
            "tools/list is {} bytes, over its {TOOLS_LIST_BUDGET}-byte budget",
            line.len()
        );
        for tool in &listed.tools {
            assert!(tool.output_schema.is_none(), "{}", tool.name);
            let schema = Value::Object(tool.input_schema.as_ref().clone());
            let mut found = Vec::new();
            keywords(&schema, false, &mut found);
            for banned in ["$ref", "$defs", "$schema", "title"] {
                assert!(
                    !found.iter().any(|k| k == banned),
                    "{} publishes {banned}",
                    tool.name
                );
            }
            let mut numeric_formats = Vec::new();
            numeric_format(&schema, &mut numeric_formats);
            assert!(
                numeric_formats.is_empty(),
                "{} publishes a Rust integer width as a format: {numeric_formats:?}",
                tool.name
            );
        }
    }

    /// A `format` on an integer or number schema: schemars writes the Rust
    /// width there (`uint`, `int64`), which is not a JSON Schema format a
    /// strict client knows. A string's `format` (`uuid`) is a real one.
    fn numeric_format(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                let numeric = |t: &Value| matches!(t.as_str(), Some("integer" | "number"));
                let is_numeric = map.get("type").is_some_and(|t| {
                    numeric(t) || t.as_array().is_some_and(|many| many.iter().any(numeric))
                });
                if is_numeric && let Some(Value::String(format)) = map.get("format") {
                    out.push(format.clone());
                }
                map.values().for_each(|child| numeric_format(child, out));
            }
            Value::Array(items) => items.iter().for_each(|item| numeric_format(item, out)),
            _ => {}
        }
    }

    /// Serve a bridge over an in-memory transport and hand back a client.
    async fn paged_client(
        paged: bool,
    ) -> (
        rmcp::service::RunningService<rmcp::RoleClient, ()>,
        tokio::task::JoinHandle<()>,
    ) {
        use rmcp::ServiceExt;
        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local()).with_paged_tools(paged);
        let server_task = tokio::spawn(async move {
            if let Ok(running) = server.serve(server_transport).await {
                let _ = running.waiting().await;
            }
        });
        let client = ().serve(client_transport).await.expect("MCP initialize handshake");
        (client, server_task)
    }

    async fn list_page(
        client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
        cursor: Option<String>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ServiceError> {
        client
            .peer()
            .list_tools(Some(
                rmcp::model::PaginatedRequestParams::default().with_cursor(cursor),
            ))
            .await
    }

    /// Paged, every tool appears on exactly one page, each page's response
    /// line fits under Grok's 8,192-byte read with room to spare, and the
    /// router's order is kept.
    #[tokio::test]
    async fn paged_tools_list_every_tool_exactly_once_in_lines_under_the_page_budget() {
        let (client, server_task) = paged_client(true).await;
        let mut seen = Vec::new();
        let mut cursor = None;
        let mut pages = 0;
        loop {
            let page = list_page(&client, cursor.clone()).await.expect("a page");
            pages += 1;
            let line =
                serde_json::to_string(&json!({"jsonrpc": "2.0", "id": 1_000_000, "result": &page}))
                    .expect("reply JSON");
            assert!(
                line.len() < tool_pages::PAGE_BYTES,
                "page {pages} is {} bytes",
                line.len()
            );
            seen.extend(page.tools.into_iter().map(|tool| tool.name.to_string()));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        server_task.abort();

        assert!(pages > 1, "21 compacted tools do not fit one page");
        let expected: Vec<String> = KoruMcp::new(Arc::new(Fake), RequestContext::local())
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(expected.len(), 21);
        assert_eq!(seen, expected, "each tool once, in the router's order");
    }

    /// A cursor is the same on every call and from every bridge serving this
    /// build, so a client that pages across a reconnect lands where it meant.
    #[tokio::test]
    async fn a_tools_list_cursor_is_stable_across_calls_and_servers() {
        let (first, first_task) = paged_client(true).await;
        let (second, second_task) = paged_client(true).await;
        let a = list_page(&first, None).await.unwrap().next_cursor;
        let b = list_page(&first, None).await.unwrap().next_cursor;
        let c = list_page(&second, None).await.unwrap().next_cursor;
        assert!(a.is_some());
        assert_eq!(a, b);
        assert_eq!(a, c);
        // A cursor from one bridge serves the same page on the other.
        let on_first = list_page(&first, a.clone()).await.unwrap();
        let on_second = list_page(&second, a).await.unwrap();
        let names = |page: &rmcp::model::ListToolsResult| {
            page.tools
                .iter()
                .map(|t| t.name.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&on_first), names(&on_second));
        first_task.abort();
        second_task.abort();
    }

    /// A cursor this bridge never handed out is an MCP `invalid_params`
    /// error, paged or not — never a silent page one.
    #[tokio::test]
    async fn an_unknown_tools_list_cursor_is_an_invalid_params_error() {
        for paged in [true, false] {
            let (client, server_task) = paged_client(paged).await;
            for cursor in [
                "",
                "1",
                "koru-tools:no_such_tool",
                "koru-tools:compare_document_versions",
            ] {
                let error = list_page(&client, Some(cursor.into()))
                    .await
                    .expect_err(&format!("{cursor:?} was accepted (paged={paged})"));
                match error {
                    rmcp::ServiceError::McpError(data) => assert_eq!(
                        data.code,
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        "{cursor:?}"
                    ),
                    other => panic!("{cursor:?}: {other:?}"),
                }
            }
            server_task.abort();
        }
    }

    /// Koru AI reads the catalog, not `tools/list`: paging never hides a tool
    /// from it.
    #[test]
    fn koru_ai_catalog_sees_every_tool_whether_or_not_the_bridge_pages() {
        let names: HashSet<String> = workspace_tool_catalog()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(names.len(), 21);
        let routed: HashSet<String> = KoruMcp::new(Arc::new(Fake), RequestContext::local())
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(names, routed);
    }

    /// Compaction loses no parameter: every property name and every tag or
    /// enum value the Rust request types generate is still published.
    #[test]
    fn compaction_keeps_every_parameter_name_and_value() {
        fn names(value: &Value, parent_is_properties: bool, out: &mut HashSet<String>) {
            match value {
                Value::Object(map) => {
                    for (key, child) in map {
                        if parent_is_properties {
                            out.insert(format!("property {key}"));
                        } else if key == "const" || key == "enum" {
                            let values = child
                                .as_array()
                                .cloned()
                                .unwrap_or_else(|| vec![child.clone()]);
                            for value in values {
                                out.insert(format!("value {value}"));
                            }
                        }
                        names(child, key == "properties" && !parent_is_properties, out);
                    }
                }
                Value::Array(items) => items.iter().for_each(|item| names(item, false, out)),
                _ => {}
            }
        }
        fn resolve(value: &Value, defs: &JsonObject) -> Value {
            match value {
                Value::Object(map) => {
                    if let Some(name) = map
                        .get("$ref")
                        .and_then(Value::as_str)
                        .and_then(|r| r.strip_prefix("#/$defs/"))
                    {
                        return resolve(&defs[name], defs);
                    }
                    Value::Object(
                        map.iter()
                            .filter(|(key, _)| *key != "$defs")
                            .map(|(key, child)| (key.clone(), resolve(child, defs)))
                            .collect(),
                    )
                }
                Value::Array(items) => {
                    Value::Array(items.iter().map(|item| resolve(item, defs)).collect())
                }
                other => other.clone(),
            }
        }

        let published: HashMap<String, Arc<JsonObject>> =
            KoruMcp::new(Arc::new(Fake), RequestContext::local())
                .tool_router
                .list_all()
                .into_iter()
                .map(|tool| (tool.name.to_string(), tool.input_schema))
                .collect();
        for tool in KoruMcp::tool_router().list_all() {
            let defs = tool
                .input_schema
                .get("$defs")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut before = HashSet::new();
            names(
                &resolve(&Value::Object(tool.input_schema.as_ref().clone()), &defs),
                false,
                &mut before,
            );
            let mut after = HashSet::new();
            names(
                &Value::Object(published[tool.name.as_ref()].as_ref().clone()),
                false,
                &mut after,
            );
            let lost: Vec<_> = before.difference(&after).collect();
            assert!(lost.is_empty(), "{} lost {lost:?}", tool.name);
        }
    }

    #[test]
    fn every_published_tool_is_named_in_the_model_contract() {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let guide = read_model_document().expect("canonical MCP guide");
        for tool in server.tool_router.list_all() {
            assert!(
                guide.contains(&format!("`{}`", tool.name)),
                "crates/koru-mcp-server/MCP.md must name the public tool {}",
                tool.name
            );
        }
    }

    /// The contract teaches diagrams where an agent writing content looks: a
    /// Diagrams section that names the language which makes a block one, and
    /// every status a read can carry, so a client reading `diagram` finds each
    /// value it may branch on explained.
    #[test]
    fn the_model_contract_explains_every_diagram_status() {
        let guide = read_model_document().expect("canonical MCP guide");
        let start = guide
            .find("## Diagrams")
            .expect("crates/koru-mcp-server/MCP.md must have a Diagrams section");
        let section = &guide[start..];
        assert!(section.contains("`\"mermaid\"`"), "the language is named");
        assert!(section.contains("`diagram`"), "the field is named");
        for status in [
            "draws",
            "not_parsed",
            "kind_not_drawn",
            "too_large",
            "empty",
        ] {
            assert!(
                section.contains(&format!("`{status}`")),
                "crates/koru-mcp-server/MCP.md must explain the diagram status `{status}`"
            );
        }
    }

    #[test]
    fn tool_results_have_text_and_structured_content() {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let result = server.list_workspaces();
        assert!(result.structured_content.is_some());
        assert!(!result.content.is_empty());
        assert_eq!(result.is_error, Some(false));
    }

    #[test]
    fn structured_content_is_always_an_object_for_strict_mcp_clients() {
        let server = KoruMcp::new(Arc::new(ArrayFake), RequestContext::local());
        let result = server.list_workspaces();

        assert_eq!(
            result.structured_content,
            Some(json!({"result":[{"id":"one"}]}))
        );
    }

    #[test]
    fn edit_arguments_reject_unknown_ordering_fields() {
        let error = serde_json::from_value::<EditDocumentRequest>(json!({
            "workspace_id":"00000000-0000-4000-8000-000000000001",
            "document_id":"00000000-0000-4000-8000-000000000002",
            "base_revision":"00",
            "operations":[{
                "op":"insert",
                "node_id":"00000000-0000-4000-8000-000000000003",
                "parent_id":"00000000-0000-4000-8000-000000000004",
                "ordinal":2,
                "kind":"paragraph",
                "text":"hello"
            }]
        }))
        .expect_err("ordinal must not be silently discarded");

        assert!(error.to_string().contains("unknown field `ordinal`"));
        assert!(error.to_string().contains("after_node_id"));
    }

    #[test]
    fn insert_and_move_require_an_explicit_parent() {
        for operation in [
            json!({
                "op":"insert",
                "node_id":"00000000-0000-4000-8000-000000000003",
                "kind":"paragraph",
                "text":"hello"
            }),
            json!({
                "op":"move",
                "node_id":"00000000-0000-4000-8000-000000000003",
                "expected":"hello"
            }),
        ] {
            let error = serde_json::from_value::<EditDocumentRequest>(json!({
                "workspace_id":"00000000-0000-4000-8000-000000000001",
                "document_id":"00000000-0000-4000-8000-000000000002",
                "base_revision":"00",
                "operations":[operation]
            }))
            .expect_err("root-level document content must be impossible");

            assert!(error.to_string().contains("missing field `parent_id`"));
        }
    }

    #[test]
    fn edit_tool_schema_explains_parent_and_stable_sibling_ordering() {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "edit_document")
            .expect("edit_document tool");
        let schema = serde_json::to_value(&tool.input_schema).expect("JSON schema");
        let encoded = serde_json::to_string(&schema).expect("schema text");
        let description = tool.description.as_deref().expect("a description");

        // The safety wording lives in the tool's description, condensed; the
        // schema carries which fields each op requires, generated from it.
        for phrase in [
            "Read koru://docs/model before the first mutation",
            "expected, its exact current text",
            "on stale_node or a mismatch, reread and retry",
            "never the absolute CRDT root",
            "numeric ordinals are unsupported",
            "UTF-8 byte offset",
        ] {
            assert!(description.contains(phrase), "missing {phrase:?}");
        }
        assert!(
            encoded.contains("insert(node_id, parent_id, kind, text, after_node_id?)"),
            "{encoded}"
        );
        assert!(encoded.contains("insert_inline"));
        // A new operation is published with its fields, optional ones marked.
        // Their order follows the JSON map in use, which is sorted when this
        // crate is built alone and in declaration order when another crate in
        // the same build keeps insertion order, so only the shape is pinned.
        let summary = encoded
            .split("set_callout_look(")
            .nth(1)
            .and_then(|rest| rest.split(')').next())
            .expect("a published set_callout_look summary");
        assert!(summary.starts_with("node_id, expected, "), "{summary}");
        assert!(
            summary.contains("icon?") && summary.contains("color?"),
            "{summary}"
        );
        assert!(
            !encoded.contains("$ref"),
            "operation variants must be inline"
        );
        for kind in [
            "task",
            "paragraph",
            "quote",
            "listblock",
            "table",
            "code",
            "contact",
            "divider",
            "callout",
        ] {
            assert!(encoded.contains(&format!("\"{kind}\"")), "missing {kind}");
        }
    }

    #[test]
    fn create_schema_inlines_only_meaningful_initial_operations() {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "create_document")
            .expect("create_document tool");
        let schema = serde_json::to_value(&tool.input_schema).expect("JSON schema");
        let encoded = serde_json::to_string(&schema).expect("schema text");

        assert!(!encoded.contains("$ref"), "seed variants must be inline");
        for operation in [
            "insert",
            "insert_inline",
            "set_done",
            "set_language",
            "set_callout_look",
        ] {
            assert!(
                encoded.contains(&format!("\"{operation}\"")),
                "missing seed operation {operation}"
            );
        }
        for operation in ["replace_text", "move", "remove"] {
            assert!(
                !encoded.contains(&format!("\"{operation}\"")),
                "nonsensical seed operation {operation} was published"
            );
        }
    }

    #[test]
    fn create_retry_identity_uses_semantics_not_uuid_spelling_or_the_key() {
        let workspace_id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let root = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";
        let paragraph = "cccccccc-cccc-4ccc-8ccc-cccccccccccc";
        let make = |upper: bool, explicit_kind: bool, key: &str, title: &str| {
            let spell = |value: &str| {
                if upper {
                    value.to_uppercase()
                } else {
                    value.to_owned()
                }
            };
            CreateDocumentRequest {
                workspace_id: spell(workspace_id),
                title: title.to_owned(),
                kind: explicit_kind.then(|| "page".to_owned()),
                parent_document_id: None,
                idempotency_key: Some(key.to_owned()),
                root_node_id: Some(spell(root)),
                initial_operations: Some(vec![InitialOperation::Insert {
                    node_id: spell(paragraph),
                    parent_id: spell(root),
                    after_node_id: None,
                    kind: InsertKind::Paragraph,
                    text: "same body".into(),
                }]),
            }
        };
        let workspace = workspace(workspace_id).unwrap();
        let first = create_request_hash(
            &make(false, false, "11111111-1111-4111-8111-111111111111", "Same"),
            workspace,
        )
        .unwrap();
        let replay = create_request_hash(
            &make(true, true, "22222222-2222-4222-8222-222222222222", "Same"),
            workspace,
        )
        .unwrap();
        let changed = create_request_hash(
            &make(
                false,
                false,
                "11111111-1111-4111-8111-111111111111",
                "Changed",
            ),
            workspace,
        )
        .unwrap();

        assert_eq!(first, replay);
        assert_ne!(first, changed);

        // **Where a document goes is part of what was asked for.** Two creates
        // differing only in their parent are two documents, so a retry key
        // reused across them has to be a conflict rather than a replay that
        // hands back a document nested somewhere else. And the parent is an id,
        // so its spelling is normalised like every other.
        let parent = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
        let mut nested = make(false, false, "11111111-1111-4111-8111-111111111111", "Same");
        nested.parent_document_id = Some(parent.to_owned());
        let nested_hash = create_request_hash(&nested, workspace).unwrap();
        assert_ne!(first, nested_hash);
        nested.parent_document_id = Some(parent.to_uppercase());
        assert_eq!(
            nested_hash,
            create_request_hash(&nested, workspace).unwrap()
        );
    }

    #[test]
    fn initial_operations_build_exact_order_without_a_default_paragraph() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = NodeId::new();
        let mut root_value = Node::document("Seeded").unwrap();
        root_value.id = root;
        doc.insert(Parent::Root, root_value).unwrap();
        let first = NodeId::new();
        let second = NodeId::new();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        apply_initial_ops(
            &mut doc,
            root,
            &[
                InitialOperation::Insert {
                    node_id: first.to_storage(),
                    parent_id: root.to_storage(),
                    after_node_id: None,
                    kind: InsertKind::Paragraph,
                    text: "One".into(),
                },
                InitialOperation::Insert {
                    node_id: second.to_storage(),
                    parent_id: root.to_storage(),
                    after_node_id: Some(first.to_storage()),
                    kind: InsertKind::Paragraph,
                    text: "Two".into(),
                },
            ],
            &context,
        )
        .unwrap();

        assert_eq!(
            doc.children(Parent::Node(root)).unwrap(),
            vec![first, second]
        );
        assert_eq!(doc.walk_all().unwrap().len(), 3);
    }

    #[test]
    fn initial_operations_reject_empty_batches_and_forward_references() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Seeded").unwrap())
            .unwrap();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };
        assert!(apply_initial_ops(&mut doc, root, &[], &context).is_err());

        let error = apply_initial_ops(
            &mut doc,
            root,
            &[InitialOperation::Insert {
                node_id: NodeId::new().to_storage(),
                parent_id: NodeId::new().to_storage(),
                after_node_id: None,
                kind: InsertKind::Paragraph,
                text: "orphan".into(),
            }],
            &context,
        )
        .expect_err("parent must already exist in the seed batch");
        assert!(error.message.contains("earlier insert"));
    }

    #[test]
    fn moving_forward_uses_the_post_removal_sibling_coordinate() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Source").unwrap())
            .unwrap();
        let a = doc
            .insert(Parent::Node(root), Node::paragraph("A").unwrap())
            .unwrap();
        let b = doc
            .insert(Parent::Node(root), Node::paragraph("B").unwrap())
            .unwrap();
        let c = doc
            .insert(Parent::Node(root), Node::paragraph("C").unwrap())
            .unwrap();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        apply_ops(
            &mut doc,
            &[EditOperation::Move {
                node_id: a.to_storage(),
                expected: "A".into(),
                parent_id: root.to_storage(),
                after_node_id: Some(c.to_storage()),
            }],
            &context,
        )
        .unwrap();

        assert_eq!(doc.children(Parent::Node(root)).unwrap(), vec![b, c, a]);
    }

    /// One `insert` of kind `callout` still makes a one-line callout: the
    /// box under the asked-for id, with the text as a first paragraph inside
    /// it, read back as a container with no text and one child.
    #[test]
    fn insert_callout_with_text_makes_a_box_with_one_paragraph_inside() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Boxes").unwrap())
            .unwrap();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };
        let box_id = NodeId::new();
        let empty_id = NodeId::new();

        apply_ops(
            &mut doc,
            &[
                EditOperation::Insert {
                    node_id: box_id.to_storage(),
                    parent_id: root.to_storage(),
                    after_node_id: None,
                    kind: InsertKind::Callout,
                    text: "Mind the gap".into(),
                },
                EditOperation::Insert {
                    node_id: empty_id.to_storage(),
                    parent_id: root.to_storage(),
                    after_node_id: Some(box_id.to_storage()),
                    kind: InsertKind::Callout,
                    text: String::new(),
                },
            ],
            &context,
        )
        .unwrap();

        assert_eq!(
            doc.children(Parent::Node(root)).unwrap(),
            vec![box_id, empty_id]
        );
        let callout = doc.read(box_id).unwrap();
        assert_eq!(callout.kind, Kind::Callout);
        assert_eq!(callout.content.as_text(), Some(""));
        let lines = doc.children(Parent::Node(box_id)).unwrap();
        assert_eq!(lines.len(), 1, "one paragraph inside the box");
        let line = doc.read(lines[0]).unwrap();
        assert_eq!(line.kind, Kind::Paragraph);
        assert_eq!(line.content.as_text(), Some("Mind the gap"));
        assert_eq!(doc.parent(lines[0]).unwrap(), Parent::Node(box_id));

        // What a client reads back: the box's kind and look are on the box.
        let read = node_json(&callout, Parent::Node(root), 0, 1, &[]);
        assert_eq!(read["kind"], "callout");
        assert!(read["metadata"]["icon"].is_null());
        assert!(read["metadata"]["colour"].is_null());
        assert_eq!(read["content"]["text"], "");

        // An empty text is just the box.
        assert!(doc.children(Parent::Node(empty_id)).unwrap().is_empty());
        assert_eq!(doc.read(empty_id).unwrap().kind, Kind::Callout);
    }

    #[test]
    fn set_language_labels_a_code_block_and_null_restores_auto() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Code").unwrap())
            .unwrap();
        let block = doc
            .insert(Parent::Node(root), Node::code("fn main() {}").unwrap())
            .unwrap();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        apply_ops(
            &mut doc,
            &[EditOperation::SetLanguage {
                node_id: block.to_storage(),
                expected: "fn main() {}".into(),
                language: Some("rust".into()),
            }],
            &context,
        )
        .unwrap();
        assert_eq!(
            doc.read(block).unwrap().meta.language.as_deref(),
            Some("rust")
        );

        // `null` is Auto: stored as absence, so an older reader is unchanged.
        apply_ops(
            &mut doc,
            &[EditOperation::SetLanguage {
                node_id: block.to_storage(),
                expected: "fn main() {}".into(),
                language: None,
            }],
            &context,
        )
        .unwrap();
        assert_eq!(doc.read(block).unwrap().meta.language, None);
    }

    #[test]
    fn set_language_refuses_anything_that_is_not_a_code_block() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Prose").unwrap())
            .unwrap();
        let paragraph = doc
            .insert(Parent::Node(root), Node::paragraph("words").unwrap())
            .unwrap();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        let error = apply_ops(
            &mut doc,
            &[EditOperation::SetLanguage {
                node_id: paragraph.to_storage(),
                expected: "words".into(),
                language: Some("rust".into()),
            }],
            &context,
        )
        .expect_err("language on prose is a silent no-op waiting to happen");

        assert!(error.message.contains("code block"));
        assert_eq!(doc.read(paragraph).unwrap().meta.language, None);
    }

    #[test]
    fn a_seed_batch_can_name_its_code_blocks_language() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Seeded").unwrap())
            .unwrap();
        let block = NodeId::new();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        apply_initial_ops(
            &mut doc,
            root,
            &[
                InitialOperation::Insert {
                    node_id: block.to_storage(),
                    parent_id: root.to_storage(),
                    after_node_id: None,
                    kind: InsertKind::Code,
                    text: "print('hi')".into(),
                },
                InitialOperation::SetLanguage {
                    node_id: block.to_storage(),
                    expected: "print('hi')".into(),
                    language: Some("python".into()),
                },
            ],
            &context,
        )
        .unwrap();

        assert_eq!(
            doc.read(block).unwrap().meta.language.as_deref(),
            Some("python")
        );

        // The creation-time guard matches every other seeded op: the target
        // must be an earlier insert in this batch, never the root.
        let error = apply_initial_ops(
            &mut doc,
            root,
            &[InitialOperation::SetLanguage {
                node_id: root.to_storage(),
                expected: "Seeded".into(),
                language: Some("rust".into()),
            }],
            &context,
        )
        .expect_err("the title root is not an inserted code block");
        assert!(error.message.contains("earlier insert"));
    }

    // ------------------------------------------------------- callout looks

    /// A document holding one callout box with one line inside it, and an
    /// empty resolution context.
    fn a_callout_to_restyle() -> (WorkspaceDoc, NodeId, NodeId, NodeId, InlineContext) {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Looks").unwrap())
            .unwrap();
        let callout = doc
            .insert(Parent::Node(root), Node::callout(None, None).unwrap())
            .unwrap();
        let line = doc
            .insert(Parent::Node(callout), Node::paragraph("inside").unwrap())
            .unwrap();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };
        (doc, root, callout, line, context)
    }

    fn look(
        callout: NodeId,
        icon: Option<Option<&str>>,
        color: Option<Option<&str>>,
    ) -> EditOperation {
        EditOperation::SetCalloutLook {
            node_id: callout.to_storage(),
            expected: String::new(),
            icon: icon.map(|icon| icon.map(str::to_owned)),
            color: color.map(|color| color.map(str::to_owned)),
        }
    }

    #[test]
    fn set_callout_look_sets_each_field_alone_and_null_restores_the_default() {
        let (mut doc, root, callout, _, context) = a_callout_to_restyle();

        apply_ops(
            &mut doc,
            &[look(callout, Some(Some("⚠️")), Some(Some("red")))],
            &context,
        )
        .unwrap();
        let read = node_json(&doc.read(callout).unwrap(), Parent::Node(root), 0, 0, &[]);
        assert_eq!(read["kind"], "callout");
        assert_eq!(read["metadata"]["icon"], "⚠️");
        assert_eq!(read["metadata"]["colour"], "red");

        // Leaving a field out leaves it alone; `null` restores its default.
        apply_ops(&mut doc, &[look(callout, Some(None), None)], &context).unwrap();
        let node = doc.read(callout).unwrap();
        assert_eq!(node.meta.icon, None);
        assert_eq!(node.meta.colour.as_deref(), Some("red"));

        apply_ops(&mut doc, &[look(callout, None, Some(None))], &context).unwrap();
        assert_eq!(doc.read(callout).unwrap().meta.colour, None);
    }

    #[test]
    fn set_callout_look_stores_an_emoji_the_way_koru_spells_it() {
        let (mut doc, _, callout, _, context) = a_callout_to_restyle();

        // U+26A0 alone is the unqualified spelling; the box stores the
        // fully qualified one, which is what draws as a color emoji.
        apply_ops(
            &mut doc,
            &[look(callout, Some(Some("\u{26a0}")), None)],
            &context,
        )
        .unwrap();
        assert_eq!(
            doc.read(callout).unwrap().meta.icon.as_deref(),
            Some("\u{26a0}\u{fe0f}")
        );

        apply_ops(
            &mut doc,
            &[look(callout, Some(Some(" 🔥 ")), None)],
            &context,
        )
        .unwrap();
        assert_eq!(doc.read(callout).unwrap().meta.icon.as_deref(), Some("🔥"));
    }

    #[test]
    fn set_callout_look_refuses_what_it_cannot_draw_and_writes_nothing() {
        let (mut doc, _, callout, line, context) = a_callout_to_restyle();
        let before = doc.version().encode();

        let refused = |doc: &mut WorkspaceDoc, op: EditOperation| {
            apply_ops(doc, &[op], &context).expect_err("this look cannot be drawn")
        };

        let error = refused(&mut doc, look(callout, Some(Some("warning")), None));
        assert!(error.message.contains("one emoji"), "{}", error.message);
        let error = refused(&mut doc, look(callout, Some(Some("")), None));
        assert!(error.message.contains("one emoji"), "{}", error.message);
        let error = refused(&mut doc, look(callout, None, Some(Some("magenta"))));
        assert!(error.message.contains("red, orange"), "{}", error.message);

        // A good icon beside a bad color changes neither.
        refused(
            &mut doc,
            look(callout, Some(Some("🔥")), Some(Some("magenta"))),
        );
        let error = refused(&mut doc, look(callout, None, None));
        assert!(error.message.contains("icon, color or both"));

        // Only a callout has a look.
        let error = refused(&mut doc, look(line, Some(Some("🔥")), None));
        assert!(error.message.contains("callout"), "{}", error.message);

        let stale = EditOperation::SetCalloutLook {
            node_id: callout.to_storage(),
            expected: "not empty".into(),
            icon: Some(Some("🔥".into())),
            color: None,
        };
        assert_eq!(refused(&mut doc, stale).code, "precondition_failed");

        assert_eq!(doc.read(callout).unwrap().meta.icon, None);
        assert_eq!(
            doc.version().encode(),
            before,
            "a refused look writes no operation"
        );
    }

    #[test]
    fn set_callout_look_leaves_no_trace_when_nothing_changes() {
        let (mut doc, _, callout, _, context) = a_callout_to_restyle();
        apply_ops(
            &mut doc,
            &[look(callout, Some(Some("🔥")), Some(Some("blue")))],
            &context,
        )
        .unwrap();
        let before = doc.version().encode();

        // The same look again, and a `null` on a field that is already the
        // default: neither writes an operation, so neither is an undo step.
        apply_ops(
            &mut doc,
            &[look(callout, Some(Some("🔥")), Some(Some("blue")))],
            &context,
        )
        .unwrap();
        assert_eq!(doc.version().encode(), before);
        let (mut fresh, _, fresh_callout, _, context) = a_callout_to_restyle();
        let untouched = fresh.version().encode();
        apply_ops(
            &mut fresh,
            &[look(fresh_callout, Some(None), Some(None))],
            &context,
        )
        .unwrap();
        assert_eq!(fresh.version().encode(), untouched);
    }

    #[test]
    fn set_callout_look_tells_absent_from_null_and_accepts_the_stored_spelling() {
        let id = NodeId::new().to_storage();
        let parse = |fields: Value| -> EditOperation {
            let mut op = json!({"op": "set_callout_look", "node_id": id, "expected": ""});
            op.as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            serde_json::from_value(op).expect("a well-formed look")
        };
        let fields = |op: EditOperation| match op {
            EditOperation::SetCalloutLook { icon, color, .. } => (icon, color),
            other => panic!("not a look: {other:?}"),
        };

        assert_eq!(
            fields(parse(json!({"color": "red"}))),
            (None, Some(Some("red".to_owned())))
        );
        assert_eq!(fields(parse(json!({"icon": null}))), (Some(None), None));
        // `colour` is what `read_document` reports; it names the same field.
        assert_eq!(
            fields(parse(json!({"colour": "blue"}))),
            (None, Some(Some("blue".to_owned())))
        );

        // What was sent is what is serialized: absent stays absent.
        let sent = serde_json::to_value(parse(json!({"icon": null}))).unwrap();
        assert!(sent["icon"].is_null() && sent.get("icon").is_some());
        assert!(sent.get("color").is_none());

        let misspelt =
            json!({"op": "set_callout_look", "node_id": id, "expected": "", "tint": "red"});
        assert!(serde_json::from_value::<EditOperation>(misspelt).is_err());
    }

    #[test]
    fn a_seed_batch_can_set_the_look_of_a_callout_it_inserted() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Seeded").unwrap())
            .unwrap();
        let callout = NodeId::new();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        apply_initial_ops(
            &mut doc,
            root,
            &[
                InitialOperation::Insert {
                    node_id: callout.to_storage(),
                    parent_id: root.to_storage(),
                    after_node_id: None,
                    kind: InsertKind::Callout,
                    text: "Mind the gap".into(),
                },
                InitialOperation::SetCalloutLook {
                    node_id: callout.to_storage(),
                    expected: String::new(),
                    icon: Some(Some("⚠️".into())),
                    color: Some(Some("orange".into())),
                },
            ],
            &context,
        )
        .unwrap();
        let node = doc.read(callout).unwrap();
        assert_eq!(node.meta.icon.as_deref(), Some("⚠️"));
        assert_eq!(node.meta.colour.as_deref(), Some("orange"));
        assert_eq!(doc.children(Parent::Node(callout)).unwrap().len(), 1);

        // The same guard as every seeded op: an earlier insert, never the root.
        let error = apply_initial_ops(
            &mut doc,
            root,
            &[InitialOperation::SetCalloutLook {
                node_id: root.to_storage(),
                expected: "Seeded".into(),
                icon: Some(Some("🔥".into())),
                color: None,
            }],
            &context,
        )
        .expect_err("the title root is not an inserted callout");
        assert!(error.message.contains("earlier insert"));
    }

    // -------------------------------------------------------------- diagrams
    //
    // A `mermaid` code block reads back with `diagram`: whether its text
    // draws, in the shape `koru_diagram::Status` serialises to and `MCP.md`
    // explains under Diagrams. The texts are koru-diagram's own fixtures,
    // included rather than copied, so the two crates cannot drift apart on
    // what a showcase contains. Nothing here touches a disk, a store or a
    // renderer: a document in memory, projected the way a tool projects it.

    const FLOWCHART_SHOWCASE: &str =
        include_str!("../../koru-diagram/tests/fixtures/flowchart-showcase.mmd");
    const SEQUENCE_SHOWCASE: &str =
        include_str!("../../koru-diagram/tests/fixtures/sequence-showcase.mmd");
    const BAD_ARROW_LINE7: &str =
        include_str!("../../koru-diagram/tests/fixtures/bad-arrow-line7.mmd");
    const PIE: &str = include_str!("../../koru-diagram/tests/fixtures/pie.mmd");
    const GANTT: &str = include_str!("../../koru-diagram/tests/fixtures/gantt.mmd");
    const EMPTY_DIAGRAM: &str = include_str!("../../koru-diagram/tests/fixtures/empty.mmd");

    /// One node **as `read_document` publishes it** — through `document_json`,
    /// so an assertion is about the projection a tool sees rather than a
    /// second reading of the model.
    fn published_node(doc: &WorkspaceDoc, node: NodeId) -> Value {
        let published = document_json(StoreDocumentId::new(), doc).unwrap();
        published["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|value| value["node_id"] == node.to_storage())
            .expect("the node is in the projection")
            .clone()
    }

    /// The `diagram` field of a code block holding `text` under `language`,
    /// or `None` when the projection carries no such field.
    fn published_diagram(language: Option<&str>, text: &str) -> Option<Value> {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Diagrams").unwrap())
            .unwrap();
        let block = doc
            .insert(Parent::Node(root), Node::code(text).unwrap())
            .unwrap();
        doc.set_language(block, language).unwrap();
        published_node(&doc, block).get("diagram").cloned()
    }

    /// `flowchart TD` with `n` boxes in one chain, one arrow per line.
    fn chain(n: usize) -> String {
        let mut text = String::from("flowchart TD\n");
        for i in 1..n {
            text.push_str(&format!("    N{} --> N{i}\n", i - 1));
        }
        text
    }

    #[test]
    fn a_flowchart_that_draws_reads_back_with_its_counts() {
        assert_eq!(
            published_diagram(Some("mermaid"), FLOWCHART_SHOWCASE),
            Some(json!({
                "status": "draws", "kind": "flowchart", "nodes": 10, "edges": 12, "groups": 2
            }))
        );
    }

    #[test]
    fn a_sequence_diagram_that_draws_reads_back_with_its_counts() {
        assert_eq!(
            published_diagram(Some("mermaid"), SEQUENCE_SHOWCASE),
            Some(json!({
                "status": "draws", "kind": "sequence",
                "participants": 4, "messages": 12, "notes": 2
            }))
        );
    }

    /// The line and the code are pinned. The message is the parser's own
    /// English, relayed and not owned here, so it is held to being present
    /// rather than to its wording.
    #[test]
    fn a_diagram_that_does_not_parse_names_its_line() {
        let diagram = published_diagram(Some("mermaid"), BAD_ARROW_LINE7).expect("a mermaid block");
        assert_eq!(diagram["status"], "not_parsed");
        assert_eq!(diagram["line"], 7);
        assert_eq!(diagram["code"], "merman.parse.diagram_parse");
        assert!(
            diagram["message"]
                .as_str()
                .is_some_and(|message| !message.is_empty()),
            "{diagram}"
        );
        assert_eq!(diagram.as_object().unwrap().len(), 4, "{diagram}");
    }

    #[test]
    fn a_pie_chart_reads_back_as_drawn_with_its_slices() {
        assert_eq!(
            published_diagram(Some("mermaid"), PIE),
            Some(json!({"status": "draws", "kind": "pie", "slices": 2}))
        );
    }

    #[test]
    fn a_kind_koru_does_not_draw_reads_back_as_such() {
        assert_eq!(
            published_diagram(Some("mermaid"), GANTT),
            Some(json!({"status": "kind_not_drawn", "kind": "gantt"}))
        );
        // Prose under the mermaid label: no diagram keyword at all.
        assert_eq!(
            published_diagram(Some("mermaid"), "hello world"),
            Some(json!({"status": "kind_not_drawn", "kind": null}))
        );
    }

    #[test]
    fn a_diagram_over_the_limit_reads_back_too_large() {
        assert_eq!(
            published_diagram(Some("mermaid"), &chain(1000)),
            Some(json!({"status": "too_large", "kind": "flowchart", "count": 1000}))
        );
    }

    #[test]
    fn an_empty_diagram_block_reads_back_empty() {
        assert_eq!(EMPTY_DIAGRAM, "");
        assert_eq!(
            published_diagram(Some("mermaid"), EMPTY_DIAGRAM),
            Some(json!({"status": "empty"}))
        );
    }

    /// Only a code block labelled exactly `mermaid` is inspected: not another
    /// grammar holding a flowchart, not an unlabelled block, not a differently
    /// cased label — `set_language` stores what it is given — and never a
    /// paragraph or the title, whatever their text says.
    #[test]
    fn only_a_mermaid_code_block_carries_a_diagram() {
        assert_eq!(published_diagram(Some("rust"), FLOWCHART_SHOWCASE), None);
        assert_eq!(published_diagram(None, FLOWCHART_SHOWCASE), None);
        assert_eq!(published_diagram(Some("Mermaid"), FLOWCHART_SHOWCASE), None);

        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("flowchart LR").unwrap())
            .unwrap();
        let paragraph = doc
            .insert(Parent::Node(root), Node::paragraph("flowchart LR").unwrap())
            .unwrap();
        assert!(published_node(&doc, root).get("diagram").is_none());
        assert!(published_node(&doc, paragraph).get("diagram").is_none());
    }

    /// A seed batch is the other way a diagram is written, and
    /// `create_document` answers with the same projection a read does, so the
    /// status is in the creation result before any read.
    #[test]
    fn a_seeded_diagram_is_in_the_creation_projection() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Seeded").unwrap())
            .unwrap();
        let block = NodeId::new();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };
        apply_initial_ops(
            &mut doc,
            root,
            &[
                InitialOperation::Insert {
                    node_id: block.to_storage(),
                    parent_id: root.to_storage(),
                    after_node_id: None,
                    kind: InsertKind::Code,
                    text: GANTT.into(),
                },
                InitialOperation::SetLanguage {
                    node_id: block.to_storage(),
                    expected: GANTT.into(),
                    language: Some("mermaid".into()),
                },
            ],
            &context,
        )
        .unwrap();
        assert_eq!(
            published_node(&doc, block)["diagram"],
            json!({"status": "kind_not_drawn", "kind": "gantt"})
        );
    }

    /// The whole path an agent takes, through the embedded store: `insert` a
    /// code block and `set_language` it `mermaid` in one `edit_document`
    /// batch, then read. The status rides on the read; nothing is written
    /// back, and the nodes beside the block stay as they were.
    #[test]
    fn a_diagram_written_through_edit_document_reads_back_drawing() {
        let store = MemoryStore::new();
        let page = store.add_page("Diagrams");
        let backend = store.backend();
        let workspace_id = store.workspace.to_storage();
        let document_id = page.to_storage();

        let read = call(
            &backend,
            "read_document",
            json!({"workspace_id": workspace_id, "document_id": document_id}),
        )
        .unwrap();
        let block = NodeId::new().to_storage();
        call(
            &backend,
            "edit_document",
            json!({
                "workspace_id": workspace_id,
                "document_id": document_id,
                "base_revision": read["revision"],
                "operations": [
                    {
                        "op": "insert",
                        "node_id": block,
                        "parent_id": read["root_node_id"],
                        "after_node_id": null,
                        "kind": "code",
                        "text": SEQUENCE_SHOWCASE,
                    },
                    {
                        "op": "set_language",
                        "node_id": block,
                        "expected": SEQUENCE_SHOWCASE,
                        "language": "mermaid",
                    },
                ]
            }),
        )
        .unwrap();

        let reread = call(
            &backend,
            "read_document",
            json!({"workspace_id": workspace_id, "document_id": document_id}),
        )
        .unwrap();
        let nodes = reread["nodes"].as_array().unwrap();
        let node = nodes
            .iter()
            .find(|node| node["node_id"] == block)
            .expect("the block is in the read");
        assert_eq!(node["kind"], "code");
        assert_eq!(node["metadata"]["language"], "mermaid");
        assert_eq!(node["content"]["text"], SEQUENCE_SHOWCASE);
        assert_eq!(
            node["diagram"],
            json!({
                "status": "draws", "kind": "sequence",
                "participants": 4, "messages": 12, "notes": 2
            })
        );
        for other in nodes.iter().filter(|node| node["node_id"] != block) {
            assert!(other.get("diagram").is_none(), "{other}");
        }
        // The store holds exactly what was written: the block and its
        // language, and no status.
        let stored = WorkspaceDoc::from_snapshot(&store.snapshots.lock().unwrap()[&page]).unwrap();
        let stored_block = stored.read(NodeId::parse(&block).unwrap()).unwrap();
        assert_eq!(stored_block.meta.language.as_deref(), Some("mermaid"));
    }

    // ------------------------------------------------------------ attributes

    /// A document holding one task, and an empty resolution context. Every
    /// attribute test starts here so the assertions are about the operation
    /// rather than about the fixture.
    fn a_task_to_annotate() -> (WorkspaceDoc, NodeId, InlineContext) {
        let (doc, task, _, context) = two_tasks_to_annotate();
        (doc, task, context)
    }

    /// The same fixture with a second row, for the tests that copy a value
    /// from one node to another.
    fn two_tasks_to_annotate() -> (WorkspaceDoc, NodeId, NodeId, InlineContext) {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Plan").unwrap())
            .unwrap();
        let task = doc
            .insert(Parent::Node(root), Node::task("Ship it").unwrap())
            .unwrap();
        let second = doc
            .insert(Parent::Node(root), Node::task("Also").unwrap())
            .unwrap();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };
        (doc, task, second, context)
    }

    /// One node's attributes **as `read_document` publishes them** — through
    /// `document_json`, not through a second reading of the model. That is the
    /// whole point of the round trip: the two functions have to agree, so the
    /// assertion must go through the projection a tool actually sees.
    fn published_attributes(doc: &WorkspaceDoc, node: NodeId) -> Vec<Value> {
        published_node(doc, node)["content"]["attributes"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    fn published_value(doc: &WorkspaceDoc, node: NodeId, key: &str) -> Option<Value> {
        published_attributes(doc, node)
            .into_iter()
            .find(|attr| attr["role"] == key)
            .map(|attr| attr["value"].clone())
    }

    #[test]
    fn set_attribute_writes_the_three_system_keys_and_they_read_back() {
        let (mut doc, task, context) = a_task_to_annotate();

        apply_ops(
            &mut doc,
            &[
                EditOperation::SetAttribute {
                    node_id: task.to_storage(),
                    expected: "Ship it".into(),
                    key: "koru.due".into(),
                    value: AttributeValue::Date {
                        value: "2026-08-30".into(),
                    },
                },
                EditOperation::SetAttribute {
                    node_id: task.to_storage(),
                    expected: "Ship it".into(),
                    key: "koru.priority".into(),
                    value: AttributeValue::Text {
                        value: "high".into(),
                    },
                },
                EditOperation::SetAttribute {
                    node_id: task.to_storage(),
                    expected: "Ship it".into(),
                    key: "koru.color".into(),
                    value: AttributeValue::Text {
                        value: "teal".into(),
                    },
                },
            ],
            &context,
        )
        .unwrap();

        assert_eq!(
            published_value(&doc, task, "koru.due"),
            Some(json!({"type":"date", "value":"2026-08-30"}))
        );
        assert_eq!(
            published_value(&doc, task, "koru.priority"),
            Some(json!({"type":"text", "value":"high"}))
        );
        assert_eq!(
            published_value(&doc, task, "koru.color"),
            Some(json!({"type":"text", "value":"teal"}))
        );
    }

    /// **The symmetry that makes the vocabulary usable.** A tool that reads a
    /// value off one node must be able to write the identical JSON onto
    /// another without understanding it. Every shape `attr_json` emits for a
    /// settable value is fed straight back through `set_attribute`, and what
    /// comes out of the projection the second time has to be the same object.
    ///
    /// It runs through the projection at both ends deliberately: comparing an
    /// `AttrValue` to itself would pass while `attr_json` and `AttributeValue`
    /// disagreed about a field name, which is the drift this pins.
    #[test]
    fn a_published_attribute_value_writes_back_unchanged() {
        let zone = TimeZoneName::parse("Europe/Zurich").unwrap();
        let written = [
            (
                "koru.due",
                AttrValue::Date(DateOnly::parse("2026-08-30").unwrap()),
            ),
            (
                "koru.due",
                AttrValue::ZonedDateTime(ZonedDateTime::new(1_787_000_000_000, zone)),
            ),
            ("koru.priority", AttrValue::Text("urgent".into())),
            ("koru.color", AttrValue::Text("purple".into())),
            ("koru.tag", AttrValue::Text(TagId::new().to_storage())),
            ("custom.count", AttrValue::Number(3.0)),
            ("custom.flag", AttrValue::Bool(true)),
            ("custom.note", AttrValue::Text("a note".into())),
            (
                "custom.when",
                AttrValue::FloatingDateTime(
                    FloatingDateTime::parse("2026-08-30T09:00:00").unwrap(),
                ),
            ),
        ];
        // The three custom definitions the workspace would hold; the system
        // keys answer for themselves.
        let mut context = AttrContext::default();
        for (key, value_type) in [
            ("custom.count", koru_core::ValueType::Number),
            ("custom.flag", koru_core::ValueType::Boolean),
            ("custom.note", koru_core::ValueType::Text),
            ("custom.when", koru_core::ValueType::FloatingDateTime),
        ] {
            context.definitions.insert(
                key.into(),
                AttributeDefinition::custom(key, key, value_type),
            );
        }

        for (key, value) in written {
            let (mut doc, task, second, mut inline) = two_tasks_to_annotate();
            inline.attrs = AttrContext {
                tags: HashMap::new(),
                definitions: context.definitions.clone(),
            };

            // Put the value there the way the model does, then read it the way
            // a tool does.
            match koru_core::attributes::system_definition(key) {
                Some(_) => doc.set_attribute(task, key, value.clone()).map(|_| ()),
                None => doc
                    .set_attribute_defined(task, &inline.attrs.definitions[key], value.clone())
                    .map(|_| ()),
            }
            .unwrap();
            let published = published_value(&doc, task, key).expect("the value is published");

            // Write that exact JSON back onto another node.
            let parsed: AttributeValue = serde_json::from_value(published.clone())
                .unwrap_or_else(|e| panic!("{key} does not parse back: {e} ({published})"));
            apply_ops(
                &mut doc,
                &[EditOperation::SetAttribute {
                    node_id: second.to_storage(),
                    expected: "Also".into(),
                    key: key.into(),
                    value: parsed,
                }],
                &inline,
            )
            .unwrap_or_else(|e| panic!("{key} does not write back: {e}"));

            assert_eq!(
                published_value(&doc, second, key),
                Some(published),
                "{key} did not survive the round trip"
            );
        }
    }

    /// Clearing a key that is not there is a **guarded no-op**, and the guard
    /// is the model's. Asserting on the document version rather than on the
    /// absence of the attribute is what catches the phantom: a delete of an
    /// absent key still writes an op, and that op is an undo step the person
    /// at the keyboard has to press through.
    #[test]
    fn clearing_an_absent_key_writes_nothing() {
        let (mut doc, task, context) = a_task_to_annotate();
        let before = doc.version();

        apply_ops(
            &mut doc,
            &[EditOperation::ClearAttribute {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                key: "koru.priority".into(),
            }],
            &context,
        )
        .unwrap();

        assert_eq!(doc.version(), before, "an absent key left an operation");
        assert!(published_attributes(&doc, task).is_empty());
    }

    #[test]
    fn clear_attribute_removes_the_value_that_is_there() {
        let (mut doc, task, context) = a_task_to_annotate();
        doc.set_attribute(task, "koru.priority", AttrValue::Text("low".into()))
            .unwrap();
        assert!(published_value(&doc, task, "koru.priority").is_some());

        apply_ops(
            &mut doc,
            &[EditOperation::ClearAttribute {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                key: "koru.priority".into(),
            }],
            &context,
        )
        .unwrap();

        assert_eq!(published_value(&doc, task, "koru.priority"), None);
    }

    // ------------------------------- content- and priority-changed activity (T-0)
    //
    // Two events this host owes that no command produces:
    // `task.content.changed`, whose burst is **the call**, and
    // `task.priority.changed`, which the priority door owes when — and only
    // when — the target is already a task ([ADR-286]).

    /// The events one batch would append, in delivery order.
    fn appended(wrapper: &tasks::Wrapper) -> Vec<&'static str> {
        wrapper
            .attempts()
            .into_iter()
            .map(|attempt| attempt.event)
            .collect()
    }

    /// Make the fixture's row an actual task. `Node::task` is a *kind*; a task
    /// is a marker or a due, and nothing else (ADR-272).
    fn make_it_a_task(doc: &mut WorkspaceDoc, node: NodeId) {
        koru_core::tasks::ensure_task(doc, node, koru_core::attributes::TaskReason::Explicit)
            .unwrap();
    }

    /// **MCP's burst is the call**, and the wrapper dedupes by task: forty
    /// rewrites of one task's text are one `task.content.changed`, never forty.
    #[test]
    fn a_batch_of_text_edits_on_one_task_is_one_content_event() {
        let (mut doc, task, context) = a_task_to_annotate();
        make_it_a_task(&mut doc, task);

        let mut operations = Vec::new();
        let mut expected = "Ship it".to_string();
        for step in 0..40 {
            let next = format!("Ship it {step}");
            operations.push(EditOperation::ReplaceText {
                node_id: task.to_storage(),
                expected: std::mem::replace(&mut expected, next.clone()),
                text: next,
            });
        }

        let wrapper = apply_ops(&mut doc, &operations, &context).unwrap();
        assert_eq!(appended(&wrapper), vec!["task.content.changed"]);
    }

    /// Two tasks in one call are one event each: the rule is *one per task per
    /// burst*, with the call standing in for the burst.
    #[test]
    fn two_tasks_edited_in_one_call_are_one_content_event_each() {
        let (mut doc, first, second, context) = two_tasks_to_annotate();
        make_it_a_task(&mut doc, first);
        make_it_a_task(&mut doc, second);

        let wrapper = apply_ops(
            &mut doc,
            &[
                EditOperation::ReplaceText {
                    node_id: first.to_storage(),
                    expected: "Ship it".into(),
                    text: "Ship it today".into(),
                },
                EditOperation::ReplaceText {
                    node_id: second.to_storage(),
                    expected: "Also".into(),
                    text: "Also this".into(),
                },
            ],
            &context,
        )
        .unwrap();
        assert_eq!(
            appended(&wrapper),
            vec!["task.content.changed", "task.content.changed"]
        );
    }

    /// **Never the text**, and never a plain row. The event has no attrs at all
    /// and no correlation — it belongs to no command, so it is its own chain
    /// root — and a paragraph that is not a task owes nothing.
    #[test]
    fn a_content_event_carries_nothing_and_a_plain_row_owes_none() {
        let (mut doc, task, plain, context) = two_tasks_to_annotate();
        make_it_a_task(&mut doc, task);

        let wrapper = apply_ops(
            &mut doc,
            &[
                EditOperation::ReplaceText {
                    node_id: plain.to_storage(),
                    expected: "Also".into(),
                    text: "Also this".into(),
                },
                EditOperation::ReplaceText {
                    node_id: task.to_storage(),
                    expected: "Ship it".into(),
                    text: "Ship it today".into(),
                },
            ],
            &context,
        )
        .unwrap();

        let attempts = wrapper.attempts();
        assert_eq!(attempts.len(), 1, "only the task's own text counts");
        assert_eq!(attempts[0].event, "task.content.changed");
        assert!(
            attempts[0].attrs.is_empty(),
            "no attrs at all — the text is what it must never carry"
        );
        assert_eq!(attempts[0].correlation, None, "its own chain root");
        assert_eq!(attempts[0].subject.node, task.as_uuid());
    }

    /// Re-asserting the text a task already has writes no op, and an event
    /// saying somebody was editing when nobody was is worse than none.
    #[test]
    fn rewriting_a_task_with_its_own_text_owes_no_content_event() {
        let (mut doc, task, context) = a_task_to_annotate();
        make_it_a_task(&mut doc, task);

        let wrapper = apply_ops(
            &mut doc,
            &[EditOperation::ReplaceText {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                text: "Ship it".into(),
            }],
            &context,
        )
        .unwrap();
        assert!(wrapper.is_empty());
    }

    /// A pill is an anchor character in the row's own text, so inserting one
    /// into a task is that task's content changing.
    #[test]
    fn inserting_a_pill_into_a_task_is_a_content_change() {
        let (mut doc, task, second, mut context) = two_tasks_to_annotate();
        make_it_a_task(&mut doc, task);
        let document = StoreDocumentId::new();
        context.documents.insert(document, "Plan".into());

        let wrapper = apply_ops(
            &mut doc,
            &[EditOperation::InsertInline {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                at_utf8: 4,
                inline: InlineValue::Document {
                    document_id: document.to_storage(),
                    node_id: None,
                    label: None,
                },
            }],
            &context,
        )
        .unwrap();
        assert_eq!(appended(&wrapper), vec!["task.content.changed"]);
        let _ = second;
    }

    /// **The priority door**: `koru.priority` on a node that is already a task
    /// takes the ordinary attribute door — no command, no marker, no lifecycle —
    /// and then owes one `task.priority.changed`, `set` the first time.
    #[test]
    fn priority_on_a_task_owes_one_event() {
        let (mut doc, task, context) = a_task_to_annotate();
        make_it_a_task(&mut doc, task);

        let wrapper = apply_ops(
            &mut doc,
            &[EditOperation::SetAttribute {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                key: "koru.priority".into(),
                value: AttributeValue::Text {
                    value: "high".into(),
                },
            }],
            &context,
        )
        .unwrap();

        let attempts = wrapper.attempts();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].event, "task.priority.changed");
        assert_eq!(
            attempts[0].attrs,
            vec![
                ("change", tasks::AttrToken::Token("set")),
                ("priority", tasks::AttrToken::Token("high")),
            ]
        );
        assert_eq!(
            published_value(&doc, task, "koru.priority"),
            Some(json!({"type":"text", "value":"high"})),
            "and the ordinary door still did the write"
        );
    }

    /// **Priority on a plain node emits nothing** — ADR-273's table says *only
    /// when the target already `is_task`*, and a priority is not a thing that
    /// makes one.
    #[test]
    fn priority_on_a_plain_node_owes_nothing() {
        let (mut doc, task, context) = a_task_to_annotate();

        let wrapper = apply_ops(
            &mut doc,
            &[EditOperation::SetAttribute {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                key: "koru.priority".into(),
                value: AttributeValue::Text {
                    value: "high".into(),
                },
            }],
            &context,
        )
        .unwrap();

        assert!(wrapper.is_empty(), "a plain row owes no task activity");
        assert!(
            !koru_core::tasks::is_task_node(&doc, task).unwrap(),
            "and writing a priority did not make it a task"
        );
    }

    /// The three tokens the row declares, over the door's two halves: `set`,
    /// then `changed`, then `cleared` — and `priority` absent once it is gone,
    /// because absence means removed and never `low`.
    #[test]
    fn the_priority_door_names_set_changed_and_cleared() {
        let (mut doc, task, context) = a_task_to_annotate();
        make_it_a_task(&mut doc, task);
        let set = |value: &str| EditOperation::SetAttribute {
            node_id: task.to_storage(),
            expected: "Ship it".into(),
            key: "koru.priority".into(),
            value: AttributeValue::Text {
                value: value.into(),
            },
        };

        for (operation, change, priority) in [
            (set("low"), "set", Some("low")),
            (set("urgent"), "changed", Some("urgent")),
            (
                EditOperation::ClearAttribute {
                    node_id: task.to_storage(),
                    expected: "Ship it".into(),
                    key: "koru.priority".into(),
                },
                "cleared",
                None,
            ),
        ] {
            let wrapper = apply_ops(&mut doc, &[operation], &context).unwrap();
            let attempts = wrapper.attempts();
            assert_eq!(attempts.len(), 1, "{change}");
            assert_eq!(attempts[0].event, "task.priority.changed");
            let mut expected = vec![("change", tasks::AttrToken::Token(change))];
            if let Some(priority) = priority {
                expected.push(("priority", tasks::AttrToken::Token(priority)));
            }
            assert_eq!(attempts[0].attrs, expected, "{change}");
        }
    }

    /// A value the task already had writes no op, so it owes no event either.
    /// The guard is the comparison, not the key.
    #[test]
    fn re_asserting_a_priority_owes_nothing() {
        let (mut doc, task, context) = a_task_to_annotate();
        make_it_a_task(&mut doc, task);
        doc.set_attribute(task, "koru.priority", AttrValue::Text("high".into()))
            .unwrap();

        let wrapper = apply_ops(
            &mut doc,
            &[EditOperation::SetAttribute {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                key: "koru.priority".into(),
                value: AttributeValue::Text {
                    value: "high".into(),
                },
            }],
            &context,
        )
        .unwrap();
        assert!(wrapper.is_empty());
    }

    /// Clearing a priority a task never had is already a guarded no-op at the
    /// attribute door; it owes no event for the same reason.
    #[test]
    fn clearing_a_priority_a_task_never_had_owes_nothing() {
        let (mut doc, task, context) = a_task_to_annotate();
        make_it_a_task(&mut doc, task);

        let wrapper = apply_ops(
            &mut doc,
            &[EditOperation::ClearAttribute {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                key: "koru.priority".into(),
            }],
            &context,
        )
        .unwrap();
        assert!(wrapper.is_empty());
    }

    /// A key nothing defines is refused on both doors. A `clear` that shrugged
    /// would be indistinguishable from a successful one, which is exactly the
    /// "never turn a bad field into a successful no-op" rule.
    #[test]
    fn an_undefined_key_is_refused_by_both_set_and_clear() {
        for operation in [
            EditOperation::SetAttribute {
                node_id: String::new(),
                expected: "Ship it".into(),
                key: "koru.invented".into(),
                value: AttributeValue::Text { value: "x".into() },
            },
            EditOperation::ClearAttribute {
                node_id: String::new(),
                expected: "Ship it".into(),
                key: "koru.invented".into(),
            },
        ] {
            let (mut doc, task, context) = a_task_to_annotate();
            let operation = match operation {
                EditOperation::SetAttribute {
                    expected,
                    key,
                    value,
                    ..
                } => EditOperation::SetAttribute {
                    node_id: task.to_storage(),
                    expected,
                    key,
                    value,
                },
                EditOperation::ClearAttribute { expected, key, .. } => {
                    EditOperation::ClearAttribute {
                        node_id: task.to_storage(),
                        expected,
                        key,
                    }
                }
                other => other,
            };
            let error = apply_ops(&mut doc, &[operation], &context)
                .expect_err("an invented key names nothing");
            assert_eq!(error.code, "unknown_attribute");
            assert!(error.message.contains("koru.invented"));
        }
    }

    /// The model owns validation and its refusal reaches the wire intact — a
    /// structured code and the key, never a panic and never a silent write.
    #[test]
    fn a_value_that_does_not_fit_its_key_is_a_structured_error() {
        for (key, value, expected) in [
            // A floating wall clock offered to a *shared* deadline (ADR-183).
            (
                "koru.due",
                AttributeValue::DatetimeFloating {
                    wall: "2026-08-30T09:00:00".into(),
                },
                "floating wall clock",
            ),
            // A token outside the select's options.
            (
                "koru.priority",
                AttributeValue::Text {
                    value: "screaming".into(),
                },
                "options",
            ),
            // The wrong type entirely.
            (
                "koru.color",
                AttributeValue::Number { value: 4.0 },
                "expects a select value",
            ),
            // A personal key never lands on a shared node.
            (
                "koru.plan",
                AttributeValue::DatetimeFloating {
                    wall: "2026-08-30T09:00:00".into(),
                },
                "personal",
            ),
            // A tag attribute takes an id, not a name.
            (
                "koru.tag",
                AttributeValue::Text {
                    value: "home".into(),
                },
                "tag id",
            ),
        ] {
            let (mut doc, task, context) = a_task_to_annotate();
            let error = apply_ops(
                &mut doc,
                &[EditOperation::SetAttribute {
                    node_id: task.to_storage(),
                    expected: "Ship it".into(),
                    key: key.into(),
                    value,
                }],
                &context,
            )
            .unwrap_err();

            assert_eq!(error.code, "attribute_rejected", "{key}");
            assert!(
                error.message.contains(expected),
                "{key}: {:?} does not say {expected}",
                error.message
            );
            assert_eq!(error.details.as_ref().unwrap()["key"], key);
            assert!(
                published_value(&doc, task, key).is_none(),
                "{key} was written anyway"
            );
        }
    }

    #[test]
    fn a_malformed_temporal_value_is_refused_before_the_model_sees_it() {
        for value in [
            AttributeValue::Date {
                value: "2026-9-1".into(),
            },
            AttributeValue::DatetimeFloating {
                wall: "2026-08-30 09:00".into(),
            },
            AttributeValue::DatetimeZoned {
                instant_ms: 0,
                timezone: "+02:00".into(),
            },
        ] {
            let error = value
                .to_core()
                .expect_err("a malformed value must not parse");
            assert_eq!(error.code, "invalid_request");
        }
    }

    // ------------------------------------------------------------------ tags

    #[test]
    fn add_tag_by_a_name_that_exists_reuses_that_record() {
        let mut system = SystemDoc::create().unwrap();
        let existing = system.create_tag("home", None, 1).unwrap();

        let operations = [EditOperation::AddTag {
            node_id: NodeId::new().to_storage(),
            expected: String::new(),
            // Folded for identity, preserved for display: `#HOME` is `#home`.
            tag: Some("HOME".into()),
            tag_id: None,
        }];
        let (context, created) = AttrRequests::read(&operations)
            .unwrap()
            .resolve(&mut system, 2)
            .unwrap();

        assert_eq!(context.tags.get("home"), Some(&existing));
        assert!(!created, "reuse must not write to the tag table");
        assert_eq!(system.tags().len(), 1);
        assert_eq!(
            system.tags()[0].name,
            "home",
            "display stays as first typed"
        );
    }

    #[test]
    fn add_tag_by_a_new_name_creates_it() {
        let mut system = SystemDoc::create().unwrap();
        let operations = [EditOperation::AddTag {
            node_id: NodeId::new().to_storage(),
            expected: String::new(),
            tag: Some("mvp".into()),
            tag_id: None,
        }];

        let (context, created) = AttrRequests::read(&operations)
            .unwrap()
            .resolve(&mut system, 1)
            .unwrap();

        assert!(created);
        let id = *context.tags.get("mvp").expect("the tag was resolved");
        assert_eq!(
            system.tag_entry(id).map(|entry| entry.name),
            Some("mvp".into())
        );
    }

    /// A `/`-path creates its parents, so `fun` is a real tag in its own right
    /// rather than a prefix of somebody else's name (ADR-185). That belongs to
    /// `create_tag`; this pins that the MCP route goes through it.
    #[test]
    fn add_tag_by_a_path_creates_its_parents() {
        let mut system = SystemDoc::create().unwrap();
        let operations = [EditOperation::AddTag {
            node_id: NodeId::new().to_storage(),
            expected: String::new(),
            tag: Some("fun/home".into()),
            tag_id: None,
        }];

        AttrRequests::read(&operations)
            .unwrap()
            .resolve(&mut system, 1)
            .unwrap();

        let names: Vec<String> = system.tags().into_iter().map(|entry| entry.name).collect();
        assert_eq!(names, vec!["fun".to_string(), "fun/home".into()]);
    }

    /// **A removal never writes to the tag table on its way past.** Naming a
    /// tag nobody has ever created removes nothing and creates nothing; the
    /// alternative — create-then-remove — would leave a workspace collecting
    /// tags from an agent's typos.
    #[test]
    fn remove_tag_by_an_unknown_name_creates_nothing() {
        let mut system = SystemDoc::create().unwrap();
        let operations = [EditOperation::RemoveTag {
            node_id: NodeId::new().to_storage(),
            expected: String::new(),
            tag: Some("never-existed".into()),
            tag_id: None,
        }];

        let asked = AttrRequests::read(&operations).unwrap();
        assert!(!asked.creates(), "a removal must not ask for creation");
        let (context, created) = asked.resolve(&mut system, 1).unwrap();

        assert!(context.tags.is_empty());
        assert!(!created);
        assert!(system.tags().is_empty());
    }

    #[test]
    fn remove_tag_removes_only_the_named_one() {
        let (mut doc, task, mut context) = a_task_to_annotate();
        let keep = TagId::new();
        let go = TagId::new();
        doc.add_tag(task, keep).unwrap();
        doc.add_tag(task, go).unwrap();
        context.attrs.tags.insert("go".into(), go);

        apply_ops(
            &mut doc,
            &[EditOperation::RemoveTag {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                tag: Some("go".into()),
                tag_id: None,
            }],
            &context,
        )
        .unwrap();

        let left: Vec<TagId> = doc.read(task).unwrap().tag_ids().collect();
        assert_eq!(left, vec![keep]);
    }

    /// The id form is accepted because a `koru.tag` value read off one node is
    /// already an id, and refusing it would make an agent invent a name for a
    /// tag it can see.
    #[test]
    fn a_tag_can_also_be_named_by_its_id() {
        let (mut doc, task, context) = a_task_to_annotate();
        let id = TagId::new();

        apply_ops(
            &mut doc,
            &[EditOperation::AddTag {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                tag: None,
                tag_id: Some(id.to_storage()),
            }],
            &context,
        )
        .unwrap();

        assert_eq!(
            doc.read(task).unwrap().tag_ids().collect::<Vec<_>>(),
            vec![id]
        );
    }

    #[test]
    fn a_tag_operation_takes_exactly_one_of_tag_and_tag_id() {
        let id = TagId::new().to_storage();
        for (tag, tag_id, expected) in [
            (Some("home".to_string()), Some(id.clone()), "never both"),
            (None, None, "needs a tag name"),
            (Some("///".to_string()), None, "empty once separators"),
            (None, Some("not-a-uuid".to_string()), "not a UUID"),
        ] {
            let (mut doc, task, context) = a_task_to_annotate();
            let error = apply_ops(
                &mut doc,
                &[EditOperation::AddTag {
                    node_id: task.to_storage(),
                    expected: "Ship it".into(),
                    tag,
                    tag_id,
                }],
                &context,
            )
            .expect_err("an ambiguous tag reference must be refused");
            assert!(
                error.message.contains(expected),
                "{:?} does not say {expected}",
                error.message
            );
        }
    }

    /// A batch that names no tag and no custom key never opens the system
    /// document. That is the cost control: an ordinary text edit stays one
    /// store round trip.
    #[test]
    fn an_ordinary_batch_asks_the_system_document_nothing() {
        let operations = [
            EditOperation::ReplaceText {
                node_id: NodeId::new().to_storage(),
                expected: String::new(),
                text: "hello".into(),
            },
            // A *system* key answers from the registry in code, so it is not a
            // question for the workspace either.
            EditOperation::SetAttribute {
                node_id: NodeId::new().to_storage(),
                expected: String::new(),
                key: "koru.due".into(),
                value: AttributeValue::Date {
                    value: "2026-08-30".into(),
                },
            },
            EditOperation::AddTag {
                node_id: NodeId::new().to_storage(),
                expected: String::new(),
                tag: None,
                tag_id: Some(TagId::new().to_storage()),
            },
        ];

        assert!(AttrRequests::read(&operations).unwrap().is_empty());
    }

    /// A custom key resolves through the *workspace's* definition, which lives
    /// in the system document — the seam that keeps a workspace's own
    /// vocabulary out of this build's registry.
    #[test]
    fn a_custom_key_resolves_through_the_workspace_definition() {
        let mut system = SystemDoc::create().unwrap();
        system
            .define(&AttributeDefinition::custom(
                "team.effort",
                "Effort",
                koru_core::ValueType::Number,
            ))
            .unwrap();

        let (mut doc, task, mut inline) = a_task_to_annotate();
        let operations = [EditOperation::SetAttribute {
            node_id: task.to_storage(),
            expected: "Ship it".into(),
            key: "team.effort".into(),
            value: AttributeValue::Number { value: 5.0 },
        }];
        let asked = AttrRequests::read(&operations).unwrap();
        assert_eq!(asked.keys, vec!["team.effort"]);
        inline.attrs = asked.resolve(&mut system, 1).unwrap().0;

        apply_ops(&mut doc, &operations, &inline).unwrap();

        assert_eq!(
            published_value(&doc, task, "team.effort"),
            Some(json!({"type":"number", "value":5.0}))
        );
    }

    /// A custom key the workspace has not defined is `unknown_attribute`, not
    /// a value written under a key nothing will ever read.
    #[test]
    fn a_custom_key_the_workspace_has_not_defined_is_refused() {
        let mut system = SystemDoc::create().unwrap();
        let operations = [EditOperation::SetAttribute {
            node_id: NodeId::new().to_storage(),
            expected: String::new(),
            key: "team.effort".into(),
            value: AttributeValue::Number { value: 5.0 },
        }];

        let error = AttrRequests::read(&operations)
            .unwrap()
            .resolve(&mut system, 1)
            .expect_err("an undefined workspace key names nothing");

        assert_eq!(error.code, "unknown_attribute");
    }

    // ------------------------------------ a rejected batch writes nothing (Q41)

    /// One batch, run the way `edit_document` runs one: names resolved on a
    /// **fork** of the system document, the whole batch proved against a fork
    /// of the content document, and only then the table written and the real
    /// pass applied.
    ///
    /// The backend reaches that order through `PendingAttrs`, whose `commit`
    /// is the only door to a write; this is the same three calls in the same
    /// sequence, with no store to dial. What it therefore cannot catch is the
    /// backend calling them in a different order — which is why `commit` is
    /// one method with one caller rather than a step somebody can move.
    fn run_batch(
        doc: &mut WorkspaceDoc,
        inline: &mut InlineContext,
        system: &mut SystemDoc,
        operations: &[EditOperation],
    ) -> Result<(), ToolError> {
        let asked = AttrRequests::read(operations)?;
        let (provisional, _) = asked.resolve_on_fork(system, 1)?;
        inline.attrs = provisional;

        let mut validation = doc.fork_for_read();
        apply_ops(&mut validation, operations, inline)?;

        let (committed, _) = asked.resolve(system, 2)?;
        inline.attrs = committed;
        apply_ops(doc, operations, inline).map(|_| ())
    }

    /// A tag by name, then a priority the model refuses — the batch Q41 was
    /// filed about.
    fn a_tag_then_a_bad_priority(task: NodeId, tag: &str) -> [EditOperation; 2] {
        [
            EditOperation::AddTag {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                tag: Some(tag.into()),
                tag_id: None,
            },
            EditOperation::SetAttribute {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                key: "koru.priority".into(),
                // `koru.priority` is a select over four tokens, so a number is
                // refused by the definition rather than by this layer.
                value: AttributeValue::Number { value: 3.0 },
            },
        ]
    }

    /// **Q41, closed.** A batch that names a tag nobody has created and then
    /// fails validation leaves the tag table exactly as it was. The assertion
    /// is on the table, not on the error: the error was never in doubt.
    #[test]
    fn a_rejected_batch_creates_no_tag() {
        let mut system = SystemDoc::create().unwrap();
        let (mut doc, task, mut inline) = a_task_to_annotate();

        let error = run_batch(
            &mut doc,
            &mut inline,
            &mut system,
            &a_tag_then_a_bad_priority(task, "launch"),
        )
        .expect_err("a priority the definition refuses fails the batch");

        assert_eq!(error.code, "attribute_rejected");
        assert!(
            system.tags().is_empty(),
            "a rejected batch left {:?} behind",
            system
                .tags()
                .into_iter()
                .map(|t| t.name)
                .collect::<Vec<_>>()
        );
        assert!(doc.read(task).unwrap().tag_ids().next().is_none());
    }

    /// The other half of the same trade: the batch that succeeds still creates
    /// the tag, **once** — not once per pass — and the node carries the id the
    /// table actually holds rather than the fork's.
    #[test]
    fn the_same_batch_succeeding_creates_the_tag_once() {
        let mut system = SystemDoc::create().unwrap();
        let (mut doc, task, mut inline) = a_task_to_annotate();
        let mut operations = a_tag_then_a_bad_priority(task, "launch");
        operations[1] = EditOperation::SetAttribute {
            node_id: task.to_storage(),
            expected: "Ship it".into(),
            key: "koru.priority".into(),
            value: AttributeValue::Text {
                value: "urgent".into(),
            },
        };

        run_batch(&mut doc, &mut inline, &mut system, &operations).unwrap();

        assert_eq!(system.tags().len(), 1, "the tag was created twice");
        let carried: Vec<TagId> = doc.read(task).unwrap().tag_ids().collect();
        assert_eq!(carried.len(), 1);
        assert_eq!(
            system.tag_entry(carried[0]).map(|entry| entry.name),
            Some("launch".into()),
            "the node carries an id the table does not know — the fork's"
        );
        assert_eq!(
            published_value(&doc, task, "koru.priority"),
            Some(json!({"type":"text", "value":"urgent"}))
        );
    }

    /// A rejected batch must not disturb a tag that was already there either:
    /// resolution happens on a copy, so reuse costs the record nothing.
    #[test]
    fn a_rejected_batch_leaves_an_existing_tag_alone() {
        let mut system = SystemDoc::create().unwrap();
        let existing = system.create_tag("launch", None, 1).unwrap();
        let (mut doc, task, mut inline) = a_task_to_annotate();

        run_batch(
            &mut doc,
            &mut inline,
            &mut system,
            // Mis-cased on purpose: the fold key matches, so this is the reuse
            // path rather than the creation one.
            &a_tag_then_a_bad_priority(task, "LAUNCH"),
        )
        .expect_err("the batch is refused");

        let left: Vec<(TagId, String)> = system
            .tags()
            .into_iter()
            .map(|entry| (entry.id, entry.name))
            .collect();
        assert_eq!(left, vec![(existing, "launch".to_string())]);
    }

    /// A `/`-path creates its parents, and that is a write like any other: it
    /// waits for the batch to be proved (ADR-185, ADR-190 amended).
    #[test]
    fn a_path_creates_its_parents_only_when_the_batch_survives() {
        let names = |system: &SystemDoc| -> Vec<String> {
            system.tags().into_iter().map(|entry| entry.name).collect()
        };

        let mut refused = SystemDoc::create().unwrap();
        let (mut doc, task, mut inline) = a_task_to_annotate();
        run_batch(
            &mut doc,
            &mut inline,
            &mut refused,
            &a_tag_then_a_bad_priority(task, "fun/home"),
        )
        .expect_err("the batch is refused");
        assert!(names(&refused).is_empty(), "a parent outlived its batch");

        let mut kept = SystemDoc::create().unwrap();
        let (mut doc, task, mut inline) = a_task_to_annotate();
        run_batch(
            &mut doc,
            &mut inline,
            &mut kept,
            &[EditOperation::AddTag {
                node_id: task.to_storage(),
                expected: "Ship it".into(),
                tag: Some("fun/home".into()),
                tag_id: None,
            }],
        )
        .unwrap();
        assert_eq!(names(&kept), vec!["fun".to_string(), "fun/home".into()]);
    }

    /// The fork is a copy, not a handle: resolving on one answers the ids the
    /// real table would answer for tags that exist, mints throwaway ones for
    /// tags that do not, and writes to neither.
    #[test]
    fn resolving_on_a_fork_reuses_but_never_writes() {
        let mut system = SystemDoc::create().unwrap();
        let existing = system.create_tag("home", None, 1).unwrap();
        let operations = [
            EditOperation::AddTag {
                node_id: NodeId::new().to_storage(),
                expected: String::new(),
                tag: Some("home".into()),
                tag_id: None,
            },
            EditOperation::AddTag {
                node_id: NodeId::new().to_storage(),
                expected: String::new(),
                tag: Some("away".into()),
                tag_id: None,
            },
        ];

        let (context, created) = AttrRequests::read(&operations)
            .unwrap()
            .resolve_on_fork(&system, 2)
            .unwrap();

        assert_eq!(context.tags.get("home"), Some(&existing));
        assert!(created, "the fork had to mint one, which is what commits");
        assert_eq!(
            system.tags().len(),
            1,
            "the fork wrote through to the real table"
        );
        let provisional = context.tags.get("away").copied().unwrap();
        assert!(
            system.tag_entry(provisional).is_none(),
            "a fork's id must not name a record anybody can see"
        );
    }

    #[test]
    fn the_edit_schema_publishes_every_attribute_operation_and_value_shape() {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "edit_document")
            .expect("edit_document tool");
        let schema = serde_json::to_value(&tool.input_schema).expect("JSON schema");
        let encoded = serde_json::to_string(&schema).expect("schema text");

        for operation in ["set_attribute", "clear_attribute", "add_tag", "remove_tag"] {
            assert!(
                encoded.contains(&format!("\"{operation}\"")),
                "missing operation {operation}"
            );
        }
        // The three temporal shapes are three names on the wire, so a tool
        // reading one knows what it has without probing for a zone.
        for shape in [
            "date",
            "datetime_floating",
            "datetime_zoned",
            "text",
            "number",
            "bool",
            "url",
        ] {
            assert!(encoded.contains(&format!("\"{shape}\"")), "missing {shape}");
        }
        assert!(!encoded.contains("$ref"), "value shapes must be inline");
    }

    /// Seeding a body deliberately does not take attribute operations. A tag
    /// resolved for a document that does not exist yet would have to write to
    /// the system document before the create had happened, and the create is
    /// the one call in this surface that replays. Written down rather than
    /// missed — `crates/koru-mcp-server/MCP.md` says the same.
    #[test]
    fn creation_does_not_offer_the_attribute_operations() {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "create_document")
            .expect("create_document tool");
        let encoded =
            serde_json::to_string(&serde_json::to_value(&tool.input_schema).unwrap()).unwrap();

        for operation in ["set_attribute", "clear_attribute", "add_tag", "remove_tag"] {
            assert!(
                !encoded.contains(&format!("\"{operation}\"")),
                "seeding published {operation}"
            );
        }
    }

    #[test]
    fn a_concurrent_split_cannot_make_move_target_the_wrong_surviving_half() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Source").unwrap())
            .unwrap();
        let original = doc
            .insert(Parent::Node(root), Node::paragraph("Japanese").unwrap())
            .unwrap();
        let _new_right_half = doc.split(original, 0).unwrap();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        let error = apply_ops(
            &mut doc,
            &[EditOperation::Move {
                node_id: original.to_storage(),
                expected: "Japanese".into(),
                parent_id: root.to_storage(),
                after_node_id: None,
            }],
            &context,
        )
        .expect_err("the old UUID now names the empty left half");

        assert_eq!(error.code, "precondition_failed");
        assert_eq!(error.details.as_ref().unwrap()["retry"], "read_document");
    }

    #[test]
    fn stale_node_errors_expose_the_complete_uuid_and_a_retry() {
        let id = NodeId::new();
        let error = ToolError::from(koru_core::StoreError::NoSuchNode(id));
        assert_eq!(error.code, "stale_node");
        assert!(error.message.contains(&id.to_storage()));
        assert_eq!(error.details.as_ref().unwrap()["node_id"], id.to_storage());
        assert_eq!(error.details.as_ref().unwrap()["retry"], "read_document");
    }

    #[test]
    fn insert_inline_builds_a_routed_document_pill() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Source").unwrap())
            .unwrap();
        let paragraph = doc
            .insert(Parent::Node(root), Node::paragraph("héllo").unwrap())
            .unwrap();
        let profile = CoreProfileId::new();
        let workspace = StoreWorkspaceId::new();
        let target = StoreDocumentId::new();
        let context = InlineContext {
            profile,
            workspace: CoreWorkspaceId::from_uuid(workspace.as_uuid()),
            documents: HashMap::from([(target, "Target".into())]),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        apply_ops(
            &mut doc,
            &[EditOperation::InsertInline {
                node_id: paragraph.to_storage(),
                expected: "héllo".into(),
                at_utf8: 3,
                inline: InlineValue::Document {
                    document_id: target.to_storage(),
                    node_id: None,
                    label: None,
                },
            }],
            &context,
        )
        .unwrap();

        let stored = doc.read(paragraph).unwrap();
        assert_eq!(stored.content.as_text(), Some("hé\u{2060}llo"));
        let attr = stored.content.attrs().first().expect("document attribute");
        assert_eq!(attr.role, "document");
        assert_eq!(attr.label.as_deref(), Some("Target"));
        let reference = attr
            .as_document_reference()
            .expect("stable document reference");
        assert_eq!(reference.document.as_uuid(), target.as_uuid());
        assert_eq!(reference.node, None);
        assert_eq!(
            doc.runs(paragraph)
                .unwrap()
                .iter()
                .filter(|run| run.pill.is_some())
                .count(),
            1
        );
    }

    /// The other half of the `@` picker's `#`: an assistant can author the same
    /// deep link a person can.
    #[test]
    fn insert_inline_accepts_a_node_id() {
        let profile = CoreProfileId::new();
        let workspace = StoreWorkspaceId::new();
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Source").unwrap())
            .unwrap();
        let paragraph = doc
            .insert(Parent::Node(root), Node::paragraph("héllo").unwrap())
            .unwrap();
        let target = StoreDocumentId::new();
        let block = NodeId::new();
        let context = InlineContext {
            profile,
            workspace: CoreWorkspaceId::from_uuid(workspace.as_uuid()),
            documents: HashMap::from([(target, "Target".into())]),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        apply_ops(
            &mut doc,
            &[EditOperation::InsertInline {
                node_id: paragraph.to_storage(),
                expected: "héllo".into(),
                at_utf8: 3,
                inline: InlineValue::Document {
                    document_id: target.to_storage(),
                    node_id: Some(block.to_storage()),
                    label: None,
                },
            }],
            &context,
        )
        .unwrap();

        let reference = doc
            .read(paragraph)
            .unwrap()
            .content
            .attrs()
            .first()
            .and_then(|attr| attr.as_document_reference())
            .expect("stable document reference");
        assert_eq!(reference.document.as_uuid(), target.as_uuid());
        assert_eq!(reference.node, Some(block));
    }

    /// **Shape is checked, membership is not.** Whether the block lives in that
    /// document needs its CRDT state, which this context has not got -- and an
    /// unresolvable node degrades to opening the document, which a stale
    /// reference already promises. A non-uuid names nothing at all, though, and
    /// refusing it is cheaper than a pill that can never resolve.
    #[test]
    fn insert_inline_rejects_a_node_id_that_is_not_a_uuid() {
        let profile = CoreProfileId::new();
        let workspace = StoreWorkspaceId::new();
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Source").unwrap())
            .unwrap();
        let paragraph = doc
            .insert(Parent::Node(root), Node::paragraph("héllo").unwrap())
            .unwrap();
        let target = StoreDocumentId::new();
        let context = InlineContext {
            profile,
            workspace: CoreWorkspaceId::from_uuid(workspace.as_uuid()),
            documents: HashMap::from([(target, "Target".into())]),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };

        let refused = apply_ops(
            &mut doc,
            &[EditOperation::InsertInline {
                node_id: paragraph.to_storage(),
                expected: "héllo".into(),
                at_utf8: 3,
                inline: InlineValue::Document {
                    document_id: target.to_storage(),
                    node_id: Some("not-a-uuid".into()),
                    label: None,
                },
            }],
            &context,
        );

        assert!(refused.is_err());
    }

    #[test]
    fn insert_inline_builds_a_routed_attachment_pill() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Source").unwrap())
            .unwrap();
        let paragraph = doc
            .insert(Parent::Node(root), Node::paragraph("").unwrap())
            .unwrap();
        let file = StoreDocumentId::new();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::new(),
            files: HashMap::from([(file, "report.pdf".into())]),
            attrs: AttrContext::default(),
        };
        apply_ops(
            &mut doc,
            &[EditOperation::InsertInline {
                node_id: paragraph.to_storage(),
                expected: "".into(),
                at_utf8: 0,
                inline: InlineValue::Attachment {
                    file_id: file.to_storage(),
                    label: None,
                },
            }],
            &context,
        )
        .unwrap();
        let node = doc.read(paragraph).unwrap();
        let attr = node.content.attrs().first().unwrap();
        assert_eq!(attr.role, "attachment");
        assert_eq!(attr.label.as_deref(), Some("report.pdf"));
        assert_eq!(attr.as_ref_target().unwrap().node.as_uuid(), file.as_uuid());
    }

    #[test]
    fn insert_inline_rejects_a_non_boundary_utf8_offset() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Source").unwrap())
            .unwrap();
        let paragraph = doc
            .insert(Parent::Node(root), Node::paragraph("hé").unwrap())
            .unwrap();
        let target = StoreDocumentId::new();
        let context = InlineContext {
            profile: CoreProfileId::new(),
            workspace: CoreWorkspaceId::new(),
            documents: HashMap::from([(target, "Target".into())]),
            files: HashMap::new(),
            attrs: AttrContext::default(),
        };
        let error = apply_ops(
            &mut doc,
            &[EditOperation::InsertInline {
                node_id: paragraph.to_storage(),
                expected: "hé".into(),
                at_utf8: 2,
                inline: InlineValue::Document {
                    document_id: target.to_storage(),
                    node_id: None,
                    label: None,
                },
            }],
            &context,
        )
        .expect_err("the middle of a UTF-8 scalar is not an insertion point");

        assert_eq!(error.code, "invalid_request");
        assert!(error.message.contains("UTF-8 byte boundary"));
    }

    #[test]
    fn replacing_a_subtree_with_a_pill_preserves_its_slot() {
        let mut doc = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Source").unwrap())
            .unwrap();
        let before = doc
            .insert(Parent::Node(root), Node::paragraph("before").unwrap())
            .unwrap();
        let source = doc
            .insert(Parent::Node(root), Node::paragraph("move me").unwrap())
            .unwrap();
        doc.insert(Parent::Node(source), Node::paragraph("nested").unwrap())
            .unwrap();
        let after = doc
            .insert(Parent::Node(root), Node::paragraph("after").unwrap())
            .unwrap();
        let attr = Attr::labelled("document", AttrValue::Text("Target".into()), "Target");

        let replacement = doc.replace_subtree_with_pill(source, &attr).unwrap();
        assert_eq!(
            doc.children(Parent::Node(root)).unwrap(),
            vec![before, replacement, after]
        );
        assert!(doc.read(source).is_err());
        assert_eq!(doc.read(replacement).unwrap().content.attrs().len(), 1);
    }

    /// The multi-root / existing-target / under-heading fields are additive:
    /// a request written against the original single-subtree contract still
    /// parses, so no existing caller breaks.
    #[test]
    fn an_extract_request_without_the_new_fields_still_parses() {
        let a: ExtractToDocumentRequest = serde_json::from_value(json!({
            "workspace_id": StoreWorkspaceId::new().to_storage(),
            "source_document_id": StoreDocumentId::new().to_storage(),
            "base_revision": "r",
            "source_node_id": NodeId::new().to_storage(),
            "expected": "move me",
            "new_document_title": "Fresh page",
        }))
        .expect("the original shape still parses");
        assert!(a.extra_source_node_ids.is_empty());
        assert!(a.target_document_id.is_none());
        assert!(a.under_node_id.is_none());
        assert_eq!(a.new_document_title.as_deref(), Some("Fresh page"));
    }

    fn schema_contains_required_field(value: &Value, field: &str) -> bool {
        match value {
            Value::Array(values) => values
                .iter()
                .any(|value| schema_contains_required_field(value, field)),
            Value::Object(map) => {
                map.get("required")
                    .and_then(Value::as_array)
                    .is_some_and(|required| required.iter().any(|value| value == field))
                    || map
                        .values()
                        .any(|value| schema_contains_required_field(value, field))
            }
            _ => false,
        }
    }

    // --------------------------------------------- the schema is a promise
    //
    // A published schema is a promise a strict client holds the other side
    // to. Tools used to publish output schemas too, and two of them shipped
    // disagreeing with what the tool returned — `schema_version` a required
    // integer sent as `null`, `unchanged` a boolean sent as a count — each
    // found by a strict client refusing the whole response (ADR-130). Output
    // schemas are no longer published (ADR-433-compacttools); the input
    // schemas are compacted from the Rust types, so what follows checks real
    // requests against what is published, and pins the two values that broke.

    /// The JSON Schema type name for a value, as the schemas we publish use
    /// them. `1.0` is a `number` and not an `integer`, which is the distinction
    /// that matters.
    fn json_type(value: &Value) -> &'static str {
        match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(number) => {
                if number.is_i64() || number.is_u64() {
                    "integer"
                } else {
                    "number"
                }
            }
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        }
    }

    /// Check a value against the slice of JSON Schema we publish:
    /// `type` (one name or a union), `required`, `properties`, `items`, and the
    /// `anyOf`/`oneOf` an optional composite generates. Deliberately small —
    /// a validator crate for a handful of flat object schemas would be a dependency
    /// bought to re-find a mismatch this catches in forty lines.
    fn violations(schema: &Value, value: &Value, path: &str, out: &mut Vec<String>) {
        // `true` and `false` are legal schemas. Neither constrains a field we
        // publish, and `Vec<Value>` generates the first as its `items`.
        let Some(schema) = schema.as_object() else {
            return;
        };
        if let Some(branches) = schema
            .get("anyOf")
            .or_else(|| schema.get("oneOf"))
            .and_then(Value::as_array)
        {
            let matched = branches.iter().any(|branch| {
                let mut branch_out = Vec::new();
                violations(branch, value, path, &mut branch_out);
                branch_out.is_empty()
            });
            if !matched {
                out.push(format!(
                    "{path} is {} and matches no branch",
                    json_type(value)
                ));
            }
            return;
        }
        if let Some(declared) = schema.get("type") {
            let allowed: Vec<&str> = match declared {
                Value::String(one) => vec![one.as_str()],
                Value::Array(many) => many.iter().filter_map(Value::as_str).collect(),
                _ => Vec::new(),
            };
            let found = json_type(value);
            let ok = allowed.is_empty()
                || allowed
                    .iter()
                    .any(|name| *name == found || (*name == "number" && found == "integer"));
            if !ok {
                out.push(format!("{path} is {found}, published as {declared}"));
                return;
            }
        }
        if let Some(object) = value.as_object() {
            for name in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if !object.contains_key(name) {
                    out.push(format!("{path}/{name} is required and absent"));
                }
            }
            if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                for (name, member) in object {
                    if let Some(member_schema) = properties.get(name) {
                        violations(member_schema, member, &format!("{path}/{name}"), out);
                    }
                }
            }
        }
        if let (Some(items), Some(schema_for_items)) = (value.as_array(), schema.get("items")) {
            for (index, item) in items.iter().enumerate() {
                violations(schema_for_items, item, &format!("{path}/{index}"), out);
            }
        }
    }

    /// A document as the store makes one: an **empty** `LoroDoc` the app seeds
    /// nodes into, so `WorkspaceDoc::create` never ran and it carries no schema
    /// marker. Building it any other way is what hid the null for a release.
    fn a_store_shaped_document() -> (WorkspaceDoc, NodeId, NodeId) {
        let empty = loro::LoroDoc::new();
        let mut doc =
            WorkspaceDoc::from_snapshot(&empty.export(loro::ExportMode::Snapshot).unwrap())
                .unwrap();
        assert_eq!(doc.schema_version(), None, "the case that broke");
        let root = doc
            .insert(Parent::Root, Node::document("会議メモ").unwrap())
            .unwrap();
        let line = doc
            .insert(Parent::Node(root), Node::paragraph("First line").unwrap())
            .unwrap();
        (doc, root, line)
    }

    fn a_history_entry() -> koru_store_wire::HistoryEntry {
        koru_store_wire::HistoryEntry {
            id: HistoryEntryId::new(),
            document: StoreDocumentId::new(),
            version: koru_store_core::ids::DocumentVersionId::new(),
            reason: "manual".into(),
            recorded_at: 1_700_000_000_000,
            label: Some("Before the rewrite".into()),
            message: None,
            boundary: Some(uuid::Uuid::new_v4().to_string()),
            pinned: true,
        }
    }

    /// What the backend really answers with, per tool. The values come from the
    /// Store and model types and from the two projection functions; only the
    /// one-line envelope is repeated, and a tool with no entry here fails
    /// rather than being skipped.
    fn a_task_attribute(
        role: &str,
        kind: &str,
        value: &str,
        zone: Option<&str>,
    ) -> koru_store_wire::attributes::AttributeRowValue {
        koru_store_wire::attributes::AttributeRowValue {
            id: NodeId::new().to_storage(),
            role: role.to_owned(),
            kind: kind.to_owned(),
            value: value.to_owned(),
            zone: zone.map(str::to_owned),
            label: None,
        }
    }

    /// A page as the store answers one: two rows, one dated and prioritised and
    /// in a list, one carrying nothing but its marker.
    fn a_task_page() -> TaskQueryResponse {
        let workspace = StoreWorkspaceId::new();
        let document = StoreDocumentId::new();
        let row = |title: &str, attributes| koru_store_wire::tasks::TaskRow {
            workspace,
            document,
            node: NodeId::new().to_storage(),
            task: format!(
                "task:{}:{}:{}",
                workspace.to_storage(),
                document.to_storage(),
                NodeId::new().to_storage()
            ),
            node_kind: "paragraph".into(),
            lifecycle: "open".into(),
            document_trashed: false,
            in_trash: false,
            breadcrumb: "Спецификации / 会議録".into(),
            title: title.into(),
            attributes,
        };
        TaskQueryResponse {
            rows: vec![
                row(
                    "Ship the report",
                    vec![
                        a_task_attribute(
                            koru_core::attributes::KEY_DUE,
                            "date",
                            "2026-09-04",
                            None,
                        ),
                        a_task_attribute(koru_core::attributes::KEY_PRIORITY, "text", "high", None),
                        a_task_attribute(
                            koru_core::attributes::KEY_LIST,
                            "text",
                            "01931f00-0000-4000-8000-00000000000a",
                            None,
                        ),
                    ],
                ),
                row("Undated but marked", Vec::new()),
            ],
            definitions: Vec::new(),
            tags: Vec::new(),
            groups: Vec::new(),
            projected: true,
            updating_workspaces: Vec::new(),
            limit: 100,
            next_after: None,
            restarted: false,
            covered: None,
        }
    }

    /// **A `due` read here goes straight back into `set_attribute`.** The read
    /// model keeps a flat `(kind, value, zone)` triple and the edit door takes
    /// the three temporal shapes; if this mapping drifts, an agent that lists a
    /// task and moves its deadline has to invent a fourth spelling of a date.
    #[test]
    fn a_listed_due_is_the_shape_the_edit_door_accepts() {
        assert_eq!(
            task_due_json(&a_task_attribute("koru.due", "date", "2026-09-04", None)),
            json!({"type":"date", "value":"2026-09-04"})
        );
        assert_eq!(
            task_due_json(&a_task_attribute(
                "koru.due",
                "datetime_floating",
                "2026-09-04T09:30:00",
                None
            )),
            json!({"type":"datetime_floating", "wall":"2026-09-04T09:30:00"})
        );
        assert_eq!(
            task_due_json(&a_task_attribute(
                "koru.due",
                "datetime_zoned",
                "1788998400000",
                Some("Europe/Zurich")
            )),
            json!({"type":"datetime_zoned", "instant_ms":1_788_998_400_000_i64,
                   "timezone":"Europe/Zurich"}),
            "the instant is a number here, as it is everywhere else in this surface"
        );
    }

    /// A row with no deadline, no priority and no list publishes those as
    /// **absent**, not as empty strings — a marked task needs no date, and
    /// `""` would read as a value somebody wrote.
    #[test]
    fn a_task_carrying_nothing_reports_nothing_rather_than_blanks() {
        let page = task_page_json(a_task_page());
        let bare = &page["tasks"][1];
        assert!(bare["due"].is_null(), "{bare}");
        assert!(bare["priority"].is_null(), "{bare}");
        assert!(bare["list"].is_null(), "{bare}");
        assert_eq!(bare["lifecycle"], "open");

        let full = &page["tasks"][0];
        assert_eq!(full["due"], json!({"type":"date", "value":"2026-09-04"}));
        assert_eq!(full["priority"], "high");
        assert_eq!(full["list"], "01931f00-0000-4000-8000-00000000000a");
        // The triple is spelled out beside the reference, or every follow-up
        // call would begin by parsing the `task:` string.
        assert!(full["workspace_id"].is_string() && full["document_id"].is_string());
        assert!(full["node_id"].is_string());
        assert!(
            full["task"].as_str().unwrap().starts_with("task:"),
            "{full}"
        );
    }

    /// The house rule, on this tool too: a misspelled filter is refused rather
    /// than silently ignored, because an ignored `min_priority` answers a
    /// question nobody asked and looks like an answer.
    #[test]
    fn list_tasks_arguments_reject_unknown_fields() {
        let error = serde_json::from_value::<ListTasksRequest>(json!({
            "workspace_ids": [],
            "minimum_priority": "high"
        }))
        .expect_err("an unknown filter must not be discarded");
        assert!(
            error
                .to_string()
                .contains("unknown field `minimum_priority`"),
            "{error}"
        );
    }

    /// Absence is the useful default all the way down: open tasks, soonest
    /// first, live only, page one.
    #[test]
    fn list_tasks_defaults_to_what_is_outstanding() {
        let request: ListTasksRequest = serde_json::from_value(json!({})).expect("an empty query");
        assert_eq!(request.lifecycle, TaskLifecycleFilter::Open);
        assert_eq!(request.sort, TaskSort::Due);
        assert!(!request.include_trashed);
        assert!(request.workspace_ids.is_empty());
        assert!(request.limit.is_none() && request.cursor.is_none());
    }

    /// A principal with no reachable workspace is answered in **this** tool's
    /// shape. It used to be answered in the search tool's, because one function
    /// built the empty page for every scoped tool: a client would have read
    /// `hits` where it published `tasks` and found no list at all.
    #[test]
    fn an_unreachable_scope_answers_an_empty_task_page_not_an_empty_search() {
        let backend = Arc::new(RecordingFake::default());
        let executor = WorkspaceToolExecutor::new(backend.clone());
        let context = RequestContext::selected(
            "local",
            "koru-ai",
            [Scope::ReadDocuments],
            [StoreWorkspaceId::new()],
        );
        let answer = executor
            .invoke(
                &context,
                "list_tasks",
                json!({"workspace_ids":[StoreWorkspaceId::new().to_storage()]}),
            )
            .expect("an empty page, not a refusal");
        assert_eq!(answer, empty_task_page());
        assert!(
            answer["tasks"].as_array().is_some_and(|t| t.is_empty()),
            "the envelope is named `tasks`: {answer}"
        );
        assert!(
            backend.calls.lock().unwrap().is_empty(),
            "and the backend was never asked"
        );
    }

    /// One of every operation, as a caller would send it.
    fn every_edit_operation() -> Vec<EditOperation> {
        let id = || NodeId::new().to_storage();
        let expected = || "Now".to_owned();
        let value = |value: AttributeValue| EditOperation::SetAttribute {
            node_id: id(),
            expected: expected(),
            key: "koru.due".into(),
            value,
        };
        vec![
            EditOperation::ReplaceText {
                node_id: id(),
                expected: expected(),
                text: "Later".into(),
            },
            EditOperation::InsertInline {
                node_id: id(),
                expected: expected(),
                at_utf8: 3,
                inline: InlineValue::Document {
                    document_id: StoreDocumentId::new().to_storage(),
                    node_id: Some(id()),
                    label: None,
                },
            },
            EditOperation::InsertInline {
                node_id: id(),
                expected: expected(),
                at_utf8: 0,
                inline: InlineValue::Attachment {
                    file_id: StoreDocumentId::new().to_storage(),
                    label: Some("diagram.png".into()),
                },
            },
            EditOperation::Insert {
                node_id: id(),
                parent_id: id(),
                after_node_id: None,
                kind: InsertKind::Task,
                text: "New".into(),
            },
            EditOperation::Move {
                node_id: id(),
                expected: expected(),
                parent_id: id(),
                after_node_id: Some(id()),
            },
            EditOperation::Remove {
                node_id: id(),
                expected: expected(),
            },
            EditOperation::SetDone {
                node_id: id(),
                expected: expected(),
                done: true,
            },
            EditOperation::SetLanguage {
                node_id: id(),
                expected: expected(),
                language: None,
            },
            EditOperation::SetCalloutLook {
                node_id: id(),
                expected: expected(),
                icon: Some(Some("⚠️".into())),
                color: Some(Some("red".into())),
            },
            // `null` restores the default and must pass the published schema.
            EditOperation::SetCalloutLook {
                node_id: id(),
                expected: expected(),
                icon: Some(None),
                color: None,
            },
            value(AttributeValue::Date {
                value: "2026-09-04".into(),
            }),
            value(AttributeValue::DatetimeFloating {
                wall: "2026-09-04T09:30:00".into(),
            }),
            value(AttributeValue::DatetimeZoned {
                instant_ms: 1_788_998_400_000,
                timezone: "Europe/Zurich".into(),
            }),
            value(AttributeValue::Text {
                value: "high".into(),
            }),
            value(AttributeValue::Number { value: 2.5 }),
            value(AttributeValue::Bool { value: true }),
            value(AttributeValue::Url {
                value: "https://koru.do".into(),
            }),
            EditOperation::ClearAttribute {
                node_id: id(),
                expected: expected(),
                key: "koru.due".into(),
            },
            EditOperation::AddTag {
                node_id: id(),
                expected: expected(),
                tag: Some("fun/home".into()),
                tag_id: None,
            },
            EditOperation::RemoveTag {
                node_id: id(),
                expected: expected(),
                tag: None,
                tag_id: Some(uuid::Uuid::new_v4().to_string()),
            },
        ]
    }

    fn published_input_schema(tool: &str) -> Value {
        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|candidate| candidate.name == tool)
            .expect("a published tool");
        Value::Object(tool.input_schema.as_ref().clone())
    }

    /// A union flattened for publication must still accept every real shape:
    /// `value` is a string, a number or a boolean by variant, and a merge that
    /// kept only the first would refuse two of them in a strict client.
    #[test]
    fn every_operation_fits_the_published_input_schema() {
        let edit = published_input_schema("edit_document");
        let operations = every_edit_operation();
        let request = json!({
            "workspace_id": StoreWorkspaceId::new().to_storage(),
            "document_id": StoreDocumentId::new().to_storage(),
            "base_revision": "00",
            "operations": operations,
        });
        let mut out = Vec::new();
        violations(&edit, &request, "edit_document", &mut out);
        assert!(out.is_empty(), "{}", out.join("; "));
        let published = edit["properties"]["operations"]["items"]["properties"]["op"]["enum"]
            .as_array()
            .expect("the op enum")
            .len();
        let sent: HashSet<_> = operations
            .iter()
            .map(|op| serde_json::to_value(op).unwrap()["op"].clone())
            .collect();
        assert_eq!(sent.len(), published, "a sample for every published op");

        let create = published_input_schema("create_document");
        let root = NodeId::new().to_storage();
        let first = NodeId::new().to_storage();
        let seed = vec![
            InitialOperation::Insert {
                node_id: first.clone(),
                parent_id: root.clone(),
                after_node_id: None,
                kind: InsertKind::Code,
                text: "fn main() {}".into(),
            },
            InitialOperation::InsertInline {
                node_id: first.clone(),
                expected: "fn main() {}".into(),
                at_utf8: 0,
                inline: InlineValue::Document {
                    document_id: StoreDocumentId::new().to_storage(),
                    node_id: None,
                    label: None,
                },
            },
            InitialOperation::SetDone {
                node_id: first.clone(),
                expected: "fn main() {}".into(),
                done: false,
            },
            InitialOperation::SetLanguage {
                node_id: first.clone(),
                expected: "fn main() {}".into(),
                language: Some("rust".into()),
            },
            InitialOperation::SetCalloutLook {
                node_id: first,
                expected: String::new(),
                icon: Some(Some("🔥".into())),
                color: Some(None),
            },
        ];
        let request = json!({
            "workspace_id": StoreWorkspaceId::new().to_storage(),
            "title": "Seeded",
            "root_node_id": root,
            "initial_operations": seed,
        });
        out.clear();
        violations(&create, &request, "create_document", &mut out);
        assert!(out.is_empty(), "{}", out.join("; "));
    }

    /// The comparison of a real edit, through `diff_json` itself.
    fn a_real_comparison() -> Value {
        let (mut doc, root, line) = a_store_shaped_document();
        let before = doc.export_snapshot().unwrap();
        doc.set_text(line, "First line, rewritten").unwrap();
        doc.insert(Parent::Node(root), Node::paragraph("Added").unwrap())
            .unwrap();
        let after = doc.export_snapshot().unwrap();
        let diff = koru_history::compare(
            &koru_history::SemanticDocument::from_snapshot(&before).unwrap(),
            &koru_history::SemanticDocument::from_snapshot(&after).unwrap(),
        );
        assert!(diff.unchanged > 0 && !diff.changes.is_empty());
        diff_json(&diff)
    }

    /// The checker is only worth having if it fails, and both shipped bugs are
    /// shapes it has to reject: a `null` where an integer was promised, and a
    /// count where a boolean was.
    #[test]
    fn the_schema_checker_rejects_the_two_shapes_that_shipped() {
        let integer = json!({
            "type": "object",
            "required": ["schema_version"],
            "properties": { "schema_version": { "type": "integer" } }
        });
        let mut out = Vec::new();
        violations(
            &integer,
            &json!({ "schema_version": null }),
            "read",
            &mut out,
        );
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("is null, published as"), "{out:?}");

        out.clear();
        violations(&integer, &json!({}), "read", &mut out);
        assert!(out[0].contains("required and absent"), "{out:?}");

        out.clear();
        let boolean = json!({
            "type": "object",
            "properties": { "unchanged": { "type": "boolean" } }
        });
        violations(&boolean, &json!({ "unchanged": 12 }), "compare", &mut out);
        assert_eq!(out.len(), 1, "{out:?}");

        out.clear();
        violations(&boolean, &json!({ "unchanged": true }), "compare", &mut out);
        assert!(out.is_empty(), "{out:?}");
    }

    /// The comparison result names a count, and says so. A field called
    /// `unchanged` holding `217` reads as a flag to whatever is choosing the
    /// next tool call, which is the audience.
    #[test]
    fn a_comparison_reports_how_many_nodes_are_untouched() {
        let compared = a_real_comparison();
        assert!(compared["unchanged_nodes"].is_u64(), "{compared}");
        assert!(compared.get("unchanged").is_none(), "{compared}");
        assert!(
            compared["changes"]
                .as_array()
                .is_some_and(|c| !c.is_empty())
        );
    }

    /// The whole tool failed on the absence of one field's value, so the value
    /// itself is worth pinning: a document with no marker reads as version 1.
    #[test]
    fn a_document_the_store_made_reads_back_with_a_schema_version() {
        let (doc, _, _) = a_store_shaped_document();
        let read = document_json(StoreDocumentId::new(), &doc).unwrap();
        assert_eq!(read["schema_version"], json!(1));

        // And one that *does* record a version reports that, not the fallback:
        // the projection answers what the document says.
        let mut created = WorkspaceDoc::create(CoreWorkspaceId::new(), "test").unwrap();
        created
            .insert(Parent::Root, Node::document("Marked").unwrap())
            .unwrap();
        assert_eq!(
            document_json(StoreDocumentId::new(), &created).unwrap()["schema_version"],
            json!(koru_core::schema::SCHEMA_VERSION)
        );
    }

    #[tokio::test]
    async fn a_standard_stdio_client_discovers_tools_and_the_model_resource() {
        use rmcp::ServiceExt;

        let (server_transport, client_transport) = tokio::io::duplex(64 * 1024);
        // Deliberately use a backend that returns an array. The server must
        // repair it before the result crosses the real MCP transport.
        let server = KoruMcp::new(Arc::new(ArrayFake), RequestContext::local());
        let server_task = tokio::spawn(async move { server.serve(server_transport).await });
        let client = ().serve(client_transport).await.expect("MCP initialize handshake");
        let tools = client
            .peer()
            .list_all_tools()
            .await
            .expect("tools/list over stdio");

        assert_eq!(tools.len(), 21);
        assert!(
            tools
                .iter()
                .any(|tool| tool.name == "restore_document_version")
        );

        let result = client
            .peer()
            .call_tool(rmcp::model::CallToolRequestParams::new("list_workspaces"))
            .await
            .expect("tools/call over stdio");
        assert!(matches!(result.structured_content, Some(Value::Object(_))));

        let resources = client
            .peer()
            .list_resources(None)
            .await
            .expect("resources/list over stdio");
        assert_eq!(resources.resources.len(), 1);
        assert_eq!(resources.resources[0].uri, MODEL_RESOURCE_URI);
        assert_eq!(
            resources.resources[0].mime_type.as_deref(),
            Some(MODEL_DOCUMENT_MIME)
        );

        let resource = client
            .peer()
            .read_resource(ReadResourceRequestParams::new(MODEL_RESOURCE_URI))
            .await
            .expect("resources/read over stdio");
        let resource_json = serde_json::to_value(resource.contents).expect("resource JSON");
        assert_eq!(resource_json[0]["uri"], MODEL_RESOURCE_URI);
        assert_eq!(resource_json[0]["mimeType"], MODEL_DOCUMENT_MIME);
        assert!(
            resource_json[0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("# MCP semantics"))
        );
        // The section an agent writing a diagram reads reaches it over the
        // wire, not only from the checked-out file.
        assert!(
            resource_json[0]["text"]
                .as_str()
                .is_some_and(|text| text.contains("## Diagrams"))
        );

        let documentation = client
            .peer()
            .call_tool(
                rmcp::model::CallToolRequestParams::new("get_documentation").with_arguments(
                    serde_json::from_value(json!({ "uri": MODEL_RESOURCE_URI }))
                        .expect("arguments object"),
                ),
            )
            .await
            .expect("documentation fallback over stdio");
        assert_eq!(
            documentation
                .structured_content
                .as_ref()
                .and_then(|value| value["uri"].as_str()),
            Some(MODEL_RESOURCE_URI)
        );

        let missing = client
            .peer()
            .read_resource(ReadResourceRequestParams::new("koru://docs/missing"))
            .await
            .expect_err("unknown resources must fail");
        assert!(missing.to_string().contains("not found"));

        client.cancel().await.expect("close MCP client");
        server_task.abort();
    }

    // ------------------------------------------------------ file notes, in memory
    //
    // The note surface runs the real backend paths — manifest entries,
    // snapshots, durable update application — against an in-memory
    // `EmbeddedStore`, so these tests dial nothing and touch no disk. The fake
    // mirrors the store's contract (idempotency, files-only refusal, an empty
    // note at creation, a history marker that keeps the document as it stood);
    // it is not the thing under test — the filters and the wiring in
    // `LocalBackend` are.

    struct MemoryStore {
        workspace: StoreWorkspaceId,
        entries: Mutex<Vec<DocumentEntry>>,
        snapshots: Mutex<HashMap<StoreDocumentId, Vec<u8>>>,
        blobs: Mutex<Vec<Blob>>,
        search_hits: Mutex<Vec<koru_store_wire::search::SearchHit>>,
        /// Every history marker captured, with the document as it stood.
        markers: Mutex<Vec<(HistoryEntry, Vec<u8>)>>,
    }

    impl MemoryStore {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                workspace: StoreWorkspaceId::new(),
                entries: Mutex::new(Vec::new()),
                snapshots: Mutex::new(HashMap::new()),
                blobs: Mutex::new(Vec::new()),
                search_hits: Mutex::new(Vec::new()),
                markers: Mutex::new(Vec::new()),
            })
        }

        /// Register a file entry exactly as an import leaves one: a manifest
        /// row of kind `file` whose document carries a single `Kind::File`
        /// root, plus the blob metadata `file_json` measures size from.
        fn add_file(self: &Arc<Self>, name: &str, mime: &str) -> StoreDocumentId {
            let id = StoreDocumentId::new();
            let hash = koru_core::BlobHash::new("b3", "0123abcd");
            let mut doc =
                WorkspaceDoc::create(CoreWorkspaceId::from_uuid(self.workspace.as_uuid()), name)
                    .unwrap();
            doc.insert(Parent::Root, Node::file(name, hash.clone(), mime).unwrap())
                .unwrap();
            self.snapshots
                .lock()
                .unwrap()
                .insert(id, doc.export_snapshot().unwrap());
            self.entries.lock().unwrap().push(DocumentEntry {
                id,
                parent: None,
                ordinal: 0,
                title: name.into(),
                kind: koru_core::explore::KIND_FILE.into(),
                mime: Some(mime.into()),
                note: None,
                trashed_at: None,
                trashed_by: None,
            });
            self.blobs.lock().unwrap().push(Blob {
                hash: koru_store_core::model::BlobHash::parse(&hash.0).unwrap(),
                size: 3,
                mime: mime.into(),
                filename: Some(name.into()),
                created_at: koru_store_core::model::Timestamp::from_millis(1),
                attributes: Default::default(),
            });
            id
        }

        /// Register an ordinary page, so a refusal can be told apart from a
        /// missing id.
        fn add_page(self: &Arc<Self>, title: &str) -> StoreDocumentId {
            self.add_document(koru_core::explore::KIND_DOCUMENT, title, "")
        }

        /// Register a document of any manifest kind as an ordinary node tree —
        /// a root and one `line` — so that a tool which did not ask the kind
        /// would have everything it needs to succeed.
        fn add_document(self: &Arc<Self>, kind: &str, title: &str, line: &str) -> StoreDocumentId {
            let id = StoreDocumentId::new();
            self.entries.lock().unwrap().push(DocumentEntry {
                id,
                parent: None,
                ordinal: 0,
                title: title.into(),
                kind: kind.into(),
                mime: None,
                note: None,
                trashed_at: None,
                trashed_by: None,
            });
            self.seed_root_and_line(id, title, line);
            id
        }

        /// Write the `Kind::Document` root and first paragraph — the opener's
        /// job in every app (ADR-80), done here so a note the tool just
        /// created can be read and edited the way a desktop-opened one is.
        fn seed_document_root(&self, document: StoreDocumentId, title: &str) {
            self.seed_root_and_line(document, title, "");
        }

        fn seed_root_and_line(&self, document: StoreDocumentId, title: &str, line: &str) {
            let mut doc =
                WorkspaceDoc::create(CoreWorkspaceId::from_uuid(self.workspace.as_uuid()), title)
                    .unwrap();
            let root = doc
                .insert(Parent::Root, Node::document(title).unwrap())
                .unwrap();
            doc.insert(Parent::Node(root), Node::paragraph(line).unwrap())
                .unwrap();
            self.snapshots
                .lock()
                .unwrap()
                .insert(document, doc.export_snapshot().unwrap());
        }

        /// What a client would have read before writing — the revision and the
        /// first body line — taken from the store, because `read_document`
        /// refuses some of the documents these tests write to.
        fn stored(&self, document: StoreDocumentId) -> (String, NodeId) {
            let doc =
                WorkspaceDoc::from_snapshot(&self.snapshots.lock().unwrap()[&document]).unwrap();
            let root = doc.children(Parent::Root).unwrap()[0];
            let line = doc.children(Parent::Node(root)).unwrap()[0];
            (revision(&doc), line)
        }

        /// A history marker taken through the port itself, for a document no
        /// tool may mark.
        fn mark(&self, document: StoreDocumentId) -> HistoryEntry {
            self.capture_history(
                self.workspace,
                document,
                CaptureHistory {
                    reason: "manual".into(),
                    frontiers: None,
                    label: None,
                    message: None,
                    boundary: None,
                },
            )
            .unwrap()
        }

        /// Everything a write could change: every stored document, the number
        /// of history markers and the number of manifest rows.
        fn state(&self) -> (HashMap<StoreDocumentId, Vec<u8>>, usize, usize) {
            (
                self.snapshots.lock().unwrap().clone(),
                self.markers.lock().unwrap().len(),
                self.entries.lock().unwrap().len(),
            )
        }

        fn backend(self: &Arc<Self>) -> LocalBackend {
            LocalBackend::with_embedded(self.clone())
        }
    }

    impl EmbeddedStore for MemoryStore {
        fn workspaces(&self) -> Result<Vec<Workspace>, ToolError> {
            let mut workspace = Workspace::new(
                koru_store_core::ProfileId::new(),
                "Notes",
                koru_store_core::model::Timestamp::from_millis(1),
            )
            .unwrap();
            workspace.id = self.workspace;
            Ok(vec![workspace])
        }

        fn documents(&self, workspace: StoreWorkspaceId) -> Result<Vec<DocumentEntry>, ToolError> {
            assert_eq!(workspace, self.workspace);
            Ok(self.entries.lock().unwrap().clone())
        }

        fn search(
            &self,
            _request: SearchRequest,
        ) -> Result<koru_store_wire::search::SearchResponse, ToolError> {
            Ok(koru_store_wire::search::SearchResponse {
                page: koru_store_wire::search::SearchPage {
                    hits: self.search_hits.lock().unwrap().clone(),
                    next_offset: None,
                    updating_workspaces: Vec::new(),
                    request_seq: 0,
                    corrected_query: None,
                },
                facets: None,
                recent_queries: None,
            })
        }

        fn snapshot(
            &self,
            _workspace: StoreWorkspaceId,
            document: StoreDocumentId,
        ) -> Result<Vec<u8>, ToolError> {
            self.snapshots
                .lock()
                .unwrap()
                .get(&document)
                .cloned()
                .ok_or_else(|| ToolError::invalid("the memory store has no such document"))
        }

        fn create_document(
            &self,
            _workspace: StoreWorkspaceId,
            _title: String,
            _kind: String,
        ) -> Result<StoreDocumentId, ToolError> {
            unreachable!("no test here creates a bare document, and a refused extract must not")
        }

        fn ensure_file_note(
            &self,
            workspace: StoreWorkspaceId,
            file: StoreDocumentId,
        ) -> Result<(StoreDocumentId, bool), ToolError> {
            assert_eq!(workspace, self.workspace);
            let mut entries = self.entries.lock().unwrap();
            let position = entries
                .iter()
                .position(|entry| entry.id == file)
                .ok_or_else(|| ToolError {
                    code: "document_not_found".into(),
                    message: "no such document".into(),
                    details: None,
                })?;
            if let Some(note) = entries[position].note {
                return Ok((note, false));
            }
            if entries[position].kind != koru_core::explore::KIND_FILE {
                return Err(ToolError::invalid("a note can only be attached to a file"));
            }
            let note = StoreDocumentId::new();
            let title = entries[position].title.clone();
            entries[position].note = Some(note);
            entries.push(DocumentEntry {
                id: note,
                parent: Some(file),
                ordinal: 0,
                title,
                kind: koru_core::explore::KIND_NOTE.into(),
                mime: None,
                note: None,
                trashed_at: None,
                trashed_by: None,
            });
            drop(entries);
            // Registered empty, exactly as `Store::ensure_file_note` leaves it.
            let empty = loro::LoroDoc::new();
            self.snapshots
                .lock()
                .unwrap()
                .insert(note, empty.export(loro::ExportMode::Snapshot).unwrap());
            Ok((note, true))
        }

        fn seed_document(
            &self,
            _request: EmbeddedSeedDocument,
        ) -> Result<EmbeddedSeededDocument, ToolError> {
            unreachable!("the note tests never seed through the store")
        }

        fn persist_update(
            &self,
            _workspace: StoreWorkspaceId,
            document: StoreDocumentId,
            update: Vec<u8>,
        ) -> Result<(), ToolError> {
            let doc = loro::LoroDoc::new();
            let mut snapshots = self.snapshots.lock().unwrap();
            let stored = snapshots
                .get(&document)
                .ok_or_else(|| ToolError::invalid("the memory store has no such document"))?;
            doc.import(stored)
                .and_then(|_| doc.import(&update))
                .map_err(|error| ToolError::invalid(error.to_string()))?;
            snapshots.insert(
                document,
                doc.export(loro::ExportMode::Snapshot)
                    .map_err(|error| ToolError::invalid(error.to_string()))?,
            );
            Ok(())
        }

        /// A marker keeps the document as the store holds it, which is all
        /// the tools here need back: `history` lists it and
        /// `history_snapshot` opens it.
        fn capture_history(
            &self,
            workspace: StoreWorkspaceId,
            document: StoreDocumentId,
            request: CaptureHistory,
        ) -> Result<HistoryEntry, ToolError> {
            let snapshot = self.snapshot(workspace, document)?;
            let mut entry = a_history_entry();
            entry.document = document;
            entry.reason = request.reason;
            self.markers.lock().unwrap().push((entry.clone(), snapshot));
            Ok(entry)
        }

        fn history(
            &self,
            _workspace: StoreWorkspaceId,
            document: StoreDocumentId,
            _query: HistoryQuery,
        ) -> Result<HistoryPage, ToolError> {
            Ok(HistoryPage {
                entries: self
                    .markers
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(entry, _)| entry.document == document)
                    .map(|(entry, _)| entry.clone())
                    .collect(),
                next_before_at: None,
                next_before_id: None,
                stale: None,
            })
        }

        fn history_snapshot(
            &self,
            _workspace: StoreWorkspaceId,
            entry: HistoryEntryId,
        ) -> Result<Vec<u8>, ToolError> {
            self.markers
                .lock()
                .unwrap()
                .iter()
                .find(|(marker, _)| marker.id == entry)
                .map(|(_, snapshot)| snapshot.clone())
                .ok_or_else(|| ToolError::invalid("the memory store has no such history entry"))
        }

        fn blobs(&self, _workspace: StoreWorkspaceId) -> Result<Vec<Blob>, ToolError> {
            Ok(self.blobs.lock().unwrap().clone())
        }

        fn get_blob(
            &self,
            _workspace: StoreWorkspaceId,
            _hash: koru_store_core::model::BlobHash,
        ) -> Result<Vec<u8>, ToolError> {
            unreachable!("the note tests never read blob bytes")
        }

        fn status(&self) -> Result<Status, ToolError> {
            unreachable!("the note tests never ask for status")
        }
    }

    /// One canned search hit; only the manifest kind it claims to live in
    /// varies, because `document_kind` is the whole of the admission rule.
    fn a_search_hit(document_kind: &str) -> koru_store_wire::search::SearchHit {
        koru_store_wire::search::SearchHit {
            workspace: StoreWorkspaceId::new().to_storage(),
            workspace_name: "Notes".into(),
            document: Some(StoreDocumentId::new().to_storage()),
            node: Some(NodeId::new().to_storage()),
            document_kind: document_kind.into(),
            mime: None,
            kind: "paragraph".into(),
            field: "body".into(),
            title: "A note".into(),
            breadcrumb: "Notes / A note".into(),
            breadcrumb_spans: Vec::new(),
            snippet: "the remembered detail".into(),
            spans: Vec::new(),
            lifecycle: koru_store_wire::search::Lifecycle::Live,
            trashed_at: None,
            trashed_by: None,
            score: 1.0,
            observed_modified_at_ms: None,
            workspace_last_used_at_ms: None,
        }
    }

    /// `list_files`, `search_files` and `get_file` all build their answer
    /// through `file_json`, so one path proves all three reveal the link — and
    /// that a file without a note says `null` rather than omitting the key.
    #[test]
    fn a_files_payload_carries_its_note_id() {
        let store = MemoryStore::new();
        let file = store.add_file("diagram.png", "image/png");
        let backend = store.backend();
        let context = RequestContext::local();
        let workspace_id = store.workspace.to_storage();

        let before = backend
            .invoke(
                &context,
                "get_file",
                json!({"workspace_id": workspace_id, "file_id": file.to_storage()}),
            )
            .unwrap();
        assert_eq!(before["note_id"], Value::Null);

        let ensured = backend
            .invoke(
                &context,
                "ensure_file_note",
                json!({"workspace_id": workspace_id, "file_id": file.to_storage()}),
            )
            .unwrap();
        let note = ensured["note_id"].as_str().unwrap();

        let listed = backend
            .invoke(
                &context,
                "list_files",
                json!({"workspace_id": workspace_id}),
            )
            .unwrap();
        assert_eq!(listed["files"][0]["note_id"], note, "list_files");
        let fetched = backend
            .invoke(
                &context,
                "get_file",
                json!({"workspace_id": workspace_id, "file_id": file.to_storage()}),
            )
            .unwrap();
        assert_eq!(fetched["note_id"], note, "get_file");
        // And the note stays out of the page listing — discoverable through
        // its file, never as a document of its own.
        let pages = backend
            .invoke(
                &context,
                "list_documents",
                json!({"workspace_id": workspace_id}),
            )
            .unwrap();
        assert!(
            !pages["documents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["id"] == note),
            "a note must not appear in list_documents"
        );
    }

    /// `ensure_file_note` answers *Add note* and *Open note* with the same
    /// call: created once, replayed afterwards, and a non-file is the store's
    /// refusal surfacing as the tool's error.
    #[test]
    fn ensure_file_note_creates_once_replays_and_refuses_a_page() {
        let store = MemoryStore::new();
        let file = store.add_file("diagram.png", "image/png");
        let page = store.add_page("Not a file");
        let backend = store.backend();
        let context = RequestContext::local();
        let workspace_id = store.workspace.to_storage();

        let first = backend
            .invoke(
                &context,
                "ensure_file_note",
                json!({"workspace_id": workspace_id, "file_id": file.to_storage()}),
            )
            .unwrap();
        assert_eq!(first["created"], true);
        let again = backend
            .invoke(
                &context,
                "ensure_file_note",
                json!({"workspace_id": workspace_id, "file_id": file.to_storage()}),
            )
            .unwrap();
        assert_eq!(again["created"], false);
        assert_eq!(again["note_id"], first["note_id"]);

        let refused = backend
            .invoke(
                &context,
                "ensure_file_note",
                json!({"workspace_id": workspace_id, "file_id": page.to_storage()}),
            )
            .unwrap_err();
        assert_eq!(refused.code, "invalid_request");
        assert!(refused.message.contains("a file"), "{refused}");
    }

    /// The tool needs the edit scope even though a replay writes nothing —
    /// *this* call cannot know which it will be.
    #[test]
    fn ensuring_a_note_requires_the_edit_scope() {
        let workspace = StoreWorkspaceId::new();
        let backend = Arc::new(RecordingFake::default());
        let executor = WorkspaceToolExecutor::new(backend.clone());
        let context =
            RequestContext::selected("local", "koru-ai", [Scope::ReadDocuments], [workspace]);

        let error = executor
            .invoke(
                &context,
                "ensure_file_note",
                json!({
                    "workspace_id": workspace.to_storage(),
                    "file_id": StoreDocumentId::new().to_storage(),
                }),
            )
            .unwrap_err();
        assert_eq!(error.code, "permission_denied");
        assert!(backend.calls.lock().unwrap().is_empty());
    }

    /// The advertised contract: edit-scoped, idempotent, not destructive, with
    /// the file as its only target beside the workspace.
    #[test]
    fn ensure_file_note_is_advertised_with_its_schema_and_safety() {
        let policy = tool_policy("ensure_file_note").expect("a policy");
        assert_eq!(policy.scope, Some(Scope::EditDocuments));
        assert!(matches!(policy.workspace, WorkspaceTarget::One));
        assert!(matches!(policy.domain, ToolDomain::Workspace));

        let server = KoruMcp::new(Arc::new(Fake), RequestContext::local());
        let tool = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|tool| tool.name == "ensure_file_note")
            .expect("ensure_file_note tool");
        let annotations = tool.annotations.clone().unwrap_or_default();
        assert_eq!(annotations.read_only_hint, Some(false));
        assert_eq!(annotations.destructive_hint, Some(false));
        assert_eq!(annotations.idempotent_hint, Some(true));
        assert_eq!(annotations.open_world_hint, Some(false));

        let input = serde_json::to_value(&tool.input_schema).expect("input schema JSON");
        for field in ["workspace_id", "file_id"] {
            assert!(
                schema_contains_required_field(&input, field),
                "input must require {field}"
            );
        }
        let description = tool.description.as_deref().expect("a description");
        assert!(
            description.contains("created says which happened"),
            "{description}"
        );
    }

    /// A note is an ordinary document: `read_document` opens it. A file is
    /// still refused, with the get_file guidance intact.
    #[test]
    fn read_document_opens_a_note_and_still_refuses_a_file() {
        let store = MemoryStore::new();
        let file = store.add_file("diagram.png", "image/png");
        let backend = store.backend();
        let context = RequestContext::local();
        let workspace_id = store.workspace.to_storage();

        let note = backend
            .invoke(
                &context,
                "ensure_file_note",
                json!({"workspace_id": workspace_id, "file_id": file.to_storage()}),
            )
            .unwrap();
        let note_id = note["note_id"].as_str().unwrap();
        // What every opener does before the note is read: the store registers
        // it empty, the first opener plants the root (ADR-80).
        store.seed_document_root(StoreDocumentId::parse(note_id).unwrap(), "diagram.png");

        let read = backend
            .invoke(
                &context,
                "read_document",
                json!({"workspace_id": workspace_id, "document_id": note_id}),
            )
            .unwrap();
        assert_eq!(read["document_id"], note_id);
        assert!(read["root_node_id"].is_string());

        let refused = backend
            .invoke(
                &context,
                "read_document",
                json!({"workspace_id": workspace_id, "document_id": file.to_storage()}),
            )
            .unwrap_err();
        assert_eq!(refused.code, "invalid_request");
        assert!(refused.message.contains("get_file"), "{refused}");
    }

    /// Search admits note hits beside page hits; file hits stay out, and a
    /// workspace-level hit (no document) stays out with them.
    #[test]
    fn search_documents_returns_note_hits() {
        let store = MemoryStore::new();
        store.add_file("diagram.png", "image/png");
        {
            let mut hits = store.search_hits.lock().unwrap();
            hits.push(a_search_hit(koru_core::explore::KIND_DOCUMENT));
            hits.push(a_search_hit(koru_core::explore::KIND_NOTE));
            hits.push(a_search_hit(koru_core::explore::KIND_FILE));
            let mut workspace_hit = a_search_hit("");
            workspace_hit.document = None;
            hits.push(workspace_hit);
        }
        let backend = store.backend();

        let found = backend
            .invoke(
                &RequestContext::local(),
                "search_documents",
                json!({"query": "remembered"}),
            )
            .unwrap();
        let kinds: Vec<&str> = found["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|hit| hit["document_kind"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                koru_core::explore::KIND_DOCUMENT,
                koru_core::explore::KIND_NOTE
            ]
        );
    }

    /// `edit_document` admits every kind `read_document` reads, so a note takes
    /// the same durable edit a page does — persisted through the embedded port
    /// and visible to the next read.
    #[test]
    fn edit_document_writes_to_a_note() {
        let store = MemoryStore::new();
        let file = store.add_file("diagram.png", "image/png");
        let backend = store.backend();
        let context = RequestContext::local();
        let workspace_id = store.workspace.to_storage();

        let note = backend
            .invoke(
                &context,
                "ensure_file_note",
                json!({"workspace_id": workspace_id, "file_id": file.to_storage()}),
            )
            .unwrap();
        let note_id = note["note_id"].as_str().unwrap();
        store.seed_document_root(StoreDocumentId::parse(note_id).unwrap(), "diagram.png");

        let read = backend
            .invoke(
                &context,
                "read_document",
                json!({"workspace_id": workspace_id, "document_id": note_id}),
            )
            .unwrap();
        let paragraph = read["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["kind"] == "paragraph")
            .unwrap()["node_id"]
            .as_str()
            .unwrap()
            .to_owned();

        backend
            .invoke(
                &context,
                "edit_document",
                json!({
                    "workspace_id": workspace_id,
                    "document_id": note_id,
                    "base_revision": read["revision"],
                    "operations": [{
                        "op": "replace_text",
                        "node_id": paragraph,
                        "expected": "",
                        "text": "Taken with the wide lens",
                    }]
                }),
            )
            .unwrap();

        let reread = backend
            .invoke(
                &context,
                "read_document",
                json!({"workspace_id": workspace_id, "document_id": note_id}),
            )
            .unwrap();
        assert!(
            reread["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| node["content"]["text"] == "Taken with the wide lens"),
            "the edit did not persist: {reread}"
        );
    }

    // ------------------------------------------ writes reach only what reads reach
    //
    // Every write and history tool admits the manifest kinds `read_document`
    // opens and refuses the rest before it joins, stamps, captures or creates
    // anything. A run is today's case and a canvas the next (canvas P1.3a).
    // Both are seeded as ordinary node trees, so a tool that forgot to ask
    // would *succeed* here rather than fail for some other reason.

    /// Manifest kinds the read tools refuse. `"canvas"` is spelled out: its
    /// constant lands in `koru_core::explore` with the canvas itself.
    const UNREADABLE: [&str; 2] = [koru_core::explore::KIND_RUN, "canvas"];

    fn call(backend: &LocalBackend, tool: &str, arguments: Value) -> Result<Value, ToolError> {
        backend.invoke(&RequestContext::local(), tool, arguments)
    }

    /// Refused by kind, in `read_document`'s own words, naming `field`.
    #[track_caller]
    fn assert_refused_by_kind(result: Result<Value, ToolError>, field: &str) {
        let error = result.expect_err("a kind the read tools refuse must be refused");
        assert_eq!(error.code, "invalid_request", "{error}");
        assert!(
            error
                .message
                .starts_with(&format!("{field} is not a page or a note")),
            "{error}"
        );
    }

    /// Whether `read_document` shows `text` on some node of `document`.
    fn reads_back(
        backend: &LocalBackend,
        workspace_id: &str,
        document: StoreDocumentId,
        text: &str,
    ) -> bool {
        let read = call(
            backend,
            "read_document",
            json!({"workspace_id": workspace_id, "document_id": document.to_storage()}),
        )
        .unwrap();
        read["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|node| node["content"]["text"] == text)
    }

    /// The rule's other half, pinned beside it: the same kinds stay out of
    /// listing, search and reading, so the gate is the read set rather than a
    /// second list that can drift from it.
    #[test]
    fn the_read_tools_refuse_the_kinds_the_write_tools_refuse() {
        let store = MemoryStore::new();
        let backend = store.backend();
        let workspace_id = store.workspace.to_storage();
        for kind in UNREADABLE {
            let unreadable = store.add_document(kind, "Not a page", "");
            store.search_hits.lock().unwrap().push(a_search_hit(kind));
            assert_refused_by_kind(
                call(
                    &backend,
                    "read_document",
                    json!({"workspace_id": workspace_id, "document_id": unreadable.to_storage()}),
                ),
                "document_id",
            );
        }
        let listed = call(
            &backend,
            "list_documents",
            json!({"workspace_id": workspace_id}),
        )
        .unwrap();
        assert_eq!(listed["documents"], json!([]), "{listed}");
        let found = call(&backend, "search_documents", json!({"query": "remembered"})).unwrap();
        assert_eq!(found["hits"], json!([]), "{found}");
    }

    #[test]
    fn edit_document_writes_only_the_kinds_read_document_reads() {
        let store = MemoryStore::new();
        let backend = store.backend();
        let workspace_id = store.workspace.to_storage();
        let edit = |document: StoreDocumentId| {
            let (revision, line) = store.stored(document);
            call(
                &backend,
                "edit_document",
                json!({
                    "workspace_id": workspace_id,
                    "document_id": document.to_storage(),
                    "base_revision": revision,
                    "operations": [{
                        "op": "replace_text",
                        "node_id": line.to_storage(),
                        "expected": "",
                        "text": "Written through MCP",
                    }]
                }),
            )
        };

        for kind in UNREADABLE {
            let unreadable = store.add_document(kind, "Not a page", "");
            let before = store.state();
            assert_refused_by_kind(edit(unreadable), "document_id");
            // Not even the id stamp an edit's join would have persisted.
            assert!(store.state() == before, "a refused {kind} edit wrote");
        }

        let page = store.add_page("A page");
        edit(page).unwrap();
        assert!(reads_back(
            &backend,
            &workspace_id,
            page,
            "Written through MCP"
        ));
    }

    #[test]
    fn extract_to_document_moves_only_between_kinds_read_document_reads() {
        let store = MemoryStore::new();
        let backend = store.backend();
        let workspace_id = store.workspace.to_storage();
        let page_kind = koru_core::explore::KIND_DOCUMENT;
        // Into `target` when there is one, into a new page otherwise.
        let extract = |source: StoreDocumentId, target: Option<StoreDocumentId>| {
            let (revision, line) = store.stored(source);
            call(
                &backend,
                "extract_to_document",
                json!({
                    "workspace_id": workspace_id,
                    "source_document_id": source.to_storage(),
                    "base_revision": revision,
                    "source_node_id": line.to_storage(),
                    "expected": "Moved across",
                    "target_document_id": target.map(|target| target.to_storage()),
                    "new_document_title": target.is_none().then_some("Never created"),
                }),
            )
        };

        for kind in UNREADABLE {
            let unreadable = store.add_document(kind, "Not a page", "Moved across");
            let page = store.add_document(page_kind, "A page", "Moved across");
            let before = store.state();
            // Out of it, into a page or into a new one — which would otherwise
            // have been created before the source was ever read.
            assert_refused_by_kind(extract(unreadable, Some(page)), "source_document_id");
            assert_refused_by_kind(extract(unreadable, None), "source_document_id");
            // And out of a page into it.
            assert_refused_by_kind(extract(page, Some(unreadable)), "target_document_id");
            assert!(store.state() == before, "a refused {kind} extract wrote");
        }

        let source = store.add_document(page_kind, "Source", "Moved across");
        let destination = store.add_page("Destination");
        let moved = extract(source, Some(destination)).unwrap();
        assert_eq!(moved["document_id"], destination.to_storage());
        assert!(reads_back(
            &backend,
            &workspace_id,
            destination,
            "Moved across"
        ));
        assert!(!reads_back(&backend, &workspace_id, source, "Moved across"));
    }

    #[test]
    fn save_document_version_marks_only_the_kinds_read_document_reads() {
        let store = MemoryStore::new();
        let backend = store.backend();
        let workspace_id = store.workspace.to_storage();
        let save = |document: StoreDocumentId| {
            call(
                &backend,
                "save_document_version",
                json!({
                    "workspace_id": workspace_id,
                    "document_id": document.to_storage(),
                    "label": "Before the rewrite",
                }),
            )
        };

        for kind in UNREADABLE {
            let unreadable = store.add_document(kind, "Not a page", "");
            let before = store.state();
            assert_refused_by_kind(save(unreadable), "document_id");
            assert!(store.state() == before, "a refused {kind} save captured");
        }

        let page = store.add_page("A page");
        let saved = save(page).unwrap();
        assert_eq!(saved["document"], page.to_storage());
        assert_eq!(store.markers.lock().unwrap().len(), 1);
    }

    #[test]
    fn list_document_versions_lists_only_the_kinds_read_document_reads() {
        let store = MemoryStore::new();
        let backend = store.backend();
        let workspace_id = store.workspace.to_storage();
        let list = |document: StoreDocumentId| {
            call(
                &backend,
                "list_document_versions",
                json!({"workspace_id": workspace_id, "document_id": document.to_storage()}),
            )
        };

        for kind in UNREADABLE {
            let unreadable = store.add_document(kind, "Not a page", "");
            // It has a history to list, so only its kind can refuse it.
            store.mark(unreadable);
            assert_refused_by_kind(list(unreadable), "document_id");
        }

        let page = store.add_page("A page");
        let marked = store.mark(page);
        let listed = list(page).unwrap();
        assert_eq!(
            listed["entries"][0]["id"],
            marked.id.to_storage(),
            "{listed}"
        );
        assert_eq!(listed["entries"].as_array().unwrap().len(), 1, "{listed}");
    }

    #[test]
    fn restore_document_version_restores_only_the_kinds_read_document_reads() {
        let store = MemoryStore::new();
        let backend = store.backend();
        let workspace_id = store.workspace.to_storage();
        let restore = |document: StoreDocumentId, entry: &HistoryEntry| {
            let (revision, _) = store.stored(document);
            call(
                &backend,
                "restore_document_version",
                json!({
                    "workspace_id": workspace_id,
                    "document_id": document.to_storage(),
                    "entry_id": entry.id.to_storage(),
                    "base_revision": revision,
                }),
            )
        };

        for kind in UNREADABLE {
            let unreadable = store.add_document(kind, "Not a page", "");
            let entry = store.mark(unreadable);
            let before = store.state();
            assert_refused_by_kind(restore(unreadable, &entry), "document_id");
            assert!(store.state() == before, "a refused {kind} restore wrote");
        }

        // A page restores: marked, rewritten, and back to what was marked.
        let page = store.add_document(koru_core::explore::KIND_DOCUMENT, "A page", "Kept");
        let entry = store.mark(page);
        let (revision, line) = store.stored(page);
        call(
            &backend,
            "edit_document",
            json!({
                "workspace_id": workspace_id,
                "document_id": page.to_storage(),
                "base_revision": revision,
                "operations": [{
                    "op": "replace_text",
                    "node_id": line.to_storage(),
                    "expected": "Kept",
                    "text": "Rewritten",
                }]
            }),
        )
        .unwrap();
        assert!(reads_back(&backend, &workspace_id, page, "Rewritten"));
        restore(page, &entry).unwrap();
        assert!(reads_back(&backend, &workspace_id, page, "Kept"));
    }
}
