// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui.editor

import android.content.res.Resources
import android.graphics.BitmapFactory
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.ClickableText
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.BrokenImage
import androidx.compose.material.icons.outlined.Cloud
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.platform.LocalSoftwareKeyboardController
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.*
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.BaselineShift
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import app.koru.mobile.AppViewModel
import app.koru.mobile.R
import app.koru.mobile.domain.*
import app.koru.mobile.services.BlobLoadState
import app.koru.mobile.ui.run.runPillLabel
import app.koru.mobile.ui.calloutTint
import app.koru.mobile.services.BlockEditKey
import app.koru.mobile.services.BlockDraft
import app.koru.mobile.services.editableText
import app.koru.mobile.services.withPendingParagraphs
import app.koru.mobile.ui.theme.KoruSpacing
import kotlinx.coroutines.launch

/** Documents open in read mode. Only an explicit pencil action enables input. */
@OptIn(ExperimentalMaterial3Api::class, ExperimentalLayoutApi::class)
@Composable
fun DocumentViewer(
    document: DocumentSnapshot?,
    model: AppViewModel,
    onBack: () -> Unit,
    onFollow: (EntityReference) -> Unit,
) {
    if (document == null) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        return
    }
    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    var editing by rememberSaveable(document.id.value) { mutableStateOf(false) }
    var finishRequested by remember(document.id) { mutableStateOf(false) }
    var newParagraph by rememberSaveable(document.id.value) { mutableStateOf(BlockId.new().value) }
    val drafts by model.documentEdits.drafts.collectAsState()
    val paragraphs by model.documentEdits.paragraphs.collectAsState()
    val all = remember(document.blocks, document.id, editing, newParagraph, paragraphs) {
        val base = if (editing && document.blocks.none { it.id.value == newParagraph })
            document.blocks + Block(BlockId(newParagraph), content = BlockContent.Paragraph(RichText(""))) else document.blocks
        withPendingParagraphs(base, paragraphs.filterKeys { it.document == document.id }.values.toList())
    }
    // A list block and a callout box are containers: their lines are the
    // rows, and the container itself is not one.
    val visible = remember(all) {
        all.filterNot { it.content is BlockContent.ListContainer || it.content is BlockContent.Callout }
    }
    val depths = remember(all) { all.associate { block -> block.id to depth(all, block) } }
    // The first row inside each callout box is the one that shows its icon.
    val calloutHeads = remember(all) {
        val heads = mutableMapOf<BlockId, BlockId>()
        for (block in visible) calloutBox(all, block)?.let { heads.putIfAbsent(it.id, block.id) }
        heads
    }
    var focusBlock by remember(document.id) { mutableStateOf<BlockId?>(null) }
    LaunchedEffect(focusBlock) {
        val index = visible.indexOfFirst { it.id == focusBlock }
        if (index >= 0) listState.animateScrollToItem(index)
    }
    val localDrafts = drafts.filterKeys { it.document == document.id }
    val saving = localDrafts.values.any { it.saving }
    val failed = localDrafts.filterValues { it.error != null }
    val keyboard = LocalSoftwareKeyboardController.current
    val view = LocalView.current
    val clipboard = LocalClipboardManager.current
    val syncStatus by model.contentSyncStatus.collectAsState()
    val lifecycle = LocalLifecycleOwner.current.lifecycle
    DisposableEffect(document.id, lifecycle, model) {
        val observer = LifecycleEventObserver { _, event ->
            if (event == Lifecycle.Event.ON_STOP) model.documentEdits.flush(document.id)
        }
        lifecycle.addObserver(observer)
        onDispose {
            lifecycle.removeObserver(observer)
            model.documentEdits.flush(document.id)
        }
    }
    var discard by remember { mutableStateOf<BlockEditKey?>(null) }
    val finish: () -> Unit = {
        view.rootView.findFocus()?.clearFocus()
        keyboard?.hide()
        model.documentEdits.flush(document.id)
        finishRequested = true
    }
    LaunchedEffect(finishRequested, saving, failed.isEmpty()) {
        if (finishRequested && !saving && failed.isEmpty()) {
            editing = false
            finishRequested = false
        }
    }
    BackHandler(editing, finish)
    val follow: (EntityReference) -> Unit = { reference ->
        val localBlock = (reference as? EntityReference.Block)
            ?.takeIf { it.documentId == null || it.documentId == document.id }
        val index = localBlock?.let { target -> visible.indexOfFirst { it.id == target.id } } ?: -1
        if (index >= 0) scope.launch { listState.animateScrollToItem(index) } else onFollow(reference)
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(document.title, maxLines = 1) },
                navigationIcon = {
                    TextButton(onClick = { if (editing) finish() else onBack() }) { Text(stringResource(R.string.back)) }
                },
                actions = {
                    if (saving) Text(stringResource(R.string.document_saving), style = MaterialTheme.typography.labelSmall)
                    if (syncStatus.cloudActive) Icon(
                        Icons.Outlined.Cloud,
                        contentDescription = stringResource(R.string.document_cloud_sync_active),
                        modifier = Modifier.padding(horizontal = KoruSpacing.sm),
                    )
                    if (editing) TextButton(onClick = finish) { Text(stringResource(R.string.done)) }
                    else IconButton(onClick = { editing = true }) {
                        Icon(Icons.Outlined.Edit, contentDescription = stringResource(R.string.document_edit))
                    }
                },
            )
        },
        bottomBar = {
            failed.entries.firstOrNull()?.let { (key, draft) ->
                Surface(tonalElevation = 3.dp) {
                    Column(Modifier.fillMaxWidth().padding(KoruSpacing.md)) {
                        Text(stringResource(if (draft.error?.contains("text_edit_conflict") == true)
                            R.string.document_edit_conflict else R.string.document_edit_failed),
                            color = MaterialTheme.colorScheme.error)
                        FlowRow {
                            TextButton(onClick = { model.documentEdits.retry(key) }) { Text(stringResource(R.string.retry)) }
                            TextButton(onClick = { clipboard.setText(AnnotatedString(model.documentEdits.copyDraft(key))) }) { Text(stringResource(R.string.document_copy_draft)) }
                            TextButton(onClick = { discard = key }) { Text(stringResource(R.string.document_use_saved)) }
                        }
                    }
                }
            }
        },
    ) { padding ->
        LazyColumn(
            state = listState,
            modifier = Modifier.padding(padding).fillMaxSize().imePadding(),
            contentPadding = PaddingValues(horizontal = KoruSpacing.md, vertical = KoruSpacing.md),
            verticalArrangement = Arrangement.spacedBy(KoruSpacing.sm),
        ) {
            itemsIndexed(visible, key = { _, block -> block.id.value }) { _, block ->
                BlockRow(block, all, depths[block.id] ?: 0, calloutHeads.containsValue(block.id),
                    document.runs, model, follow,
                    editing, localDrafts[BlockEditKey(document.id, block.id)],
                    { before, text ->
                        finishRequested = false
                        block.content.editableText()?.let { rich ->
                            model.documentEdits.change(BlockEditKey(document.id, block.id), rich.copy(text = before), text,
                                createParagraph = document.blocks.none { it.id == block.id })
                        }
                    }, finish,
                    onSplit = { before, text ->
                        finishRequested = false
                        focusBlock = model.documentEdits.enter(BlockEditKey(document.id, block.id), block, before, text,
                            hasChildren = all.any { it.parentId == block.id },
                            createParagraph = document.blocks.none { it.id == block.id })
                    },
                    requestFocus = focusBlock == block.id,
                    onFocused = { if (focusBlock == block.id) focusBlock = null })
            }
            if (editing) {
                val id = BlockId(newParagraph)
                if (document.blocks.any { it.id == id }) item(key = "add_paragraph") {
                    TextButton(onClick = { newParagraph = BlockId.new().value }) { Text(stringResource(R.string.document_add_paragraph)) }
                }
            }
        }
    }
    discard?.let { key ->
        AlertDialog(onDismissRequest = { discard = null },
            title = { Text(stringResource(R.string.document_use_saved)) },
            text = { Text(stringResource(R.string.document_discard_draft)) },
            confirmButton = { TextButton(onClick = { model.documentEdits.discard(key); discard = null }) { Text(stringResource(R.string.document_use_saved)) } },
            dismissButton = { TextButton(onClick = { discard = null }) { Text(stringResource(R.string.cancel)) } })
    }
}

