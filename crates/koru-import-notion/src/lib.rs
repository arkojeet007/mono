// SPDX-License-Identifier: MPL-2.0

//! Notion Markdown & CSV zip importer.
//!
//! Two passes, matching AFFiNE and Koru's OneNote importer:
//!
//! 1. **Plan** — extract the zip safely, strip Notion's 32-hex id suffixes,
//!    mint a `DocumentId` per page and file, and build path → id maps so
//!    relative links can resolve before their targets exist.
//! 2. **Build** — rewrite intra-export `.md` links to `[[koru://doc/…]]`,
//!    hand each page body to the Markdown importer (images resolve through
//!    the extracted tree), convert database CSVs to table blocks, attach
//!    files, and nest everything under one root container document.
//!
//! **An import is a faithful restore, not a thing a person just did**
//! ([ADR-286], SPEC §5). A Notion to-do lands as a *checked box*, which is
//! content, and never as a completed task: no marker is fabricated, no deadline
//! is invented, no lifecycle command runs and nothing classifies — so nothing
//! here can reopen a task the export recorded as done, and no activity is owed.
//! The exemption is not a flag this crate sets; it is that the classification
//! entry point and the six canonical commands are not on this path at all,
//! which `a_notion_to_do_lands_verbatim_and_no_task_is_invented` reads off the
//! source itself. Anything that ever *does* need to write task state here goes
//! through `koru_core::tasks::faithful_import` and nowhere else.
//!
//! [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use koru_core::NodeId;
use koru_core::explore::{KIND_DOCUMENT, KIND_FILE};
use koru_core::node::{Attr, AttrValue, BlobHash, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{DocRef, Ref};
use koru_import::{
    ImportContext, ImportError, ImportPreview, ImportProduct, ImportProgress, ImportSource,
    ImportStage, ImportWarning, ImportedContent, ImportedDocument, Importer, ImporterDescriptor,
    PreparedImport, PreviewNode, SourceRequest,
};
use koru_store_core::ids::{DocumentId, WorkspaceId as StoreWorkspaceId};
use tempfile::TempDir;

const MAX_ARCHIVE_ENTRIES: usize = 20_000;
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

pub static DESCRIPTOR: ImporterDescriptor = ImporterDescriptor {
    id: "notion",
    name: "Notion",
    source: SourceRequest::Files {
        extensions: &["zip"],
    },
    menu_name: Some("Notion"),
    picker_title: "Import a Notion export (.zip)",
    claims_zip: Some(claims_notion_zip),
    roots_import: true,
    dedups_known_files: true,
};

/// A dropped `.zip` is a Notion export when any entry name carries Notion's
/// trailing 32-hex page id (on a `.md` file or a directory).
pub fn claims_notion_zip(names: &[String]) -> bool {
    names.iter().any(|name| {
        let name = name.replace('\\', "/");
        let leaf = name.rsplit('/').next().unwrap_or(name.as_str());
        let stem = leaf.strip_suffix('/').unwrap_or(leaf);
        let stem = stem
            .strip_suffix(".md")
            .or_else(|| stem.strip_suffix(".MD"))
            .unwrap_or(stem);
        has_notion_id_suffix(stem)
    })
}

fn has_notion_id_suffix(stem: &str) -> bool {
    let Some((name, id)) = stem.rsplit_once(' ') else {
        return false;
    };
    !name.is_empty() && id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())
}

pub struct NotionImporter;

#[derive(Clone)]
struct PlannedPage {
    id: DocumentId,
    rel: String,
    absolute: PathBuf,
    title: String,
    parent: Option<DocumentId>,
    is_csv: bool,
}

struct PlannedFile {
    id: DocumentId,
    absolute: PathBuf,
    title: String,
    mime: String,
    attach_to: Option<DocumentId>,
}

struct PreparedNotion {
    _temp: TempDir,
    root_id: DocumentId,
    root_title: String,
    pages: Vec<PlannedPage>,
    files: Vec<PlannedFile>,
    path_ids: HashMap<String, DocumentId>,
    preview: ImportPreview,
    warnings: Vec<ImportWarning>,
}

