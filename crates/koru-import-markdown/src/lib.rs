// SPDX-License-Identifier: MPL-2.0

//! Markdown import and the process-wide Markdown semantic adapter.
//!
//! Files and clipboard paste deliberately use this one parser. The importer
//! turns its semantic blocks into a complete Koru document; `koru-workspace-host`
//! adapts the same blocks to its rich-paste transport.
//!
//! **An import is a faithful restore, not a thing a person just did**
//! ([ADR-286], SPEC §5). A `- [x]` lands as a *checked box*, which is content,
//! and never as a completed task: no marker is fabricated, no deadline is
//! invented, no lifecycle command runs and nothing classifies — so nothing here
//! can reopen a task the archive recorded as done, and no activity is owed. The
//! exemption is not a flag this crate sets; it is that the classification entry
//! point and the six canonical commands are not on this path at all, which
//! `a_checked_box_lands_verbatim_and_no_task_is_invented` reads off the source
//! itself. Anything that ever *does* need to write task state here goes through
//! `koru_core::tasks::faithful_import` and nowhere else.
//!
//! [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them

use std::path::{Path, PathBuf};

use koru_core::node::{Attr, AttrValue, BlobHash, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{Heading, ListStyle, Mark, MarkSet, NodeId};
use koru_import::{
    IdentityMode, ImportContext, ImportError, ImportOptions, ImportPreview, ImportProduct,
    ImportProgress, ImportSource, ImportStage, ImportWarning, ImportedContent, ImportedDocument,
    Importer, ImporterDescriptor, PreparedImport, PreviewNode, SourceRequest,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub static DESCRIPTOR: ImporterDescriptor = ImporterDescriptor {
    id: "markdown",
    name: "Markdown",
    source: SourceRequest::Files {
        extensions: &["md", "markdown"],
    },
    menu_name: Some("Markdown"),
    picker_title: "Import Markdown files",
    claims_zip: None,
    roots_import: false,
    dedups_known_files: false,
};

pub struct MarkdownImporter;

impl Importer for MarkdownImporter {
    fn descriptor(&self) -> &'static ImporterDescriptor {
        &DESCRIPTOR
    }

    fn inspect(
        &self,
        source: ImportSource,
        progress: &dyn ImportProgress,
    ) -> Result<Box<dyn PreparedImport>, ImportError> {
        let ImportSource::Files(paths) = source else {
            return Err(ImportError::new("Markdown import needs one or more files"));
        };
        if paths.is_empty() {
            return Err(ImportError::new("no Markdown files were selected"));
        }
        let total = paths.len();
        let mut files = Vec::new();
        let mut warnings = Vec::new();
        let mut previews = Vec::new();
        let mut node_count = 0usize;
        let mut image_count = 0usize;
        for (index, path) in paths.into_iter().enumerate() {
            if progress.cancelled() {
                return Err(ImportError::cancelled());
            }
            progress.update(ImportStage::Inspecting, index, Some(total));
            if !is_markdown(&path) {
                warnings.push(ImportWarning {
                    source: path.clone(),
                    code: None,
                    arguments: Vec::new(),
                    message: "The file extension is not .md or .markdown.".into(),
                });
                continue;
            }
            match read_markdown(&path) {
                Ok(text) => {
                    let parsed = parse_document(&path, &text);
                    let nodes = parsed
                        .blocks
                        .iter()
                        .filter(|b| !matches!(b, Block::CalloutEnd))
                        .count()
                        + 1;
                    image_count += parsed
                        .blocks
                        .iter()
                        .filter(|b| matches!(b, Block::Image(_)))
                        .count();
                    node_count += nodes;
                    previews.push(PreviewNode {
                        title: parsed.title.clone(),
                        children: Vec::new(),
                        nodes,
                        trashed: false,
                    });
                    files.push(SourceFile { path, text });
                }
                Err(error) => warnings.push(ImportWarning {
                    source: path,
                    code: None,
                    arguments: Vec::new(),
                    message: error.to_string(),
                }),
            }
        }
        if files.is_empty() {
            let details = warnings
                .first()
                .map(|w| w.message.as_str())
                .unwrap_or("none of the selected files could be read");
            return Err(ImportError::new(details));
        }
        progress.update(ImportStage::Inspecting, total, Some(total));
        let display_warnings = warnings
            .iter()
            .map(|warning| format!("{}: {}", warning.source.display(), warning.message))
            .collect();
        Ok(Box::new(PreparedMarkdown {
            files,
            warnings,
            preview: ImportPreview {
                native_workspace_id: None,
                workspace_name: "Current workspace".into(),
                document_count: previews.len(),
                node_count,
                image_count,
                documents: previews,
                warnings: display_warnings,
                identity_modes: vec![IdentityMode::Copy],
            },
        }))
    }
}

struct SourceFile {
    path: PathBuf,
    text: String,
}

struct PreparedMarkdown {
    files: Vec<SourceFile>,
    warnings: Vec<ImportWarning>,
    preview: ImportPreview,
}

impl PreparedImport for PreparedMarkdown {
    fn preview(&self) -> &ImportPreview {
        &self.preview
    }

    fn build(
        self: Box<Self>,
        _: ImportOptions,
        context: &ImportContext<'_>,
        progress: &dyn ImportProgress,
    ) -> Result<ImportProduct, ImportError> {
        let target = context
            .target_workspace
            .ok_or_else(|| ImportError::new("Markdown import needs an open workspace"))?;
        let workspace = koru_core::WorkspaceId::from_uuid(target.as_uuid());
        let total = self.files.len();
        let mut documents = Vec::with_capacity(total);
        let mut warnings = self.warnings;
        for (index, file) in self.files.into_iter().enumerate() {
            if progress.cancelled() {
                return Err(ImportError::cancelled());
            }
            progress.update(ImportStage::Converting, index, Some(total));
            let parsed = parse_document(&file.path, &file.text);
            match build_document(workspace, &file.path, parsed, context, progress) {
                Ok((title, snapshot, mut file_warnings)) => {
                    warnings.append(&mut file_warnings);
                    documents.push(ImportedDocument {
                        id: koru_store_core::ids::DocumentId::new(),
                        source: file.path,
                        title,
                        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
                        mime: None,
                        parent: None,
                        trashed: false,
                        trashed_at: None,
                        snapshot,
                    });
                }
                Err(error) => warnings.push(ImportWarning {
                    source: file.path,
                    code: None,
                    arguments: Vec::new(),
                    message: error.to_string(),
                }),
            }
        }
        progress.update(ImportStage::BuildingBundle, total, Some(total));
        if documents.is_empty() {
            return Err(ImportError::new(
                "none of the Markdown files could be converted",
            ));
        }
        Ok(ImportProduct::Documents(ImportedContent {
            documents,
            warnings,
        }))
    }
}

fn is_markdown(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(extension.to_ascii_lowercase().as_str(), "md" | "markdown")
        })
}

fn read_markdown(path: &Path) -> Result<String, ImportError> {
    let bytes = std::fs::read(path)
        .map_err(|error| ImportError::new(format!("could not read the file: {error}")))?;
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(&bytes);
    String::from_utf8(bytes.to_vec())
        .map_err(|_| ImportError::new("the file is not UTF-8 Markdown"))
}

struct ParsedDocument {
    title: String,
    blocks: Vec<Block>,
}

/// Convert authored Markdown directly into one complete Koru document
/// snapshot. This is the content-first adapter used by AI exports; it avoids a
/// temporary file while sharing the exact importer semantics with file import.
pub fn document_snapshot(
    workspace: koru_core::WorkspaceId,
    title: &str,
    markdown: &str,
) -> Result<Vec<u8>, ImportError> {
    if title.trim().is_empty() {
        return Err(ImportError::new("a document title is required"));
    }
    let blobs = koru_import::NoBlobSink;
    let progress = koru_import::NoProgress;
    let target = koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid());
    let context = ImportContext {
        target_profile: None,
        target_workspace: Some(target),
        occupied_titles: &std::collections::BTreeSet::new(),
        blobs: &blobs,
        tags: &koru_import::NoTagSink,
    };
    let (_, snapshot, warnings) = build_document(
        workspace,
        Path::new("koru-ai-export.md"),
        ParsedDocument {
            title: title.to_owned(),
            blocks: parse_body(markdown),
        },
        &context,
        &progress,
    )?;
    if warnings.is_empty() {
        Ok(snapshot)
    } else {
        Err(ImportError::new(warnings[0].message.clone()))
    }
}

/// Parse a body into Koru-shaped blocks, carrying YAML frontmatter over as a
/// code block. Title extraction is deliberately *not* here: a file derives its
/// title from the first `H1` (or its filename), while a service export such as
/// Google Docs already has an authoritative title and must not mistake a real
/// section heading for one.
fn parse_body(text: &str) -> Vec<Block> {
    let (frontmatter, body) = split_frontmatter(text);
    let mut blocks = Vec::new();
    if let Some(yaml) = frontmatter {
        blocks.push(Block::Text(TextBlock {
            text: yaml.to_owned(),
            kind: TextKind::Code {
                language: Some("yaml".into()),
            },
            ..Default::default()
        }));
    }
    blocks.extend(parse(body));
    blocks
}

/// Build a complete document snapshot from Markdown *text*, not a file on disk.
///
/// This is the seam every non-file Markdown source goes through — the Google
/// Docs importer hands it the text returned by `files.export(text/markdown)`
/// and the document's real title, so the two never drift into separate
/// dialects. The title is authoritative here, unlike [`parse_document`], which
/// derives one because a `.md` file carries no other name.
pub fn build_document_from_text(
    workspace: koru_core::WorkspaceId,
    title: &str,
    source_name: &str,
    text: &str,
    context: &ImportContext<'_>,
    progress: &dyn ImportProgress,
) -> Result<(String, Vec<u8>, Vec<ImportWarning>), ImportError> {
    let parsed = ParsedDocument {
        title: title.to_owned(),
        blocks: parse_body(text),
    };
    build_document(workspace, Path::new(source_name), parsed, context, progress)
}

