// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.contract.v1.BlockEnvelope
import app.koru.contract.v1.BlockKind
import app.koru.contract.v1.CalloutPayload
import app.koru.contract.v1.CodePayload
import app.koru.contract.v1.ContactPayload
import app.koru.contract.v1.DividerPayload
import app.koru.contract.v1.DocumentProjection
import app.koru.contract.v1.EmbedPayload
import app.koru.contract.v1.EntityReference as ContractEntityReference
import app.koru.contract.v1.ImagePayload
import app.koru.contract.v1.ParagraphPayload
import app.koru.contract.v1.RichText as ContractRichText
import app.koru.contract.v1.TaskPayload
import app.koru.contract.v1.TablePayload
import app.koru.mobile.domain.*
import java.util.UUID

/** The only place generated document DTOs become long-lived Android domain state. */
object ContractMapper {
    fun document(value: DocumentProjection): DocumentSnapshot {
        requireUuid(value.takeIf { it.hasId() }?.id?.value, "document_id")
        requireUuid(value.takeIf { it.hasWorkspaceId() }?.workspaceId?.value, "workspace_id")
        requireValue(value.title, "document_title")

        val allIds = value.blocksList.map { block ->
            requireUuid(block.takeIf { it.hasId() }?.id?.value, "block_id")
        }
        if (allIds.toSet().size != allIds.size) invalid("duplicate_block")
        val emitted = mutableSetOf<String>()
        value.blocksList.forEach { block ->
            if (block.hasParentId()) {
                val parent = requireUuid(block.parentId.value, "parent_id")
                if (parent !in allIds) invalid("missing_parent")
                if (parent !in emitted) invalid("parent_after_child")
            }
            emitted += block.id.value
        }

        return DocumentSnapshot(
            id = DocumentId(value.id.value),
            workspaceId = WorkspaceId(value.workspaceId.value),
            title = value.title,
            revision = value.revision,
            blocks = value.blocksList.map(::block),
        )
    }