impl Importer for NotionImporter {
    fn descriptor(&self) -> &'static ImporterDescriptor {
        &DESCRIPTOR
    }

    fn inspect(
        &self,
        source: ImportSource,
        progress: &dyn ImportProgress,
    ) -> Result<Box<dyn PreparedImport>, ImportError> {
        let ImportSource::Files(paths) = source else {
            return Err(ImportError::new("Notion import needs a .zip export"));
        };
        if paths.len() != 1 {
            return Err(ImportError::new(
                "Notion import takes one zip at a time — pick a single export",
            ));
        }
        let path = &paths[0];
        progress.update(ImportStage::Inspecting, 0, Some(1));
        if progress.cancelled() {
            return Err(ImportError::cancelled());
        }
        let temp = TempDir::new().map_err(failed)?;
        let mut warnings = Vec::new();
        extract_zip(path, temp.path(), &mut warnings)?;
        let plan = plan_tree(temp.path(), path, &mut warnings)?;
        let preview = ImportPreview {
            native_workspace_id: None,
            workspace_name: plan.root_title.clone(),
            documents: plan
                .pages
                .iter()
                .filter(|p| p.parent == Some(plan.root_id))
                .map(|p| PreviewNode {
                    title: p.title.clone(),
                    children: Vec::new(),
                    nodes: 1,
                    trashed: false,
                })
                .collect(),
            document_count: plan.pages.len() + 1,
            node_count: plan.pages.len() + plan.files.len() + 1,
            image_count: plan
                .files
                .iter()
                .filter(|f| f.mime.starts_with("image/"))
                .count(),
            warnings: warnings.iter().map(|w| w.message.clone()).collect(),
            identity_modes: vec![koru_import::IdentityMode::Copy],
        };
        progress.update(ImportStage::Inspecting, 1, Some(1));
        Ok(Box::new(PreparedNotion {
            _temp: temp,
            root_id: plan.root_id,
            root_title: plan.root_title,
            pages: plan.pages,
            files: plan.files,
            path_ids: plan.path_ids,
            preview,
            warnings,
        }))
    }
}

struct Plan {
    root_id: DocumentId,
    root_title: String,
    pages: Vec<PlannedPage>,
    files: Vec<PlannedFile>,
    path_ids: HashMap<String, DocumentId>,
}

fn plan_tree(
    root: &Path,
    zip_path: &Path,
    warnings: &mut Vec<ImportWarning>,
) -> Result<Plan, ImportError> {
    let root_title = zip_path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(strip_notion_id)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Notion Import".into());
    let root_id = DocumentId::new();

    let mut pages = Vec::new();
    let mut files = Vec::new();
    let mut path_ids: HashMap<String, DocumentId> = HashMap::new();
    let mut folder_owners: HashMap<String, DocumentId> = HashMap::new();

    walk_files(root, &mut |abs, rel| {
        let lower = rel.to_ascii_lowercase();
        if !(lower.ends_with(".md") || lower.ends_with(".csv")) {
            return;
        }
        let is_csv = lower.ends_with(".csv");
        let id = DocumentId::new();
        let title = strip_notion_id(file_stem(&rel));
        pages.push(PlannedPage {
            id,
            rel: rel.clone(),
            absolute: abs,
            title,
            parent: None,
            is_csv,
        });
        register_path_variants(&mut path_ids, &rel, id);
        if let Some(stem_path) = rel.rsplit_once('.').map(|(s, _)| s.to_owned()) {
            folder_owners.insert(stem_path, id);
        }
    })?;

    for page in &mut pages {
        let parent_folder = parent_rel(&page.rel);
        if parent_folder.is_empty() {
            page.parent = Some(root_id);
            continue;
        }
        page.parent = folder_owners.get(&parent_folder).copied().or(Some(root_id));
    }

    walk_files(root, &mut |abs, rel| {
        let lower = rel.to_ascii_lowercase();
        if lower.ends_with(".md") || lower.ends_with(".csv") || is_noise(&lower) {
            return;
        }
        let meta = match fs::metadata(&abs) {
            Ok(m) => m,
            Err(error) => {
                warnings.push(ImportWarning {
                    source: abs.clone(),
                    code: None,
                    arguments: Vec::new(),
                    message: format!("could not read file: {error}"),
                });
                return;
            }
        };
        if meta.len() > MAX_FILE_BYTES {
            warnings.push(ImportWarning {
                source: abs.clone(),
                code: None,
                arguments: Vec::new(),
                message: "file exceeds the 2 GiB attachment limit".into(),
            });
            return;
        }
        let id = DocumentId::new();
        let title = Path::new(&rel)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_owned();
        let mime = mime_for(&title);
        let parent_folder = parent_rel(&rel);
        let attach_to = folder_owners.get(&parent_folder).copied().or(Some(root_id));
        files.push(PlannedFile {
            id,
            absolute: abs,
            title,
            mime,
            attach_to,
        });
    })?;

    Ok(Plan {
        root_id,
        root_title,
        pages,
        files,
        path_ids,
    })
}