@Composable
private fun BlockRow(
    block: Block,
    all: List<Block>,
    depth: Int,
    calloutHead: Boolean,
    runs: Map<DocumentId, RunSnapshot>,
    model: AppViewModel,
    follow: (EntityReference) -> Unit,
    editing: Boolean,
    draft: BlockDraft?,
    onText: (String, String) -> Unit,
    onDone: () -> Unit,
    onSplit: (String, String) -> Unit,
    requestFocus: Boolean,
    onFocused: () -> Unit,
) {
    val pillLabel = runPillLabels(runs)
    val editor: (@Composable (RichText, TextStyle, Modifier) -> Unit)? = if (editing) {
        { rich, style, modifier ->
            NativeRichEditor(rich, style, draft?.displayedText, onText, onDone, onSplit,
                locked = draft?.split != null, requestFocus = requestFocus, onFocused = onFocused,
                code = block.content is BlockContent.Code,
                hint = stringResource(R.string.document_type_here), modifier = modifier.fillMaxWidth())
        }
    } else null
    val listParent = block.parentId
        ?.let { id -> all.firstOrNull { it.id == id }?.content as? BlockContent.ListContainer }
    // A line inside a callout box draws on the box's tint, the way a list
    // item finds its container; the box's first line leads with its icon and
    // the others keep the icon's column so the text lines up under it.
    val calloutBox = calloutBox(all, block)
    val dark = MaterialTheme.colorScheme.background.luminance() < 0.5f
    val tint = calloutBox?.let { calloutTint(it.properties.color, dark) ?: MaterialTheme.colorScheme.surfaceVariant }
    Row(
        Modifier.fillMaxWidth().padding(start = (depth * 16).dp).then(
            if (tint != null) Modifier.background(tint, MaterialTheme.shapes.small)
                .padding(horizontal = KoruSpacing.md, vertical = KoruSpacing.sm)
            else Modifier,
        ),
        verticalAlignment = Alignment.Top,
    ) {
        if (calloutBox != null) {
            Box(Modifier.width(30.dp)) {
                if (calloutHead) Text(calloutBox.properties.calloutIcon(), style = MaterialTheme.typography.bodyLarge)
            }
        }
        Marker(block, all, listParent)
        Box(Modifier.weight(1f)) {
            when (val content = block.content) {
                is BlockContent.Paragraph -> RichTextBlock(content.richText, block.properties, follow, pillLabel = pillLabel, editor = editor)
                is BlockContent.Quote -> Surface(
                    color = MaterialTheme.colorScheme.surfaceVariant,
                    shape = MaterialTheme.shapes.small,
                ) {
                    RichTextBlock(
                        content.richText,
                        block.properties,
                        follow,
                        Modifier.padding(horizontal = KoruSpacing.md, vertical = KoruSpacing.sm),
                        italic = true,
                        pillLabel = pillLabel,
                        editor = editor,
                    )
                }
                is BlockContent.Task -> RichTextBlock(content.richText, block.properties, follow, pillLabel = pillLabel, editor = editor)
                is BlockContent.Code -> CodeBlock(content, block.properties, follow, pillLabel, editor)
                is BlockContent.Table -> TableBlock(content.richText.text)
                BlockContent.Divider -> HorizontalDivider(Modifier.padding(vertical = KoruSpacing.md))
                is BlockContent.Image -> BlobImage(content.blob, model, block.properties.name)
                is BlockContent.Canvas -> BlobImage(content.blob, model, block.properties.name)
                is BlockContent.Embed -> ReferenceCard(
                    content.reference,
                    (content.reference as? EntityReference.Run)?.let { runs[it.documentId] }
                        ?.let { pillLabel(InlineObject(InlineObjectId(""), 0, "koru.run", content.reference, block.properties.name)) }
                        ?: block.properties.name,
                    follow,
                )
                is BlockContent.Contact -> ContactCard(content.name, follow, pillLabel)
                is BlockContent.Unsupported -> OutlinedCard {
                    Text(
                        androidx.compose.ui.res.stringResource(R.string.unsupported_block, content.kindCode),
                        Modifier.padding(KoruSpacing.md),
                    )
                }
                is BlockContent.ListContainer -> Unit
                is BlockContent.Callout -> Unit
            }
        }
    }
}

