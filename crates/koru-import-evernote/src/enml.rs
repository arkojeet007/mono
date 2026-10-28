// SPDX-License-Identifier: MPL-2.0

use std::collections::{BTreeMap, BTreeSet};
use std::io::Cursor;
use std::path::{Path, PathBuf};

use koru_core::ids::{DocRef, DocumentReference, Ref};
use koru_core::node::{Attr, AttrValue, BlobHash, Heading, Kind, ListStyle, Node};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::text::Mark;
use koru_core::{NodeId, WorkspaceId};
use koru_import::{ImportError, ImportWarning, ImportWarningCode, ImportedDocument};
use koru_store_core::ids::DocumentId as StoreDocumentId;
use quick_xml::events::{BytesStart, Event};
use quick_xml::{Reader, XmlVersion};

use crate::GuidIndex;
use crate::enex::{NoExternalEntityFetcher, XmlSecurityError, validate_xml};

const MAX_ENML_DEPTH: usize = 128;
const ENCRYPTED_PLACEHOLDER: &str = "Imported encrypted content";

#[derive(Clone, Copy)]
pub(crate) struct LinkContext<'a> {
    pub file_index: usize,
    pub guids: &'a GuidIndex,
}

pub(crate) struct ConvertedNote {
    pub snapshot: Vec<u8>,
    pub warnings: Vec<ImportWarning>,
    pub files: Vec<ImportedDocument>,
    pub bindings: Vec<PendingResourceBinding>,
}

#[derive(Clone, Debug)]
pub(crate) struct NoteResource {
    pub ordinal: usize,
    pub md5: Option<String>,
    pub spool: Option<usize>,
    pub mime: String,
    pub filename: Option<String>,
    pub attachment: bool,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

pub(crate) struct PendingResourceBinding {
    pub document: StoreDocumentId,
    pub node: NodeId,
    pub spool: usize,
    pub mime: String,
    pub filename: Option<String>,
}

#[derive(Debug)]
pub(crate) enum EnmlError {
    /// Rejects the complete ENEX source, including documents already staged.
    Security,
    /// Drops only this note; later notes in the same source continue.
    Malformed(String),
    Build(ImportError),
}

#[derive(Clone, Debug)]
enum EnmlNode {
    Element(EnmlElement),
    Text(String),
}

#[derive(Clone, Debug)]
struct EnmlElement {
    name: String,
    attributes: Vec<(String, String)>,
    children: Vec<EnmlNode>,
}

impl EnmlElement {
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value.as_str()))
    }

    fn hidden(&self) -> bool {
        self.style_value("display")
            .and_then(|value| value.split('!').next())
            .map(str::trim)
            .is_some_and(|value| value.eq_ignore_ascii_case("none"))
    }

    fn style_value(&self, wanted: &str) -> Option<&str> {
        self.attribute("style")?.split(';').find_map(|declaration| {
            let (name, value) = declaration.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case(wanted)
                .then_some(value.trim())
        })
    }

    /// Whether an Evernote `--en-*: true` marker is set in the style.
    fn style_flag(&self, wanted: &str) -> bool {
        self.style_value(wanted)
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    }

    fn has_element_child(&self) -> bool {
        self.children
            .iter()
            .any(|child| matches!(child, EnmlNode::Element(_)))
    }
}

#[derive(Clone, Default)]
struct InlineState {
    marks: Vec<Mark>,
    highlight: Option<String>,
}

#[derive(Clone)]
struct MarkSpan {
    start: usize,
    end: usize,
    mark: Mark,
}

#[derive(Clone)]
struct HighlightSpan {
    start: usize,
    end: usize,
    color: String,
}

#[derive(Clone)]
struct LinkSpan {
    start: usize,
    end: usize,
    target: String,
}

#[derive(Clone, Default)]
struct InlineBuffer {
    text: String,
    marks: Vec<MarkSpan>,
    highlights: Vec<HighlightSpan>,
    links: Vec<LinkSpan>,
}

impl InlineBuffer {
    fn append(&mut self, text: &str, state: &InlineState) {
        let start = self.text.len();
        self.text.push_str(text);
        let end = self.text.len();
        if start == end {
            return;
        }
        self.marks.extend(
            state
                .marks
                .iter()
                .copied()
                .map(|mark| MarkSpan { start, end, mark }),
        );
        if let Some(color) = &state.highlight {
            self.highlights.push(HighlightSpan {
                start,
                end,
                color: color.clone(),
            });
        }
    }
}

#[cfg(test)]
pub(crate) fn convert_note(
    workspace: WorkspaceId,
    title: &str,
    enml: &str,
    source: &Path,
) -> Result<ConvertedNote, EnmlError> {
    convert_note_with_resources(
        workspace,
        StoreDocumentId::new(),
        0,
        title,
        enml,
        source,
        &[],
        None,
    )
}