/// Markdown as a document **with no title of its own** — every block of the
/// text, in the order it was written, and nothing added.
///
/// The seam for a surface that is *showing* a file rather than importing it:
/// a delegated run's result read out of the artifact store, the
/// delegated-agent preamble read from beside the executables. Those go back
/// where they came from when a person saves them, so this path adds no title
/// line and moves no heading. [`parse_document`] does both — a `.md` file
/// arriving in the library needs a name, and takes its first `H1` for one —
/// and either would be a visible change to a file nobody asked to have
/// changed.
///
/// Warnings are dropped rather than returned: a reader shows what it could
/// read, and an image whose bytes no blob sink accepted has already become a
/// link in the document.
///
/// Touches no disk: images resolve through `context.blobs`, and the context
/// this builds has [`koru_import::NoBlobSink`].
pub fn document_from_text(
    workspace: koru_core::WorkspaceId,
    text: &str,
) -> Result<Vec<u8>, ImportError> {
    let blobs = koru_import::NoBlobSink;
    let progress = koru_import::NoProgress;
    let target = koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid());
    let context = ImportContext {
        target_profile: None,
        target_workspace: Some(target),
        occupied_titles: &std::collections::BTreeSet::new(),
        blobs: &blobs,
        tags: &koru_import::NoTagSink,
    };
    let parsed = ParsedDocument {
        title: String::new(),
        blocks: parse_body(text),
    };
    let (_, snapshot, _) = build_document(
        workspace,
        Path::new("rendered.md"),
        parsed,
        &context,
        &progress,
    )?;
    Ok(snapshot)
}

fn parse_document(path: &Path, text: &str) -> ParsedDocument {
    let mut blocks = parse_body(text);
    let title_index = blocks
        .iter()
        .position(|block| matches!(block, Block::Text(text) if text.heading == Some(Heading::H1)));
    let title = title_index
        .and_then(|index| match &blocks[index] {
            Block::Text(text) => Some(text.text.trim().to_owned()),
            _ => None,
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .filter(|stem| !stem.is_empty())
                .unwrap_or("Untitled")
                .to_owned()
        });
    if let Some(index) = title_index {
        blocks.remove(index);
    }
    ParsedDocument { title, blocks }
}

fn split_frontmatter(text: &str) -> (Option<&str>, &str) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return (None, text);
    };
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed == "---" {
            return (
                Some(rest[..offset].trim_end_matches(['\r', '\n'])),
                &rest[offset + line.len()..],
            );
        }
        offset += line.len();
    }
    (None, text)
}

/// One block of the stream. A callout box is flat here: it opens with a
/// [`Block::CalloutStart`] carrying its look, every block up to the matching
/// [`Block::CalloutEnd`] is inside it, and the builder nests those under one
/// box node ([`koru_core::callout`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Text(TextBlock),
    Table(String),
    Image(ImageBlock),
    Rule,
    /// A callout box opens. Markdown carries a box two ways: the `<aside>` an
    /// export writes, and the blockquote this crate's exporter writes, whose
    /// first line leads with the icon. `icon` is an emoji, `tint` an
    /// `AccentColor` token; `None` is the default for either.
    CalloutStart {
        icon: Option<String>,
        tint: Option<String>,
    },
    /// The box closes: the blocks since its start are its children.
    CalloutEnd,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TextBlock {
    pub text: String,
    pub list: Option<ListSpec>,
    pub heading: Option<Heading>,
    pub kind: TextKind,
    pub runs: Vec<TextRun>,
    pub links: Vec<TextLink>,
    pub task: Option<bool>,
    /// Where the line sits in a callout box — between its
    /// [`Block::CalloutStart`] and [`Block::CalloutEnd`] — or `None` outside
    /// one. The markers say the same, and the builder reads those; this is
    /// for a renderer that sees one block at a time and still has to draw
    /// the line as inside the box, with the icon on the first.
    pub callout: Option<CalloutLine>,
}

