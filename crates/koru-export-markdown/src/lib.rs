// SPDX-License-Identifier: MPL-2.0

//! Markdown export for one Koru document (or several into a folder).
//!
//! Source of truth is the tree + runs + meta + attrs — never stored markdown
//! delimiters (except [`Kind::Table`], which already holds GFM source).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use koru_core::node::{Attr, AttrValue, Content, Heading, Kind, ListStyle, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{NodeId, TextRun};
use koru_export::{
    BlobSource, ExportDocument, ExportError, ExportProgress, ExportResult, ExportStage, Exporter,
    ExporterDescriptor, unique_path,
};

static DESCRIPTOR: ExporterDescriptor = ExporterDescriptor {
    id: "markdown",
    name: "Markdown",
    extensions: &["md"],
};

pub struct MarkdownExporter;

impl Exporter for MarkdownExporter {
    fn descriptor(&self) -> &'static ExporterDescriptor {
        &DESCRIPTOR
    }

    fn export(
        &self,
        documents: &[ExportDocument],
        destination: &Path,
        blobs: &dyn BlobSource,
        progress: &dyn ExportProgress,
    ) -> Result<ExportResult, ExportError> {
        if documents.is_empty() {
            return Err(ExportError::new("nothing to export"));
        }
        if progress.cancelled() {
            return Err(ExportError::cancelled());
        }

        let multi = documents.len() > 1;
        let (dir, single_path): (PathBuf, Option<PathBuf>) = if multi {
            if !destination.is_dir() {
                return Err(ExportError::new(format!(
                    "multi-document export needs a directory, got {}",
                    destination.display()
                )));
            }
            (destination.to_path_buf(), None)
        } else {
            let parent = destination.parent().unwrap_or_else(|| Path::new("."));
            (parent.to_path_buf(), Some(destination.to_path_buf()))
        };

        let total = documents.len();
        let mut files = Vec::with_capacity(total);
        for (i, document) in documents.iter().enumerate() {
            progress.update(ExportStage::Converting, i, Some(total));
            if progress.cancelled() {
                return Err(ExportError::cancelled());
            }

            let path = match &single_path {
                Some(p) => p.clone(),
                None => {
                    let name = koru_export::suggested_file_name(&document.title, &["md"]);
                    unique_path(&dir, &name)
                }
            };

            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("document");
            let assets_rel = format!("{stem}.assets");
            let rendered = render_document(&document.doc, blobs, &format!("./{assets_rel}"))?;

            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| ExportError::new(e.to_string()))?;
            }
            progress.update(ExportStage::Writing, i, Some(total));
            write_text_atomic(&path, &rendered.markdown)?;

            if !rendered.assets.is_empty() {
                progress.update(ExportStage::WritingMedia, i, Some(total));
                let assets_dir = path
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(&assets_rel);
                fs::create_dir_all(&assets_dir).map_err(|e| ExportError::new(e.to_string()))?;
                for asset in &rendered.assets {
                    let dest = assets_dir.join(&asset.file_name);
                    write_bytes_atomic(&dest, &asset.bytes)?;
                }
            }
            files.push(path);
        }

        let remembered_dir = if multi {
            destination.to_path_buf()
        } else {
            files
                .first()
                .and_then(|p| p.parent())
                .unwrap_or(destination)
                .to_path_buf()
        };

        Ok(ExportResult {
            files,
            remembered_dir,
        })
    }
}