impl PreparedImport for PreparedNotion {
    fn preview(&self) -> &ImportPreview {
        &self.preview
    }

    fn build(
        self: Box<Self>,
        _options: koru_import::ImportOptions,
        context: &ImportContext<'_>,
        progress: &dyn ImportProgress,
    ) -> Result<ImportProduct, ImportError> {
        let store_ws = context
            .target_workspace
            .ok_or_else(|| ImportError::new("Notion import needs a target workspace"))?;
        let workspace = koru_core::WorkspaceId::from_uuid(store_ws.as_uuid());
        let mut documents = Vec::new();
        let mut warnings = self.warnings;
        let total = self.pages.len() + self.files.len() + 1;
        progress.update(ImportStage::Converting, 0, Some(total));

        let root_snapshot = empty_document(workspace, &self.root_title)?;
        documents.push(ImportedDocument {
            id: self.root_id,
            source: PathBuf::from("notion-root"),
            title: self.root_title.clone(),
            kind: KIND_DOCUMENT.into(),
            mime: None,
            parent: None,
            trashed: false,
            trashed_at: None,
            snapshot: root_snapshot,
        });

        let mut known_files: HashSet<DocumentId> = HashSet::new();
        for (i, file) in self.files.iter().enumerate() {
            if progress.cancelled() {
                return Err(ImportError::cancelled());
            }
            match build_file_document(workspace, file, context) {
                Ok(snapshot) => {
                    documents.push(ImportedDocument {
                        id: file.id,
                        source: file.absolute.clone(),
                        title: file.title.clone(),
                        kind: KIND_FILE.into(),
                        mime: Some(file.mime.clone()),
                        parent: None,
                        trashed: false,
                        trashed_at: None,
                        snapshot,
                    });
                    known_files.insert(file.id);
                }
                Err(error) => warnings.push(ImportWarning {
                    source: file.absolute.clone(),
                    code: None,
                    arguments: Vec::new(),
                    message: error.to_string(),
                }),
            }
            progress.update(ImportStage::Converting, i + 1, Some(total));
        }

        // CSV pages also emit a file entry for the raw CSV (table + attachment).
        for page in self.pages.iter().filter(|p| p.is_csv) {
            if progress.cancelled() {
                return Err(ImportError::cancelled());
            }
            match build_csv_file_document(workspace, page, context) {
                Ok((file_id, title, snapshot)) => {
                    documents.push(ImportedDocument {
                        id: file_id,
                        source: page.absolute.clone(),
                        title: title.clone(),
                        kind: KIND_FILE.into(),
                        mime: Some("text/csv".into()),
                        parent: None,
                        trashed: false,
                        trashed_at: None,
                        snapshot,
                    });
                    known_files.insert(file_id);
                }
                Err(error) => warnings.push(ImportWarning {
                    source: page.absolute.clone(),
                    code: None,
                    arguments: Vec::new(),
                    message: format!("CSV file entry: {error}"),
                }),
            }
        }

        let mut order = self.pages.clone();
        order.sort_by_key(|p| path_depth(&p.rel));

        for (i, page) in order.iter().enumerate() {
            if progress.cancelled() {
                return Err(ImportError::cancelled());
            }
            let csv_file = if page.is_csv {
                documents
                    .iter()
                    .find(|d| d.source == page.absolute && d.kind == KIND_FILE)
                    .map(|d| (d.id, d.title.clone()))
            } else {
                None
            };
            let result = if page.is_csv {
                build_csv_page(workspace, page, csv_file.as_ref(), &mut warnings)
            } else {
                build_md_page(
                    workspace,
                    page,
                    &self.path_ids,
                    context,
                    progress,
                    &mut warnings,
                )
            };
            match result {
                Ok(mut snapshot) => {
                    let attachments: Vec<_> = self
                        .files
                        .iter()
                        .filter(|f| f.attach_to == Some(page.id) && known_files.contains(&f.id))
                        .map(|f| (f.id, f.title.clone()))
                        .collect();
                    if !attachments.is_empty()
                        && let Ok(patched) =
                            append_attachment_pills(&snapshot, &page.title, &attachments)
                    {
                        snapshot = patched;
                    }
                    documents.push(ImportedDocument {
                        id: page.id,
                        source: page.absolute.clone(),
                        title: page.title.clone(),
                        kind: KIND_DOCUMENT.into(),
                        mime: None,
                        parent: page.parent.or(Some(self.root_id)),
                        trashed: false,
                        trashed_at: None,
                        snapshot,
                    });
                }
                Err(error) => warnings.push(ImportWarning {
                    source: page.absolute.clone(),
                    code: None,
                    arguments: Vec::new(),
                    message: error.to_string(),
                }),
            }
            progress.update(
                ImportStage::Converting,
                self.files.len() + i + 1,
                Some(total),
            );
        }

        progress.update(ImportStage::BuildingBundle, total, Some(total));
        let _ = StoreWorkspaceId::from_uuid(store_ws.as_uuid());
        Ok(ImportProduct::Documents(ImportedContent {
            documents,
            warnings,
        }))
    }
}