pub(crate) fn convert_note_with_resources(
    workspace: WorkspaceId,
    document_id: StoreDocumentId,
    note_ordinal: usize,
    title: &str,
    enml: &str,
    source: &Path,
    resources: &[NoteResource],
    links: Option<LinkContext<'_>>,
) -> Result<ConvertedNote, EnmlError> {
    let root = parse_enml(enml)?;
    if root.name != "en-note" {
        return Err(EnmlError::Malformed(
            "Evernote note content has no en-note root".into(),
        ));
    }

    let mut document = WorkspaceDoc::create(workspace, title).map_err(build_error)?;
    let title_row = document
        .insert(Parent::Root, Node::document(title).map_err(build_error)?)
        .map_err(build_error)?;
    let mut converter = Converter {
        document,
        source: source.to_path_buf(),
        warnings: Vec::new(),
        workspace,
        document_id,
        note_ordinal,
        resources,
        used_resources: BTreeSet::new(),
        files: Vec::new(),
        file_by_resource: BTreeMap::new(),
        bindings: Vec::new(),
        links,
    };
    if contains_modern_task_payload(&root) {
        converter.warn_once(
            ImportWarningCode::EvernoteTaskPayloadUnsupported,
            "Evernote task payload was preserved without task semantics",
        );
    }
    if !root.hidden() {
        converter.render_flow(&root.children, title_row, false)?;
    }
    converter.render_unreferenced(title_row)?;
    let snapshot = converter.document.export_snapshot().map_err(build_error)?;
    Ok(ConvertedNote {
        snapshot,
        warnings: converter.warnings,
        files: converter.files,
        bindings: converter.bindings,
    })
}

fn build_error(error: impl std::fmt::Display) -> EnmlError {
    EnmlError::Build(ImportError::new(error.to_string()))
}

fn parse_enml(enml: &str) -> Result<EnmlElement, EnmlError> {
    validate_xml(Cursor::new(enml.as_bytes()), &NoExternalEntityFetcher).map_err(|error| {
        match error {
            XmlSecurityError::Malformed(message) => EnmlError::Malformed(message),
            XmlSecurityError::ForbiddenDoctype
            | XmlSecurityError::ForbiddenEntity(_)
            | XmlSecurityError::ExternalFetch(_) => EnmlError::Security,
        }
    })?;

    let mut reader = Reader::from_str(enml);
    reader.config_mut().check_end_names = true;
    let mut stack = Vec::<EnmlElement>::new();
    let mut root = None;

    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                if stack.len() >= MAX_ENML_DEPTH {
                    return Err(EnmlError::Security);
                }
                stack.push(element(&reader, &start)?);
            }
            Ok(Event::Empty(start)) => {
                if stack.len() >= MAX_ENML_DEPTH {
                    return Err(EnmlError::Security);
                }
                append_element(&mut stack, &mut root, element(&reader, &start)?)?;
            }
            Ok(Event::Text(text)) => {
                let decoded = text
                    .xml10_content()
                    .map_err(|error| EnmlError::Malformed(error.to_string()))?;
                append_text(&mut stack, decoded.into_owned())?;
            }
            Ok(Event::CData(text)) => {
                let decoded = text
                    .xml10_content()
                    .map_err(|error| EnmlError::Malformed(error.to_string()))?;
                append_text(&mut stack, decoded.into_owned())?;
            }
            Ok(Event::GeneralRef(reference)) => {
                let decoded = decode_reference(reference.as_ref())
                    .ok_or_else(|| EnmlError::Malformed("invalid character reference".into()))?;
                append_text(&mut stack, decoded)?;
            }
            Ok(Event::End(_)) => {
                let completed = stack
                    .pop()
                    .ok_or_else(|| EnmlError::Malformed("ENML end tag without a start".into()))?;
                append_element(&mut stack, &mut root, completed)?;
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(EnmlError::Malformed(error.to_string())),
        }
    }

    if !stack.is_empty() {
        return Err(EnmlError::Malformed("truncated ENML".into()));
    }
    root.ok_or_else(|| EnmlError::Malformed("empty ENML".into()))
}

pub(crate) fn validate_note(enml: &str) -> Result<(), EnmlError> {
    parse_enml(enml).map(|_| ())
}

fn element(reader: &Reader<&[u8]>, start: &BytesStart<'_>) -> Result<EnmlElement, EnmlError> {
    let name = String::from_utf8_lossy(start.local_name().as_ref()).to_ascii_lowercase();
    let mut attributes = Vec::new();
    for attribute in start.attributes() {
        let attribute = attribute.map_err(|error| EnmlError::Malformed(error.to_string()))?;
        let key = String::from_utf8_lossy(attribute.key.local_name().as_ref()).to_ascii_lowercase();
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|error| EnmlError::Malformed(error.to_string()))?
            .into_owned();
        attributes.push((key, value));
    }
    Ok(EnmlElement {
        name,
        attributes,
        children: Vec::new(),
    })
}

fn append_element(
    stack: &mut [EnmlElement],
    root: &mut Option<EnmlElement>,
    element: EnmlElement,
) -> Result<(), EnmlError> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(EnmlNode::Element(element));
        return Ok(());
    }
    if root.replace(element).is_some() {
        return Err(EnmlError::Malformed("multiple ENML roots".into()));
    }
    Ok(())
}