@Composable
private fun Marker(block: Block, all: List<Block>, listParent: BlockContent.ListContainer?) {
    val modifier = Modifier.width(30.dp).padding(top = 3.dp)
    when {
        block.content is BlockContent.Task || listParent?.style == ListStyle.Checkbox ->
            Checkbox(block.properties.done, onCheckedChange = null, modifier = Modifier.size(24.dp))
        listParent?.style == ListStyle.Bullet -> Text("•", modifier, textAlign = TextAlign.Center)
        listParent != null -> Text(
            listOrdinal(listParent.style, ordinal(all, block)),
            modifier,
            textAlign = TextAlign.End,
            style = MaterialTheme.typography.bodyMedium,
        )
        else -> Spacer(Modifier.width(4.dp))
    }
}

/**
 * How a pill in this document is labelled. A run pill (ADR-349) shows its
 * run's status glyph, the executing device when it is not this one (D16)
 * and the objective's first line; every other pill shows its stored label
 * or its reference's own value. Resolved once per document so the strings
 * are read in composition and the text builder stays pure.
 */
@Composable
private fun runPillLabels(runs: Map<DocumentId, RunSnapshot>): (InlineObject) -> String {
    val fallback = androidx.compose.ui.res.stringResource(R.string.run_pill_fallback)
    val anotherDevice = androidx.compose.ui.res.stringResource(R.string.run_another_device)
    val unlabelled = rememberReferenceFallback()
    return remember(runs, fallback, anotherDevice, unlabelled) {
        { pill ->
            when (val reference = pill.reference) {
                is EntityReference.Run -> runPillLabel(runs[reference.documentId], pill.label, fallback, anotherDevice)
                else -> pill.label ?: unlabelled(reference)
            }
        }
    }
}

