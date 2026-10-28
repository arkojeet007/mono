// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.domain

import java.util.UUID

@JvmInline value class ProfileId(val value: String) { companion object { fun new() = ProfileId(UUID.randomUUID().toString()) } }
@JvmInline value class WorkspaceId(val value: String) { companion object { fun new() = WorkspaceId(UUID.randomUUID().toString()) } }
@JvmInline value class DocumentId(val value: String) { companion object { fun new() = DocumentId(UUID.randomUUID().toString()) } }
@JvmInline value class BlockId(val value: String) { companion object { fun new() = BlockId(UUID.randomUUID().toString()) } }
@JvmInline value class BlobId(val value: String)
@JvmInline value class InlineObjectId(val value: String) { companion object { fun new() = InlineObjectId(UUID.randomUUID().toString()) } }

data class Profile(
    val id: ProfileId,
    val displayName: String,
    val locale: String,
    val icon: String? = null,
    val leadingEmoji: String? = null,
    val displayLabel: String = displayName,
    val color: String? = null,
    val accountBinding: String = "",
)
data class Workspace(
    val id: WorkspaceId,
    val profileId: ProfileId,
    val title: String,
    val archived: Boolean = false,
    val deletedAtMillis: Long? = null,
    val modifiedAtMillis: Long = 0,
    val icon: String? = null,
    val color: String? = null,
    val leadingEmoji: String? = null,
    val displayTitle: String = title,
)
data class DocumentSummary(
    val id: DocumentId,
    val workspaceId: WorkspaceId,
    val title: String,
    val displayTitle: String = title,
    val leadingEmoji: String? = null,
    val kind: String = "document",
    val mimeType: String? = null,
    /** The file's one note, when it has one. Notes open through the file's
     *  row, never as a row of their own. */
    val noteId: DocumentId? = null,
    /** When this last changed. Zero when nothing recorded it, which sorts
     *  oldest rather than claiming a recency nobody measured. */
    val updatedAtMillis: Long = 0,
)
data class BlobReference(
    val id: BlobId,
    val mimeType: String,
    val size: Long,
    val width: Int? = null,
    val height: Int? = null,
    val workspaceId: WorkspaceId? = null,
)

data class TextRange(val startUtf8: Int, val endUtf8: Int)

enum class MarkKind(val wireName: String) {
    Bold("bold"), Italic("italic"), Underline("underline"), Strike("strike"),
    Code("code"), Superscript("superscript"), Subscript("subscript"),
    Size("size"), Highlight("highlight"), Unknown("unknown"),
}

data class TextMark(
    val range: TextRange,
    val kind: MarkKind,
    val value: ByteArray = byteArrayOf(),
    val wireKind: String = kind.wireName,
) {
    override fun equals(other: Any?) = other is TextMark && range == other.range && kind == other.kind && wireKind == other.wireKind && value.contentEquals(other.value)
    override fun hashCode() = 31 * (31 * (31 * range.hashCode() + kind.hashCode()) + wireKind.hashCode()) + value.contentHashCode()
}

sealed interface EntityReference {
    data class Document(val id: DocumentId) : EntityReference
    /** A delegated run, by its Run document — the pill's target (P1.18). */
    data class Run(val documentId: DocumentId) : EntityReference
    data class Block(val id: BlockId, val documentId: DocumentId? = null) : EntityReference
    data class User(val id: String) : EntityReference
    data class Url(val value: String) : EntityReference
    data class Label(val value: String) : EntityReference
}

data class InlineObject(
    val id: InlineObjectId,
    val anchorUtf8: Int,
    val role: String,
    val reference: EntityReference,
    val label: String?,
)

data class RichText(
    val text: String,
    val marks: List<TextMark> = emptyList(),
    val inlineObjects: List<InlineObject> = emptyList(),
)

enum class Alignment { Start, Center, End, Justify }
enum class ListStyle { Bullet, Checkbox, Numbered, Letter, Roman }

data class BlockProperties(
    val name: String? = null,
    val icon: String? = null,
    val color: String? = null,
    val done: Boolean = false,
    val alignment: Alignment = Alignment.Start,
    val headingLevel: Int? = null,
    val listStyle: ListStyle? = null,
    val imageWidth: Int? = null,
    val imageHeight: Int? = null,
    val language: String? = null,
)