fn append_text(stack: &mut [EnmlElement], text: String) -> Result<(), EnmlError> {
    let Some(parent) = stack.last_mut() else {
        if text.trim().is_empty() {
            return Ok(());
        }
        return Err(EnmlError::Malformed("text outside the ENML root".into()));
    };
    parent.children.push(EnmlNode::Text(text));
    Ok(())
}

fn decode_reference(reference: &[u8]) -> Option<String> {
    let builtin = match reference {
        b"amp" => Some('&'),
        b"lt" => Some('<'),
        b"gt" => Some('>'),
        b"apos" => Some('\''),
        b"quot" => Some('"'),
        _ => None,
    };
    if let Some(character) = builtin {
        return Some(character.to_string());
    }
    let value = if let Some(hex) = reference
        .strip_prefix(b"#x")
        .or_else(|| reference.strip_prefix(b"#X"))
    {
        u32::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?
    } else {
        std::str::from_utf8(reference.strip_prefix(b"#")?)
            .ok()?
            .parse()
            .ok()?
    };
    char::from_u32(value).map(|character| character.to_string())
}

struct Converter<'a> {
    document: WorkspaceDoc,
    source: PathBuf,
    warnings: Vec<ImportWarning>,
    workspace: WorkspaceId,
    document_id: StoreDocumentId,
    note_ordinal: usize,
    resources: &'a [NoteResource],
    used_resources: BTreeSet<usize>,
    files: Vec<ImportedDocument>,
    file_by_resource: BTreeMap<usize, (StoreDocumentId, NodeId, String)>,
    bindings: Vec<PendingResourceBinding>,
    links: Option<LinkContext<'a>>,
}