/** A pill's label, or the localized name of what it points at. */
@Composable
private fun defaultPillLabel(): (InlineObject) -> String {
    val unlabelled = rememberReferenceFallback()
    return remember(unlabelled) { { pill -> pill.label ?: unlabelled(pill.reference) } }
}

@Composable
private fun RichTextBlock(
    rich: RichText,
    properties: BlockProperties,
    follow: (EntityReference) -> Unit,
    modifier: Modifier = Modifier,
    italic: Boolean = false,
    pillLabel: (InlineObject) -> String = defaultPillLabel(),
    editor: (@Composable (RichText, TextStyle, Modifier) -> Unit)? = null,
) {
    val linkColor = MaterialTheme.colorScheme.primary
    val annotated = remember(rich, linkColor, pillLabel) { annotatedRichText(rich, linkColor, pillLabel) }
    val style = when (properties.headingLevel) {
        1 -> MaterialTheme.typography.headlineLarge
        2 -> MaterialTheme.typography.headlineMedium
        3 -> MaterialTheme.typography.headlineSmall
        4 -> MaterialTheme.typography.titleLarge
        5 -> MaterialTheme.typography.titleMedium
        6 -> MaterialTheme.typography.titleSmall
        else -> MaterialTheme.typography.bodyLarge
    }.copy(
        textAlign = when (properties.alignment) {
            app.koru.mobile.domain.Alignment.Center -> TextAlign.Center
            app.koru.mobile.domain.Alignment.End -> TextAlign.End
            app.koru.mobile.domain.Alignment.Justify -> TextAlign.Justify
            app.koru.mobile.domain.Alignment.Start -> TextAlign.Start
        },
        fontStyle = if (italic) FontStyle.Italic else null,
    )
    if (editor != null) {
        editor(rich, style.copy(color = MaterialTheme.colorScheme.onSurface), modifier)
        return
    }
    @Suppress("DEPRECATION")
    ClickableText(
        text = annotated,
        modifier = modifier.fillMaxWidth(),
        style = style.copy(color = MaterialTheme.colorScheme.onSurface),
        onClick = { offset -> followAnnotation(annotated, rich, offset, follow) },
    )
}