sealed interface BlockContent {
    data class Paragraph(val richText: RichText) : BlockContent
    data class Quote(val richText: RichText) : BlockContent
    /** A callout box. It has no text of its own: its icon and tint are the
     *  block's properties (icon, color), and the lines inside the box are
     *  the blocks whose parentId is this block, as with a list. */
    data object Callout : BlockContent {
        /** The icon a callout shows when none is set — the same default
         *  as koru-core's `callout::DEFAULT_ICON`. */
        const val DEFAULT_ICON = "💡"
    }
    data class Task(val richText: RichText) : BlockContent
    data class ListContainer(val style: ListStyle) : BlockContent
    data class Image(val blob: BlobReference) : BlockContent
    data class Canvas(val blob: BlobReference) : BlockContent
    data class Code(val richText: RichText) : BlockContent
    data class Table(val richText: RichText) : BlockContent
    data object Divider : BlockContent
    data class Embed(val reference: EntityReference) : BlockContent
    data class Contact(val name: RichText) : BlockContent
    data class Unsupported(val kindCode: Int, val typeUrl: String, val payload: ByteArray) : BlockContent {
        override fun equals(other: Any?) = other is Unsupported && kindCode == other.kindCode && typeUrl == other.typeUrl && payload.contentEquals(other.payload)
        override fun hashCode() = 31 * (31 * kindCode + typeUrl.hashCode()) + payload.contentHashCode()
    }
}

data class Block(
    val id: BlockId,
    val parentId: BlockId? = null,
    val properties: BlockProperties = BlockProperties(),
    val content: BlockContent,
)

/** The icon a callout box draws, with the default filled in. */
fun BlockProperties.calloutIcon(): String = icon?.takeIf { it.isNotBlank() } ?: BlockContent.Callout.DEFAULT_ICON

data class DocumentSnapshot(
    val id: DocumentId,
    val workspaceId: WorkspaceId,
    val title: String,
    val revision: Long,
    val blocks: List<Block>,
    /** The runs this document's pills point at, resolved with the snapshot
     *  so a pill shows its run's status and executing device (D16). A run
     *  that has not reached this phone is simply absent. */
    val runs: Map<DocumentId, RunSnapshot> = emptyMap(),
    /** Local read order, never persisted or sent over the wire. */
    val observation: Long = 0,
)

/**
 * [BlobHeld]: a file a person asked to open is not on this device and did
 * not arrive. The failure's message is the **stable code** that says why
 * (`blob_waiting_for_peer`, `no_linked_devices`, or one of the engine's hold
 * reasons), and the sentence is chosen from it — see `blobHeldMessage`.
 */
enum class AppErrorCode { NotFound, PermissionDenied, Unavailable, InvalidData, Unsupported, Unknown, BlobHeld }
data class AppFailure(val code: AppErrorCode, override val message: String) : Exception(message)

sealed interface DocumentCommand {
    data class EditBlockText(val blockId: BlockId, val before: String, val after: String, val createParagraph: Boolean = false, val split: BlockTextSplit? = null) : DocumentCommand
    data class CreateBlock(val block: Block, val siblingIndex: Int) : DocumentCommand
    data class DeleteBlock(val blockId: BlockId) : DocumentCommand
    data class MoveBlock(val blockId: BlockId, val parentId: BlockId?, val siblingIndex: Int) : DocumentCommand
    data class IndentBlock(val blockId: BlockId) : DocumentCommand
    data class OutdentBlock(val blockId: BlockId) : DocumentCommand
    data class InsertText(val blockId: BlockId, val atUtf8: Int, val text: String) : DocumentCommand
    data class DeleteText(val blockId: BlockId, val range: TextRange) : DocumentCommand
    data class AddMark(val blockId: BlockId, val mark: TextMark) : DocumentCommand
    data class RemoveMark(val blockId: BlockId, val range: TextRange, val kind: MarkKind) : DocumentCommand
    data class ToggleTask(val blockId: BlockId, val done: Boolean) : DocumentCommand
    data class InsertInlineObject(val blockId: BlockId, val value: InlineObject) : DocumentCommand
    data class RemoveInlineObject(val blockId: BlockId, val objectId: InlineObjectId) : DocumentCommand
    data class AttachBlob(val blockId: BlockId, val blob: BlobReference) : DocumentCommand
    data class RenameDocument(val title: String) : DocumentCommand
}

sealed interface DocumentEvent {
    data class Changed(val revision: Long, val blockIds: Set<BlockId>) : DocumentEvent
    data class Renamed(val title: String) : DocumentEvent
    data class BlobAvailable(val blobId: BlobId) : DocumentEvent
    data class BlobUnavailable(val blobId: BlobId) : DocumentEvent
    data class SessionError(val failure: AppFailure) : DocumentEvent
    data class Unknown(val typeUrl: String, val payload: ByteArray) : DocumentEvent
}