/// One rendered media file. Format containers such as TextBundle reuse the
/// Markdown renderer and decide where these bytes live.
pub struct RenderedAsset {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

/// Markdown text plus sidecar image bytes ready to write.
pub struct Rendered {
    pub markdown: String,
    pub assets: Vec<RenderedAsset>,
}

/// Pure render used by tests and the exporter.
pub fn render_document(
    ws: &WorkspaceDoc,
    blobs: &dyn BlobSource,
    assets_rel: &str,
) -> Result<Rendered, ExportError> {
    let root = document_root(ws).ok_or_else(|| ExportError::new("document has no root"))?;
    let title = title_of(ws, root);
    let mut out = String::new();
    let mut assets = Vec::new();

    if !title.is_empty() {
        out.push_str("# ");
        out.push_str(&escape_md_text(&title));
        out.push_str("\n\n");
    }

    emit_children(
        ws,
        Parent::Node(root),
        0,
        None,
        blobs,
        assets_rel,
        &mut out,
        &mut assets,
    )?;
    // One trailing newline is enough; trim excess blank lines at the end.
    let markdown = out.trim_end().to_owned() + "\n";
    Ok(Rendered { markdown, assets })
}

/// One block and its subtree, with the same rules as the whole document —
/// for a caller that was given a paragraph rather than a page (a delegated
/// run's block-level context, ROADMAP §7.2). No title line: the block's own
/// text is the content, and a heading it did not have would be invented.
pub fn render_node(
    ws: &WorkspaceDoc,
    id: NodeId,
    blobs: &dyn BlobSource,
    assets_rel: &str,
) -> Result<Rendered, ExportError> {
    let node = ws.read(id).map_err(|e| ExportError::new(e.to_string()))?;
    let mut out = String::new();
    let mut assets = Vec::new();
    emit_node(
        ws,
        id,
        &node,
        0,
        None,
        0,
        blobs,
        assets_rel,
        &mut out,
        &mut assets,
    )?;
    let markdown = out.trim_end().to_owned() + "\n";
    Ok(Rendered { markdown, assets })
}

/// A chat message typed as a Koru document — the AI chat's composer (AGENTUX
/// I6): each child paragraph of `root` **as it was typed**, pills in this
/// exporter's own spelling, paragraphs joined by **one** newline.
///
/// Two things differ from [`render_node`], each on purpose:
///
/// - **Text is not escaped.** A person writing to a chat writes Markdown by
///   hand — a fence, a `*`, an `_` in an identifier — and the chat has always
///   sent those bytes as typed. Escaping them would hand an agent
///   `my\_var \* 2` for what the person wrote. Marks are not written either:
///   the composer policy refuses them, so a composer paragraph has none.
/// - **Paragraphs are lines.** The composer's line break is a new paragraph
///   (a paragraph may not hold `\n`), and a message reads exactly as the
///   one-block field it replaced sent it: Shift+Enter is one newline, not a
///   blank line between blocks.
///
/// What is shared is the part that must never drift: a document pill is
/// `[[koru://doc/<id>]]` (or `…#<node>`) here exactly as in a document export,
/// so the importer, the chat renderer and this writer agree on one token.
pub fn message_text(ws: &WorkspaceDoc, root: NodeId) -> Result<String, ExportError> {
    let children = ws
        .children(Parent::Node(root))
        .map_err(|e| ExportError::new(e.to_string()))?;
    let mut lines = Vec::with_capacity(children.len());
    for id in children {
        let node = ws.read(id).map_err(|e| ExportError::new(e.to_string()))?;
        if node.content.as_text().is_none() {
            continue;
        }
        let runs = ws.runs(id).map_err(|e| ExportError::new(e.to_string()))?;
        let attrs = node.content.attrs();
        let mut line = String::new();
        for run in &runs {
            if let Some(pill) = run.pill {
                if let Some(attr) = attrs.iter().find(|a| a.id == pill) {
                    line.push_str(&serialize_pill(attr));
                }
                continue;
            }
            if run.is_pill() {
                continue;
            }
            line.push_str(&run.text);
        }
        lines.push(line);
    }
    Ok(lines.join("\n"))
}

fn document_root(ws: &WorkspaceDoc) -> Option<NodeId> {
    ws.children(Parent::Root)
        .ok()?
        .into_iter()
        .find(|&id| ws.kind_of(id) == Some(Kind::Document))
}

fn title_of(ws: &WorkspaceDoc, root: NodeId) -> String {
    ws.read(root)
        .ok()
        .and_then(|n| n.content.as_text().map(str::to_owned))
        .unwrap_or_default()
}

fn emit_children(
    ws: &WorkspaceDoc,
    parent: Parent,
    list_depth: usize,
    list_style: Option<ListStyle>,
    blobs: &dyn BlobSource,
    assets_rel: &str,
    out: &mut String,
    assets: &mut Vec<RenderedAsset>,
) -> Result<(), ExportError> {
    let children = ws
        .children(parent)
        .map_err(|e| ExportError::new(e.to_string()))?;
    let mut item_index = 0usize;
    for id in children {
        let node = ws.read(id).map_err(|e| ExportError::new(e.to_string()))?;
        // Nested explore pages are separate documents — only export if selected.
        if node.kind == Kind::Document {
            continue;
        }
        if list_style.is_some() {
            item_index += 1;
        }
        emit_node(
            ws, id, &node, list_depth, list_style, item_index, blobs, assets_rel, out, assets,
        )?;
    }
    Ok(())
}

fn emit_node(
    ws: &WorkspaceDoc,
    id: NodeId,
    node: &Node,
    list_depth: usize,
    parent_list: Option<ListStyle>,
    item_index: usize,
    blobs: &dyn BlobSource,
    assets_rel: &str,
    out: &mut String,
    assets: &mut Vec<RenderedAsset>,
) -> Result<(), ExportError> {
    match node.kind {
        Kind::ListBlock => {
            let style = node.meta.list_style.unwrap_or(ListStyle::Bullet);
            if matches!(style, ListStyle::Letter | ListStyle::Roman) {
                out.push_str("<!-- letter/roman list omitted -->\n\n");
                return Ok(());
            }
            emit_children(
                ws,
                Parent::Node(id),
                list_depth,
                Some(style),
                blobs,
                assets_rel,
                out,
                assets,
            )?;
            if list_depth == 0 {
                out.push('\n');
            }
        }
        Kind::Paragraph | Kind::Task | Kind::Quote | Kind::Contact => {
            if let Some(style) = parent_list {
                let indent = "  ".repeat(list_depth);
                let marker = list_marker(style, item_index, node.meta.done);
                out.push_str(&indent);
                out.push_str(&marker);
                out.push_str(&inline_line(ws, id, node)?);
                out.push('\n');
                // Nested blocks under a list item.
                emit_children(
                    ws,
                    Parent::Node(id),
                    list_depth + 1,
                    None,
                    blobs,
                    assets_rel,
                    out,
                    assets,
                )?;
            } else if node.kind == Kind::Task {
                let indent = "  ".repeat(list_depth);
                let box_ = if node.meta.done { "[x]" } else { "[ ]" };
                out.push_str(&indent);
                out.push_str("- ");
                out.push_str(box_);
                out.push(' ');
                out.push_str(&inline_line(ws, id, node)?);
                out.push('\n');
                emit_children(
                    ws,
                    Parent::Node(id),
                    list_depth + 1,
                    None,
                    blobs,
                    assets_rel,
                    out,
                    assets,
                )?;
                if list_depth == 0 {
                    out.push('\n');
                }
            } else {
                let prefix = block_prefix(node);
                out.push_str(prefix);
                out.push_str(&inline_line(ws, id, node)?);
                out.push_str("\n\n");
                emit_children(
                    ws,
                    Parent::Node(id),
                    list_depth,
                    None,
                    blobs,
                    assets_rel,
                    out,
                    assets,
                )?;
            }
        }
        Kind::Code => {
            let lang = node.meta.language.as_deref().unwrap_or("");
            let body = node.content.as_text().unwrap_or("");
            // The model spells the fence, and the clipboard's plain-text
            // flavor spells it the same way, so a block exported and a block
            // copied read alike elsewhere. It lengthens past any backtick run
            // the body holds; three was once written unconditionally, and a
            // body quoting a fence ended the block early.
            out.push_str(&koru_core::markdown::fenced_code(lang, body));
            out.push_str("\n\n");
        }
        Kind::Table => {
            let body = node.content.as_text().unwrap_or("").trim_end();
            out.push_str(body);
            out.push_str("\n\n");
        }
        Kind::Divider => {
            out.push_str("---\n\n");
        }
        Kind::Image => match &node.content {
            Content::Blob { hash, mime, .. } => {
                let alt = node.meta.name.clone().unwrap_or_else(|| "image".into());
                match blobs.get(hash) {
                    Ok(bytes) => {
                        let file_name = image_file_name(hash, mime, assets.len());
                        out.push_str(&format!(
                            "![{}]({}/{})\n\n",
                            escape_md_text(&alt),
                            assets_rel,
                            file_name
                        ));
                        assets.push(RenderedAsset { file_name, bytes });
                    }
                    Err(_) => {
                        out.push_str(&format!(
                            "<!-- image omitted: {} -->\n\n",
                            escape_md_text(&alt)
                        ));
                    }
                }
            }
            _ => out.push_str("<!-- image omitted -->\n\n"),
        },
        Kind::Canvas => {
            out.push_str("<!-- canvas omitted -->\n\n");
        }
        Kind::Trash | Kind::Document | Kind::File => {}
    }

    // Block-level embeds replace content; skip with a marker when present.
    if matches!(node.content, Content::Embed { .. }) {
        out.push_str("<!-- embed omitted -->\n\n");
    }
    Ok(())
}

fn block_prefix(node: &Node) -> &'static str {
    if node.kind == Kind::Quote {
        return "> ";
    }
    match node.meta.heading {
        Some(Heading::H1) => "# ",
        Some(Heading::H2) => "## ",
        Some(Heading::H3) => "### ",
        Some(Heading::H4) => "#### ",
        Some(Heading::H5) => "##### ",
        Some(Heading::H6) => "###### ",
        None => "",
    }
}