@Composable
private fun CodeBlock(
    content: BlockContent.Code,
    properties: BlockProperties,
    follow: (EntityReference) -> Unit,
    pillLabel: (InlineObject) -> String,
    editor: (@Composable (RichText, TextStyle, Modifier) -> Unit)? = null,
) {
    Surface(color = MaterialTheme.colorScheme.surfaceVariant, shape = MaterialTheme.shapes.small) {
        Column(Modifier.padding(KoruSpacing.md)) {
            properties.language?.takeIf(String::isNotBlank)?.let {
                Text(it, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.primary)
                Spacer(Modifier.height(KoruSpacing.xs))
            }
            val annotated = annotatedRichText(content.richText, MaterialTheme.colorScheme.primary, pillLabel)
            val style = MaterialTheme.typography.bodyMedium.copy(
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                fontFamily = FontFamily.Monospace,
            )
            if (editor != null) editor(content.richText, style, Modifier) else {
                @Suppress("DEPRECATION")
                ClickableText(
                    text = annotated,
                    modifier = Modifier.fillMaxWidth(),
                    style = style,
                    onClick = { offset -> followAnnotation(annotated, content.richText, offset, follow) },
                )
            }
        }
    }
}

private fun followAnnotation(
    annotated: AnnotatedString,
    rich: RichText,
    offset: Int,
    follow: (EntityReference) -> Unit,
) {
    annotated.getStringAnnotations(REFERENCE_TAG, offset, offset).firstOrNull()
        ?.item?.toIntOrNull()?.let(rich.inlineObjects::getOrNull)
        ?.let { follow(it.reference) }
}

@Composable
private fun TableBlock(source: String) {
    val rows = remember(source) { parseMarkdownTable(source) }
    if (rows.isEmpty()) {
        Text(source, fontFamily = FontFamily.Monospace)
        return
    }
    Column(
        Modifier.fillMaxWidth().horizontalScroll(rememberScrollState())
            .background(MaterialTheme.colorScheme.surfaceVariant, MaterialTheme.shapes.small),
    ) {
        rows.forEachIndexed { rowIndex, cells ->
            Row {
                cells.forEach { cell ->
                    Text(
                        cell,
                        Modifier.widthIn(min = 112.dp).padding(KoruSpacing.sm),
                        fontWeight = if (rowIndex == 0) FontWeight.SemiBold else FontWeight.Normal,
                    )
                }
            }
            if (rowIndex == 0) HorizontalDivider()
        }
    }
}