impl Converter<'_> {
    fn render_flow(
        &mut self,
        nodes: &[EnmlNode],
        parent: NodeId,
        ensure_empty: bool,
    ) -> Result<(), EnmlError> {
        let mut inline = Vec::<EnmlNode>::new();
        let mut rendered = false;
        for node in nodes {
            if let EnmlNode::Element(element) = node
                && element.hidden()
            {
                continue;
            }
            if let EnmlNode::Element(element) = node
                && is_block(element)
            {
                rendered |= self.flush_inline(&inline, parent, Kind::Paragraph, None, false)?;
                inline.clear();
                rendered |= self.render_block(element, parent)?;
            } else {
                inline.push(node.clone());
            }
        }
        self.flush_inline(
            &inline,
            parent,
            Kind::Paragraph,
            None,
            ensure_empty && !rendered,
        )?;
        Ok(())
    }

    fn render_block(&mut self, element: &EnmlElement, parent: NodeId) -> Result<bool, EnmlError> {
        if element.hidden() {
            return Ok(false);
        }
        match element.name.as_str() {
            "table" => self.render_table(element, parent),
            "en-crypt" => {
                self.document
                    .insert(
                        Parent::Node(parent),
                        Node::paragraph(ENCRYPTED_PLACEHOLDER).map_err(build_error)?,
                    )
                    .map_err(build_error)?;
                Ok(true)
            }
            "en-media" => self.render_media(element, parent),
            "p" if element.children.iter().any(
                |child| matches!(child, EnmlNode::Element(child) if child.name == "en-media"),
            ) =>
            {
                self.render_flow(&element.children, parent, true)?;
                Ok(true)
            }
            "p" => self.flush_inline(&element.children, parent, Kind::Paragraph, None, true),
            "div" if element.style_flag("--en-callout") => self.render_callout(element, parent),
            "div" => {
                self.render_flow(&element.children, parent, true)?;
                Ok(true)
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let level = element.name.as_bytes()[1] - b'0';
                self.flush_inline(
                    &element.children,
                    parent,
                    Kind::Paragraph,
                    Heading::from_level(i64::from(level)),
                    true,
                )
            }
            "blockquote" => self.flush_inline(&element.children, parent, Kind::Quote, None, true),
            "pre" | "code" => {
                let mut text = String::new();
                collect_code_text(&element.children, &mut text);
                self.document
                    .insert(Parent::Node(parent), Node::code(text).map_err(build_error)?)
                    .map_err(build_error)?;
                Ok(true)
            }
            "ul" => self.render_list(element, parent, ListStyle::Bullet),
            "ol" => self.render_list(element, parent, ListStyle::Numbered),
            "hr" => {
                self.document
                    .insert(Parent::Node(parent), Node::divider().map_err(build_error)?)
                    .map_err(build_error)?;
                Ok(true)
            }
            _ if element.has_element_child() => {
                self.render_flow(&element.children, parent, false)?;
                Ok(true)
            }
            _ => {
                if textual_content(element).trim().is_empty() {
                    return Ok(false);
                }
                self.warn_unknown(&element.name);
                self.flush_inline(&element.children, parent, Kind::Paragraph, None, false)
            }
        }
    }

    /// An Evernote callout — `<div style="--en-callout:true; --en-emoji:🤖">`
    /// around its blocks — is a Koru callout box at the div's slot, carrying
    /// the emoji (absent means Evernote's 💡, which is also Koru's default),
    /// with everything inside the div — paragraphs, lists, tables, headings
    /// — rendered under it in the order it was written. A callout with
    /// nothing in it gets an empty line, as any empty flow does, so the box
    /// is never a node no row shows.
    fn render_callout(&mut self, element: &EnmlElement, parent: NodeId) -> Result<bool, EnmlError> {
        let icon = element
            .style_value("--en-emoji")
            .map(|value| value.trim_matches(['"', '\'']).trim())
            .filter(|value| !value.is_empty());
        let callout = self
            .document
            .insert(
                Parent::Node(parent),
                Node::callout(icon, None).map_err(build_error)?,
            )
            .map_err(build_error)?;
        self.render_flow(&element.children, callout, true)?;
        Ok(true)
    }

    fn render_table(&mut self, table: &EnmlElement, parent: NodeId) -> Result<bool, EnmlError> {
        let rows = table_rows(table);
        if rows.is_empty() {
            return Ok(false);
        }
        let merged = rows
            .iter()
            .flatten()
            .any(|cell| attribute_span(cell, "rowspan") > 1 || attribute_span(cell, "colspan") > 1);
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        if width == 0 {
            return Ok(false);
        }
        let mut rendered = rows
            .iter()
            .map(|row| {
                let mut cells = row
                    .iter()
                    .map(|cell| gfm_cell_text(cell))
                    .collect::<Vec<_>>();
                cells.resize(width, String::new());
                format!("| {} |", cells.join(" | "))
            })
            .collect::<Vec<_>>();
        rendered.insert(1, format!("| {} |", vec!["---"; width].join(" | ")));
        self.document
            .insert(
                Parent::Node(parent),
                Node::table(rendered.join("\n")).map_err(build_error)?,
            )
            .map_err(build_error)?;
        if merged {
            self.warn_once(
                ImportWarningCode::EvernoteMergedTableCells,
                "Evernote merged table cells were rectangularized",
            );
        }
        Ok(true)
    }

    fn render_media(&mut self, element: &EnmlElement, parent: NodeId) -> Result<bool, EnmlError> {
        let Some(hash) = element
            .attribute("hash")
            .map(|hash| hash.trim().to_ascii_lowercase())
        else {
            return self.render_missing(parent);
        };
        let Some(resource) = self
            .resources
            .iter()
            .find(|resource| resource.md5.as_deref() == Some(hash.as_str()))
            .cloned()
        else {
            return self.render_missing(parent);
        };
        self.used_resources.insert(resource.ordinal);
        if let Some(media_type) = element.attribute("type")
            && !media_type.trim().eq_ignore_ascii_case(&resource.mime)
        {
            self.warn_resource(
                ImportWarningCode::EvernoteMimeMismatch,
                "Evernote media MIME did not match its resource",
            );
        }
        self.render_resource(&resource, parent)?;
        Ok(true)
    }

    fn render_missing(&mut self, parent: NodeId) -> Result<bool, EnmlError> {
        self.document
            .insert(
                Parent::Node(parent),
                Node::paragraph("Missing attachment").map_err(build_error)?,
            )
            .map_err(build_error)?;
        self.warn_resource(
            ImportWarningCode::EvernoteMissingResource,
            "Evernote media referenced a missing resource",
        );
        Ok(true)
    }

    fn render_resource(
        &mut self,
        resource: &NoteResource,
        parent: NodeId,
    ) -> Result<NodeId, EnmlError> {
        let Some(spool) = resource.spool else {
            self.render_missing(parent)?;
            return Ok(parent);
        };
        let filename = resource.filename.clone().unwrap_or_else(|| {
            generated_filename(self.note_ordinal, resource.ordinal, &resource.mime)
        });
        if resource.mime.starts_with("image/") && !resource.attachment {
            let node = self
                .document
                .insert(
                    Parent::Node(parent),
                    Node::image(
                        pending_hash(spool),
                        resource.mime.clone(),
                        resource.width,
                        resource.height,
                    )
                    .map_err(build_error)?,
                )
                .map_err(build_error)?;
            let _ = self.document.set_name(node, Some(&filename));
            self.bindings.push(PendingResourceBinding {
                document: self.document_id,
                node,
                spool,
                mime: resource.mime.clone(),
                filename: Some(filename),
            });
            return Ok(node);
        }

        let (file_id, root_id, label) = if let Some(existing) =
            self.file_by_resource.get(&resource.ordinal)
        {
            existing.clone()
        } else {
            let file_id = StoreDocumentId::new();
            let root_id = NodeId::from_uuid(file_id.as_uuid());
            let mut file = WorkspaceDoc::create(self.workspace, &filename).map_err(build_error)?;
            let mut root = Node::file(&filename, pending_hash(spool), resource.mime.clone())
                .map_err(build_error)?;
            root.id = root_id;
            file.insert(Parent::Root, root).map_err(build_error)?;
            self.files.push(ImportedDocument {
                id: file_id,
                source: self.source.clone(),
                title: filename.clone(),
                kind: koru_core::explore::KIND_FILE.to_owned(),
                mime: Some(resource.mime.clone()),
                parent: None,
                trashed: false,
                trashed_at: None,
                snapshot: file.export_snapshot().map_err(build_error)?,
            });
            self.bindings.push(PendingResourceBinding {
                document: file_id,
                node: root_id,
                spool,
                mime: resource.mime.clone(),
                filename: Some(filename.clone()),
            });
            self.file_by_resource
                .insert(resource.ordinal, (file_id, root_id, filename.clone()));
            (file_id, root_id, filename)
        };
        let paragraph = self
            .document
            .insert(
                Parent::Node(parent),
                Node::paragraph("").map_err(build_error)?,
            )
            .map_err(build_error)?;
        let target = Ref::in_doc(
            root_id,
            DocRef::Document(koru_core::DocumentId::from_uuid(file_id.as_uuid())),
        );
        let attr = Attr::labelled("attachment", AttrValue::Ref(target), label);
        self.document
            .insert_pill(paragraph, 0, &attr)
            .map_err(build_error)?;
        Ok(paragraph)
    }

    fn render_unreferenced(&mut self, parent: NodeId) -> Result<(), EnmlError> {
        let unreferenced = self
            .resources
            .iter()
            .filter(|resource| !self.used_resources.contains(&resource.ordinal))
            .cloned()
            .collect::<Vec<_>>();
        if unreferenced.is_empty() {
            return Ok(());
        }
        let heading = self
            .document
            .insert(
                Parent::Node(parent),
                Node::paragraph("Attachments").map_err(build_error)?,
            )
            .map_err(build_error)?;
        self.document
            .set_heading(heading, Heading::from_level(2))
            .map_err(build_error)?;
        for resource in unreferenced {
            self.used_resources.insert(resource.ordinal);
            self.render_resource(&resource, parent)?;
        }
        Ok(())
    }

    fn warn_resource(&mut self, code: ImportWarningCode, message: &str) {
        self.warnings.push(ImportWarning {
            source: self.source.clone(),
            code: Some(code),
            arguments: Vec::new(),
            message: message.to_owned(),
        });
    }

    fn render_list(
        &mut self,
        element: &EnmlElement,
        parent: NodeId,
        style: ListStyle,
    ) -> Result<bool, EnmlError> {
        let block = self
            .document
            .insert(
                Parent::Node(parent),
                Node::list_block().map_err(build_error)?,
            )
            .map_err(build_error)?;
        self.document
            .set_list_style(block, Some(style))
            .map_err(build_error)?;

        let mut items = 0usize;
        for child in &element.children {
            let EnmlNode::Element(item) = child else {
                continue;
            };
            if item.name != "li" || item.hidden() {
                continue;
            }
            let mut inline = Vec::new();
            let mut nested = Vec::new();
            for child in &item.children {
                if let EnmlNode::Element(element) = child
                    && matches!(element.name.as_str(), "ul" | "ol")
                {
                    nested.push(element);
                } else {
                    inline.push(child.clone());
                }
            }
            let segments = self.inline_segments(&inline);
            let segments = if segments.is_empty() {
                vec![InlineBuffer::default()]
            } else {
                segments
            };
            let mut nested_parent = None;
            for segment in segments {
                let item_id = self.insert_text_node(block, Kind::Paragraph, None, segment)?;
                nested_parent = Some(item_id);
                items += 1;
            }
            let nested_parent = nested_parent.expect("a list item always creates a paragraph");
            for list in nested {
                self.render_list(
                    list,
                    nested_parent,
                    if list.name == "ol" {
                        ListStyle::Numbered
                    } else {
                        ListStyle::Bullet
                    },
                )?;
            }
        }

        if items == 0 {
            // Empty list containers are not useful document rows.
            self.document.delete(block).map_err(build_error)?;
            Ok(false)
        } else {
            Ok(true)
        }
    }

    fn flush_inline(
        &mut self,
        nodes: &[EnmlNode],
        parent: NodeId,
        kind: Kind,
        heading: Option<Heading>,
        ensure_empty: bool,
    ) -> Result<bool, EnmlError> {
        if !ensure_empty && !nodes_are_meaningful(nodes) {
            return Ok(false);
        }
        if let Some((todo_at, done)) = leading_todo(nodes) {
            let block = self
                .document
                .insert(
                    Parent::Node(parent),
                    Node::list_block().map_err(build_error)?,
                )
                .map_err(build_error)?;
            self.document
                .set_list_style(block, Some(ListStyle::Checkbox))
                .map_err(build_error)?;

            // Leading whitespace is layout around Evernote's control, not
            // note content. Everything after the control still goes through
            // the ordinary inline converter, preserving Unicode, marks and
            // links. A `<br>` creates another checkbox item in the same block.
            let mut segments = self.inline_segments(&nodes[todo_at + 1..]);
            if segments.is_empty() {
                segments.push(InlineBuffer::default());
            }
            for segment in segments {
                let item = self.insert_text_node(block, Kind::Paragraph, heading, segment)?;
                self.document.set_done(item, done).map_err(build_error)?;
            }
            return Ok(true);
        }
        let mut segments = self.inline_segments(nodes);
        if segments.is_empty() && ensure_empty {
            segments.push(InlineBuffer::default());
        }
        let rendered = !segments.is_empty();
        for segment in segments {
            self.insert_text_node(parent, kind, heading, segment)?;
        }
        Ok(rendered)
    }

    fn inline_segments(&mut self, nodes: &[EnmlNode]) -> Vec<InlineBuffer> {
        let mut segments = vec![InlineBuffer::default()];
        let state = InlineState::default();
        for node in nodes {
            self.collect_inline(node, &state, &mut segments);
        }
        segments
    }

    fn collect_inline(
        &mut self,
        node: &EnmlNode,
        state: &InlineState,
        segments: &mut Vec<InlineBuffer>,
    ) {
        match node {
            EnmlNode::Text(text) => segments
                .last_mut()
                .expect("inline collection starts with a segment")
                .append(text, state),
            EnmlNode::Element(element) if element.hidden() => {}
            EnmlNode::Element(element) if element.name == "en-crypt" => segments
                .last_mut()
                .expect("inline collection starts with a segment")
                .append(ENCRYPTED_PLACEHOLDER, state),
            EnmlNode::Element(element) if element.name == "en-todo" => {
                let checked = todo_checked(element);
                segments
                    .last_mut()
                    .expect("inline collection starts with a segment")
                    .append(if checked { "☑" } else { "☐" }, state);
                self.warnings.push(ImportWarning {
                    source: self.source.clone(),
                    code: Some(ImportWarningCode::InlineCheckboxSimplified),
                    arguments: Vec::new(),
                    message: "an inline Evernote checkbox was simplified".into(),
                });
            }
            EnmlNode::Element(element) if element.name == "br" => {
                segments.push(InlineBuffer::default());
            }
            EnmlNode::Element(element) => {
                let mut nested = state.clone();
                if let Some(mark) = mark_for(element) {
                    if !nested.marks.contains(&mark) {
                        nested.marks.push(mark);
                    }
                }
                if let Some(highlight) = highlight_for(element) {
                    nested.highlight = Some(highlight);
                }

                let link_segment = segments.len() - 1;
                let link_start = segments[link_segment].text.len();
                for child in &element.children {
                    self.collect_inline(child, &nested, segments);
                }
                if element.name == "a" {
                    self.finish_link(element, segments, link_segment, link_start);
                } else if !known_inline_or_block(&element.name)
                    && !element.has_element_child()
                    && !textual_content(element).trim().is_empty()
                {
                    self.warn_unknown(&element.name);
                }
            }
        }
    }

    fn finish_link(
        &mut self,
        element: &EnmlElement,
        segments: &mut [InlineBuffer],
        first_segment: usize,
        start: usize,
    ) {
        let Some(href) = element.attribute("href").map(str::trim) else {
            return;
        };
        match href_class(href) {
            HrefClass::Allowed => {
                for (index, current) in segments.iter_mut().enumerate().skip(first_segment) {
                    let start = if index == first_segment { start } else { 0 };
                    let end = current.text.len();
                    if start < end {
                        current.links.push(LinkSpan {
                            start,
                            end,
                            target: href.to_owned(),
                        });
                    }
                }
            }
            HrefClass::Unsafe => self.warn_unknown("a"),
            HrefClass::Unsupported => {}
        }
    }

    fn insert_text_node(
        &mut self,
        parent: NodeId,
        kind: Kind,
        heading: Option<Heading>,
        inline: InlineBuffer,
    ) -> Result<NodeId, EnmlError> {
        let node = match kind {
            Kind::Quote => Node::quote(&inline.text),
            _ => Node::paragraph(&inline.text),
        }
        .map_err(build_error)?;
        let id = self
            .document
            .insert(Parent::Node(parent), node)
            .map_err(build_error)?;
        if let Some(heading) = heading {
            self.document
                .set_heading(id, Some(heading))
                .map_err(build_error)?;
        }
        for span in inline.marks {
            self.document
                .set_mark(id, span.start..span.end, span.mark, true)
                .map_err(build_error)?;
        }
        for span in inline.highlights {
            self.document
                .set_highlight(id, span.start..span.end, Some(&span.color))
                .map_err(build_error)?;
        }
        let mut links = inline.links;
        links.sort_by_key(|link| std::cmp::Reverse(link.start));
        for link in links {
            let label = inline
                .text
                .get(link.start..link.end)
                .unwrap_or_default()
                .to_owned();
            self.document
                .delete_text(id, link.start..link.end)
                .map_err(build_error)?;
            let attr = self.link_attr(&link.target, label);
            self.document
                .insert_pill(id, link.start, &attr)
                .map_err(build_error)?;
        }
        Ok(id)
    }

    fn warn_unknown(&mut self, element: &str) {
        self.warnings.push(ImportWarning {
            source: self.source.clone(),
            code: Some(ImportWarningCode::EvernoteUnknownElement),
            arguments: Vec::new(),
            message: format!("unsupported Evernote element <{element}> was simplified"),
        });
    }

    fn warn_once(&mut self, code: ImportWarningCode, message: &str) {
        if self
            .warnings
            .iter()
            .any(|warning| warning.code == Some(code))
        {
            return;
        }
        self.warnings.push(ImportWarning {
            source: self.source.clone(),
            code: Some(code),
            arguments: Vec::new(),
            message: message.to_owned(),
        });
    }

    fn link_attr(&mut self, target: &str, label: String) -> Attr {
        if target
            .split_once(':')
            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("evernote"))
            && let Some(context) = self.links
        {
            let Some(guid) = evernote_link_guid(target) else {
                return Attr::labelled("link", AttrValue::Url(target.to_owned()), label);
            };
            if let Some(document) = context.guids.resolve(context.file_index, &guid) {
                return Attr::labelled(
                    "document",
                    AttrValue::Document(DocumentReference::new(koru_core::DocumentId::from_uuid(
                        document.as_uuid(),
                    ))),
                    label,
                );
            }
            self.warn_once(
                ImportWarningCode::EvernoteLinkAmbiguous,
                "Evernote link target was missing or ambiguous",
            );
        }
        Attr::labelled("link", AttrValue::Url(target.to_owned()), label)
    }
}