    private fun block(value: BlockEnvelope): Block {
        if (!value.hasPayload() || value.payload.typeUrl.isBlank()) invalid("block_payload")
        val properties = value.properties.takeIf { value.hasProperties() }
        val mappedProperties = BlockProperties(
            name = properties?.name?.takeIf { properties.hasName() },
            icon = properties?.icon?.takeIf { properties.hasIcon() },
            color = properties?.color?.takeIf { properties.hasColor() },
            done = properties?.done ?: false,
            alignment = when (properties?.alignmentValue) {
                2 -> Alignment.Center
                3 -> Alignment.End
                4 -> Alignment.Justify
                else -> Alignment.Start
            },
            headingLevel = properties?.headingLevel?.toInt()?.takeIf { properties.hasHeadingLevel() },
            listStyle = properties?.listStyleValue?.let(::listStyle),
            imageWidth = properties?.imageSize?.width?.toInt()?.takeIf { properties.hasImageSize() },
            imageHeight = properties?.imageSize?.height?.toInt()?.takeIf { properties.hasImageSize() },
            language = properties?.language?.takeIf { properties.hasLanguage() },
        )
        val payload = value.payload
        val content = when (value.kindValue) {
            BlockKind.BLOCK_KIND_PARAGRAPH_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.ParagraphPayload")
                BlockContent.Paragraph(richText(ParagraphPayload.parseFrom(payload.value).richText))
            }
            BlockKind.BLOCK_KIND_CALLOUT_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.CalloutPayload")
                CalloutPayload.parseFrom(payload.value)
                BlockContent.Callout
            }
            BlockKind.BLOCK_KIND_TASK_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.TaskPayload")
                BlockContent.Task(richText(TaskPayload.parseFrom(payload.value).richText))
            }
            BlockKind.BLOCK_KIND_LIST_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.ListPayload")
                BlockContent.ListContainer(mappedProperties.listStyle ?: ListStyle.Bullet)
            }
            BlockKind.BLOCK_KIND_IMAGE_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.ImagePayload")
                val image = ImagePayload.parseFrom(payload.value)
                if (!image.hasBlob()) invalid("blob")
                BlockContent.Image(blob(image.blob))
            }
            BlockKind.BLOCK_KIND_CODE_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.CodePayload")
                BlockContent.Code(richText(CodePayload.parseFrom(payload.value).richText))
            }
            BlockKind.BLOCK_KIND_TABLE_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.TablePayload")
                BlockContent.Table(richText(TablePayload.parseFrom(payload.value).richText))
            }
            BlockKind.BLOCK_KIND_DIVIDER_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.DividerPayload")
                DividerPayload.parseFrom(payload.value)
                BlockContent.Divider
            }
            BlockKind.BLOCK_KIND_EMBED_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.EmbedPayload")
                val embed = EmbedPayload.parseFrom(payload.value)
                if (!embed.hasTarget()) invalid("embed_target")
                BlockContent.Embed(reference(embed.target))
            }
            BlockKind.BLOCK_KIND_CONTACT_VALUE -> {
                requireType(payload.typeUrl, "koru.v1.ContactPayload")
                BlockContent.Contact(richText(ContactPayload.parseFrom(payload.value).name))
            }
            else -> BlockContent.Unsupported(value.kindValue, payload.typeUrl, payload.value.toByteArray())
        }
        return Block(
            id = BlockId(value.id.value),
            parentId = value.parentId.value.takeIf { value.hasParentId() }?.let(::BlockId),
            properties = mappedProperties,
            content = content,
        )
    }

    private fun richText(value: ContractRichText): RichText {
        val anchors = value.text.mapIndexedNotNull { index, char -> if (char == '\uFFFC') value.text.substring(0, index).encodeToByteArray().size else null }.toSet()
        val objects = value.inlineObjectsList.map { item ->
            requireUuid(item.takeIf { it.hasId() }?.id?.value, "inline_object_id")
            if (item.role.isBlank() || !item.hasReference()) invalid("inline_object")
            InlineObject(
                id = InlineObjectId(item.id.value),
                anchorUtf8 = item.anchorUtf8,
                role = item.role,
                reference = reference(item.reference),
                label = item.label.takeIf { item.hasLabel() },
            )
        }
        if (objects.map { it.anchorUtf8 }.toSet() != anchors || objects.size != anchors.size) invalid("inline_anchor")
        val marks = value.marksList.map { mark ->
            if (!mark.hasRange() || mark.kind.isBlank()) invalid("text_mark")
            validateRange(value.text, mark.range.startUtf8, mark.range.endUtf8)
            val kind = MarkKind.entries.firstOrNull { it != MarkKind.Unknown && it.wireName == mark.kind } ?: MarkKind.Unknown
            TextMark(TextRange(mark.range.startUtf8, mark.range.endUtf8), kind, mark.value.toByteArray(), mark.kind)
        }
        return RichText(value.text, marks, objects)
    }

    private fun reference(value: ContractEntityReference): EntityReference = when (value.targetCase) {
        ContractEntityReference.TargetCase.DOCUMENT_ID -> EntityReference.Document(DocumentId(requireUuid(value.documentId.value, "document_id")))
        ContractEntityReference.TargetCase.BLOCK_ID -> EntityReference.Block(BlockId(requireUuid(value.blockId.value, "block_id")))
        ContractEntityReference.TargetCase.USER_ID -> EntityReference.User(requireUuid(value.userId.value, "user_id"))
        ContractEntityReference.TargetCase.URL -> EntityReference.Url(requireValue(value.url, "url"))
        else -> invalid("inline_reference")
    }

    private fun blob(value: app.koru.contract.v1.BlobReference): BlobReference {
        if (!value.hasId()) invalid("blob_id")
        return BlobReference(
            id = BlobId(requireValue(value.id.value, "blob_id")),
            mimeType = requireValue(value.mimeType, "blob_mime_type"),
            size = value.size,
            width = value.width.takeIf { value.hasWidth() },
            height = value.height.takeIf { value.hasHeight() },
        )
    }

    private fun validateRange(text: String, start: Int, end: Int) {
        if (start < 0 || end < start || end > text.encodeToByteArray().size) invalid("text_range")
        val boundaries = buildSet {
            add(0)
            var bytes = 0
            var index = 0
            while (index < text.length) {
                val codePoint = text.codePointAt(index)
                bytes += String(Character.toChars(codePoint)).encodeToByteArray().size
                index += Character.charCount(codePoint)
                add(bytes)
            }
        }
        if (start !in boundaries || end !in boundaries) invalid("text_range")
    }

    private fun listStyle(value: Int): ListStyle? = when (value) {
        1 -> ListStyle.Bullet
        2 -> ListStyle.Checkbox
        3 -> ListStyle.Numbered
        4 -> ListStyle.Letter
        5 -> ListStyle.Roman
        else -> null
    }

    private fun requireType(typeUrl: String, name: String) {
        if (!typeUrl.endsWith(name)) invalid("block_payload_type")
    }

    private fun requireUuid(value: String?, field: String): String {
        val present = requireValue(value, field)
        try { UUID.fromString(present) } catch (_: IllegalArgumentException) { invalid(field) }
        return present
    }

    private fun requireValue(value: String?, field: String): String = value?.takeIf(String::isNotBlank) ?: invalid(field)
    private fun invalid(field: String): Nothing = throw AppFailure(AppErrorCode.InvalidData, "Invalid contract field: $field")
}