@Composable
private fun BlobImage(blob: BlobReference, model: AppViewModel, label: String?) {
    var retryGeneration by remember(blob.id, blob.workspaceId) { mutableIntStateOf(0) }
    val state by produceState<BlobLoadState>(
        BlobLoadState.CheckingLocal,
        blob.id,
        blob.workspaceId,
        retryGeneration,
    ) {
        model.observeBlob(blob).collect { value = it }
    }
    val bytes = (state as? BlobLoadState.Available)?.bytes
    val bitmap = remember(bytes) {
        bytes?.let { BitmapFactory.decodeByteArray(it, 0, it.size) }
    }
    when {
        bitmap != null -> Image(
            bitmap.asImageBitmap(),
            contentDescription = label ?: androidx.compose.ui.res.stringResource(R.string.document_image),
            modifier = Modifier.fillMaxWidth().heightIn(min = 100.dp, max = 420.dp),
        )
        state is BlobLoadState.CheckingLocal || state is BlobLoadState.Fetching -> Column {
            LinearProgressIndicator(Modifier.fillMaxWidth())
            Text(
                androidx.compose.ui.res.stringResource(R.string.image_downloading),
                style = MaterialTheme.typography.bodySmall,
            )
        }
        state is BlobLoadState.NoLinkedDevices -> OutlinedCard {
            Row(Modifier.padding(KoruSpacing.md), verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.BrokenImage, contentDescription = null)
                Spacer(Modifier.width(KoruSpacing.sm))
                Text(androidx.compose.ui.res.stringResource(R.string.image_link_device))
            }
        }
        // **Paused is its own arm, not a friendlier failure.** The picture is
        // waiting for a connection this device may use (D50), it clears by
        // itself, and a person may ask for this one anyway — which is the
        // override the contract's `request_blob` exists for.
        state is BlobLoadState.Paused -> OutlinedCard {
            val paused = state as BlobLoadState.Paused
            Row(Modifier.padding(KoruSpacing.md), verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.CloudOff, contentDescription = null)
                Spacer(Modifier.width(KoruSpacing.sm))
                Text(
                    androidx.compose.ui.res.stringResource(
                        // One table, and the engine's own codes. D259's second
                        // correction was a placeholder naming the wrong cause:
                        // "waiting for a device to come online" over a hold
                        // that had nothing to do with a device sends a person
                        // to restart hardware they do not need.
                        when (app.koru.mobile.services.BlobHold.of(paused.code)) {
                            app.koru.mobile.services.BlobHold.LowData -> R.string.image_paused_low_data
                            app.koru.mobile.services.BlobHold.Offline -> R.string.image_paused_offline
                            app.koru.mobile.services.BlobHold.Person -> R.string.image_paused_person
                            app.koru.mobile.services.BlobHold.Cloud -> R.string.image_paused_no_cloud
                            else -> R.string.image_paused_metered
                        },
                    ),
                    modifier = Modifier.weight(1f),
                )
                if (paused.requestable) {
                    // No retry bump: the request runs to the end of its pass
                    // and the arrival announcement is what turns this card
                    // into the picture. Re-reading now would only find the
                    // same absence and say the same thing again.
                    TextButton(onClick = { model.requestBlob(blob) }) {
                        Text(androidx.compose.ui.res.stringResource(R.string.image_download_now))
                    }
                }
            }
        }
        // **A wait, not damage.** A peer that lacks the bytes and a holder
        // that is offline both mean nobody reachable has this picture yet —
        // the Cloud progress screen's "waiting for …" / "no device online has
        // it", said here as a sentence. Never "Image unavailable".
        state is BlobLoadState.WaitingForPeer -> OutlinedCard {
            val holders = (state as BlobLoadState.WaitingForPeer).holders
            Row(Modifier.padding(KoruSpacing.md), verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.CloudOff, contentDescription = null)
                Spacer(Modifier.width(KoruSpacing.sm))
                Text(
                    if (holders.isEmpty()) {
                        androidx.compose.ui.res.stringResource(R.string.image_waiting_no_holder)
                    } else {
                        androidx.compose.ui.res.stringResource(
                            R.string.image_waiting_for_device,
                            holders.joinToString(", "),
                        )
                    },
                )
            }
        }
        else -> OutlinedCard {
            Row(Modifier.padding(KoruSpacing.md), verticalAlignment = Alignment.CenterVertically) {
                Icon(Icons.Outlined.BrokenImage, contentDescription = null)
                Spacer(Modifier.width(KoruSpacing.sm))
                Text(
                    // A device with no room says so: the remedy is the
                    // person's, and "image unavailable" hides it.
                    if ((state as? BlobLoadState.Failed)?.code == "insufficient_storage") {
                        androidx.compose.ui.res.stringResource(R.string.image_no_room)
                    } else {
                        label ?: androidx.compose.ui.res.stringResource(R.string.image_unavailable)
                    },
                    modifier = Modifier.weight(1f),
                )
                if ((state as? BlobLoadState.Failed)?.retryable == true) {
                    TextButton(onClick = { retryGeneration += 1 }) {
                        Text(androidx.compose.ui.res.stringResource(R.string.retry))
                    }
                }
            }
        }
    }
}

