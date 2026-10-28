// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import app.koru.mobile.domain.*
import org.junit.Assert.*
import org.junit.Test

class NativeDocumentReadModelTest {
    @Test
    fun native_projection_maps_every_readable_block_kind() {
        val snapshot = parseNativeDocument(documentJson())

        assertEquals("Complete", snapshot.title)
        assertEquals(14, snapshot.blocks.size)
        assertTrue(snapshot.blocks[0].content is BlockContent.Paragraph)
        assertTrue(snapshot.blocks[1].content is BlockContent.Quote)
        assertTrue(snapshot.blocks[2].content is BlockContent.Task)
        assertTrue(snapshot.blocks[3].content is BlockContent.ListContainer)
        assertTrue(snapshot.blocks[4].content is BlockContent.Code)
        assertTrue(snapshot.blocks[5].content is BlockContent.Table)
        assertSame(BlockContent.Divider, snapshot.blocks[6].content)
        assertTrue(snapshot.blocks[7].content is BlockContent.Image)
        assertTrue(snapshot.blocks[8].content is BlockContent.Canvas)
        assertTrue(snapshot.blocks[9].content is BlockContent.Contact)
        assertTrue(snapshot.blocks[10].content is BlockContent.Embed)
        assertSame(BlockContent.Callout, snapshot.blocks[11].content)

        // A callout is a box: the container has no text, carries the icon and
        // tint, and its lines follow it with the box as their parent.
        val callout = snapshot.blocks[11]
        assertEquals("⚠️", callout.properties.calloutIcon())
        assertEquals("red", callout.properties.color)
        assertEquals(BlockContent.Callout.DEFAULT_ICON, snapshot.blocks[0].properties.calloutIcon())
        val lines = snapshot.blocks.filter { it.parentId == callout.id }
        assertEquals(listOf(snapshot.blocks[12], snapshot.blocks[13]), lines)
        assertEquals(
            listOf("Inside the box", "Second line"),
            lines.map { (it.content as BlockContent.Paragraph).richText.text },
        )

        val paragraph = snapshot.blocks[0].content as BlockContent.Paragraph
        assertEquals(MarkKind.Bold, paragraph.richText.marks.single().kind)
        val target = paragraph.richText.inlineObjects.single().reference
        assertEquals(
            DocumentId("10000000-0000-4000-8000-000000000099"),
            (target as EntityReference.Document).id,
        )
        val image = snapshot.blocks[7].content as BlockContent.Image
        assertEquals(snapshot.workspaceId, image.blob.workspaceId)
        assertEquals(42, image.blob.size)
    }

    private fun documentJson(): String {
        val kinds = listOf(
            "paragraph" to rich("Hello⁠", true),
            "quote" to rich("Quote"),
            "task" to rich("Task"),
            "listblock" to "",
            "code" to rich("fun main() {}"),
            "table" to rich("| A | B |\\n|---|---|\\n| 1 | 2 |"),
            "divider" to "",
            "image" to "",
            "canvas" to "",
            "contact" to rich("Ada"),
            "embed" to "",
            "callout" to "",
        )
        val blocks = kinds.mapIndexed { index, (kind, rich) -> block(id(index + 10), null, kind, rich) }
        // The callout's lines follow it, with the box as their parent.
        val lines = listOf("Inside the box", "Second line").mapIndexed { index, text ->
            block(id(index + 30), id(21), "paragraph", rich(text))
        }
        val all = (blocks + lines).joinToString(",")
        return """{"document":{"id":"10000000-0000-4000-8000-000000000001","workspace_id":"10000000-0000-4000-8000-000000000002","title":"Complete","revision":7,"blocks":[$all]}}"""
    }

    private fun id(n: Int) = "10000000-0000-4000-8000-${n.toString().padStart(12, '0')}"

    private fun block(id: String, parent: String?, kind: String, rich: String): String {
        val callout = kind == "callout"
        val properties = """{"name":null,"icon":${if (callout) "\"⚠️\"" else "null"},"color":${if (callout) "\"red\"" else "null"},"done":false,"alignment":"start","heading_level":null,"list_style":${if (kind == "listblock") "\"roman\"" else "null"},"image_width":null,"image_height":null,"language":${if (kind == "code") "\"kotlin\"" else "null"}}"""
        val extra = when (kind) {
            "image", "canvas" -> ""","blob":{"id":"b3:aaaa","mime_type":"image/png","size":42,"width":10,"height":20}"""
            "embed" -> ""","reference":{"kind":"document","document_id":"10000000-0000-4000-8000-000000000099"}"""
            else -> if (rich.isNotEmpty()) ",\"rich_text\":$rich" else ""
        }
        val parentJson = if (parent == null) "null" else "\"$parent\""
        return """{"id":"$id","parent_id":$parentJson,"kind":"$kind","properties":$properties$extra}"""
    }

    private fun rich(text: String, linked: Boolean = false): String {
        val marks = if (linked) """[{"start_utf8":0,"end_utf8":5,"kind":"bold"}]""" else "[]"
        val links = if (linked) """[{"id":"10000000-0000-4000-8000-000000000098","anchor_utf8":5,"role":"document","label":"Next","reference":{"kind":"document","document_id":"10000000-0000-4000-8000-000000000099"}}]""" else "[]"
        return """{"text":"$text","marks":$marks,"inline_objects":$links}"""
    }
}