fn nodes_are_meaningful(nodes: &[EnmlNode]) -> bool {
    nodes.iter().any(|node| match node {
        EnmlNode::Text(text) => !text.trim().is_empty(),
        EnmlNode::Element(element) => !element.hidden(),
    })
}

/// A checkbox is structural only when nothing visible precedes it in this
/// block. Hidden elements and whitespace-only text do not count as content.
fn leading_todo(nodes: &[EnmlNode]) -> Option<(usize, bool)> {
    for (index, node) in nodes.iter().enumerate() {
        match node {
            EnmlNode::Text(text) if text.trim().is_empty() => continue,
            EnmlNode::Element(element) if element.hidden() => continue,
            EnmlNode::Element(element) if element.name == "en-todo" => {
                return Some((index, todo_checked(element)));
            }
            _ => return None,
        }
    }
    None
}

fn todo_checked(element: &EnmlElement) -> bool {
    element
        .attribute("checked")
        .map(str::trim)
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("true") || value == "1" || value.eq_ignore_ascii_case("yes")
        })
}

fn textual_content(element: &EnmlElement) -> String {
    fn append(nodes: &[EnmlNode], output: &mut String) {
        for node in nodes {
            match node {
                EnmlNode::Text(text) => output.push_str(text),
                EnmlNode::Element(element) if !element.hidden() => {
                    append(&element.children, output)
                }
                EnmlNode::Element(_) => {}
            }
        }
    }
    let mut output = String::new();
    append(&element.children, &mut output);
    output
}