@Composable
private fun ReferenceCard(reference: EntityReference, label: String?, follow: (EntityReference) -> Unit) {
    OutlinedCard(Modifier.fillMaxWidth().clickable { follow(reference) }) {
        Row(Modifier.padding(KoruSpacing.md), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Outlined.Link, contentDescription = null)
            Spacer(Modifier.width(KoruSpacing.sm))
            Text(
                label ?: rememberReferenceFallback()(reference),
                color = MaterialTheme.colorScheme.primary,
            )
        }
    }
}

@Composable
private fun ContactCard(name: RichText, follow: (EntityReference) -> Unit, pillLabel: (InlineObject) -> String) {
    OutlinedCard(Modifier.fillMaxWidth()) {
        Row(Modifier.padding(KoruSpacing.md), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Outlined.AccountCircle, contentDescription = null)
            Spacer(Modifier.width(KoruSpacing.sm))
            RichTextBlock(name, BlockProperties(), follow, Modifier.weight(1f), pillLabel = pillLabel)
        }
    }
}

/**
 * `pillLabel` answers *what is this pill called* and is passed in rather than
 * defaulted here: this function is not composable — `remember` calls it — and
 * a default English answer would be the same "English is parity" gap the
 * localization sweep exists to close. Composables get theirs from
 * [defaultPillLabel] or [runPillLabels]; a test names the words it asserts on.
 */
internal fun annotatedRichText(
    rich: RichText,
    linkColor: Color,
    pillLabel: (InlineObject) -> String,
): AnnotatedString {
    val builder = AnnotatedString.Builder()
    val boundaries = mutableMapOf(0 to 0)
    val links = rich.inlineObjects.withIndex().associateBy { it.value.anchorUtf8 }
    var utf16 = 0
    var utf8 = 0
    while (utf16 < rich.text.length) {
        boundaries[utf8] = builder.length
        val codePoint = rich.text.codePointAt(utf16)
        val chars = Character.charCount(codePoint)
        val source = rich.text.substring(utf16, utf16 + chars)
        val bytes = source.encodeToByteArray().size
        val link = links[utf8]
        if (link != null) {
            val start = builder.length
            builder.append(pillLabel(link.value))
            val end = builder.length
            builder.addStringAnnotation(REFERENCE_TAG, link.index.toString(), start, end)
            builder.addStyle(SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline), start, end)
        } else {
            builder.append(source)
        }
        utf16 += chars
        utf8 += bytes
        boundaries[utf8] = builder.length
    }
    rich.marks.forEach { mark ->
        val start = boundaries[mark.range.startUtf8] ?: return@forEach
        val end = boundaries[mark.range.endUtf8] ?: return@forEach
        if (start >= end) return@forEach
        val style = when (mark.kind) {
            // The bundled mobile Inter family currently contains 400 and 500
            // only. Compose resolves a request for 700 to the closest bundled
            // face, which made a bold mark look unchanged on a real device.
            // Use Android's actual sans-serif bold face for the marked range;
            // this is visually honest on every supported device and avoids
            // pretending the Medium file is a Bold file.
            MarkKind.Bold -> SpanStyle(fontFamily = FontFamily.SansSerif, fontWeight = FontWeight.Bold)
            MarkKind.Italic -> SpanStyle(fontStyle = FontStyle.Italic)
            MarkKind.Underline -> SpanStyle(textDecoration = TextDecoration.Underline)
            MarkKind.Strike -> SpanStyle(textDecoration = TextDecoration.LineThrough)
            MarkKind.Code -> SpanStyle(fontFamily = FontFamily.Monospace, background = Color(0x18000000))
            MarkKind.Superscript -> SpanStyle(baselineShift = BaselineShift.Superscript, fontSize = 12.sp)
            MarkKind.Subscript -> SpanStyle(baselineShift = BaselineShift.Subscript, fontSize = 12.sp)
            MarkKind.Size -> SpanStyle(fontSize = mark.value.decodeToString().toFloatOrNull()?.sp ?: 18.sp)
            MarkKind.Highlight -> SpanStyle(background = Color(0x66FFE066))
            MarkKind.Unknown -> return@forEach
        }
        builder.addStyle(style, start, end)
    }
    return builder.toAnnotatedString()
}

