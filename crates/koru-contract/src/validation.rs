// SPDX-License-Identifier: MPL-2.0

use std::collections::{HashMap, HashSet};

use prost::Message;
use uuid::Uuid;

use crate::proto::{
    BlobReference, BlockEnvelope, BlockKind, DocumentCommandEnvelope, DocumentProjection,
    InlineObject, RichText, TextRange, document_command_envelope,
};

const OBJECT_REPLACEMENT: char = '\u{fffc}';

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationError {
    Missing(&'static str),
    InvalidUuid(&'static str),
    DuplicateBlock,
    MissingParent,
    ParentAfterChild,
    Cycle,
    InvalidPayload,
    InvalidTextRange,
    InvalidInlineAnchor,
    UnsupportedCommand,
}

pub fn validate_document(document: &DocumentProjection) -> Result<(), ValidationError> {
    validate_uuid(
        document.id.as_ref().map(|id| id.value.as_str()),
        "document_id",
    )?;
    validate_uuid(
        document.workspace_id.as_ref().map(|id| id.value.as_str()),
        "workspace_id",
    )?;

    if document.title.is_empty() {
        return Err(ValidationError::Missing("document_title"));
    }

    let mut seen = HashSet::new();
    for block in &document.blocks {
        let id = block
            .id
            .as_ref()
            .ok_or(ValidationError::Missing("block_id"))?;
        validate_uuid(Some(&id.value), "block_id")?;
        if !seen.insert(id.value.clone()) {
            return Err(ValidationError::DuplicateBlock);
        }
    }

    let mut parents = HashMap::new();
    for block in &document.blocks {
        let id = block.id.as_ref().unwrap();
        if let Some(parent) = &block.parent_id {
            validate_uuid(Some(&parent.value), "parent_id")?;
            if !seen.contains(&parent.value) {
                return Err(ValidationError::MissingParent);
            }
            parents.insert(id.value.clone(), parent.value.clone());
        }
        validate_block(block)?;
    }

    for mut id in parents.keys().map(String::as_str) {
        let mut path = HashSet::new();
        while let Some(parent) = parents.get(id).map(String::as_str) {
            if !path.insert(parent) {
                return Err(ValidationError::Cycle);
            }
            id = parent;
        }
    }

    let mut emitted = HashSet::new();
    for block in &document.blocks {
        if let Some(parent) = &block.parent_id
            && !emitted.contains(parent.value.as_str())
        {
            return Err(ValidationError::ParentAfterChild);
        }
        emitted.insert(block.id.as_ref().unwrap().value.as_str());
    }
    Ok(())
}

pub fn validate_command(command: &DocumentCommandEnvelope) -> Result<(), ValidationError> {
    validate_uuid(
        command.document_id.as_ref().map(|id| id.value.as_str()),
        "document_id",
    )?;
    validate_uuid(Some(&command.operation_id), "operation_id")?;
    match command.command.as_ref() {
        Some(document_command_envelope::Command::CreateBlock(create)) => {
            validate_block(
                create
                    .block
                    .as_ref()
                    .ok_or(ValidationError::Missing("block"))?,
            )?;
        }
        Some(document_command_envelope::Command::DeleteBlock(delete)) => {
            validate_uuid(
                delete.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
        }
        Some(document_command_envelope::Command::MoveBlock(moved)) => {
            validate_uuid(
                moved.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            if let Some(parent) = &moved.parent_id {
                validate_uuid(Some(&parent.value), "parent_id")?;
            }
        }
        Some(document_command_envelope::Command::IndentBlock(indent)) => {
            validate_uuid(
                indent.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
        }
        Some(document_command_envelope::Command::OutdentBlock(outdent)) => {
            validate_uuid(
                outdent.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
        }
        Some(document_command_envelope::Command::InsertText(insert)) => {
            validate_uuid(
                insert.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            if insert.text.is_empty() {
                return Err(ValidationError::Missing("text"));
            } else if insert.text.contains(OBJECT_REPLACEMENT) {
                return Err(ValidationError::InvalidInlineAnchor);
            }
        }
        Some(document_command_envelope::Command::DeleteText(delete)) => {
            validate_uuid(
                delete.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            validate_range_shape(delete.range.as_ref())?;
        }
        Some(document_command_envelope::Command::AddTextMark(add)) => {
            validate_uuid(
                add.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            let mark = add
                .mark
                .as_ref()
                .ok_or(ValidationError::Missing("text_mark"))?;
            validate_range_shape(mark.range.as_ref())?;
            if mark.kind.is_empty() {
                return Err(ValidationError::Missing("mark_kind"));
            }
        }
        Some(document_command_envelope::Command::RemoveTextMark(remove)) => {
            validate_uuid(
                remove.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            validate_range_shape(remove.range.as_ref())?;
            if remove.kind.is_empty() {
                return Err(ValidationError::Missing("mark_kind"));
            }
        }
        Some(document_command_envelope::Command::SetBlockProperty(set)) => {
            validate_uuid(
                set.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            if set.property.is_none() {
                return Err(ValidationError::Missing("block_property"));
            }
        }
        Some(document_command_envelope::Command::ToggleTask(toggle)) => {
            validate_uuid(
                toggle.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
        }
        Some(document_command_envelope::Command::InsertInlineObject(insert)) => {
            validate_uuid(
                insert.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            validate_inline_object(insert.object.as_ref())?;
        }
        Some(document_command_envelope::Command::RemoveInlineObject(remove)) => {
            validate_uuid(
                remove.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            validate_uuid(
                remove.object_id.as_ref().map(|id| id.value.as_str()),
                "inline_object_id",
            )?;
        }
        Some(document_command_envelope::Command::AttachBlob(attach)) => {
            validate_uuid(
                attach.block_id.as_ref().map(|id| id.value.as_str()),
                "block_id",
            )?;
            validate_blob(attach.blob.as_ref())?;
        }
        Some(document_command_envelope::Command::RenameDocument(rename)) => {
            if rename.title.is_empty() {
                return Err(ValidationError::Missing("document_title"));
            }
        }
        None => return Err(ValidationError::UnsupportedCommand),
    }
    Ok(())
}

fn validate_block(block: &BlockEnvelope) -> Result<(), ValidationError> {
    validate_uuid(block.id.as_ref().map(|id| id.value.as_str()), "block_id")?;
    if let Some(parent) = &block.parent_id {
        validate_uuid(Some(&parent.value), "parent_id")?;
    }
    let payload = block
        .payload
        .as_ref()
        .ok_or(ValidationError::Missing("block_payload"))?;
    if payload.type_url.is_empty() {
        return Err(ValidationError::InvalidPayload);
    }

    let kind = BlockKind::try_from(block.kind).unwrap_or(BlockKind::Unspecified);
    let expected = match kind {
        BlockKind::Paragraph => Some("koru.v1.ParagraphPayload"),
        BlockKind::Task => Some("koru.v1.TaskPayload"),
        BlockKind::List => Some("koru.v1.ListPayload"),
        BlockKind::Image => Some("koru.v1.ImagePayload"),
        BlockKind::Code => Some("koru.v1.CodePayload"),
        BlockKind::Table => Some("koru.v1.TablePayload"),
        BlockKind::Divider => Some("koru.v1.DividerPayload"),
        BlockKind::Embed => Some("koru.v1.EmbedPayload"),
        BlockKind::Contact => Some("koru.v1.ContactPayload"),
        BlockKind::Callout => Some("koru.v1.CalloutPayload"),
        BlockKind::Canvas | BlockKind::Unspecified => None,
    };
    if let Some(expected) = expected
        && !payload.type_url.ends_with(expected)
    {
        return Err(ValidationError::InvalidPayload);
    }

    match kind {
        BlockKind::Paragraph => validate_rich_text(
            crate::proto::ParagraphPayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?
                .rich_text
                .as_ref(),
        )?,
        BlockKind::Task => validate_rich_text(
            crate::proto::TaskPayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?
                .rich_text
                .as_ref(),
        )?,
        BlockKind::Code => validate_rich_text(
            crate::proto::CodePayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?
                .rich_text
                .as_ref(),
        )?,
        BlockKind::Table => validate_rich_text(
            crate::proto::TablePayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?
                .rich_text
                .as_ref(),
        )?,
        BlockKind::Contact => validate_rich_text(
            crate::proto::ContactPayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?
                .name
                .as_ref(),
        )?,
        BlockKind::Image => validate_blob(
            crate::proto::ImagePayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?
                .blob
                .as_ref(),
        )?,
        BlockKind::List => {
            crate::proto::ListPayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?;
        }
        BlockKind::Divider => {
            crate::proto::DividerPayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?;
        }
        BlockKind::Callout => {
            crate::proto::CalloutPayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?;
        }
        BlockKind::Embed => {
            let embed = crate::proto::EmbedPayload::decode(payload.value.as_slice())
                .map_err(|_| ValidationError::InvalidPayload)?;
            if embed.target.and_then(|target| target.target).is_none() {
                return Err(ValidationError::Missing("embed_target"));
            }
        }
        BlockKind::Canvas | BlockKind::Unspecified => {}
    }
    Ok(())
}

fn validate_rich_text(rich: Option<&RichText>) -> Result<(), ValidationError> {
    let rich = rich.ok_or(ValidationError::Missing("rich_text"))?;
    for mark in &rich.marks {
        validate_range(&rich.text, mark.range.as_ref())?;
        if mark.kind.is_empty() {
            return Err(ValidationError::Missing("mark_kind"));
        }
    }

    let anchors: HashSet<usize> = rich
        .text
        .char_indices()
        .filter_map(|(offset, ch)| (ch == OBJECT_REPLACEMENT).then_some(offset))
        .collect();
    let mut claimed = HashSet::new();
    for object in &rich.inline_objects {
        validate_inline_object(Some(object))?;
        let at = object.anchor_utf8 as usize;
        if !anchors.contains(&at) || !claimed.insert(at) {
            return Err(ValidationError::InvalidInlineAnchor);
        }
    }
    if anchors != claimed {
        return Err(ValidationError::InvalidInlineAnchor);
    }
    Ok(())
}

fn validate_inline_object(object: Option<&InlineObject>) -> Result<(), ValidationError> {
    let object = object.ok_or(ValidationError::Missing("inline_object"))?;
    validate_uuid(
        object.id.as_ref().map(|id| id.value.as_str()),
        "inline_object_id",
    )?;
    if object.role.is_empty() {
        return Err(ValidationError::Missing("inline_object_role"));
    }
    if object
        .reference
        .as_ref()
        .and_then(|reference| reference.target.as_ref())
        .is_none()
    {
        return Err(ValidationError::Missing("inline_object_reference"));
    }
    Ok(())
}

fn validate_blob(blob: Option<&BlobReference>) -> Result<(), ValidationError> {
    let blob = blob.ok_or(ValidationError::Missing("blob"))?;
    let id = blob
        .id
        .as_ref()
        .ok_or(ValidationError::Missing("blob_id"))?;
    if id.value.is_empty() {
        return Err(ValidationError::Missing("blob_id"));
    }
    if blob.mime_type.is_empty() {
        return Err(ValidationError::Missing("blob_mime_type"));
    }
    Ok(())
}

fn validate_range(text: &str, range: Option<&TextRange>) -> Result<(), ValidationError> {
    let range = validate_range_shape(range)?;
    let start = range.start_utf8 as usize;
    let end = range.end_utf8 as usize;
    if end > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(end) {
        return Err(ValidationError::InvalidTextRange);
    }
    if text[start..end].contains(OBJECT_REPLACEMENT) {
        return Err(ValidationError::InvalidInlineAnchor);
    }
    Ok(())
}

fn validate_range_shape(range: Option<&TextRange>) -> Result<&TextRange, ValidationError> {
    let range = range.ok_or(ValidationError::Missing("text_range"))?;
    if range.start_utf8 > range.end_utf8 {
        return Err(ValidationError::InvalidTextRange);
    }
    Ok(range)
}

fn validate_uuid(value: Option<&str>, field: &'static str) -> Result<(), ValidationError> {
    let value = value.ok_or(ValidationError::Missing(field))?;
    Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| ValidationError::InvalidUuid(field))
}

#[cfg(test)]
mod tests {
    use prost::Message;
    use prost_types::Any;

    use super::*;
    use crate::proto::{BlockId, DocumentId, ParagraphPayload, RichText, WorkspaceId};

    const DOC: &str = "10000000-0000-4000-8000-000000000001";
    const WORKSPACE: &str = "10000000-0000-4000-8000-000000000002";
    const BLOCK: &str = "10000000-0000-4000-8000-000000000003";

    fn paragraph() -> DocumentProjection {
        let payload = ParagraphPayload {
            rich_text: Some(RichText {
                text: "Hello مرحبا 👋".into(),
                marks: Vec::new(),
                inline_objects: Vec::new(),
            }),
        };
        DocumentProjection {
            id: Some(DocumentId { value: DOC.into() }),
            workspace_id: Some(WorkspaceId {
                value: WORKSPACE.into(),
            }),
            title: "Fixture".into(),
            revision: 1,
            blocks: vec![BlockEnvelope {
                id: Some(BlockId {
                    value: BLOCK.into(),
                }),
                parent_id: None,
                kind: BlockKind::Paragraph as i32,
                properties: None,
                payload: Some(Any {
                    type_url: "type.googleapis.com/koru.v1.ParagraphPayload".into(),
                    value: payload.encode_to_vec(),
                }),
            }],
        }
    }

    #[test]
    fn valid_multilingual_paragraph_passes() {
        assert_eq!(validate_document(&paragraph()), Ok(()));
    }

    #[test]
    fn unknown_block_payload_is_accepted_and_retained() {
        let mut document = paragraph();
        let block = &mut document.blocks[0];
        block.kind = 999;
        block.payload = Some(Any {
            type_url: "type.koru.do/koru.v9.FutureBlock".into(),
            value: vec![0x08, 0x96, 0x01],
        });
        let bytes = document.encode_to_vec();
        let decoded = DocumentProjection::decode(bytes.as_slice()).unwrap();
        assert_eq!(validate_document(&decoded), Ok(()));
        assert_eq!(
            decoded.blocks[0].payload.as_ref().unwrap().value,
            vec![0x08, 0x96, 0x01]
        );
    }

    #[test]
    fn utf8_ranges_must_land_on_scalar_boundaries() {
        let mut document = paragraph();
        let payload = document.blocks[0].payload.as_mut().unwrap();
        let mut paragraph = ParagraphPayload::decode(payload.value.as_slice()).unwrap();
        paragraph
            .rich_text
            .as_mut()
            .unwrap()
            .marks
            .push(crate::proto::TextMark {
                range: Some(TextRange {
                    start_utf8: 7,
                    end_utf8: 8,
                }),
                kind: "bold".into(),
                value: Vec::new(),
            });
        payload.value = paragraph.encode_to_vec();
        assert_eq!(
            validate_document(&document),
            Err(ValidationError::InvalidTextRange)
        );
    }

    #[test]
    fn absent_or_future_command_is_rejected_without_guessing() {
        let command = DocumentCommandEnvelope {
            document_id: Some(DocumentId { value: DOC.into() }),
            operation_id: "10000000-0000-4000-8000-000000000099".into(),
            command: None,
        };
        assert_eq!(
            validate_command(&command),
            Err(ValidationError::UnsupportedCommand)
        );
    }
}