fn build_md_page(
    workspace: koru_core::WorkspaceId,
    page: &PlannedPage,
    path_ids: &HashMap<String, DocumentId>,
    context: &ImportContext<'_>,
    progress: &dyn ImportProgress,
    warnings: &mut Vec<ImportWarning>,
) -> Result<Vec<u8>, ImportError> {
    let text = fs::read_to_string(&page.absolute).map_err(failed)?;
    let rewritten = rewrite_notion_links(&text, &page.rel, path_ids, warnings, &page.absolute);
    let (_title, snapshot, page_warnings) = koru_import_markdown::build_document_from_text(
        workspace,
        &page.title,
        page.absolute.to_str().unwrap_or("page.md"),
        &rewritten,
        context,
        progress,
    )?;
    warnings.extend(page_warnings);
    Ok(snapshot)
}

fn build_csv_page(
    workspace: koru_core::WorkspaceId,
    page: &PlannedPage,
    csv_file: Option<&(DocumentId, String)>,
    warnings: &mut Vec<ImportWarning>,
) -> Result<Vec<u8>, ImportError> {
    let bytes = fs::read(&page.absolute).map_err(failed)?;
    let table = match csv_to_gfm_table(&bytes) {
        Ok(table) => table,
        Err(error) => {
            warnings.push(ImportWarning {
                source: page.absolute.clone(),
                code: None,
                arguments: Vec::new(),
                message: format!("could not parse CSV as a table: {error}"),
            });
            String::new()
        }
    };

    let mut doc = WorkspaceDoc::create(workspace, &page.title).map_err(failed)?;
    let root = doc
        .insert(Parent::Root, Node::document(&page.title).map_err(failed)?)
        .map_err(failed)?;
    if !table.is_empty() {
        let table_node = Node::table(table).map_err(failed)?;
        let _ = doc.insert(Parent::Node(root), table_node).map_err(failed)?;
    }
    if let Some((file_id, label)) = csv_file {
        let para = Node::paragraph("").map_err(failed)?;
        let row = doc.insert(Parent::Node(root), para).map_err(failed)?;
        let attr = Attr::labelled(
            "attachment",
            AttrValue::Ref(Ref::in_doc(
                NodeId::from_uuid(file_id.as_uuid()),
                DocRef::Document(koru_core::DocumentId::from_uuid(file_id.as_uuid())),
            )),
            label,
        );
        let _ = doc.insert_pill(row, 0, &attr).map_err(failed)?;
    }
    doc.export_snapshot().map_err(failed)
}

fn build_csv_file_document(
    workspace: koru_core::WorkspaceId,
    page: &PlannedPage,
    context: &ImportContext<'_>,
) -> Result<(DocumentId, String, Vec<u8>), ImportError> {
    let bytes = fs::read(&page.absolute).map_err(failed)?;
    let title = format!("{}.csv", page.title);
    let stored = context
        .blobs
        .put(bytes, "text/csv", Some(&title))
        .map_err(|e| ImportError::new(e.to_string()))?;
    let id = DocumentId::new();
    let mut doc = WorkspaceDoc::create(workspace, &title).map_err(failed)?;
    let mut node = Node::file(&title, BlobHash(stored.hash), stored.mime).map_err(failed)?;
    node.id = NodeId::from_uuid(id.as_uuid());
    let _ = doc.insert(Parent::Root, node).map_err(failed)?;
    Ok((id, title, doc.export_snapshot().map_err(failed)?))
}