fn collect_code_text(nodes: &[EnmlNode], output: &mut String) {
    for node in nodes {
        match node {
            EnmlNode::Text(text) => output.push_str(text),
            EnmlNode::Element(element) if element.hidden() => {}
            EnmlNode::Element(element) if element.name == "br" => output.push('\n'),
            EnmlNode::Element(element) if element.name == "en-crypt" => {
                output.push_str(ENCRYPTED_PLACEHOLDER)
            }
            EnmlNode::Element(element) => collect_code_text(&element.children, output),
        }
    }
}

fn is_block(element: &EnmlElement) -> bool {
    match element.name.as_str() {
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "pre" | "ul" | "ol"
        | "hr" | "en-media" | "table" | "en-crypt" => true,
        "code" => true,
        "div" => !element.style_value("display").is_some_and(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "inline" | "inline-block"
            )
        }),
        _ if !known_inline_or_block(&element.name) => element
            .children
            .iter()
            .any(|child| matches!(child, EnmlNode::Element(child) if is_block(child))),
        _ => false,
    }
}

fn known_inline_or_block(name: &str) -> bool {
    matches!(
        name,
        "en-note"
            | "p"
            | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "br"
            | "ul"
            | "ol"
            | "li"
            | "blockquote"
            | "pre"
            | "hr"
            | "strong"
            | "b"
            | "em"
            | "i"
            | "u"
            | "s"
            | "strike"
            | "del"
            | "code"
            | "a"
            | "span"
            | "font"
            | "center"
            | "en-media"
            | "table"
            | "thead"
            | "tbody"
            | "tfoot"
            | "tr"
            | "th"
            | "td"
            | "en-crypt"
    )
}