fn list_marker(style: ListStyle, index: usize, done: bool) -> String {
    match style {
        ListStyle::Bullet => "- ".into(),
        ListStyle::Checkbox => {
            if done {
                "- [x] ".into()
            } else {
                "- [ ] ".into()
            }
        }
        ListStyle::Numbered => format!("{index}. "),
        ListStyle::Letter | ListStyle::Roman => "- ".into(),
    }
}

fn inline_line(ws: &WorkspaceDoc, id: NodeId, node: &Node) -> Result<String, ExportError> {
    let runs = ws.runs(id).map_err(|e| ExportError::new(e.to_string()))?;
    let attrs = node.content.attrs();
    Ok(serialize_runs(&runs, attrs))
}

fn serialize_runs(runs: &[TextRun], attrs: &[Attr]) -> String {
    let mut out = String::new();
    for run in runs {
        if let Some(pill_id) = run.pill {
            if let Some(attr) = attrs.iter().find(|a| a.id == pill_id) {
                out.push_str(&serialize_pill(attr));
            }
            continue;
        }
        if run.is_pill() {
            continue;
        }
        out.push_str(&wrap_marks(&run.text, run));
    }
    out
}

fn serialize_pill(attr: &Attr) -> String {
    match &attr.value {
        AttrValue::Url(url) => {
            let label = attr
                .label
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(url.as_str());
            format!("[{}]({})", escape_md_text(label), url)
        }
        AttrValue::Ref(r) => {
            format!("[[{}]]", koru_link::node_link(r.node.as_uuid()))
        }
        // Through `koru_link`, so what is written here is by construction what
        // the importer accepts -- and so the node travels, which is the whole
        // of a deep link.
        AttrValue::Document(reference) => {
            format!(
                "[[{}]]",
                koru_link::document_link(
                    reference.document.as_uuid(),
                    reference.node.map(|node| node.as_uuid()),
                )
            )
        }
        AttrValue::Date(d) => {
            let label = attr.label.clone().unwrap_or_else(|| d.to_storage());
            escape_md_text(&label)
        }
        AttrValue::Text(t) => {
            let label = attr.label.as_deref().unwrap_or(t.as_str());
            escape_md_text(label)
        }
        // The temporal shapes (ADR-183). No pill writes them yet; when one
        // does, the label is the honest export — Markdown has no richer form.
        //
        // A floating value exports its wall clock **and says nothing about a
        // zone**, because it has none; a zoned one names its own zone rather
        // than being rendered in UTC, which would be a different moment's
        // spelling of the same instant and read as though that were what
        // somebody meant.
        AttrValue::FloatingDateTime(wall) => {
            let label = attr.label.clone().unwrap_or_else(|| wall.to_storage());
            escape_md_text(&label)
        }
        AttrValue::ZonedDateTime(zoned) => {
            let label = attr.label.clone().unwrap_or_else(|| {
                chrono::DateTime::from_timestamp_millis(zoned.instant_ms())
                    .map(|dt| format!("{} ({})", dt.format("%Y-%m-%d %H:%M UTC"), zoned.zone()))
                    .unwrap_or_else(|| zoned.instant_ms().to_string())
            });
            escape_md_text(&label)
        }
        AttrValue::Number(n) => {
            let label = attr.label.clone().unwrap_or_else(|| n.to_string());
            escape_md_text(&label)
        }
        AttrValue::Bool(b) => {
            let label = attr.label.clone().unwrap_or_else(|| b.to_string());
            escape_md_text(&label)
        }
    }
}