fn build_file_document(
    workspace: koru_core::WorkspaceId,
    file: &PlannedFile,
    context: &ImportContext<'_>,
) -> Result<Vec<u8>, ImportError> {
    let bytes = fs::read(&file.absolute).map_err(failed)?;
    let stored = context
        .blobs
        .put(bytes, &file.mime, Some(&file.title))
        .map_err(|e| ImportError::new(e.to_string()))?;
    let mut doc = WorkspaceDoc::create(workspace, &file.title).map_err(failed)?;
    let mut node = Node::file(&file.title, BlobHash(stored.hash), stored.mime).map_err(failed)?;
    node.id = NodeId::from_uuid(file.id.as_uuid());
    let _ = doc.insert(Parent::Root, node).map_err(failed)?;
    doc.export_snapshot().map_err(failed)
}

fn empty_document(workspace: koru_core::WorkspaceId, title: &str) -> Result<Vec<u8>, ImportError> {
    let mut doc = WorkspaceDoc::create(workspace, title).map_err(failed)?;
    let _ = doc
        .insert(Parent::Root, Node::document(title).map_err(failed)?)
        .map_err(failed)?;
    doc.export_snapshot().map_err(failed)
}

fn append_attachment_pills(
    snapshot: &[u8],
    title: &str,
    attachments: &[(DocumentId, String)],
) -> Result<Vec<u8>, ImportError> {
    let mut doc = WorkspaceDoc::from_snapshot(snapshot).map_err(failed)?;
    let root = doc
        .children(Parent::Root)
        .map_err(failed)?
        .into_iter()
        .next()
        .ok_or_else(|| ImportError::new(format!("page {title} has no root")))?;
    for (file, label) in attachments {
        let para = Node::paragraph("").map_err(failed)?;
        let row = doc.insert(Parent::Node(root), para).map_err(failed)?;
        let attr = Attr::labelled(
            "attachment",
            AttrValue::Ref(Ref::in_doc(
                NodeId::from_uuid(file.as_uuid()),
                DocRef::Document(koru_core::DocumentId::from_uuid(file.as_uuid())),
            )),
            label,
        );
        let _ = doc.insert_pill(row, 0, &attr).map_err(failed)?;
    }
    doc.export_snapshot().map_err(failed)
}

fn rewrite_notion_links(
    text: &str,
    page_rel: &str,
    path_ids: &HashMap<String, DocumentId>,
    warnings: &mut Vec<ImportWarning>,
    source: &Path,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let Some(label_end) = rest.find(']') else {
            out.push_str(rest);
            return out;
        };
        if rest.as_bytes().get(label_end + 1) != Some(&b'(') {
            out.push('[');
            rest = &rest[1..];
            continue;
        }
        let label = &rest[1..label_end];
        let after_paren = &rest[label_end + 2..];
        let Some(close) = after_paren.find(')') else {
            out.push_str(rest);
            return out;
        };
        let target = &after_paren[..close];
        if let Some(id) = resolve_link_target(page_rel, target, path_ids) {
            // Pill only: the MD importer stores an unlabelled Document attr and
            // the host caption cache supplies the real title. Emitting the
            // Notion label as plain text beside the token duplicated it once
            // captions worked ("Business Plan Business Plan").
            let _ = label;
            let link = koru_link::document_link(id.as_uuid(), None);
            out.push_str("[[");
            out.push_str(&link);
            out.push_str("]]");
        } else {
            out.push_str(&rest[..label_end + 2 + close + 1]);
            if looks_like_md_link(target) {
                warnings.push(ImportWarning {
                    source: source.to_path_buf(),
                    code: None,
                    arguments: Vec::new(),
                    message: format!("could not resolve Notion link: {target}"),
                });
            }
        }
        rest = &after_paren[close + 1..];
    }
    out.push_str(rest);
    out
}

fn resolve_link_target(
    page_rel: &str,
    target: &str,
    path_ids: &HashMap<String, DocumentId>,
) -> Option<DocumentId> {
    let target = target.split(['#', '?']).next().unwrap_or(target).trim();
    if target.is_empty() || target.contains("://") {
        return None;
    }
    let decoded = percent_decode(target);
    let page_dir = parent_rel(page_rel);
    let joined = if page_dir.is_empty() {
        decoded.clone()
    } else {
        format!("{page_dir}/{decoded}")
    };
    let normalized = normalize_rel(&joined);
    path_ids
        .get(&normalized)
        .or_else(|| path_ids.get(&normalize_rel(&decoded)))
        .copied()
}