fn table_rows(table: &EnmlElement) -> Vec<Vec<&EnmlElement>> {
    fn collect<'a>(element: &'a EnmlElement, rows: &mut Vec<Vec<&'a EnmlElement>>) {
        for child in &element.children {
            let EnmlNode::Element(child) = child else {
                continue;
            };
            match child.name.as_str() {
                "tr" => rows.push(
                    child
                        .children
                        .iter()
                        .filter_map(|cell| match cell {
                            EnmlNode::Element(cell)
                                if matches!(cell.name.as_str(), "td" | "th") =>
                            {
                                Some(cell)
                            }
                            _ => None,
                        })
                        .collect(),
                ),
                "thead" | "tbody" | "tfoot" => collect(child, rows),
                _ => {}
            }
        }
    }
    let mut rows = Vec::new();
    collect(table, &mut rows);
    rows
}

fn attribute_span(cell: &EnmlElement, name: &str) -> usize {
    cell.attribute(name)
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(1)
}

fn gfm_cell_text(cell: &EnmlElement) -> String {
    fn append(nodes: &[EnmlNode], output: &mut String) {
        for node in nodes {
            match node {
                EnmlNode::Text(text) => output.push_str(text),
                EnmlNode::Element(element) if element.hidden() => {}
                EnmlNode::Element(element) if element.name == "br" => output.push_str("<br>"),
                EnmlNode::Element(element) if element.name == "en-crypt" => {
                    output.push_str(ENCRYPTED_PLACEHOLDER)
                }
                EnmlNode::Element(element) => append(&element.children, output),
            }
        }
    }
    let mut text = String::new();
    append(&cell.children, &mut text);
    text.trim()
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace("\r\n", "<br>")
        .replace(['\r', '\n'], "<br>")
}