internal fun parseMarkdownTable(source: String): List<List<String>> {
    val lines = source.lines().filter(String::isNotBlank)
    if (lines.isEmpty()) return emptyList()
    fun cells(line: String) = line.trim().trim('|').split('|').map(String::trim)
    return lines.map(::cells).filterIndexed { index, cells ->
        index != 1 || !cells.all { it.matches(Regex(":?-{3,}:?")) }
    }
}

private fun depth(all: List<Block>, block: Block): Int {
    var result = 0
    var parent = block.parentId
    val seen = mutableSetOf<BlockId>()
    while (parent != null && seen.add(parent)) {
        val node = all.firstOrNull { it.id == parent }
        // A callout box is a tint, not an indent: its lines sit where it does.
        if (node?.content !is BlockContent.Callout) result++
        parent = node?.parentId
    }
    return result
}

/** The callout box a block is inside — its nearest ancestor that is one. */
private fun calloutBox(all: List<Block>, block: Block): Block? {
    var parent = block.parentId
    val seen = mutableSetOf<BlockId>()
    while (parent != null && seen.add(parent)) {
        val node = all.firstOrNull { it.id == parent } ?: return null
        if (node.content is BlockContent.Callout) return node
        parent = node.parentId
    }
    return null
}

private fun ordinal(all: List<Block>, block: Block) =
    all.filter { it.parentId == block.parentId }.indexOfFirst { it.id == block.id } + 1

private fun listOrdinal(style: ListStyle, ordinal: Int): String = when (style) {
    ListStyle.Bullet, ListStyle.Checkbox -> ""
    ListStyle.Numbered -> "$ordinal."
    ListStyle.Letter -> "${('a'.code + (ordinal - 1).coerceAtLeast(0) % 26).toChar()}."
    ListStyle.Roman -> "${roman(ordinal)}."
}

private fun roman(value: Int): String {
    var number = value.coerceAtLeast(1)
    val values = listOf(1000 to "m", 900 to "cm", 500 to "d", 400 to "cd", 100 to "c", 90 to "xc", 50 to "l", 40 to "xl", 10 to "x", 9 to "ix", 5 to "v", 4 to "iv", 1 to "i")
    return buildString { values.forEach { (amount, glyph) -> while (number >= amount) { append(glyph); number -= amount } } }
}

/**
 * What a reference with no label of its own is called.
 *
 * A URL and a label carry their own text and are shown as written — the value
 * *is* the label, so translating it would be translating content. The other
 * three are descriptions this app supplies, so they are resources, and they
 * are read here rather than inside the closure because `annotatedRichText`
 * runs under `remember` where no composition is available. The configuration
 * is read so a language change recomposes this the way `stringResource` would.
 */
@Composable
internal fun rememberReferenceFallback(): (EntityReference) -> String {
    val configuration = LocalConfiguration.current
    val resources = LocalContext.current.resources
    return remember(configuration, resources) { referenceFallback(resources) }
}

/**
 * The same table for a caller with no composition — the native editor renders
 * its spans from an `EditText`'s resources. One table, two ways in.
 */
internal fun referenceFallback(resources: Resources): (EntityReference) -> String {
    val document = resources.getString(R.string.open_document)
    val block = resources.getString(R.string.open_block)
    val person = resources.getString(R.string.person)
    val run = resources.getString(R.string.run_pill_fallback)
    return { reference: EntityReference ->
        when (reference) {
            is EntityReference.Document -> document
            is EntityReference.Block -> block
            // Labelled through `runPillLabels` wherever a document is drawn;
            // this is the bare fallback for a caller that passes no labeller.
            is EntityReference.Run -> run
            is EntityReference.Url -> reference.value
            is EntityReference.User -> person
            is EntityReference.Label -> reference.value
        }
    }
}

private const val REFERENCE_TAG = "koru-reference"