/// A text line's place inside a callout box.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CalloutLine {
    /// The first line inside the box, which leads with the box's icon
    /// (`None` is the default, [`koru_core::callout::DEFAULT_ICON`]).
    First { icon: Option<String> },
    /// Any later line.
    Inside,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum TextKind {
    #[default]
    Paragraph,
    Quote,
    Code {
        language: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListSpec {
    pub style: ListStyle,
    pub depth: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextRun {
    pub start: usize,
    pub end: usize,
    pub marks: MarkSet,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextLink {
    pub start: usize,
    pub end: usize,
    pub url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBlock {
    pub alt: String,
    pub destination: String,
}

#[derive(Default)]
struct MarkDepth {
    bold: u16,
    italic: u16,
    strike: u16,
}

impl MarkDepth {
    fn marks(&self) -> MarkSet {
        MarkSet {
            bold: self.bold > 0,
            italic: self.italic > 0,
            strike: self.strike > 0,
            ..Default::default()
        }
    }
    fn change(&mut self, mark: Mark, opening: bool) {
        let depth = match mark {
            Mark::Bold => &mut self.bold,
            Mark::Italic => &mut self.italic,
            Mark::Strike => &mut self.strike,
            _ => return,
        };
        *depth = if opening {
            depth.saturating_add(1)
        } else {
            depth.saturating_sub(1)
        };
    }
}

/// A callout box that is open: an `<aside>`, or a blockquote whose first
/// line led with an emoji. The first line inside settles the look; the
/// box's start marker goes in the stream before its first block, and every
/// block after it, until the box closes, is inside ([`Adapter::push`]).
#[derive(Default)]
struct OpenCallout {
    icon: Option<String>,
    tint: Option<String>,
    /// Whether the icon has been settled — from a leading `<img>`, or from the
    /// first line's leading emoji. Only the first line can carry it.
    settled: bool,
    /// Whether the start marker is in the stream; what follows is inside.
    started: bool,
    /// The quote depth the box opened at. A block quoted deeper is a quote
    /// inside the box; one at this depth is a line of the box.
    quote_depth: u16,
    /// Whether a blockquote opened the box, so that blockquote's end closes
    /// it; an `<aside>` closes at its `</aside>`.
    opened_by_quote: bool,
    /// Markdown an export left inside the raw HTML block that opened the
    /// aside, parsed as Markdown once that block ends ([`Adapter::flush_raw`]).
    raw: String,
}

struct Adapter {
    blocks: Vec<Block>,
    current: Option<TextBlock>,
    lists: Vec<ListStyle>,
    item_depth: u16,
    quote_depth: u16,
    callout: Option<OpenCallout>,
    /// A blockquote has just opened and its first block has not been seen:
    /// if that block is a line led by an emoji, the quote is a callout box.
    quote_head_pending: bool,
    marks: MarkDepth,
    open_links: Vec<(usize, String)>,
    image: Option<ImageBlock>,
}

impl Adapter {
    fn new() -> Self {
        Self {
            blocks: Vec::new(),
            current: None,
            lists: Vec::new(),
            item_depth: 0,
            quote_depth: 0,
            callout: None,
            quote_head_pending: false,
            marks: MarkDepth::default(),
            open_links: Vec::new(),
            image: None,
        }
    }
    fn begin(&mut self, heading: Option<Heading>, kind: TextKind) {
        self.flush();
        self.current = Some(TextBlock {
            list: (self.item_depth > 0 && !matches!(kind, TextKind::Code { .. })).then(|| {
                ListSpec {
                    style: self.lists.last().copied().unwrap_or(ListStyle::Bullet),
                    depth: self.lists.len().saturating_sub(1) as u16,
                }
            }),
            heading,
            kind,
            ..Default::default()
        });
    }
    fn ensure(&mut self) {
        if self.current.is_none() {
            // Inside a box, the box's own lines are paragraphs; only a quote
            // nested deeper than the box is a quote.
            let box_depth = self.callout.as_ref().map_or(0, |c| c.quote_depth);
            let kind = if self.quote_depth > box_depth {
                TextKind::Quote
            } else {
                TextKind::Paragraph
            };
            self.begin(None, kind);
        }
    }
    fn append(&mut self, value: &str, extra: Option<Mark>) {
        if let Some(image) = self.image.as_mut() {
            image.alt.push_str(value);
            return;
        }
        if value.is_empty() {
            return;
        }
        self.ensure();
        let block = self.current.as_mut().expect("ensured");
        let start = block.text.len();
        block.text.push_str(value);
        let end = block.text.len();
        let mut marks = self.marks.marks();
        if let Some(mark) = extra {
            marks.set(mark, true);
        }
        if marks.is_empty() {
            return;
        }
        if let Some(last) = block.runs.last_mut()
            && last.end == start
            && last.marks == marks
        {
            last.end = end;
        } else {
            block.runs.push(TextRun { start, end, marks });
        }
    }
    fn flush(&mut self) {
        self.open_links.clear();
        let Some(mut block) = self.current.take() else {
            return;
        };
        if matches!(block.kind, TextKind::Code { .. }) {
            if block.text.ends_with('\n') {
                block.text.pop();
            }
        } else {
            block.text = block.text.trim_matches(['\r', '\n']).to_owned();
        }
        // The first block of a blockquote decides whether the quote is a
        // box: a line led by an emoji opens one, the emoji its icon, and the
        // quote's blocks are the lines inside. Anything else leaves it a quote.
        if std::mem::take(&mut self.quote_head_pending)
            && block.kind == TextKind::Quote
            && block.list.is_none()
            && block.heading.is_none()
            && let Some((icon, cut)) = leading_icon(&block.text)
        {
            strip_front(&mut block, cut);
            block.kind = TextKind::Paragraph;
            self.callout = Some(OpenCallout {
                icon: Some(icon),
                settled: true,
                quote_depth: self.quote_depth,
                opened_by_quote: true,
                ..Default::default()
            });
        }
        if !block.text.trim().is_empty() || block.task.is_some() {
            self.push(Block::Text(block));
        }
    }

    /// Put a finished block in the stream. Inside an open box the first
    /// block settles the look — a plain paragraph's leading emoji is the
    /// icon, and is not prose — then the box's start marker goes in, and
    /// every block from the first on is inside the box.
    fn push(&mut self, mut block: Block) {
        self.quote_head_pending = false;
        let Some(callout) = self.callout.as_mut() else {
            self.blocks.push(block);
            return;
        };
        if !callout.started {
            if let Block::Text(text) = &mut block
                && !callout.settled
            {
                callout.settled = true;
                if text.kind == TextKind::Paragraph
                    && text.list.is_none()
                    && text.heading.is_none()
                    && text.task.is_none()
                    && let Some((icon, cut)) = leading_icon(&text.text)
                {
                    callout.icon = Some(icon);
                    strip_front(text, cut);
                    if text.text.trim().is_empty() {
                        // The icon alone on its line: the box's first line is
                        // the next one.
                        return;
                    }
                }
            }
            callout.started = true;
            self.blocks.push(Block::CalloutStart {
                icon: callout.icon.clone(),
                tint: callout.tint.clone(),
            });
            if let Block::Text(text) = &mut block {
                text.callout = Some(CalloutLine::First {
                    icon: callout.icon.clone(),
                });
            }
            self.blocks.push(block);
            return;
        }
        if let Block::Text(text) = &mut block {
            text.callout = Some(CalloutLine::Inside);
        }
        self.blocks.push(block);
    }

    /// Close the open box, if there is one: its end goes in the stream after
    /// what was inside. A box that never started — nothing was inside it —
    /// leaves nothing behind: an empty box would be a node no row shows.
    fn close_callout(&mut self) {
        self.flush();
        if let Some(callout) = self.callout.take()
            && callout.started
        {
            self.blocks.push(Block::CalloutEnd);
        }
    }

    /// Raw HTML, which is where an exported callout lives: `<aside>` opens a
    /// box, `</aside>` closes it, and the text between is inside it. Any
    /// other HTML lands as text, as it always has.
    fn html(&mut self, value: &str) {
        let mut rest = value;
        loop {
            let lower = rest.to_ascii_lowercase();
            let open = lower.find("<aside");
            let close = lower.find("</aside>");
            match (open, close) {
                (Some(o), c) if c.is_none_or(|c| o < c) => {
                    self.callout_html(&rest[..o]);
                    self.flush_raw();
                    // A box does not nest in a box: an aside inside one
                    // closes it.
                    self.close_callout();
                    self.callout = Some(OpenCallout {
                        quote_depth: self.quote_depth,
                        ..Default::default()
                    });
                    let end = rest[o..].find('>').map_or(rest.len(), |e| o + e + 1);
                    rest = &rest[end..];
                }
                (_, Some(c)) => {
                    self.callout_html(&rest[..c]);
                    self.flush_raw();
                    self.close_callout();
                    rest = &rest[c + "</aside>".len()..];
                }
                _ => return self.callout_html(rest),
            }
        }
    }

    /// HTML text that is not an `<aside>` tag. Inside a box the raw lines
    /// are its prose — and a leading `<img>` of the icon-set shape
    /// [`builtin_icon`] knows is its icon.
    fn callout_html(&mut self, raw: &str) {
        let Some(callout) = self.callout.as_mut() else {
            return self.append(raw, None);
        };
        let mut raw = raw.trim();
        if !callout.settled && raw.to_ascii_lowercase().starts_with("<img") {
            let end = raw.find('>').map_or(raw.len(), |e| e + 1);
            if let Some((icon, tint)) = html_attribute(&raw[..end], "src").and_then(builtin_icon) {
                callout.icon = icon.map(str::to_owned);
                callout.tint = tint.map(|tint| tint.as_str().to_owned());
            }
            callout.settled = true;
            raw = raw[end..].trim();
        }
        callout.raw.push_str(raw);
        callout.raw.push('\n');
    }

    /// The Markdown gathered from a raw HTML block inside a box, as the box's
    /// lines. An export writes the box's first line there, so `**bold**` in
    /// it is still bold.
    fn flush_raw(&mut self) {
        let Some(raw) = self
            .callout
            .as_mut()
            .map(|callout| std::mem::take(&mut callout.raw))
        else {
            return;
        };
        if raw.trim().is_empty() {
            return;
        }
        self.flush();
        for block in parse(&raw) {
            self.push(block);
        }
    }

    fn start_link(&mut self, url: String) {
        self.ensure();
        self.open_links
            .push((self.current.as_ref().map_or(0, |b| b.text.len()), url));
    }
    fn end_link(&mut self) {
        let Some((start, url)) = self.open_links.pop() else {
            return;
        };
        let Some(block) = self.current.as_mut() else {
            return;
        };
        let end = block.text.len();
        if start < end && !url.is_empty() {
            block.links.push(TextLink { start, end, url });
        }
    }
}

/// Drop the first `cut` bytes of `block`'s text, moving its runs and links
/// with it — the callout icon the text led with is not prose.
fn strip_front(block: &mut TextBlock, cut: usize) {
    block.text.drain(..cut);
    block
        .runs
        .retain_mut(|run| shift_span(&mut run.start, &mut run.end, cut));
    block
        .links
        .retain_mut(|link| shift_span(&mut link.start, &mut link.end, cut));
}

/// Move a `start..end` byte span left by `cut`, the first `cut` bytes of its
/// text having been dropped; `false` when nothing of the span is left. The
/// one rule every reader of an exported callout line needs, since the icon
/// the line leads with is not prose. Public for the clipboard's HTML reader.
pub fn shift_span(start: &mut usize, end: &mut usize, cut: usize) -> bool {
    *start = start.saturating_sub(cut);
    *end = end.saturating_sub(cut);
    start < end
}

/// The emoji a callout's exported text leads with — `💡 First line` — and
/// how many bytes of `text` it and the space after it take, so the caller can
/// strip them. `None` when the first word is not a single emoji. Public for
/// the clipboard reader, which meets the same shape in pasted HTML.
pub fn leading_icon(text: &str) -> Option<(String, usize)> {
    let start = text.len() - text.trim_start().len();
    let rest = &text[start..];
    let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let word = &rest[..word_end];
    emojis::get(word)?;
    let after = &rest[word_end..];
    let space = after.len() - after.trim_start().len();
    Some((word.to_owned(), start + word_end + space))
}

/// A color name as an export writes it — a Koru token, or a `<hue>`,
/// `<hue>_background` or `<hue>_bg` block color — as the tint Koru stores.
/// `Some(None)` is an explicit *default*; `None` means the name is not a
/// color at all. `brown` has no Koru tone and lands on orange, the nearest.
fn color_name(name: &str) -> Option<Option<koru_core::AccentColor>> {
    use koru_core::AccentColor;
    let name = name.trim().to_ascii_lowercase();
    let hue = name
        .strip_suffix("_background")
        .or_else(|| name.strip_suffix("_bg"))
        .unwrap_or(&name);
    match hue {
        "default" | "none" | "clear" => Some(None),
        "brown" => Some(Some(AccentColor::Orange)),
        "grey" | "lightgray" | "lightgrey" => Some(Some(AccentColor::Gray)),
        other => AccentColor::parse(other).map(Some),
    }
}

/// The icon and tint behind a built-in icon address of the shape
/// `…/icons/<name>_<color>.svg`, which is what an export writes for a
/// callout whose icon is one of the app's own rather than an emoji. Koru
/// stores an emoji, so the common shapes map onto the nearest one; a shape
/// with no near emoji keeps the default. The color suffix is the tint.
fn builtin_icon(url: &str) -> Option<(Option<&'static str>, Option<koru_core::AccentColor>)> {
    let (_, file) = url.rsplit_once("/icons/")?;
    let stem = file.strip_suffix(".svg").unwrap_or(file);
    let (name, color) = match stem.rsplit_once('_') {
        Some((name, color)) => (name, color_name(color).flatten()),
        None => (stem, None),
    };
    let base = name.split('-').next().unwrap_or(name);
    let emoji = match base {
        "info" => Some("ℹ️"),
        "warning" | "alert" | "exclamation" => Some("⚠️"),
        "lightbulb" | "idea" => Some("💡"),
        "checkmark" | "check" => Some("✅"),
        "close" | "cross" | "error" | "stop" | "forbidden" => Some("⛔"),
        "question" | "help" => Some("❓"),
        "star" => Some("⭐"),
        "fire" => Some("🔥"),
        "pin" | "pushpin" => Some("📌"),
        "bookmark" => Some("🔖"),
        "book" | "library" => Some("📚"),
        "clock" | "timer" | "hourglass" => Some("⏰"),
        "calendar" => Some("📅"),
        "code" | "terminal" => Some("💻"),
        "document" | "page" | "notes" | "note" => Some("📝"),
        "link" => Some("🔗"),
        "flag" => Some("🚩"),
        "heart" => Some("❤️"),
        "rocket" => Some("🚀"),
        "target" => Some("🎯"),
        "token" | "key" | "lock" => Some("🔑"),
        "megaphone" | "announcement" => Some("📣"),
        "cursor" => Some("👉"),
        _ => None,
    };
    Some((emoji, color))
}

/// The value of `name="…"` in one HTML tag.
fn html_attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let at = tag.to_ascii_lowercase().find(&format!("{name}="))? + name.len() + 1;
    let rest = &tag[at..];
    let quote = rest.chars().next().filter(|c| matches!(c, '"' | '\''))?;
    let rest = &rest[1..];
    Some(&rest[..rest.find(quote)?])
}

/// Parse Markdown into semantic Koru-shaped blocks. This is deliberately pure:
/// file paths and blob storage are importer concerns, clipboard conversion is a
/// shell concern.
pub fn parse(text: &str) -> Vec<Block> {
    let references: Vec<_> = text.lines().filter_map(reference_definition).collect();
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_TABLES);
    let mut out = Adapter::new();
    let mut in_table = false;
    let mut table_references = std::collections::HashSet::new();
    let candidates = table_candidate_ranges(text);
    let mut candidate_index = 0usize;
    let mut skip_candidate_until = None;
    for (event, range) in Parser::new_ext(text, options).into_offset_iter() {
        if let Some(end) = skip_candidate_until {
            if range.start < end {
                continue;
            }
            skip_candidate_until = None;
        }
        while candidates
            .get(candidate_index)
            .is_some_and(|candidate| candidate.end <= range.start)
        {
            candidate_index += 1;
        }
        if let Some(candidate) = candidates.get(candidate_index)
            && range.start <= candidate.start
            && range.end > candidate.start
        {
            out.flush();
            let mut source = text
                .get(candidate.clone())
                .unwrap_or_default()
                .trim_end()
                .to_owned();
            append_table_references(&mut source, &references, &mut table_references);
            out.push(Block::Table(source));
            skip_candidate_until = Some(candidate.end);
            candidate_index += 1;
            continue;
        }
        if in_table {
            if matches!(event, Event::End(TagEnd::Table)) {
                in_table = false;
            }
            continue;
        }
        if matches!(event, Event::Start(Tag::Table(_))) {
            out.flush();
            let mut source = text.get(range).unwrap_or_default().trim_end().to_owned();
            append_table_references(&mut source, &references, &mut table_references);
            out.push(Block::Table(source));
            in_table = true;
            continue;
        }
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => out.ensure(),
                Tag::HtmlBlock if out.callout.is_some() => out.flush(),
                Tag::Heading { level, .. } => out.begin(Some(heading(level)), TextKind::Paragraph),
                Tag::BlockQuote(_) => {
                    out.flush();
                    out.quote_depth = out.quote_depth.saturating_add(1);
                    // A box does not nest in a box: inside one, a quote is a
                    // quote.
                    out.quote_head_pending = out.callout.is_none();
                }
                Tag::CodeBlock(kind) => {
                    let language = match kind {
                        CodeBlockKind::Indented => None,
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned),
                    };
                    out.begin(None, TextKind::Code { language });
                }
                Tag::List(start) => {
                    out.flush();
                    out.lists.push(if start.is_some() {
                        ListStyle::Numbered
                    } else {
                        ListStyle::Bullet
                    });
                }
                Tag::Item => {
                    out.flush();
                    out.item_depth = out.item_depth.saturating_add(1);
                    out.ensure();
                }
                Tag::Emphasis => out.marks.change(Mark::Italic, true),
                Tag::Strong => out.marks.change(Mark::Bold, true),
                Tag::Strikethrough => out.marks.change(Mark::Strike, true),
                Tag::Link { dest_url, .. } => out.start_link(dest_url.into_string()),
                Tag::Image { dest_url, .. } => {
                    out.flush();
                    out.image = Some(ImageBlock {
                        alt: String::new(),
                        destination: dest_url.into_string(),
                    });
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock => out.flush(),
                TagEnd::HtmlBlock => out.flush_raw(),
                TagEnd::BlockQuote(_) => {
                    if out
                        .callout
                        .as_ref()
                        .is_some_and(|c| c.opened_by_quote && c.quote_depth == out.quote_depth)
                    {
                        out.close_callout();
                    } else {
                        out.flush();
                    }
                    out.quote_head_pending = false;
                    out.quote_depth = out.quote_depth.saturating_sub(1);
                }
                TagEnd::List(_) => {
                    out.flush();
                    out.lists.pop();
                }
                TagEnd::Item => {
                    out.flush();
                    out.item_depth = out.item_depth.saturating_sub(1);
                }
                TagEnd::Emphasis => out.marks.change(Mark::Italic, false),
                TagEnd::Strong => out.marks.change(Mark::Bold, false),
                TagEnd::Strikethrough => out.marks.change(Mark::Strike, false),
                TagEnd::Link => out.end_link(),
                TagEnd::Image => {
                    if let Some(image) = out.image.take() {
                        out.push(Block::Image(image));
                    }
                }
                _ => {}
            },
            Event::Text(value) => out.append(&value, None),
            Event::Code(value) | Event::InlineMath(value) => out.append(&value, Some(Mark::Code)),
            Event::DisplayMath(value) => {
                out.flush();
                out.begin(None, TextKind::Code { language: None });
                out.append(&value, None);
                out.flush();
            }
            Event::Html(value) | Event::InlineHtml(value) => out.html(&value),
            Event::FootnoteReference(label) => out.append(&format!("[{label}]"), None),
            Event::SoftBreak | Event::HardBreak => {
                let separator = if out
                    .current
                    .as_ref()
                    .is_some_and(|b| matches!(b.kind, TextKind::Code { .. }))
                {
                    "\n"
                } else {
                    " "
                };
                out.append(separator, None);
            }
            Event::Rule => {
                out.flush();
                out.push(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                out.ensure();
                if let Some(block) = out.current.as_mut() {
                    block.task = Some(done);
                    if let Some(list) = block.list.as_mut() {
                        list.style = ListStyle::Checkbox;
                    }
                }
            }
        }
    }
    // An `<aside>` nobody closed still ends with the text.
    out.close_callout();
    for reference in references {
        if table_references.contains(&reference.label) {
            continue;
        }
        // A data-URI reference definition is an embedded image whose definition
        // Google trails the export. The image itself was already decoded into an
        // image node from the resolved `Tag::Image`, so re-emitting the
        // definition here would add a "source link" pill whose hover text is the
        // whole base64 blob.
        if reference.url.starts_with("data:") {
            continue;
        }
        let label = reference
            .title
            .as_deref()
            .map(|title| format!("[{}] {title}", reference.label))
            .unwrap_or_else(|| format!("[{}] {}", reference.label, reference.url));
        let end = label.len();
        out.blocks.push(Block::Text(TextBlock {
            text: label,
            links: vec![TextLink {
                start: 0,
                end,
                url: reference.url,
            }],
            ..Default::default()
        }));
    }
    out.blocks
}

#[derive(Debug)]
struct ReferenceDefinition {
    label: String,
    url: String,
    title: Option<String>,
}

fn reference_definition(line: &str) -> Option<ReferenceDefinition> {
    let line = line.trim_start();
    let close = line.find("]: ").or_else(|| line.find("]:"))?;
    if !line.starts_with('[') || close <= 1 {
        return None;
    }
    let label = line[1..close].trim();
    if label.is_empty() || label.starts_with('^') {
        return None;
    }
    let rest = line[close + 2..].trim_start();
    let (url, rest) = if let Some(rest) = rest.strip_prefix('<') {
        let end = rest.find('>')?;
        (&rest[..end], rest[end + 1..].trim())
    } else {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        (&rest[..end], rest[end..].trim())
    };
    if url.is_empty() {
        return None;
    }
    let title = rest
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .or_else(|| rest.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    Some(ReferenceDefinition {
        label: label.to_owned(),
        url: url.to_owned(),
        title,
    })
}

fn table_candidate_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut lines = Vec::new();
    let mut start = 0usize;
    for part in text.split_inclusive('\n') {
        let end = start + part.len();
        lines.push((start, end, part.trim_end_matches(['\r', '\n'])));
        start = end;
    }
    if start < text.len() || text.is_empty() {
        lines.push((start, text.len(), &text[start..]));
    }
    // A line inside a fenced code block is code, whatever it holds. This
    // scan runs on the raw text, ahead of the Markdown parser that knows
    // about fences, and it took two consecutive lines with two pipes each
    // for a table: a Mermaid flowchart's `A -->|yes| B` under `A -->|no| C`
    // qualified, and the fence was cut into paragraphs and a broken table
    // around them.
    let fenced = fenced_lines(lines.iter().map(|(_, _, line)| *line));
    let mut ranges = Vec::new();
    let mut index = 0usize;
    while index + 1 < lines.len() {
        if fenced[index] || fenced[index + 1] {
            index += 1;
            continue;
        }
        let header = lines[index].2.trim();
        let delimiter = lines[index + 1].2.trim();
        let header_pipes = header.bytes().filter(|byte| *byte == b'|').count();
        let delimiter_pipes = delimiter.bytes().filter(|byte| *byte == b'|').count();
        if header_pipes < 2 || !delimiter.contains('-') || delimiter_pipes < 2 {
            index += 1;
            continue;
        }
        let begin = lines[index].0;
        let mut end_index = index + 2;
        while end_index < lines.len() {
            let line = lines[end_index].2.trim();
            if fenced[end_index]
                || line.is_empty()
                || line.bytes().filter(|byte| *byte == b'|').count() < 2
            {
                break;
            }
            end_index += 1;
        }
        ranges.push(begin..lines[end_index - 1].1.min(text.len()));
        index = end_index;
    }
    ranges
}

/// For each line, whether it lies in a fenced code block — the fence lines
/// themselves included. A fence opens with three or more backticks or
/// tildes after at most three spaces, and closes with at least as many of
/// the same character and nothing else on the line, as CommonMark has it;
/// a fence left open runs to the end.
fn fenced_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Vec<bool> {
    let mut open: Option<(u8, usize)> = None;
    lines
        .map(|line| {
            let body = line.trim_start_matches(' ');
            let indent = line.len() - body.len();
            let fence = |byte: u8| {
                let run = body.bytes().take_while(|b| *b == byte).count();
                (indent <= 3 && run >= 3).then_some(run)
            };
            match open {
                Some((byte, len)) => {
                    let closes = fence(byte).is_some_and(|run| run >= len)
                        && body.trim_start_matches(byte as char).trim().is_empty();
                    if closes {
                        open = None;
                    }
                    true
                }
                None => match fence(b'`')
                    .map(|run| (b'`', run))
                    .or_else(|| fence(b'~').map(|run| (b'~', run)))
                {
                    // A backtick fence's info string may not hold a backtick.
                    Some((b'`', run)) if body[run..].contains('`') => false,
                    Some(opened) => {
                        open = Some(opened);
                        true
                    }
                    None => false,
                },
            }
        })
        .collect()
}

fn append_table_references(
    source: &mut String,
    references: &[ReferenceDefinition],
    used: &mut std::collections::HashSet<String>,
) {
    for reference in references {
        if source.contains(&format!("][{}]", reference.label))
            || source.contains(&format!("[{}]", reference.label))
        {
            used.insert(reference.label.clone());
            source.push_str("\n\n[");
            source.push_str(&reference.label);
            source.push_str("]: ");
            source.push_str(&reference.url);
            if let Some(title) = &reference.title {
                source.push_str(" \"");
                source.push_str(title);
                source.push('"');
            }
        }
    }
}

fn heading(level: HeadingLevel) -> Heading {
    match level {
        HeadingLevel::H1 => Heading::H1,
        HeadingLevel::H2 => Heading::H2,
        HeadingLevel::H3 => Heading::H3,
        HeadingLevel::H4 => Heading::H4,
        HeadingLevel::H5 => Heading::H5,
        HeadingLevel::H6 => Heading::H6,
    }
}

struct ListLevel {
    container: NodeId,
    style: ListStyle,
    parent_item: Option<NodeId>,
    last_item: Option<NodeId>,
}

fn build_document(
    workspace: koru_core::WorkspaceId,
    source: &Path,
    parsed: ParsedDocument,
    context: &ImportContext<'_>,
    progress: &dyn ImportProgress,
) -> Result<(String, Vec<u8>, Vec<ImportWarning>), ImportError> {
    let mut doc = WorkspaceDoc::create(workspace, &parsed.title).map_err(import_failed)?;
    let root = doc
        .insert(
            Parent::Root,
            Node::document(&parsed.title).map_err(import_failed)?,
        )
        .map_err(import_failed)?;
    let mut warnings = Vec::new();
    let mut lists: Vec<ListLevel> = Vec::new();
    let mut last_root = None;
    // The open callout boxes, innermost last: a block lands under the
    // innermost rather than under the document.
    let mut boxes: Vec<NodeId> = Vec::new();
    let block_total = parsed.blocks.len();
    for (index, block) in parsed.blocks.into_iter().enumerate() {
        progress.update(ImportStage::ReadingMedia, index, Some(block_total));
        if progress.cancelled() {
            return Err(ImportError::cancelled());
        }
        let parent = boxes.last().copied().unwrap_or(root);
        if let Block::Text(text) = &block
            && text.list.is_some()
        {
            insert_list_text(&mut doc, parent, &mut lists, &mut last_root, text)?;
            continue;
        }
        lists.clear();
        let node = match block {
            Block::CalloutStart { icon, tint } => {
                let id = doc
                    .insert(
                        Parent::Node(parent),
                        Node::callout(icon.as_deref(), tint.as_deref()).map_err(import_failed)?,
                    )
                    .map_err(import_failed)?;
                boxes.push(id);
                last_root = Some(id);
                continue;
            }
            Block::CalloutEnd => {
                boxes.pop();
                continue;
            }
            Block::Text(text) => {
                let node = match &text.kind {
                    TextKind::Paragraph if text.task.is_some() => {
                        Node::task(text.text.replace(['\n', '\r'], ""))
                    }
                    TextKind::Paragraph => Node::paragraph(text.text.replace(['\n', '\r'], "")),
                    TextKind::Quote => Node::quote(text.text.replace(['\n', '\r'], "")),
                    TextKind::Code { .. } => Node::code(text.text.clone()),
                }
                .map_err(import_failed)?;
                let id = doc
                    .insert(Parent::Node(parent), node)
                    .map_err(import_failed)?;
                decorate_text(&mut doc, id, &text)?;
                last_root = Some(id);
                continue;
            }
            Block::Table(source) => Node::table(source),
            Block::Rule => Node::divider(),
            Block::Image(image) => {
                // Embedded (data-URI) and local images share one path: store the
                // bytes, then build an image node carrying the intrinsic size.
                match data_uri(&image.destination) {
                    Some((bytes, mime)) => {
                        let id = insert_image(
                            &mut doc,
                            parent,
                            source,
                            &image,
                            bytes,
                            &mime,
                            None,
                            None,
                            context,
                            &mut warnings,
                        )?;
                        last_root = Some(id);
                        continue;
                    }
                    None => match local_image_path(source, &image.destination) {
                        Some(path) => match std::fs::read(&path) {
                            Ok(bytes) => {
                                let mime = image_mime(&path);
                                let id = insert_image(
                                    &mut doc,
                                    parent,
                                    source,
                                    &image,
                                    bytes,
                                    mime,
                                    path.file_name().and_then(|n| n.to_str()),
                                    path.file_stem().and_then(|s| s.to_str()),
                                    context,
                                    &mut warnings,
                                )?;
                                last_root = Some(id);
                                continue;
                            }
                            Err(error) => {
                                warnings.push(ImportWarning {
                                    source: source.to_path_buf(),
                                    code: None,
                                    arguments: Vec::new(),
                                    message: format!(
                                        "could not read image {}: {error}",
                                        path.display()
                                    ),
                                });
                                fallback_link(&mut doc, parent, &image)?;
                                continue;
                            }
                        },
                        None => {
                            fallback_link(&mut doc, parent, &image)?;
                            continue;
                        }
                    },
                }
            }
        }
        .map_err(import_failed)?;
        let id = doc
            .insert(Parent::Node(parent), node)
            .map_err(import_failed)?;
        last_root = Some(id);
    }
    let snapshot = doc.export_snapshot().map_err(import_failed)?;
    Ok((parsed.title, snapshot, warnings))
}

/// One list line under `parent` — the document, or the callout box it is
/// inside.
fn insert_list_text(
    doc: &mut WorkspaceDoc,
    parent: NodeId,
    levels: &mut Vec<ListLevel>,
    last_root: &mut Option<NodeId>,
    text: &TextBlock,
) -> Result<(), ImportError> {
    let spec = text.list.expect("called for list text");
    let mut depth = (spec.depth as usize).min(levels.len());
    let mut parent_item = if depth == 0 {
        None
    } else {
        levels.get(depth - 1).and_then(|level| level.last_item)
    };
    if depth > 0 && parent_item.is_none() {
        depth = 0;
        parent_item = None;
    }
    levels.truncate((depth + 1).min(levels.len()));
    let reusable = levels
        .get(depth)
        .is_some_and(|level| level.style == spec.style && level.parent_item == parent_item);
    if !reusable {
        levels.truncate(depth);
        let list = Node::list_block().map_err(import_failed)?;
        let list_id = doc
            .insert(Parent::Node(parent_item.unwrap_or(parent)), list)
            .map_err(import_failed)?;
        doc.set_list_style(list_id, Some(spec.style))
            .map_err(import_failed)?;
        *last_root = Some(list_id);
        levels.push(ListLevel {
            container: list_id,
            style: spec.style,
            parent_item,
            last_item: None,
        });
    }
    let node = if text.task.is_some() {
        Node::task(text.text.replace(['\n', '\r'], ""))
    } else {
        Node::paragraph(text.text.replace(['\n', '\r'], ""))
    }
    .map_err(import_failed)?;
    let id = doc
        .insert(Parent::Node(levels[depth].container), node)
        .map_err(import_failed)?;
    levels[depth].last_item = Some(id);
    levels.truncate(depth + 1);
    decorate_text(doc, id, text)
}

fn decorate_text(doc: &mut WorkspaceDoc, id: NodeId, text: &TextBlock) -> Result<(), ImportError> {
    if let Some(heading) = text.heading {
        doc.set_heading(id, Some(heading)).map_err(import_failed)?;
    }
    if let TextKind::Code {
        language: Some(language),
    } = &text.kind
    {
        doc.set_language(id, Some(language))
            .map_err(import_failed)?;
    }
    if let Some(done) = text.task {
        // The named trusted door, not the raw setter: this crate's own header
        // says task state here goes through `faithful_import` and nowhere
        // else, and a `- [x]` is the one piece of task-adjacent state it
        // writes. The call is the same write — `restore_done` is `set_done`
        // with a name that says why it is allowed — which is what makes the
        // X-1 sweep able to read the difference.
        koru_core::tasks::faithful_import::restore_done(doc, id, done).map_err(import_failed)?;
    }
    for run in &text.runs {
        for mark in Mark::ALL {
            if run.marks.has(mark) {
                doc.set_mark(id, run.start..run.end, mark, true)
                    .map_err(import_failed)?;
            }
        }
    }
    // Ordinary Markdown links and Koru's own `[[koru://...]]` tokens are
    // applied together, in one pass sorted backwards. Two passes would work on
    // offsets the first had already shifted.
    let mut spans: Vec<(usize, usize, Attr)> = Vec::new();
    for link in &text.links {
        if !span_is_sound(&text.text, link.start, link.end) {
            continue;
        }
        let label = &text.text[link.start..link.end];
        spans.push((
            link.start,
            link.end,
            Attr::labelled("link", AttrValue::Url(link.url.clone()), label),
        ));
    }
    spans.extend(koru_link_spans(&text.text));
    spans.sort_by_key(|(start, _, _)| *start);
    for (start, end, attr) in spans.into_iter().rev() {
        doc.delete_text(id, start..end).map_err(import_failed)?;
        doc.insert_pill(id, start, &attr).map_err(import_failed)?;
    }
    Ok(())
}

fn span_is_sound(text: &str, start: usize, end: usize) -> bool {
    start < end && end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end)
}

/// Find every `[[koru://...]]` token and turn it back into a document pill.
///
/// **A link this build does not understand stays text.** That is the deliberate
/// other half of the parser's refuse-loudly rule: following a bad link should
/// say so, but failing an entire file over one is the wrong trade, and the
/// original token is more useful to a person than an error would be.
///
/// The bare `koru://{node}` form is *not* restored. A `NodeId` means nothing
/// outside the document that minted it, so resolving one needs the two-pass id
/// mapping ADR-126 built for OneNote, which this crate does not have. It stays
/// text rather than becoming a reference into whatever document happens to be
/// open.
fn koru_link_spans(text: &str) -> Vec<(usize, usize, Attr)> {
    let mut spans = Vec::new();
    for found in koru_link::wrapped(text) {
        let (open, end) = (found.range.start, found.range.end);
        let inner = &text[open + 2..end - 2];
        let attr = match found.link {
            koru_link::KoruLink::Document { document, node } => {
                // No label: the export carried none, and the caption cache keyed by
                // document id supplies the real title as soon as the manifest names it.
                Attr::new(
                    "document",
                    AttrValue::Document(koru_core::DocumentReference {
                        document: koru_core::DocumentId::from_uuid(document),
                        node: node.map(koru_core::NodeId::from_uuid),
                    }),
                )
            }
            koru_link::KoruLink::Chat { .. } => {
                Attr::labelled("link", AttrValue::Url(inner.to_owned()), "Koru AI chat")
            }
            koru_link::KoruLink::Node(_) => continue,
        };
        spans.push((open, end, attr));
    }
    spans
}

fn fallback_link(
    doc: &mut WorkspaceDoc,
    parent: NodeId,
    image: &ImageBlock,
) -> Result<NodeId, ImportError> {
    let label = if image.alt.trim().is_empty() {
        image.destination.as_str()
    } else {
        image.alt.as_str()
    };
    let id = doc
        .insert(
            Parent::Node(parent),
            Node::paragraph(label).map_err(import_failed)?,
        )
        .map_err(import_failed)?;
    if !image.destination.is_empty() {
        let attr = Attr::labelled("link", AttrValue::Url(image.destination.clone()), label);
        doc.delete_text(id, 0..label.len()).map_err(import_failed)?;
        doc.insert_pill(id, 0, &attr).map_err(import_failed)?;
    }
    Ok(id)
}

/// Store image bytes and build an image node that carries the intrinsic pixel
/// size, so the editor keeps the picture's aspect ratio instead of a default.
/// On a store failure the image degrades to a link, as elsewhere.
fn insert_image(
    doc: &mut WorkspaceDoc,
    parent: NodeId,
    source: &Path,
    image: &ImageBlock,
    bytes: Vec<u8>,
    mime: &str,
    filename: Option<&str>,
    name_fallback: Option<&str>,
    context: &ImportContext<'_>,
    warnings: &mut Vec<ImportWarning>,
) -> Result<NodeId, ImportError> {
    let (width, height) = image_dimensions(&bytes);
    let stored = match context.blobs.put(bytes, mime, filename) {
        Ok(stored) => stored,
        Err(error) => {
            warnings.push(ImportWarning {
                source: source.to_path_buf(),
                code: None,
                arguments: Vec::new(),
                message: match filename {
                    Some(name) => format!("could not store image {name}: {error}"),
                    None => format!("could not store the embedded image: {error}"),
                },
            });
            return fallback_link(doc, parent, image);
        }
    };
    let node =
        Node::image(BlobHash(stored.hash), stored.mime, width, height).map_err(import_failed)?;
    let id = doc
        .insert(Parent::Node(parent), node)
        .map_err(import_failed)?;
    let name = (!image.alt.trim().is_empty())
        .then_some(image.alt.as_str())
        .or(name_fallback);
    let _ = doc.set_name(id, name);
    Ok(id)
}

/// The image's intrinsic pixel size, read from its header only (no full
/// decode). `None` dimensions for an unrecognised or vector image fall back to
/// the editor's default sizing.
fn image_dimensions(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .map(|(width, height)| (Some(width), Some(height)))
        .unwrap_or((None, None))
}

fn local_image_path(markdown: &Path, destination: &str) -> Option<PathBuf> {
    let destination = destination.trim().trim_matches(['<', '>']);
    let lower = destination.to_ascii_lowercase();
    if destination.is_empty()
        || destination.starts_with('#')
        || lower.starts_with("data:")
        || lower.contains("://")
        || lower.starts_with("mailto:")
    {
        return None;
    }
    let decoded = decode_path(destination.split(['?', '#']).next().unwrap_or(destination));
    Some(
        markdown
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(decoded),
    )
}

fn decode_path(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(a), Some(b)) = (hex(bytes[index + 1]), hex(bytes[index + 2]))
        {
            out.push(a * 16 + b);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Decode a base64 `data:`-URI image into its bytes and MIME type. Google's
/// native Markdown export embeds pictures this way, so an imported image
/// arrives as text rather than a path. Anything that is not a base64 data URI —
/// a relative path, a remote `https://` URL, a non-base64 data URI — returns
/// `None` so the caller falls through to its other image handling.
fn data_uri(destination: &str) -> Option<(Vec<u8>, String)> {
    let rest = destination.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.strip_suffix(";base64")?;
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    let mime = if mime.is_empty() {
        "application/octet-stream"
    } else {
        mime
    };
    Some((bytes, mime.to_owned()))
}

fn image_mime(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn import_failed(error: impl std::fmt::Display) -> ImportError {
    ImportError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_import::{BlobSink, Importer, StoredBlob};

    struct Sink;
    impl BlobSink for Sink {
        fn put(&self, _: Vec<u8>, mime: &str, _: Option<&str>) -> Result<StoredBlob, ImportError> {
            Ok(StoredBlob {
                hash: format!("b3:{}", "0".repeat(64)),
                mime: mime.into(),
            })
        }
    }

    #[test]
    fn first_h1_is_the_title_and_not_a_body_row() {
        let parsed = parse_document(Path::new("fallback.md"), "# Roadmap\n\n## Now\n\nShip it.");
        assert_eq!(parsed.title, "Roadmap");
        assert_eq!(
            parsed
                .blocks
                .iter()
                .filter(
                    |block| matches!(block, Block::Text(text) if text.heading == Some(Heading::H1))
                )
                .count(),
            0
        );
        assert!(
            matches!(&parsed.blocks[0], Block::Text(text) if text.heading == Some(Heading::H2))
        );
    }

    #[test]
    fn filename_is_the_title_when_there_is_no_h1() {
        assert_eq!(
            parse_document(Path::new("Meeting notes.md"), "Hello").title,
            "Meeting notes"
        );
    }

    /// A fenced block whose lines carry pipes is code, not a table. The
    /// table pre-scan, which runs on the raw text, took a Mermaid flowchart's
    /// labelled edges — `A -->|yes| B` over `A -->|no| C` — for a header and
    /// its delimiter, and a document pasted from Notion arrived with two of
    /// its five diagrams cut into paragraphs around a broken table.
    #[test]
    fn a_fenced_block_with_pipes_is_code_not_a_table() {
        let text = "Before\n\n```mermaid\nflowchart TD\n  Online -->|yes| Push[Push delta]\n  Online -->|no| Queue[[Queue locally]]\n  Push --> Done\n```\n\nAfter\n\n| A | B |\n| - | - |\n";
        let blocks = parse(text);
        assert!(
            !blocks.iter().take(3).any(|b| matches!(b, Block::Table(_))),
            "no table inside the fence: {blocks:?}"
        );
        let code = blocks
            .iter()
            .find_map(|b| match b {
                Block::Text(text) if matches!(text.kind, TextKind::Code { .. }) => Some(text),
                _ => None,
            })
            .expect("one code block");
        assert!(matches!(&code.kind, TextKind::Code { language: Some(l) } if l == "mermaid"));
        assert!(code.text.contains("Online -->|yes| Push[Push delta]"));
        assert!(code.text.contains("Push --> Done"));
        // The table after the fence is still a table.
        assert!(blocks.iter().any(|b| matches!(b, Block::Table(_))));
        // A tilde fence, and one left open, are fences too.
        assert_eq!(
            fenced_lines(["~~~", "| a | b |", "| - | - |", "~~~", "| c | d |"].into_iter()),
            vec![true, true, true, true, false]
        );
        assert_eq!(
            fenced_lines(["```", "| a | b |"].into_iter()),
            vec![true, true]
        );
        assert!(table_candidate_ranges("```\n| a | b |\n| - | - |\n```\n").is_empty());
    }

    /// The stream's shape, for the callout tests: a text block as its kind,
    /// text and whether it is inside a box; a box's start with its look; a
    /// box's end; anything else.
    #[derive(Debug, PartialEq)]
    enum Row<'a> {
        Text(TextKind, &'a str, bool),
        Start(Option<&'a str>, Option<&'a str>),
        End,
        Other,
    }

    fn rows(blocks: &[Block]) -> Vec<Row<'_>> {
        blocks
            .iter()
            .map(|block| match block {
                Block::Text(text) => {
                    Row::Text(text.kind.clone(), &text.text, text.callout.is_some())
                }
                Block::CalloutStart { icon, tint } => Row::Start(icon.as_deref(), tint.as_deref()),
                Block::CalloutEnd => Row::End,
                _ => Row::Other,
            })
            .collect()
    }

    /// The whole document, built from `markdown`.
    fn built(markdown: &str) -> WorkspaceDoc {
        let context = ImportContext {
            target_profile: None,
            target_workspace: Some(koru_store_core_id()),
            occupied_titles: &std::collections::BTreeSet::new(),
            blobs: &Sink,
            tags: &koru_import::NoTagSink,
        };
        let (_, snapshot, _) = build_document_from_text(
            koru_core::WorkspaceId::new(),
            "Doc",
            "export.md",
            markdown,
            &context,
            &koru_import::NoProgress,
        )
        .unwrap();
        WorkspaceDoc::from_snapshot(&snapshot).unwrap()
    }

    /// The children of `parent` as `(kind, text, icon)`.
    fn shape(doc: &WorkspaceDoc, parent: NodeId) -> Vec<(koru_core::Kind, String, Option<String>)> {
        doc.children(Parent::Node(parent))
            .unwrap()
            .into_iter()
            .map(|id| {
                let node = doc.read(id).unwrap();
                (
                    node.kind,
                    node.content.as_text().unwrap_or_default().to_owned(),
                    node.meta.icon,
                )
            })
            .collect()
    }

    /// The `<aside>` form a Markdown export writes: the icon leading the
    /// first line, more blocks after a blank line, `</aside>`. The box opens
    /// before its first line, which carries the icon for a renderer that
    /// sees one block at a time; the rest is inside the box; nothing of the
    /// HTML is left in the text.
    #[test]
    fn an_aside_is_a_box_around_its_lines() {
        let blocks =
            parse("before\n\n<aside>\n⚠️ Back up **weekly**.\n\nSecond line\n\n</aside>\n\nafter");
        assert_eq!(
            rows(&blocks),
            vec![
                Row::Text(TextKind::Paragraph, "before", false),
                Row::Start(Some("⚠️"), None),
                Row::Text(TextKind::Paragraph, "Back up weekly.", true),
                Row::Text(TextKind::Paragraph, "Second line", true),
                Row::End,
                Row::Text(TextKind::Paragraph, "after", false),
            ]
        );
        let place = |index: usize| match &blocks[index] {
            Block::Text(text) => text.callout.clone(),
            _ => unreachable!(),
        };
        assert_eq!(
            place(2),
            Some(CalloutLine::First {
                icon: Some("⚠️".to_owned())
            })
        );
        assert_eq!(place(3), Some(CalloutLine::Inside));
    }

    /// Everything inside the aside, in order, lands under the box: its
    /// first line, a paragraph, a list with its items, a heading, an image.
    /// The box itself carries no text, only the look. A box does not nest
    /// in a box, so the next aside is the box's sibling.
    #[test]
    fn everything_inside_an_aside_builds_under_the_box() {
        use koru_core::Kind;
        let doc = built(
            "<aside>\n💡 Head\n\nSecond\n\n- one\n  - nested\n- two\n\n## Title\n\n![pic](https://example.com/a.png)\n\n</aside>\n\n<aside>\n📌 Next\n\n</aside>\n\nafter",
        );
        let root = doc.children(Parent::Root).unwrap()[0];
        assert_eq!(
            shape(&doc, root),
            vec![
                (Kind::Callout, String::new(), Some("💡".to_owned())),
                (Kind::Callout, String::new(), Some("📌".to_owned())),
                (Kind::Paragraph, "after".to_owned(), None),
            ]
        );
        let callout = doc.children(Parent::Node(root)).unwrap()[0];
        let inside = shape(&doc, callout);
        assert_eq!(inside.len(), 5, "{inside:?}");
        assert_eq!(inside[0], (Kind::Paragraph, "Head".to_owned(), None));
        assert_eq!(inside[1], (Kind::Paragraph, "Second".to_owned(), None));
        assert_eq!(inside[2].0, Kind::ListBlock);
        assert_eq!(inside[3], (Kind::Paragraph, "Title".to_owned(), None));
        assert_eq!(inside[4].0, Kind::Paragraph, "a remote image is a link");
        let list = doc.children(Parent::Node(callout)).unwrap()[2];
        let items = shape(&doc, list);
        assert_eq!(items[0].1, "one");
        assert_eq!(items[1].1, "two");
        let one = doc.children(Parent::Node(list)).unwrap()[0];
        assert_eq!(
            shape(&doc, one)[0].0,
            Kind::ListBlock,
            "nested list under its item"
        );
        let title = doc.children(Parent::Node(callout)).unwrap()[3];
        assert_eq!(doc.read(title).unwrap().meta.heading, Some(Heading::H2));
        let next = doc.children(Parent::Node(root)).unwrap()[1];
        assert_eq!(
            shape(&doc, next),
            vec![(Kind::Paragraph, "Next".to_owned(), None)]
        );
    }

    /// The icon on a line of its own, and a first line that is markdown
    /// rather than raw HTML: the marks survive the icon being cut away.
    #[test]
    fn an_icon_alone_on_its_line_still_belongs_to_the_callout() {
        let blocks = parse("<aside>\n💡\n\n**Tip:** read this\n\n</aside>");
        assert_eq!(
            rows(&blocks),
            vec![
                Row::Start(Some("💡"), None),
                Row::Text(TextKind::Paragraph, "Tip: read this", true),
                Row::End
            ]
        );
        let Block::Text(text) = &blocks[1] else {
            unreachable!()
        };
        assert_eq!(&text.text[text.runs[0].start..text.runs[0].end], "Tip:");

        let blocks = parse("<aside>\n\n🔥 **Hot** take\n\n</aside>");
        assert_eq!(rows(&blocks)[0], Row::Start(Some("🔥"), None));
        let Block::Text(text) = &blocks[1] else {
            unreachable!()
        };
        assert_eq!(text.text, "Hot take");
        assert_eq!(&text.text[text.runs[0].start..text.runs[0].end], "Hot");
    }

    /// A built-in icon arrives as an `<img>` of an icon set; its name picks
    /// the emoji and its color the tint.
    #[test]
    fn an_icon_image_becomes_the_icon_and_the_tint() {
        let blocks = parse(
            "<aside>\n<img src=\"https://example.com/icons/clock_gray.svg\" alt=\"x\" width=\"40px\" /> Keep learning\n\n</aside>",
        );
        assert_eq!(
            rows(&blocks),
            vec![
                Row::Start(Some("⏰"), Some("gray")),
                Row::Text(TextKind::Paragraph, "Keep learning", true),
                Row::End
            ]
        );
    }

    /// A box whose first block is a list, not a line: the box opens with the
    /// default look and the list is inside it rather than dropped. The same
    /// shape as a quote led by the icon alone, which is how the exporter
    /// writes a box whose first line is not prose.
    #[test]
    fn a_box_that_opens_with_a_list_is_a_box_around_the_list() {
        let expected = vec![
            Row::Start(None, None),
            Row::Text(TextKind::Paragraph, "one", true),
            Row::Text(TextKind::Paragraph, "two", true),
            Row::End,
        ];
        assert_eq!(
            rows(&parse("<aside>\n\n- one\n- two\n\n</aside>")),
            expected
        );
        let mut led = expected;
        led[0] = Row::Start(Some("💡"), None);
        assert_eq!(rows(&parse("> 💡\n>\n> - one\n> - two")), led);
    }

    /// The one-line shape some exports write, and the whole document built:
    /// the box is a callout node with the icon in its metadata and no text
    /// of its own; the line is under it.
    #[test]
    fn a_one_line_aside_builds_a_callout_node() {
        let blocks = parse("<aside> 📌 Pinned note\n</aside>");
        assert_eq!(
            rows(&blocks),
            vec![
                Row::Start(Some("📌"), None),
                Row::Text(TextKind::Paragraph, "Pinned note", true),
                Row::End
            ]
        );

        let doc = built("<aside>\n📌 Pinned note\n\n</aside>");
        let root = doc.children(Parent::Root).unwrap()[0];
        let body = doc.children(Parent::Node(root)).unwrap();
        let node = doc.read(body[0]).unwrap();
        assert_eq!(node.kind, koru_core::Kind::Callout);
        assert_eq!(node.meta.icon.as_deref(), Some("📌"));
        assert_eq!(node.content.as_text(), Some(""));
        assert_eq!(
            shape(&doc, body[0]),
            vec![(koru_core::Kind::Paragraph, "Pinned note".to_owned(), None)]
        );
    }

    /// The form this crate's exporter writes: a blockquote whose first line
    /// leads with an emoji is a box around the rest of the quote. A quote
    /// whose first line is prose stays a quote, and a quote nested inside
    /// the box is a quote.
    #[test]
    fn a_blockquote_led_by_an_emoji_is_a_box_and_a_plain_one_is_a_quote() {
        let blocks = parse(
            "> 💡 Keep it\n>\n> Second\n>\n> - one\n> - two\n>\n> > deeper\n\n> plain quote\n\n> - just a list",
        );
        assert_eq!(
            rows(&blocks),
            vec![
                Row::Start(Some("💡"), None),
                Row::Text(TextKind::Paragraph, "Keep it", true),
                Row::Text(TextKind::Paragraph, "Second", true),
                Row::Text(TextKind::Paragraph, "one", true),
                Row::Text(TextKind::Paragraph, "two", true),
                Row::Text(TextKind::Quote, "deeper", true),
                Row::End,
                Row::Text(TextKind::Quote, "plain quote", false),
                Row::Text(TextKind::Quote, "just a list", false),
            ]
        );
        let Block::Text(one) = &blocks[3] else {
            unreachable!()
        };
        assert_eq!(one.list.map(|list| list.style), Some(ListStyle::Bullet));
    }

    #[test]
    fn a_leading_emoji_is_the_icon_and_a_word_is_not() {
        assert_eq!(
            leading_icon(" 💡 Read this"),
            Some(("💡".to_owned(), " 💡 ".len()))
        );
        assert_eq!(leading_icon("⚠️"), Some(("⚠️".to_owned(), "⚠️".len())));
        assert_eq!(leading_icon("Read this"), None);
        assert_eq!(leading_icon(""), None);
    }

    #[test]
    fn exported_color_names_map_onto_koru_tints() {
        use koru_core::AccentColor;
        assert_eq!(color_name("blue_background"), Some(Some(AccentColor::Blue)));
        assert_eq!(color_name("red_bg"), Some(Some(AccentColor::Red)));
        assert_eq!(color_name("Brown"), Some(Some(AccentColor::Orange)));
        assert_eq!(color_name("default"), Some(None));
        assert_eq!(color_name("teal"), Some(Some(AccentColor::Teal)));
        assert_eq!(color_name("🔥"), None);
    }

    #[test]
    fn a_built_in_icon_address_becomes_an_emoji_and_a_tint() {
        use koru_core::AccentColor;
        assert_eq!(
            builtin_icon("https://example.com/icons/info-alternate_gray.svg"),
            Some((Some("ℹ️"), Some(AccentColor::Gray)))
        );
        assert_eq!(
            builtin_icon("/icons/token_blue.svg"),
            Some((Some("🔑"), Some(AccentColor::Blue)))
        );
        assert_eq!(
            builtin_icon("https://example.com/icons/squiggle_purple.svg"),
            Some((None, Some(AccentColor::Purple)))
        );
        assert_eq!(builtin_icon("https://example.com/a.png"), None);
    }

    /// Other HTML is still text, as it was before callouts.
    #[test]
    fn html_that_is_not_an_aside_is_unchanged() {
        let blocks = parse("a <kbd>K</kbd> key");
        assert_eq!(
            rows(&blocks),
            vec![Row::Text(TextKind::Paragraph, "a <kbd>K</kbd> key", false)]
        );
    }

    #[test]
    fn parser_keeps_tasks_tables_rules_images_and_marks() {
        let blocks = parse("- [x] **done**\n\n| A | B |\n| - | - |\n\n---\n\n![map](map.png)");
        assert!(
            matches!(&blocks[0], Block::Text(text) if text.task == Some(true) && text.runs.iter().any(|run| run.marks.bold))
        );
        assert!(blocks.iter().any(|b| matches!(b, Block::Table(_))));
        assert!(blocks.iter().any(|b| matches!(b, Block::Rule)));
        assert!(
            blocks
                .iter()
                .any(|b| matches!(b, Block::Image(image) if image.destination == "map.png"))
        );
    }

    #[test]
    fn frontmatter_is_preserved_as_yaml_code() {
        let parsed = parse_document(Path::new("note.md"), "---\ntags: [one]\n---\nBody");
        assert!(
            matches!(&parsed.blocks[0], Block::Text(TextBlock { kind: TextKind::Code { language: Some(language) }, text, .. }) if language == "yaml" && text == "tags: [one]")
        );
    }

    /// The other half of a round trip through another application: a code
    /// block leaves Koru as the fence `koru_core::markdown::fenced_code`
    /// writes (the editor's clipboard test pins that), and that fence parses
    /// back into one code block with the same language and the same body —
    /// including a body that itself quotes a fence, which is what the
    /// lengthened fence is for.
    #[test]
    fn a_fenced_block_as_the_clipboard_writes_it_parses_back_whole() {
        for (language, body) in [
            ("rust", "fn main() {\n    println!(\"hi\");\n}"),
            ("md", "a\n```\nb"),
            ("", "x = 1"),
        ] {
            let blocks = parse(&koru_core::markdown::fenced_code(language, body));
            assert_eq!(blocks.len(), 1, "{language:?} {body:?}: {blocks:?}");
            let expected = (!language.is_empty()).then(|| language.to_owned());
            assert!(
                matches!(&blocks[0], Block::Text(TextBlock { kind: TextKind::Code { language }, text, .. }) if *language == expected && text == body),
                "{language:?} {body:?}: {blocks:?}"
            );
        }
    }

    #[test]
    fn local_image_paths_are_relative_and_percent_decoded() {
        assert_eq!(
            local_image_path(Path::new("notes/one.md"), "./assets/My%20Map.png"),
            Some(PathBuf::from("notes/./assets/My Map.png"))
        );
        assert_eq!(
            local_image_path(Path::new("one.md"), "https://example.com/a.png"),
            None
        );
    }

    #[test]
    fn a_base64_data_uri_decodes_to_bytes_and_mime() {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.encode(b"png-bytes");
        let (decoded, mime) = data_uri(&format!("data:image/png;base64,{bytes}")).unwrap();
        assert_eq!(decoded, b"png-bytes");
        assert_eq!(mime, "image/png");
    }

    #[test]
    fn a_non_base64_data_uri_is_not_decoded() {
        assert_eq!(data_uri("data:image/png,notbase64"), None);
        assert_eq!(data_uri("https://example.com/a.png"), None);
        assert_eq!(data_uri("assets/a.png"), None);
    }

    #[test]
    fn build_document_from_text_uses_the_given_title_and_stores_embedded_images() {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.encode(b"png-bytes");
        let markdown = format!("# A section heading\n\n![map](data:image/png;base64,{bytes})");
        let context = ImportContext {
            target_profile: None,
            target_workspace: Some(koru_store_core_id()),
            occupied_titles: &std::collections::BTreeSet::new(),
            blobs: &Sink,
            tags: &koru_import::NoTagSink,
        };
        let (title, snapshot, warnings) = build_document_from_text(
            koru_core::WorkspaceId::new(),
            "The real title",
            "google-export",
            &markdown,
            &context,
            &koru_import::NoProgress,
        )
        .unwrap();
        assert_eq!(title, "The real title");
        assert!(warnings.is_empty());
        let doc = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
        // The heading is a body row, not consumed as the title, and the image
        // is a real image node, not a link.
        let root = doc.children(Parent::Root).unwrap()[0];
        let body = doc.children(Parent::Node(root)).unwrap();
        assert_eq!(body.len(), 2);
        assert!(
            matches!(&doc.read(body[0]).unwrap().meta.heading, Some(Heading::H1)),
            "the section heading survived rather than becoming the title"
        );
        assert_eq!(doc.read(body[1]).unwrap().kind, koru_core::Kind::Image);
    }

    #[test]
    fn a_trailing_data_uri_reference_definition_does_not_become_a_source_link_pill() {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.encode(b"png-bytes");
        // Google's Markdown export emits images as reference-style images whose
        // `data:` definitions trail the document. The image is decoded into an
        // image node; the definition must not also appear as a link pill.
        let markdown = format!("![map][img1]\n\n[img1]: data:image/png;base64,{bytes}");
        let context = ImportContext {
            target_profile: None,
            target_workspace: Some(koru_store_core_id()),
            occupied_titles: &std::collections::BTreeSet::new(),
            blobs: &Sink,
            tags: &koru_import::NoTagSink,
        };
        let (_, snapshot, warnings) = build_document_from_text(
            koru_core::WorkspaceId::new(),
            "title",
            "google-export",
            &markdown,
            &context,
            &koru_import::NoProgress,
        )
        .unwrap();
        assert!(warnings.is_empty());
        let doc = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
        let root = doc.children(Parent::Root).unwrap()[0];
        let body = doc.children(Parent::Node(root)).unwrap();
        assert_eq!(body.len(), 1, "only the image, no trailing link pill");
        assert_eq!(doc.read(body[0]).unwrap().kind, koru_core::Kind::Image);
    }

    #[test]
    fn image_dimensions_reads_the_intrinsic_size_from_the_header() {
        use image::RgbaImage;
        let img = RgbaImage::from_pixel(7, 3, image::Rgba([0, 0, 0, 255]));
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(image_dimensions(&png), (Some(7), Some(3)));
    }

    #[test]
    fn image_dimensions_is_none_for_unrecognised_bytes() {
        assert_eq!(image_dimensions(b"not an image"), (None, None));
    }

    #[test]
    fn builds_a_snapshot_with_one_document_root() {
        let parsed = parse_document(Path::new("one.md"), "# One\n\nHello **world**.");
        let context = ImportContext {
            target_profile: None,
            target_workspace: Some(koru_store_core_id()),
            occupied_titles: &std::collections::BTreeSet::new(),
            blobs: &Sink,
            tags: &koru_import::NoTagSink,
        };
        let (_, snapshot, _) = build_document(
            koru_core::WorkspaceId::new(),
            Path::new("one.md"),
            parsed,
            &context,
            &koru_import::NoProgress,
        )
        .unwrap();
        let doc = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
        assert_eq!(doc.children(Parent::Root).unwrap().len(), 1);
    }

    #[test]
    fn a_bad_file_does_not_prevent_a_good_file_in_the_same_batch() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("Good.md");
        let missing = dir.path().join("Missing.md");
        std::fs::write(&good, "# Imported\n\nHello.").unwrap();

        let prepared = MarkdownImporter
            .inspect(
                ImportSource::Files(vec![missing.clone(), good.clone()]),
                &koru_import::NoProgress,
            )
            .unwrap();
        assert_eq!(prepared.preview().document_count, 1);

        let target = koru_store_core_id();
        let context = ImportContext {
            target_profile: None,
            target_workspace: Some(target),
            occupied_titles: &std::collections::BTreeSet::new(),
            blobs: &Sink,
            tags: &koru_import::NoTagSink,
        };
        let product = prepared
            .build(
                ImportOptions {
                    identity: IdentityMode::Copy,
                    accept_warnings: true,
                },
                &context,
                &koru_import::NoProgress,
            )
            .unwrap();
        let ImportProduct::Documents(content) = product else {
            panic!("Markdown produced a workspace bundle")
        };
        assert_eq!(content.documents.len(), 1);
        assert_eq!(content.documents[0].title, "Imported");
        assert_eq!(content.warnings.len(), 1);
        assert_eq!(content.warnings[0].source, missing);
    }

    fn koru_store_core_id() -> koru_store_core::ids::WorkspaceId {
        koru_store_core::ids::WorkspaceId::new()
    }

    fn document_pill(text: &str) -> Option<koru_core::DocumentReference> {
        let spans = koru_link_spans(text);
        let (_, _, attr) = spans.into_iter().next()?;
        assert_eq!(attr.role, "document");
        match attr.value {
            AttrValue::Document(reference) => Some(reference),
            _ => None,
        }
    }

    /// The round trip this closes: a document pill exported to Markdown used to
    /// come back as literal text.
    #[test]
    fn a_koru_document_link_imports_back_into_a_document_pill() {
        let document = koru_core::DocumentId::new();
        let reference = document_pill(&format!("see [[koru://doc/{document}]] for more"))
            .expect("a document reference");

        assert_eq!(reference.document, document);
        assert_eq!(reference.node, None);
    }

    #[test]
    fn a_koru_document_link_with_a_fragment_keeps_its_node() {
        let document = koru_core::DocumentId::new();
        let node = koru_core::NodeId::new();
        let reference = document_pill(&format!("see [[koru://doc/{document}#{node}]]"))
            .expect("a document reference");

        assert_eq!(reference.document, document);
        assert_eq!(reference.node, Some(node));
    }

    /// **Quietly, and only here.** Following a bad link says so out loud; an
    /// import keeps the text, because failing a whole file over one link is the
    /// wrong trade and the original token is more use to a person than an
    /// error.
    #[test]
    fn an_unknown_koru_link_imports_as_plain_text_rather_than_failing_the_file() {
        let document = koru_core::DocumentId::new();
        for text in [
            format!("[[koru://doc/{document}/history]]"),
            "[[koru://docs/model]]".to_owned(),
            "[[not a link at all]]".to_owned(),
            // A bare node needs ADR-126's two-pass id mapping to mean anything,
            // and this crate has none -- so it stays text on purpose.
            format!("[[koru://{}]]", koru_core::NodeId::new()),
        ] {
            assert!(koru_link_spans(&text).is_empty(), "{text}");
        }
    }

    /// Two links in one paragraph are applied back-to-front, so the second
    /// one's offsets are still good when the first is replaced.
    #[test]
    fn two_koru_links_in_one_paragraph_both_survive() {
        let (one, two) = (koru_core::DocumentId::new(), koru_core::DocumentId::new());
        let spans = koru_link_spans(&format!("[[koru://doc/{one}]] and [[koru://doc/{two}]]"));

        assert_eq!(spans.len(), 2);
        assert!(spans[0].1 <= spans[1].0, "the spans do not overlap");
    }

    /// **An import restores; it never commands** (SPEC §5, ADR-273).
    ///
    /// A `- [x]` is a *checked box*, which is content, and never a completed
    /// task: writing a marker here would invent a creation reason the archive
    /// never recorded, and a completed one over an unchecked child would then
    /// have to decide whether the child reopens it. Neither question arises,
    /// because an importer's exemption is that the classification entry point
    /// and the six canonical commands are simply not on this path — and that is
    /// something this test can read off the crate itself, rather than take on
    /// trust.
    #[test]
    fn a_checked_box_lands_verbatim_and_no_task_is_invented() {
        let ws = koru_core::WorkspaceId::new();
        let (_title, snapshot, _warnings) = build_document_from_text(
            ws,
            "Shipping",
            "shipping.md",
            "- [x] ship it\n  - [ ] draft the summary\n",
            &ImportContext {
                target_profile: None,
                target_workspace: None,
                occupied_titles: &std::collections::BTreeSet::new(),
                blobs: &Sink,
                tags: &koru_import::NoTagSink,
            },
            &koru_import::NoProgress,
        )
        .unwrap();

        let doc = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
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
        for (id, _) in doc.walk_all().unwrap() {
            assert!(
                !koru_core::tasks::is_task_node(&doc, id).unwrap(),
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