fn contains_modern_task_payload(element: &EnmlElement) -> bool {
    let name = element.name.to_ascii_lowercase();
    if name == "task" || name.starts_with("en-task") {
        return true;
    }
    if element.attributes.iter().any(|(key, value)| {
        let key = key.to_ascii_lowercase();
        let value = value.to_ascii_lowercase();
        key.contains("evernote-task")
            || key.starts_with("data-task")
            || value.contains("evernote-task")
            || value.contains("en-task")
    }) {
        return true;
    }
    element.children.iter().any(|child| match child {
        EnmlNode::Element(child) => contains_modern_task_payload(child),
        EnmlNode::Text(_) => false,
    })
}

fn evernote_link_guid(target: &str) -> Option<String> {
    let mut candidates = target
        .split(|character: char| matches!(character, '/' | '?' | '#' | '&' | '='))
        .map(str::trim)
        .filter(|segment| is_evernote_guid(segment))
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.dedup();
    (candidates.len() == 1).then(|| candidates.remove(0))
}

fn is_evernote_guid(value: &str) -> bool {
    let groups = [8, 4, 4, 4, 12];
    let parts = value.split('-').collect::<Vec<_>>();
    parts.len() == groups.len()
        && parts.iter().zip(groups).all(|(part, width)| {
            part.len() == width && part.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn pending_hash(spool: usize) -> BlobHash {
    BlobHash(format!("pending-evernote-resource-{spool}"))
}

fn generated_filename(note: usize, resource: usize, mime: &str) -> String {
    let extension = extension_for_mime(mime);
    format!("attachment-{}-{}.{}", note + 1, resource + 1, extension)
}

fn extension_for_mime(mime: &str) -> &'static str {
    match mime.to_ascii_lowercase().as_str() {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "text/plain" => "txt",
        "text/html" => "html",
        "application/zip" => "zip",
        _ => "bin",
    }
}

fn mark_for(element: &EnmlElement) -> Option<Mark> {
    match element.name.as_str() {
        "strong" | "b" => Some(Mark::Bold),
        "em" | "i" => Some(Mark::Italic),
        "u" => Some(Mark::Underline),
        "s" | "strike" | "del" => Some(Mark::Strike),
        "code" => Some(Mark::Code),
        _ => None,
    }
}

fn highlight_for(element: &EnmlElement) -> Option<String> {
    let raw = element
        .style_value("background-color")
        .or_else(|| element.style_value("background"))
        .or_else(|| element.attribute("bgcolor"))?;
    let normalized = raw
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect::<String>();
    let token = match normalized.as_str() {
        "yellow" | "#ffff00" | "#fff2cc" | "#fff8b8" | "rgb(255,255,0)" => "yellow",
        "green" | "lime" | "#008000" | "#00ff00" | "#d9ead3" | "rgb(0,255,0)" => "green",
        "blue" | "#0000ff" | "#cfe2f3" | "rgb(0,0,255)" => "blue",
        "pink" | "fuchsia" | "magenta" | "#ffc0cb" | "#ff00ff" | "#ead1dc" | "rgb(255,0,255)" => {
            "pink"
        }
        _ => return None,
    };
    Some(token.to_owned())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HrefClass {
    Allowed,
    Unsafe,
    Unsupported,
}

fn href_class(href: &str) -> HrefClass {
    let Some((scheme, _)) = href.split_once(':') else {
        return HrefClass::Unsupported;
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "mailto" | "tel" | "evernote" => HrefClass::Allowed,
        "javascript" | "data" | "file" | "vbscript" => HrefClass::Unsafe,
        _ => HrefClass::Unsupported,
    }
}

#[cfg(test)]
mod tests;