fn looks_like_md_link(target: &str) -> bool {
    let t = target.to_ascii_lowercase();
    t.ends_with(".md") || t.contains(".md#") || t.contains('%')
}

fn csv_to_gfm_table(bytes: &[u8]) -> Result<String, String> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(bytes);
    let headers = reader
        .headers()
        .map_err(|e| e.to_string())?
        .iter()
        .map(escape_cell)
        .collect::<Vec<_>>();
    if headers.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    out.push('|');
    out.push_str(&headers.join("|"));
    out.push('|');
    out.push('\n');
    out.push('|');
    out.push_str(&vec!["---"; headers.len()].join("|"));
    out.push('|');
    out.push('\n');
    for row in reader.records() {
        let row = row.map_err(|e| e.to_string())?;
        let cells: Vec<String> = (0..headers.len())
            .map(|i| escape_cell(row.get(i).unwrap_or("")))
            .collect();
        out.push('|');
        out.push_str(&cells.join("|"));
        out.push('|');
        out.push('\n');
    }
    Ok(out)
}

fn escape_cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ").replace('\r', "")
}

fn extract_zip(
    source: &Path,
    target: &Path,
    warnings: &mut Vec<ImportWarning>,
) -> Result<(), ImportError> {
    let file = File::open(source).map_err(failed)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| ImportError::new(format!("not a readable zip archive: {e}")))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(ImportError::new("the Notion export has too many entries"));
    }
    let mut unique = HashSet::new();
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(failed)?;
        let path = entry.enclosed_name().ok_or_else(|| {
            ImportError::new(format!(
                "unsafe archive path: {}",
                String::from_utf8_lossy(entry.name_raw())
            ))
        })?;
        if !unique.insert(path.clone()) {
            return Err(ImportError::new("the Notion export has duplicate paths"));
        }
        expanded = expanded
            .checked_add(entry.size())
            .ok_or_else(|| ImportError::new("the Notion export size overflowed"))?;
        if expanded > MAX_ARCHIVE_BYTES {
            return Err(ImportError::new(
                "the Notion export expands beyond the safety limit",
            ));
        }
        let out = target.join(&path);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(failed)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent).map_err(failed)?;
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).map_err(failed)?;
        if bytes.len() as u64 > MAX_FILE_BYTES {
            warnings.push(ImportWarning {
                source: out.clone(),
                code: None,
                arguments: Vec::new(),
                message: "skipped: entry exceeds the 2 GiB limit".into(),
            });
            continue;
        }
        let mut file = File::create(&out).map_err(failed)?;
        file.write_all(&bytes).map_err(failed)?;
    }
    Ok(())
}

fn walk_files(root: &Path, f: &mut dyn FnMut(PathBuf, String)) -> Result<(), ImportError> {
    fn walk(
        dir: &Path,
        root: &Path,
        f: &mut dyn FnMut(PathBuf, String),
    ) -> Result<(), ImportError> {
        for entry in fs::read_dir(dir).map_err(failed)? {
            let entry = entry.map_err(failed)?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, f)?;
            } else if path.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                f(path, rel);
            }
        }
        Ok(())
    }
    walk(root, root, f)
}

fn register_path_variants(map: &mut HashMap<String, DocumentId>, rel: &str, id: DocumentId) {
    let norm = normalize_rel(rel);
    map.insert(norm.clone(), id);
    map.insert(percent_encode_path(&norm), id);
    if let Some(stripped) = norm.strip_prefix("./") {
        map.insert(stripped.to_owned(), id);
    }
    let decoded = percent_decode(&norm);
    if decoded != norm {
        map.insert(normalize_rel(&decoded), id);
    }
}