fn wrap_marks(text: &str, run: &TextRun) -> String {
    if text.is_empty() {
        return String::new();
    }
    if run.code {
        return format!("`{}`", text.replace('`', "\\`"));
    }
    let mut s = escape_md_text(text);
    // Innermost first so nested markers stay balanced for common cases.
    if run.strike {
        s = format!("~~{s}~~");
    }
    if run.italic {
        s = format!("*{s}*");
    }
    if run.bold {
        s = format!("**{s}**");
    }
    // underline / hl / sz / sc deliberately dropped
    s
}

/// Backslash what would otherwise be markup, **and nothing else**.
///
/// The set used to include `(`, `)`, `{`, `}` and every `!`, none of which can
/// start a construct on its own: parentheses and braces are ordinary text in
/// CommonMark — a link's destination is built by [`serialize_pill`] and never
/// by this function — and `!` is markup only immediately before `[`. Escaping
/// them made every export noisier than it needed to be, and it made one thing
/// outright wrong: an HTML comment came back with a backslash inside its
/// opener and stopped being a comment. That matters now that a document can be
/// read *out* of Markdown, edited and written back (ADR-363) — a round trip
/// must not quietly turn a person's comment into visible text.
fn escape_md_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '#' | '+' | '|' => {
                out.push('\\');
                out.push(c);
            }
            // An image is `![`. A lone `!` is a full stop's louder cousin.
            '!' if chars.peek() == Some(&'[') => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn image_file_name(hash: &koru_core::node::BlobHash, mime: &str, index: usize) -> String {
    let ext = mime_extension(mime);
    let raw = hash.0.as_str();
    let stem = raw
        .rsplit(':')
        .next()
        .unwrap_or(raw)
        .chars()
        .take(16)
        .collect::<String>();
    if stem.is_empty() {
        format!("image-{index}.{ext}")
    } else {
        format!("{stem}.{ext}")
    }
}

fn mime_extension(mime: &str) -> &'static str {
    match mime {
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/bmp" => "bmp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        _ => "bin",
    }
}

fn write_text_atomic(path: &Path, text: &str) -> Result<(), ExportError> {
    write_bytes_atomic(path, text.as_bytes())
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), ExportError> {
    let part = part_path(path);
    let result = (|| -> Result<(), ExportError> {
        let mut file = fs::File::create(&part).map_err(|e| ExportError::new(e.to_string()))?;
        file.write_all(bytes)
            .map_err(|e| ExportError::new(e.to_string()))?;
        file.sync_all()
            .map_err(|e| ExportError::new(e.to_string()))?;
        fs::rename(&part, path).map_err(|e| ExportError::new(e.to_string()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

fn part_path(to: &Path) -> PathBuf {
    let name = to
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("export.md");
    to.with_file_name(format!("{name}.{}.part", std::process::id()))
}

#[cfg(test)]
mod tests;