fn normalize_rel(rel: &str) -> String {
    let normalized = rel.replace('\\', "/");
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

fn parent_rel(rel: &str) -> String {
    let norm = normalize_rel(rel);
    match norm.rsplit_once('/') {
        Some((parent, _)) => parent.to_owned(),
        None => String::new(),
    }
}

fn path_depth(rel: &str) -> usize {
    normalize_rel(rel).matches('/').count()
}

fn file_stem(rel: &str) -> &str {
    Path::new(rel)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(rel)
}

fn strip_notion_id(stem: &str) -> String {
    if has_notion_id_suffix(stem) {
        stem.rsplit_once(' ')
            .map(|(name, _)| name.to_owned())
            .unwrap_or_else(|| stem.to_owned())
    } else {
        stem.to_owned()
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2]))
        {
            out.push((h << 4) | l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode_path(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'/' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn is_noise(lower: &str) -> bool {
    lower.ends_with(".ds_store") || lower.contains("__macosx/") || lower.ends_with("thumbs.db")
}

fn mime_for(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "csv" => "text/csv",
        "txt" => "text/plain",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
    .into()
}

fn failed(error: impl std::fmt::Display) -> ImportError {
    ImportError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_import::{IdentityMode, ImportOptions, NoProgress, StoredBlob};
    use std::sync::Mutex;

    struct MemBlobs(Mutex<Vec<Vec<u8>>>);

    impl koru_import::BlobSink for MemBlobs {
        fn put(
            &self,
            bytes: Vec<u8>,
            mime: &str,
            _filename: Option<&str>,
        ) -> Result<StoredBlob, ImportError> {
            let hash = format!("{:x}", bytes.len());
            self.0.lock().unwrap().push(bytes);
            Ok(StoredBlob {
                hash,
                mime: mime.to_owned(),
            })
        }
    }

    fn write_zip(entries: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.zip");
        let file = File::create(&path).unwrap();
        let mut writer = zip::ZipWriter::new(file);
        for (name, body) in entries {
            writer
                .start_file::<_, ()>(*name, zip::write::FileOptions::default())
                .unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
        (dir, path)
    }

    #[test]
    fn notion_id_suffix_is_detected() {
        assert!(has_notion_id_suffix(
            "Nested Page 22222222222222222222222222222222"
        ));
        assert!(!has_notion_id_suffix("Ordinary Name"));
        assert!(!has_notion_id_suffix("short abcd"));
    }

    #[test]
    fn sniffer_claims_notion_exports_only() {
        assert!(claims_notion_zip(&[
            "Workspace 11111111111111111111111111111111.md".into()
        ]));
        assert!(!claims_notion_zip(&["text.md".into(), "info.json".into()]));
        assert!(!claims_notion_zip(&["Notebook/Section.one".into()]));
    }

    #[test]
    fn strip_notion_id_cleans_titles() {
        assert_eq!(
            strip_notion_id("Nested Page 22222222222222222222222222222222"),
            "Nested Page"
        );
        assert_eq!(strip_notion_id("Plain"), "Plain");
    }

    #[test]
    fn csv_becomes_a_gfm_table() {
        let table = csv_to_gfm_table(b"Name,Status\nAlpha,Done\nBeta,\"In, Progress\"\n").unwrap();
        assert!(table.contains("|Name|Status|"));
        assert!(table.contains("|Alpha|Done|"));
        assert!(table.contains("In, Progress"));
    }

    #[test]
    fn percent_encoded_links_resolve() {
        let mut map = HashMap::new();
        let id = DocumentId::new();
        register_path_variants(&mut map, "Project Plan abc.md", id);
        let found = resolve_link_target("other/page.md", "../Project%20Plan%20abc.md", &map);
        assert_eq!(found, Some(id));
    }

    #[test]
    fn rewritten_page_links_are_pill_only_without_adjacent_label() {
        let mut map = HashMap::new();
        let id = DocumentId::new();
        register_path_variants(
            &mut map,
            "Business Plan 33333333333333333333333333333333.md",
            id,
        );
        let mut warnings = Vec::new();
        let out = rewrite_notion_links(
            "See [Business Plan](Business%20Plan%2033333333333333333333333333333333.md) now.",
            "page.md",
            &map,
            &mut warnings,
            Path::new("page.md"),
        );
        assert!(
            !out.contains("Business Plan [["),
            "label must not sit beside the token: {out}"
        );
        assert!(out.contains("[[koru://doc/"), "{out}");
        assert!(out.starts_with("See "));
        assert!(out.ends_with(" now."));
        assert!(warnings.is_empty());
    }

    #[test]
    fn nested_pages_strip_ids_and_nest_under_a_root() {
        let id = "11111111111111111111111111111111";
        let nid = "22222222222222222222222222222222";
        let (_dir, zip) = write_zip(&[
            (&format!("Workspace {id}.md"), "# Workspace\n\nHello\n"),
            (
                &format!("Workspace {id}/Nested Page {nid}.md"),
                "# Nested\n\nChild\n",
            ),
        ]);
        let prepared = NotionImporter
            .inspect(ImportSource::Files(vec![zip]), &NoProgress)
            .unwrap();
        assert_eq!(prepared.preview().document_count, 3);
        let blobs = MemBlobs(Mutex::new(Vec::new()));
        let product = prepared
            .build(
                ImportOptions {
                    identity: IdentityMode::Copy,
                    accept_warnings: true,
                },
                &ImportContext {
                    target_profile: None,
                    target_workspace: Some(StoreWorkspaceId::new()),
                    occupied_titles: &std::collections::BTreeSet::new(),
                    blobs: &blobs,
                    tags: &koru_import::NoTagSink,
                },
                &NoProgress,
            )
            .unwrap();
        let ImportProduct::Documents(content) = product else {
            panic!("expected documents");
        };
        assert!(content.documents.len() >= 3);
        let root = content
            .documents
            .iter()
            .find(|d| d.parent.is_none() && d.kind == KIND_DOCUMENT)
            .expect("root container");
        let workspace_page = content
            .documents
            .iter()
            .find(|d| d.title == "Workspace")
            .expect("workspace page");
        assert_eq!(workspace_page.parent, Some(root.id));
        let nested = content
            .documents
            .iter()
            .find(|d| d.title == "Nested Page")
            .expect("nested page");
        assert_eq!(nested.parent, Some(workspace_page.id));
    }

    /// **An import restores; it never commands** (SPEC §5, ADR-273).
    ///
    /// A Notion to-do is a checked box, which is content, and never a completed
    /// task: writing a marker would invent a creation reason the export never
    /// recorded, and a completed task over an unchecked child would then raise
    /// the question of whether the child reopens it. Neither question arises,
    /// because an importer's exemption is that the classification entry point
    /// and the six canonical commands are simply not on this path — which this
    /// test reads off the crate itself rather than taking on trust.
    #[test]
    fn a_notion_to_do_lands_verbatim_and_no_task_is_invented() {
        let id = "11111111111111111111111111111111";
        let (_dir, zip) = write_zip(&[(
            &format!("Shipping {id}.md"),
            "# Shipping\n\n- [x] ship it\n  - [ ] draft the summary\n",
        )]);
        let prepared = NotionImporter
            .inspect(ImportSource::Files(vec![zip]), &NoProgress)
            .unwrap();
        let blobs = MemBlobs(Mutex::new(Vec::new()));
        let product = prepared
            .build(
                ImportOptions {
                    identity: IdentityMode::Copy,
                    accept_warnings: true,
                },
                &ImportContext {
                    target_profile: None,
                    target_workspace: Some(StoreWorkspaceId::new()),
                    occupied_titles: &std::collections::BTreeSet::new(),
                    blobs: &blobs,
                    tags: &koru_import::NoTagSink,
                },
                &NoProgress,
            )
            .unwrap();
        let ImportProduct::Documents(content) = product else {
            panic!("expected documents");
        };
        let page = content
            .documents
            .iter()
            .find(|d| d.title == "Shipping")
            .expect("the page");
        let doc = WorkspaceDoc::from_snapshot(&page.snapshot).unwrap();
        let by_text = |want: &str| {
            doc.walk_all()
                .unwrap()
                .into_iter()
                .map(|(id, _)| doc.read(id).unwrap())
                .find(|node| node.content.as_text() == Some(want))
                .unwrap_or_else(|| panic!("no row reading {want:?}"))
        };
        assert!(by_text("ship it").meta.done, "the tick came across");
        assert!(
            !by_text("draft the summary").meta.done,
            "and so did the empty box — verbatim, either way"
        );
        for (node, _) in doc.walk_all().unwrap() {
            assert!(
                !koru_core::tasks::is_task_node(&doc, node).unwrap(),
                "no marker and no due were fabricated on the way in"
            );
        }

        // The names are assembled here so the scan does not find them in the
        // very source it is reading.
        let source = include_str!("lib.rs");
        for halves in [
            ["reclas", "sify"],
            ["ensure", "_task"],
            ["complete", "_task"],
            ["reopen", "_task"],
            ["set", "_due"],
            ["clear", "_due"],
            ["remove_from", "_tasks"],
        ] {
            let name = halves.concat();
            assert!(
                !source.contains(&name),
                "an importer is a faithful restore, so `{name}` is not on its path"
            );
        }
    }
}
