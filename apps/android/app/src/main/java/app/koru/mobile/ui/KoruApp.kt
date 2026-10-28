// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.InsertDriveFile
import androidx.compose.material.icons.automirrored.outlined.NoteAdd
import androidx.compose.material.icons.automirrored.outlined.StickyNote2
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.outlined.Archive
import androidx.compose.material.icons.outlined.BugReport
import androidx.compose.material.icons.outlined.CloudQueue
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.OpenInNew
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material.icons.outlined.Sync
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.RestoreFromTrash
import androidx.compose.material.icons.outlined.Unarchive
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.navigation.NavType
import androidx.navigation.compose.*
import androidx.navigation.navArgument
import app.koru.mobile.AppViewModel
import app.koru.mobile.AppUiState
import app.koru.mobile.ShareUiState
import app.koru.mobile.BuildConfig
import app.koru.mobile.R
import app.koru.mobile.domain.*
import app.koru.mobile.ui.editor.DocumentViewer
import app.koru.mobile.ui.run.RunScreen
import app.koru.mobile.ui.theme.KoruSpacing
import app.koru.mobile.ui.theme.AppearancePreferences
import app.koru.mobile.services.DeviceLinkState
import app.koru.mobile.services.AccountIntent
import app.koru.mobile.services.AccountScreenState
import app.koru.mobile.services.CloudSyncDetail
import app.koru.mobile.services.BlobTransferSnapshot
import app.koru.mobile.services.ReleasedFiles
import app.koru.mobile.services.blobTransferReport
import app.koru.mobile.services.CloudSyncState
import app.koru.mobile.services.DeviceStateEvent
import app.koru.mobile.services.LinkedDevice
import app.koru.mobile.services.linkingBlocked
import app.koru.mobile.services.RelayState
import app.koru.mobile.services.RelayStatus
import android.content.ClipData
import android.content.ClipboardManager
import android.graphics.Bitmap
import android.text.format.DateUtils
import android.widget.Toast
import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter
import kotlinx.coroutines.flow.distinctUntilChanged

private object Routes {
    const val Devices = "devices"
    const val Profiles = "profiles"
    const val Workspaces = "workspaces/{profileId}"
    const val Documents = "documents/{workspaceId}"
    const val Document = "document/{documentId}"
    const val Run = "run/{documentId}"
    const val Diagnostics = "diagnostics"
    const val Settings = "settings"
    const val Account = "account"
    const val CloudProgress = "cloud-progress/{profileId}"
    const val Licenses = "licenses"
    const val License = "license/{file}"
    fun license(file: String) = "license/$file"
    fun workspaces(id: ProfileId) = "workspaces/${id.value}"
    fun cloudProgress(id: ProfileId) = "cloud-progress/${id.value}"
    fun documents(id: WorkspaceId) = "documents/${id.value}"
    fun document(id: DocumentId) = "document/${id.value}"
    fun run(id: DocumentId) = "run/${id.value}"
}

@Composable
fun KoruApp(
    model: AppViewModel,
    implementation: String,
    appearance: AppearancePreferences,
    onShareFinished: () -> Unit = {},
    onAppearanceChanged: (AppearancePreferences) -> Unit,
) {
    val nav = rememberNavController()
    val state by model.state.collectAsState()
    val linkState by model.deviceLink.collectAsState()
    val share by model.share.collectAsState()
    if (share.receiving || share.pending != null) {
        val shareProfiles by model.visibleProfiles.collectAsState()
        val shareWorkspaces by model.shareWorkspaces.collectAsState()
        val shareRecentWorkspaces by model.shareRecentWorkspaces.collectAsState()
        ShareDestinationScreen(
            state = share,
            profiles = shareProfiles.rows,
            workspaces = shareWorkspaces,
            recentWorkspaces = shareRecentWorkspaces,
            onSelectProfile = model::selectShareProfile,
            onNameChange = model::setShareFileName,
            onNoteChange = model::setShareFileNote,
            onSave = model::saveSharedContent,
            onFinish = { model.clearShare(); onShareFinished() },
        )
        return
    }
    NavHost(navController = nav, startDestination = Routes.Profiles) {
        composable(Routes.Devices) {
            val account by model.account.collectAsState()
            LinkDeviceScreen(
                model,
                linkState,
                account,
                appearance.language,
                onBack = nav::popBackStack,
                onOpenDocument = { nav.navigate(Routes.document(it)) },
            )
        }
        composable(Routes.Profiles) {
            val visible by model.visibleProfiles.collectAsState()
            val account by model.account.collectAsState()
            val cloudSync by model.cloudSync.collectAsState()
            ProfilesScreen(
                visible.rows,
                hiddenCount = visible.hiddenCount,
                cloudSync = cloudSync,
                activeAccountId = account.activeAccountId,
                onOpen = { model.selectProfile(it); nav.navigate(Routes.workspaces(it)) },
                onRename = model::renameProfile,
                onSetCloudSync = { profile, enabled ->
                    model.setProfileCloudSync(profile, enabled, account.activeScope)
                },
                onSyncStatus = { nav.navigate(Routes.cloudProgress(it)) },
                onDevices = { nav.navigate(Routes.Devices) },
                onSettings = { nav.navigate(Routes.Settings) },
                onAccount = { nav.navigate(Routes.Account) },
            )
        }
        composable(Routes.CloudProgress, arguments = listOf(navArgument("profileId") { type = NavType.StringType })) { entry ->
            val id = ProfileId(entry.arguments?.getString("profileId").orEmpty())
            val visible by model.visibleProfiles.collectAsState()
            val account by model.account.collectAsState()
            val cloudSync by model.cloudSync.collectAsState()
            val profile = visible.rows.firstOrNull { it.id == id }
            // The same answer the Cloud sync row gives for this Profile: bound
            // to the signed-in account, this build carries Cloud, and the
            // switch is on. Anything else is the Off screen, in that row's words.
            val row = profile?.let { cloudSync.row(it.id, it.boundAccountId(), account.activeAccountId) }
            val enabled = row?.on == true
            val unattached = row?.detail is CloudSyncDetail.Unattached
            val presenter = model.cloudProgress
            LaunchedEffect(id, enabled, unattached) { presenter.show(id, enabled, unattached) }
            // Reads only while the screen is visible: the poll runs under
            // STARTED and is cancelled with it, so a backgrounded app holds no
            // timer (D244). The reads themselves run on the IO dispatcher.
            val lifecycle = LocalLifecycleOwner.current.lifecycle
            LaunchedEffect(presenter, lifecycle) {
                lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) { presenter.poll() }
            }
            val progress by presenter.state.collectAsStateWithLifecycle()
            CloudProgressScreen(
                profileName = profile?.displayLabel.orEmpty(),
                state = progress,
                onToggle = presenter::toggle,
                onBack = nav::popBackStack,
            )
        }
        composable(Routes.Workspaces, arguments = listOf(navArgument("profileId") { type = NavType.StringType })) { entry ->
            val id = ProfileId(entry.arguments?.getString("profileId").orEmpty())
            LaunchedEffect(id) { model.selectProfile(id) }
            val rows by model.workspaces.collectAsState()
            val account by model.account.collectAsState()
            WorkspacesScreen(
                rows = rows,
                throttled = account.signedIn && account.enrollments.any { it.id == id && it.failure == "enrollment_rate_limited" },
                onBack = nav::popBackStack,
                onOpen = { model.selectWorkspace(it); nav.navigate(Routes.documents(it)) },
                onRename = model::renameWorkspace,
                onArchive = model::setWorkspaceArchived,
                onDelete = model::deleteWorkspace,
                onRestore = model::restoreWorkspace,
            )
        }
        composable(Routes.Documents, arguments = listOf(navArgument("workspaceId") { type = NavType.StringType })) { entry ->
            val id = WorkspaceId(entry.arguments?.getString("workspaceId").orEmpty())
            LaunchedEffect(id) { model.selectWorkspace(id) }
            val rows by model.documents.collectAsState()
            // Collected with the lifecycle, so the read stops when the app
            // stops: D244 says a phone holds no timer of its own while
            // backgrounded, and a `while (true)` on a ViewModel scope would be
            // exactly that.
            val transfers by model.observeTransfers(id)
                .collectAsStateWithLifecycle(initialValue = null)
            val lastRelease by model.lastRelease.collectAsState()
            val context = LocalContext.current
            DocumentsScreen(
                rows,
                transfers = transfers,
                onAllowOnCellular = { model.setPicturesOnCellular(true) },
                onClearLocal = { model.clearLocalFiles(id) },
                lastRelease = lastRelease,
                onDismissRelease = model::dismissRelease,
                onBack = nav::popBackStack,
                onOpen = { nav.navigate(Routes.document(it)) },
                onOpenNote = { file -> model.openFileNote(file) { nav.navigate(Routes.document(it)) } },
                onRename = model::renameDocument,
                onTrash = model::moveDocumentToTrash,
                fileDetails = model::fileBlob,
                // The export waits for the bytes, then Android chooses the
                // handler. A phone with nothing that plays this MIME is a
                // sentence, not a crash.
                onOpenFile = { file ->
                    model.exportFile(file) { exported, mime ->
                        if (!openExportedFile(context, exported, mime, share = false)) {
                            model.reportNoHandler()
                        }
                    }
                },
                onShareFile = { file ->
                    model.exportFile(file) { exported, mime ->
                        openExportedFile(context, exported, mime, share = true)
                    }
                },
            )
        }
        composable(Routes.Document, arguments = listOf(navArgument("documentId") { type = NavType.StringType })) { entry ->
            val id = DocumentId(entry.arguments?.getString("documentId").orEmpty())
            LaunchedEffect(id) { model.openDocument(id) }
            val document by model.document.collectAsState()
            val uri = LocalUriHandler.current
            DocumentViewer(document, model, nav::popBackStack) { reference ->
                when (reference) {
                    is EntityReference.Document -> nav.navigate(Routes.document(reference.id))
                    is EntityReference.Block -> reference.documentId?.let { nav.navigate(Routes.document(it)) }
                    is EntityReference.Run -> nav.navigate(Routes.run(reference.documentId))
                    is EntityReference.Url -> runCatching { uri.openUri(reference.value) }
                    is EntityReference.User, is EntityReference.Label -> Unit
                }
            }
        }
        composable(Routes.Run, arguments = listOf(navArgument("documentId") { type = NavType.StringType })) { entry ->
            val id = DocumentId(entry.arguments?.getString("documentId").orEmpty())
            LaunchedEffect(id) { model.openRun(id) }
            DisposableEffect(id) { onDispose { model.closeRun() } }
            val run by model.run.collectAsState()
            RunScreen(run, model::runTimeline, nav::popBackStack)
        }
        composable(Routes.Account) {
            val account by model.account.collectAsState()
            val renderedScope = account.activeScope
            val localProfiles by model.visibleProfiles.collectAsState()
            AccountScreen(account, localProfiles.rows,
                onAction = { intent, profile -> model.accountAction(intent, appearance.language, renderedScope, profile) },
                onOffline = { nav.popBackStack(Routes.Profiles, false) },
                onBack = { nav.popBackStack() },
                onOpenLocal = { id ->
                    if (localProfiles.rows.any { it.id == id }) { model.selectProfile(id); nav.navigate(Routes.workspaces(id)) }
                },
                onSwitchAccount = { model.accountAction(AccountIntent.SwitchAccount, appearance.language, renderedScope, accountId = it) },
                hiddenProfileCount = localProfiles.hiddenCount,
                onSelectProfiles = { model.accountAction(AccountIntent.SelectProfiles, appearance.language, renderedScope, profiles = it) })
        }
        composable(Routes.Diagnostics) {
            val account by model.accountDiagnostics.collectAsState()
            DiagnosticsScreen(state, implementation, account, nav::popBackStack)
        }
        composable(Routes.Settings) {
            val picturesOnCellular by model.picturesOnCellular.collectAsState()
            SettingsScreen(
                appearance = appearance,
                onAppearanceChanged = onAppearanceChanged,
                onBack = nav::popBackStack,
                onDiagnostics = { nav.navigate(Routes.Diagnostics) },
                onAccount = { nav.navigate(Routes.Account) },
                onLicenses = { nav.navigate(Routes.Licenses) },
                picturesOnCellular = picturesOnCellular,
                onPicturesOnCellularChanged = model::setPicturesOnCellular,
            )
        }
        composable(Routes.Licenses) {
            OpenSourceLicensesScreen(onBack = nav::popBackStack, onOpen = { nav.navigate(Routes.license(it)) })
        }
        composable(Routes.License, arguments = listOf(navArgument("file") { type = NavType.StringType })) { entry ->
            OpenSourceLicenseDocumentScreen(entry.arguments?.getString("file").orEmpty(), nav::popBackStack)
        }
    }

    state.error?.let { failure ->
        AlertDialog(
            onDismissRequest = model::clearError,
            confirmButton = { TextButton(onClick = model::clearError) { Text(stringResource(R.string.ok)) } },
            title = { Text(stringResource(R.string.error_title)) },
            text = { Text(localizedFailure(failure)) },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LinkDeviceScreen(
    model: AppViewModel,
    state: DeviceLinkState,
    account: AccountScreenState,
    locale: String,
    onBack: (() -> Unit)?,
    onOpenDocument: (DocumentId) -> Unit,
) {
    val context = LocalContext.current
    val devices by model.linkedDevices.collectAsState()
    val thisDevice by model.thisDevice.collectAsState()
    val relinkAvailable by model.relinkAvailable.collectAsState()
    val startedLinked = remember { state is DeviceLinkState.Linked }
    var tab by remember { mutableIntStateOf(if (state is DeviceLinkState.Linked) 2 else 1) }
    var code by remember { mutableStateOf("") }
    var offer by remember { mutableStateOf<app.koru.mobile.services.PairingOffer?>(null) }
    var offerError by remember { mutableStateOf<String?>(null) }
    var offerAttempt by remember { mutableIntStateOf(0) }
    LaunchedEffect(state) {
        if (!startedLinked && state is DeviceLinkState.Linked) onBack?.invoke()
    }
    LaunchedEffect(tab, offerAttempt) {
        if (tab == 0) runCatching { model.startPairing() }
            .onSuccess { offer = it; offerError = null }
            .onFailure { offerError = it.message ?: "unknown_error" }
    }
    DisposableEffect(tab) { onDispose { if (tab == 0) model.cancelPairing() } }
    Scaffold(topBar = { TopAppBar(
        title = { Text(stringResource(R.string.link_title)) },
        navigationIcon = { if (onBack != null) TextButton(onClick = onBack) { Text(stringResource(R.string.back)) } },
    ) }) { padding ->
        Column(
            Modifier.fillMaxSize().padding(padding),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            // Above the tabs rather than inside one: the mismatch is a fact
            // about the whole screen — none of the three tabs can do its job
            // while it holds — where iOS, whose devices screen is one flat
            // list, puts the same block in its state row.
            if (state is DeviceLinkState.IdentityMismatch) {
                IdentityMismatchBanner(model::resetDeviceIdentity)
            }
            PrimaryTabRow(selectedTabIndex = tab) {
                listOf(R.string.new_device, R.string.enter_link_code, R.string.linked_devices)
                    .forEachIndexed { index, label ->
                        Tab(selected = tab == index, onClick = { tab = index }, text = { Text(stringResource(label)) })
                    }
            }
            when (tab) {
                0 -> Column(Modifier.fillMaxWidth().padding(KoruSpacing.xl), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(stringResource(R.string.scan_this_code))
                    Spacer(Modifier.height(KoruSpacing.lg))
                    offer?.let { current ->
                        Image(
                            qrBitmap(current.payload).asImageBitmap(),
                            stringResource(R.string.device_link_qr),
                            Modifier.size(260.dp),
                        )
                        Spacer(Modifier.height(KoruSpacing.md))
                        Button(onClick = {
                            // The clip's *label* is a debugging aid the system
                            // shows in the clipboard editor, not a sentence
                            // anybody reads in Koru — it stays English on
                            // purpose, like a log line. The QR keeps the
                            // https://koru.do/link-device# link so a camera
                            // opens Koru; the clipboard gets the code alone.
                            (context.getSystemService(ClipboardManager::class.java)).setPrimaryClip(ClipData.newPlainText("Koru device link", current.payload.substringAfter('#')))
                            Toast.makeText(context, R.string.code_copied, Toast.LENGTH_SHORT).show()
                        }) { Text(stringResource(R.string.copy_link_code)) }
                        // The count goes through the resource rather than a
                        // Kotlin template, so the digits are the language's:
                        // `Resources.getString` formats against the
                        // configuration's locale and `"$n"` never does.
                        Text(
                            pluralStringResource(
                                R.plurals.code_expires_in,
                                current.expiresInSeconds,
                                current.expiresInSeconds,
                            ),
                            style = MaterialTheme.typography.bodySmall,
                        )
                    } ?: offerError?.let { error ->
                        Column(horizontalAlignment = Alignment.CenterHorizontally) {
                            Text(
                                stringResource(R.string.link_code_failed, error),
                                color = MaterialTheme.colorScheme.error,
                            )
                            TextButton(onClick = {
                                offerError = null
                                offerAttempt += 1
                            }) { Text(stringResource(R.string.try_again)) }
                        }
                    } ?: CircularProgressIndicator()
                }
                1 -> Column(Modifier.fillMaxWidth().padding(KoruSpacing.xl), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(stringResource(R.string.link_explainer))
                    Spacer(Modifier.height(KoruSpacing.lg))
                    if (state is DeviceLinkState.Linking) CircularProgressIndicator()
                    QrScanButton(
                        // `linkingBlocked` covers Linking and Unavailable as well as
                        // the identity mismatch (ADR-413's guard), so it is the one
                        // condition rather than a second spelling of part of it.
                        enabled = !state.linkingBlocked,
                        onScanned = model::linkDevice,
                    )
                    Spacer(Modifier.height(KoruSpacing.md))
                OutlinedTextField(
                    value = code,
                    onValueChange = { code = it },
                    label = { Text(stringResource(R.string.enter_link_code)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Spacer(Modifier.height(KoruSpacing.sm))
                Button(
                    onClick = { model.linkDevice(code) },
                    enabled = code.isNotBlank() && !state.linkingBlocked,
                ) { Text(stringResource(R.string.link_device)) }
                }
                else -> LinkedDevicesList(
                    thisDevice,
                    devices,
                    model.relayEnabled.collectAsState().value,
                    model.relayStatus.collectAsState().value,
                    model::setRelayEnabled,
                    model::renameThisDevice,
                    model::unlinkDevice,
                    model::deviceHistory,
                    onOpenDocument,
                    account,
                    relinkAvailable,
                    {
                        model.accountAction(
                            AccountIntent.RelinkDevices,
                            locale,
                            account.activeScope,
                            profiles = account.profiles.map { it.id },
                        )
                    },
                )
            }
        }
    }
}

/**
 * The device-identity mismatch, named rather than hidden behind a disabled
 * button, with the one action that leads out of it.
 *
 * Three things it has to say and does: the identity is what broke, the content
 * is not, and relinking is the fix. The last of those is why the confirmation
 * mentions the other devices — the old entry stays listed offline on every one
 * of them and this phone has no key left to sign a revocation with, so nobody
 * else will tell them.
 *
 * *Start over* is reached only from here, and here appears only in the
 * mismatch state — plus a confirmation, so the destructive-sounding label is
 * never one stray tap away. Two guards below this one make it safe rather than
 * merely awkward: it destroys nothing (the old identity is moved aside on
 * disk) and the native host refuses it outright on a healthy device.
 */
@Composable private fun IdentityMismatchBanner(onStartOver: () -> Unit) {
    var confirming by remember { mutableStateOf(false) }
    Column(
        Modifier
            .fillMaxWidth()
            .background(MaterialTheme.colorScheme.errorContainer)
            .padding(horizontal = KoruSpacing.md, vertical = KoruSpacing.sm),
    ) {
        Text(
            stringResource(R.string.identity_mismatch),
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.onErrorContainer,
        )
        Spacer(Modifier.height(KoruSpacing.xs))
        Text(
            stringResource(R.string.identity_mismatch_explainer),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onErrorContainer,
        )
        TextButton(
            onClick = { confirming = true },
            colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
        ) { Text(stringResource(R.string.start_over_as_new_device)) }
    }
    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            title = { Text(stringResource(R.string.start_over_as_new_device)) },
            text = { Text(stringResource(R.string.start_over_explainer)) },
            confirmButton = {
                TextButton(onClick = { confirming = false; onStartOver() }) {
                    Text(
                        stringResource(R.string.start_over_confirm),
                        color = MaterialTheme.colorScheme.error,
                    )
                }
            },
            dismissButton = {
                TextButton(onClick = { confirming = false }) {
                    Text(stringResource(R.string.cancel))
                }
            },
        )
    }
}
/** The relay status row's text, the desktop tray's rendering word for word:
 *  `<host> — <state>`, the host never translated, the state word from the
 *  same translation table and the separator living here. Off carries no
 *  host worth naming — the switch beside it already says which switch — so
 *  it is the localized whole-row text alone. */
@Composable private fun relayStatusLine(status: RelayStatus): String = when (status.state) {
    RelayState.Off -> stringResource(R.string.relay_state_off)
    RelayState.Connecting -> "${status.host} — " + stringResource(R.string.relay_state_connecting)
    RelayState.Connected -> "${status.host} — " + stringResource(R.string.relay_state_connected)
    RelayState.Disconnected -> "${status.host} — " + stringResource(R.string.relay_state_disconnected)
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun LinkedDevicesList(
    thisDevice: LinkedDevice?,
    devices: List<LinkedDevice>,
    relayEnabled: Boolean,
    relayStatus: RelayStatus?,
    setRelayEnabled: (Boolean) -> Unit,
    renameThisDevice: (String) -> Unit,
    unlink: (String) -> Unit,
    loadHistory: suspend (String, Long?) -> app.koru.mobile.services.DeviceHistoryPage,
    onOpenDocument: (DocumentId) -> Unit,
    account: AccountScreenState,
    relinkAvailable: Boolean?,
    relinkDevices: () -> Unit,
) {
    var renaming by remember { mutableStateOf(false) }
    var selectedDevice by remember { mutableStateOf<LinkedDevice?>(null) }
    var history by remember { mutableStateOf<List<DeviceStateEvent>>(emptyList()) }
    var historyNext by remember { mutableStateOf<Long?>(null) }
    var historyLoading by remember { mutableStateOf(false) }
    var historyError by remember { mutableStateOf<String?>(null) }
    if (thisDevice == null && devices.isEmpty()) {
        return EmptyState(Modifier, stringResource(R.string.no_linked_devices))
    }
    LazyColumn(Modifier.fillMaxSize()) {
        thisDevice?.let { device ->
            item("this_device") {
                ListItem(
                    // Long-press renames this device, the way a long-press
                    // renames a workspace.
                    modifier = Modifier.combinedClickable(
                        onClick = {},
                        onLongClick = { renaming = true },
                    ),
                    headlineContent = { Text(device.displayName) },
                    supportingContent = {
                        Text(
                            listOfNotNull(
                                stringResource(R.string.this_device),
                                device.buildLabel,
                                stringResource(R.string.long_press_to_rename),
                            ).joinToString(" · "),
                        )
                    },
                )
                HorizontalDivider()
            }
        }
        // The device-local relay switch (iroh-relay roadmap, decision №6):
        // applies to the live host immediately, persists in SharedPreferences,
        // never syncs. Off leaves same-network and direct sync working.
        item("use_relay") {
            ListItem(
                headlineContent = { Text(stringResource(R.string.use_relay)) },
                supportingContent = {
                    Column {
                        Text(stringResource(R.string.use_relay_explainer))
                        // The connection as the host reads it now, refreshed
                        // by the same 1 s devices poll as the rows below —
                        // `<host> — <state>`, the desktop tray's rendering;
                        // Off carries no host worth naming, so the localized
                        // whole-row text stands alone. Absent (an old host,
                        // or none yet) shows nothing rather than a guess.
                        relayStatus?.let { status -> Text(relayStatusLine(status)) }
                    }
                },
                trailingContent = {
                    Switch(checked = relayEnabled, onCheckedChange = setRelayEnabled)
                },
            )
            HorizontalDivider()
        }
        if (account.showRelinkDevices(relinkAvailable)) {
            item("relink_devices") {
                ListItem(
                    headlineContent = { Text(stringResource(
                        if (account.relinkWaiting) R.string.relink_devices_waiting_state
                        else R.string.relink_devices,
                    )) },
                    supportingContent = {
                        Text(stringResource(
                            when {
                                account.relinkWaiting -> R.string.relink_devices_waiting
                                account.relinkLoading(relinkAvailable) -> R.string.relink_devices_loading
                                else -> R.string.relink_devices_explainer
                            },
                        ))
                    },
                    trailingContent = {
                        if (account.relinkWaiting) {
                            CircularProgressIndicator(Modifier.size(24.dp))
                        } else {
                            TextButton(
                                enabled = account.canRelinkDevices(relinkAvailable),
                                onClick = relinkDevices,
                            ) { Text(stringResource(R.string.relink_devices_action)) }
                        }
                    },
                    modifier = Modifier.semantics { liveRegion = LiveRegionMode.Polite },
                )
                HorizontalDivider()
            }
        }
        items(devices) { device ->
            // `getRelativeTimeSpanString` reads `Locale.getDefault()`, which
            // `AppearanceStore.applyLanguage` sets — so "3 minutes ago"
            // follows the language chosen *in Koru*, not the phone's. The
            // sentence around it has to be a resource for the same reason:
            // word order is not the same in all eight.
            // `connection` is a wire value — "direct", "relay" — and drawing
            // it as it arrived is how an English word ends up inside a
            // Japanese sentence. iOS branches on the same two; a third value
            // this build has not heard of is shown as it came rather than
            // guessed at.
            val path = when (device.connection) {
                "direct" -> stringResource(R.string.connection_direct)
                "relay" -> stringResource(R.string.connection_relay)
                else -> device.connection
            }
            val presence = when {
                device.connected && path != null ->
                    stringResource(R.string.device_online_via, path)
                device.connected -> stringResource(R.string.device_online)
                device.lastSeenMillis != null -> stringResource(
                    R.string.device_offline_last_seen,
                    DateUtils.getRelativeTimeSpanString(device.lastSeenMillis).toString(),
                )
                else -> stringResource(R.string.device_offline)
            }
            // Two devices on different peer protocol numbers do not sync: that
            // leads the line and names the side the update is on. Then the
            // relationship, then what the device runs — a device that never
            // said shows nothing rather than a guess.
            val notSyncing = when (device.updateNeededOn) {
                null -> null
                "this_device" -> stringResource(R.string.not_syncing_update_this_device)
                else -> stringResource(R.string.not_syncing_update_peer, device.displayName)
            }
            val anotherInstallation = isAnotherInstallation(thisDevice, device)
            // Both sides localized the line that base hardcoded as "via $it".
            // Ours is dev's sentence plus where to undo it, so it keeps both
            // intents: `device_via_unlink`, not `introduced_by`.
            val introduced = device.introducedBy?.let { stringResource(R.string.device_via_unlink, it) }
            val status = listOfNotNull(notSyncing, presence, introduced, device.buildLabel).joinToString(" · ")
            val sync = device.lastSyncMillis?.let {
                stringResource(
                    R.string.last_synced,
                    DateUtils.getRelativeTimeSpanString(it).toString(),
                )
            } ?: stringResource(R.string.no_state_changes)
            ListItem(
                modifier = Modifier.clickable { selectedDevice = device },
                headlineContent = {
                    Text(if (anotherInstallation) {
                        stringResource(R.string.device_another_installation, device.displayName)
                    } else device.displayName)
                },
                supportingContent = {
                    // A minor version apart in the tertiary accent (Material
                    // has no warning slot), a major apart or incompatible in
                    // the error color. The row itself is the same row.
                    val color = when (device.buildSeverity) {
                        "danger" -> MaterialTheme.colorScheme.error
                        "warning" -> MaterialTheme.colorScheme.tertiary
                        else -> Color.Unspecified
                    }
                    Text("$status\n$sync", color = color)
                },
                // A transitive device arrived through someone else's pairing, so
                // it offers no Unlink here — it disappears when the introducing
                // relationship is revoked.
                trailingContent = if (device.introducedBy == null) {
                    { TextButton(onClick = { unlink(device.deviceId) }) { Text(stringResource(R.string.unlink_device)) } }
                } else null,
            )
            HorizontalDivider()
        }
    }
    if (renaming && thisDevice != null) {
        RenameDeviceDialog(
            current = thisDevice.displayName,
            dismiss = { renaming = false },
            rename = { name ->
                renaming = false
                renameThisDevice(name)
            },
        )
    }
    selectedDevice?.let { device ->
        LaunchedEffect(device.deviceId) {
            history = emptyList()
            historyNext = null
            historyError = null
            historyLoading = true
            runCatching { loadHistory(device.deviceId, null) }
                .onSuccess { page ->
                    history = page.events
                    historyNext = page.nextBefore
                }
                .onFailure { historyError = it.message ?: "history_unavailable" }
            historyLoading = false
        }
        ModalBottomSheet(onDismissRequest = { selectedDevice = null }) {
            Column(Modifier.fillMaxWidth().padding(horizontal = KoruSpacing.xl)) {
                Text(device.displayName, style = MaterialTheme.typography.titleLarge)
                Text(
                    device.lastSyncMillis?.let {
                        stringResource(
                            R.string.last_synced,
                            DateUtils.getRelativeTimeSpanString(it).toString(),
                        )
                    } ?: stringResource(R.string.no_state_changes),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Spacer(Modifier.height(KoruSpacing.md))
                if (historyLoading && history.isEmpty()) {
                    CircularProgressIndicator()
                } else if (historyError != null && history.isEmpty()) {
                    Text(
                        stringResource(R.string.history_unavailable),
                        color = MaterialTheme.colorScheme.error,
                    )
                } else if (history.isEmpty()) {
                    Text(stringResource(R.string.history_empty))
                } else {
                    val historyList = rememberLazyListState()
                    LaunchedEffect(historyList, device.deviceId) {
                        snapshotFlow {
                            Triple(
                                historyList.layoutInfo.visibleItemsInfo.lastOrNull()?.index,
                                historyNext,
                                historyLoading,
                            )
                        }
                            .distinctUntilChanged()
                            .collect { (last, before, loading) ->
                                if (before != null && last != null && last >= history.lastIndex - 2 && !loading) {
                                    historyLoading = true
                                    runCatching { loadHistory(device.deviceId, before) }
                                        .onSuccess { page ->
                                            val known = history.mapNotNull { it.eventId }.toSet()
                                            history = history + page.events.filter { it.eventId !in known }
                                            historyNext = page.nextBefore
                                        }
                                        .onFailure {
                                            historyError = it.message ?: "history_unavailable"
                                            historyNext = null
                                        }
                                    historyLoading = false
                                }
                            }
                    }
                    LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp), state = historyList) {
                        items(history) { event ->
                            val linkedDocument = event.documentId
                                ?.takeIf { event.kind == "document_updated" }
                            ListItem(
                                modifier = Modifier.clickable(enabled = linkedDocument != null) {
                                    selectedDevice = null
                                    onOpenDocument(DocumentId(linkedDocument!!))
                                },
                                headlineContent = { Text(deviceEventLabel(event)) },
                                supportingContent = {
                                    Text(DateUtils.getRelativeTimeSpanString(event.atMillis).toString())
                                },
                                colors = ListItemDefaults.colors(
                                    headlineColor = if (linkedDocument != null) {
                                        MaterialTheme.colorScheme.primary
                                    } else {
                                        MaterialTheme.colorScheme.onSurface
                                    },
                                ),
                            )
                            HorizontalDivider()
                        }
                        if (historyLoading) item { LinearProgressIndicator(Modifier.fillMaxWidth()) }
                    }
                }
                Spacer(Modifier.height(KoruSpacing.xl))
            }
        }
    }
}

/** Clearing app storage creates a new physical DeviceId. A same-named peer is
 * therefore another installation, never this one resurrected from gossip. */
internal fun isAnotherInstallation(current: LinkedDevice?, peer: LinkedDevice): Boolean =
    current != null && current.deviceId != peer.deviceId && current.displayName == peer.displayName

/**
 * One history row's sentence.
 *
 * `event.kind` is the semantic value — it arrives on the wire and is what the
 * `when` branches on; the words beside it are drawn from resources and never
 * stored. The last arm is the honest gap: a kind this build has not heard of
 * is printed as its own identifier rather than as a guessed sentence, which is
 * the same choice `unsupported_block` makes.
 */
@Composable
private fun deviceEventLabel(event: DeviceStateEvent): String {
    if (event.kind == "document_updated") {
        val name = event.documentName?.takeIf { it.isNotBlank() }
            ?: stringResource(R.string.untitled)
        val updated = stringResource(R.string.event_document_updated_named, name)
        val operations = event.items
            ?.let { pluralStringResource(R.plurals.operation_count, it, it) }
            ?: return updated
        return "$updated · $operations"
    }
    if (event.kind == "catalog_reconciled" && event.detail != null) {
        return stringResource(R.string.event_catalog_reconciled_detail, event.detail)
    }
    val target = when {
        event.documentId != null -> event.documentId.take(8)
        event.workspaceId != null -> event.workspaceId.take(8)
        event.profileId != null -> event.profileId.take(8)
        event.blobHash != null -> event.blobHash.take(12)
        else -> null
    }
    val kindLabel = when (event.kind) {
        "catalog_reconciled" -> R.string.event_catalog_reconciled
        "profile_created" -> R.string.event_profile_created
        "profile_updated" -> R.string.event_profile_updated
        "workspace_created" -> R.string.event_workspace_created
        "workspace_updated" -> R.string.event_workspace_updated
        "document_created" -> R.string.event_document_created
        "document_updated" -> R.string.event_document_updated
        "blob_stored" -> R.string.event_blob_stored
        else -> null
    }
    val label = kindLabel?.let { stringResource(it) }
        ?: event.kind.replace('_', ' ').replaceFirstChar(Char::uppercase)
    return target?.let { "$label · $it" } ?: label
}

@Composable
private fun RenameDeviceDialog(current: String, dismiss: () -> Unit, rename: (String) -> Unit) {
    var value by remember { mutableStateOf(current) }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(R.string.rename_device)) },
        text = {
            OutlinedTextField(
                value = value,
                onValueChange = { value = it },
                label = { Text(stringResource(R.string.device_name)) },
                singleLine = true,
            )
        },
        dismissButton = { TextButton(onClick = dismiss) { Text(stringResource(R.string.cancel)) } },
        confirmButton = {
            TextButton(
                onClick = { rename(value.trim()) },
                enabled = value.isNotBlank() && value.trim() != current,
            ) { Text(stringResource(R.string.rename)) }
        },
    )
}

private fun qrBitmap(payload: String): Bitmap {
    val matrix = QRCodeWriter().encode(payload, BarcodeFormat.QR_CODE, 720, 720)
    return Bitmap.createBitmap(matrix.width, matrix.height, Bitmap.Config.ARGB_8888).also { bitmap ->
        for (y in 0 until matrix.height) for (x in 0 until matrix.width) bitmap.setPixel(x, y, if (matrix[x, y]) android.graphics.Color.BLACK else android.graphics.Color.WHITE)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun ProfilesScreen(
    rows: List<Profile>,
    hiddenCount: Int = 0,
    cloudSync: CloudSyncState = CloudSyncState(),
    activeAccountId: String? = null,
    onOpen: (ProfileId) -> Unit,
    onRename: (ProfileId, String) -> Unit,
    onSetCloudSync: (ProfileId, Boolean) -> Unit = { _, _ -> },
    onSyncStatus: (ProfileId) -> Unit = {},
    onDevices: () -> Unit,
    onSettings: () -> Unit,
    onAccount: () -> Unit,
) {
    val attribution = rows.associate { it.id to localProfileAccountLabel(it) }
    var actionProfile by remember { mutableStateOf<Profile?>(null) }
    var profileForRename by remember { mutableStateOf<Profile?>(null) }
    Box(Modifier.fillMaxSize()) {
        EntityListScreen(
            stringResource(R.string.profiles), rows, { it.displayLabel }, { attribution[it.id] },
            { onOpen(it.id) }, onCreate = null, onBack = null, trailing = onDevices,
            trailingLabel = stringResource(R.string.devices),
            secondaryTrailing = onSettings,
            secondaryTrailingLabel = stringResource(R.string.settings),
            contentHeader = {
                TextButton(onClick = onAccount, modifier = Modifier.padding(horizontal = KoruSpacing.md)) {
                    Text(stringResource(R.string.account_title))
                }
                if (hiddenCount > 0) Text(stringResource(R.string.account_switcher_hidden, hiddenCount), Modifier.padding(horizontal = KoruSpacing.md))
            },
            onLongPress = { actionProfile = it },
            leadingContent = { profile ->
                val dark = MaterialTheme.colorScheme.background.luminance() < 0.5f
                val colors = profileTileColors(
                    profile.color,
                    profile.id.value,
                    dark,
                    MaterialTheme.colorScheme.onSurface,
                )
                BoxedEntityIcon(
                    emoji = profile.icon ?: profile.leadingEmoji,
                    label = profile.displayName,
                    containerColor = colors.background,
                    contentColor = colors.foreground,
                )
            },
        )
        // The release version on the first screen, quiet and out of the way —
        // the one answer "which build is on this phone?" should never need
        // Developer → Diagnostics for. The branch is what tells two installs
        // apart when they share a version; it is empty only when git was
        // unavailable at build time, and then the label just ends.
        val versionBranch = BuildConfig.GIT_BRANCH.takeIf { it.isNotBlank() }?.let { " - $it" }.orEmpty()
        Text(
            "v${BuildConfig.VERSION_NAME} #${BuildConfig.VERSION_CODE}$versionBranch",
            modifier = Modifier
                .align(Alignment.BottomEnd)
                .padding(KoruSpacing.md),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
    actionProfile?.let { profile ->
        ModalBottomSheet(onDismissRequest = { actionProfile = null }) {
            Text(
                profile.displayLabel,
                modifier = Modifier.padding(horizontal = KoruSpacing.lg, vertical = KoruSpacing.sm),
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            WorkspaceActionRow(
                label = stringResource(R.string.rename_profile),
                icon = Icons.Outlined.Edit,
                destructive = false,
                onClick = {
                    actionProfile = null
                    profileForRename = profile
                },
            )
            // Flipping the switch leaves the sheet open: the answer belongs to
            // this row, and the other rows stay usable while it is in flight.
            cloudSync.row(profile.id, profile.boundAccountId(), activeAccountId)?.let { row ->
                CloudSyncActionRow(
                    row = row,
                    onChange = { enabled -> onSetCloudSync(profile.id, enabled) },
                )
            }
            // For every Profile, not only a synced one: the screen is where a
            // person learns that this Profile has no Cloud sync, too.
            WorkspaceActionRow(
                label = stringResource(R.string.cloud_progress_row),
                icon = Icons.Outlined.Sync,
                destructive = false,
                onClick = {
                    actionProfile = null
                    onSyncStatus(profile.id)
                },
            )
            Spacer(Modifier.height(KoruSpacing.lg))
        }
    }
    profileForRename?.let { profile ->
        RenameProfileDialog(
            profile = profile,
            dismiss = { profileForRename = null },
            rename = { displayName ->
                profileForRename = null
                onRename(profile.id, displayName)
            },
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun WorkspacesScreen(
    rows: List<Workspace>,
    throttled: Boolean,
    onBack: () -> Unit,
    onOpen: (WorkspaceId) -> Unit,
    onRename: (WorkspaceId, String) -> Unit,
    onArchive: (WorkspaceId, Boolean) -> Unit,
    onDelete: (WorkspaceId) -> Unit,
    onRestore: (WorkspaceId) -> Unit,
) {
    var showArchived by remember { mutableStateOf(false) }
    var showDeleted by remember { mutableStateOf(false) }
    var showOptions by remember { mutableStateOf(false) }
    var sort by remember { mutableStateOf(WorkspaceSort.LastModified) }
    var actionWorkspace by remember { mutableStateOf<Workspace?>(null) }
    var debugWorkspace by remember { mutableStateOf<Workspace?>(null) }
    var workspaceForRename by remember { mutableStateOf<Workspace?>(null) }
    var workspaceForDelete by remember { mutableStateOf<Workspace?>(null) }
    val visible = sortWorkspaces(
        rows.filter { workspaceVisible(it, showArchived, showDeleted) },
        sort,
    )
    val banner = if (rows.isEmpty()) stringResource(if (throttled) R.string.account_enrollment_throttled else R.string.workspaces_sync_empty) else null
    EntityListScreen(
        stringResource(R.string.workspaces),
        visible,
        { it.displayTitle },
        { workspace ->
            when {
                workspace.deletedAtMillis != null -> stringResource(R.string.deleted)
                workspace.archived -> stringResource(R.string.archived)
                else -> null
            }
        },
        { workspace -> onOpen(workspace.id) },
        null,
        onBack,
        banner = banner,
        bannerProgress = false,
        headerActions = {
            IconButton(onClick = { showOptions = true }) {
                Icon(
                    Icons.Default.MoreVert,
                    contentDescription = stringResource(R.string.workspace_options),
                )
            }
        },
        onLongPress = { actionWorkspace = it },
        canOpen = { workspace -> workspace.deletedAtMillis == null },
        leadingContent = { workspace ->
            val dark = MaterialTheme.colorScheme.background.luminance() < 0.5f
            BoxedEntityIcon(
                emoji = workspace.icon ?: workspace.leadingEmoji,
                label = workspace.title,
                containerColor = workspaceTileColor(workspace.color, dark)
                    ?: MaterialTheme.colorScheme.surfaceVariant,
                contentColor = MaterialTheme.colorScheme.onSurface,
            )
        },
    )
    if (showOptions) {
        ModalBottomSheet(onDismissRequest = { showOptions = false }) {
            Text(
                stringResource(R.string.workspace_options),
                modifier = Modifier.padding(horizontal = KoruSpacing.lg, vertical = KoruSpacing.sm),
                style = MaterialTheme.typography.titleLarge,
            )
            WorkspaceFilterRow(
                label = stringResource(R.string.show_archived),
                checked = showArchived,
                onCheckedChange = { showArchived = it },
            )
            WorkspaceFilterRow(
                label = stringResource(R.string.show_deleted),
                checked = showDeleted,
                onCheckedChange = { showDeleted = it },
            )
            HorizontalDivider(Modifier.padding(vertical = KoruSpacing.sm))
            Text(
                stringResource(R.string.sort_by),
                modifier = Modifier.padding(horizontal = KoruSpacing.lg, vertical = KoruSpacing.sm),
                style = MaterialTheme.typography.titleMedium,
            )
            WorkspaceSortRow(
                label = stringResource(R.string.last_modified),
                selected = sort == WorkspaceSort.LastModified,
                onSelect = { sort = WorkspaceSort.LastModified },
            )
            WorkspaceSortRow(
                label = stringResource(R.string.sort_az),
                selected = sort == WorkspaceSort.TitleAscending,
                onSelect = { sort = WorkspaceSort.TitleAscending },
            )
            WorkspaceSortRow(
                label = stringResource(R.string.sort_za),
                selected = sort == WorkspaceSort.TitleDescending,
                onSelect = { sort = WorkspaceSort.TitleDescending },
            )
            Spacer(Modifier.height(KoruSpacing.lg))
        }
    }
    actionWorkspace?.let { workspace ->
        ModalBottomSheet(onDismissRequest = { actionWorkspace = null }) {
            Text(
                workspace.title,
                modifier = Modifier.padding(horizontal = KoruSpacing.lg, vertical = KoruSpacing.sm),
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            workspaceActions(workspace).forEach { action ->
                val label = when (action) {
                    WorkspaceAction.Rename -> stringResource(R.string.rename_workspace)
                    WorkspaceAction.Archive -> stringResource(R.string.archive_workspace)
                    WorkspaceAction.Unarchive -> stringResource(R.string.unarchive_workspace)
                    WorkspaceAction.Delete -> stringResource(R.string.delete_workspace)
                    WorkspaceAction.Restore -> stringResource(R.string.restore_workspace)
                    WorkspaceAction.DebugInfo -> stringResource(R.string.debug_info)
                }
                val icon = when (action) {
                    WorkspaceAction.Rename -> Icons.Outlined.Edit
                    WorkspaceAction.Archive -> Icons.Outlined.Archive
                    WorkspaceAction.Unarchive -> Icons.Outlined.Unarchive
                    WorkspaceAction.Delete -> Icons.Outlined.Delete
                    WorkspaceAction.Restore -> Icons.Outlined.RestoreFromTrash
                    WorkspaceAction.DebugInfo -> Icons.Outlined.BugReport
                }
                WorkspaceActionRow(
                    label = label,
                    icon = icon,
                    destructive = action == WorkspaceAction.Delete,
                    onClick = {
                        actionWorkspace = null
                        when (action) {
                            WorkspaceAction.Rename -> workspaceForRename = workspace
                            WorkspaceAction.Archive -> onArchive(workspace.id, true)
                            WorkspaceAction.Unarchive -> onArchive(workspace.id, false)
                            WorkspaceAction.Delete -> workspaceForDelete = workspace
                            WorkspaceAction.Restore -> onRestore(workspace.id)
                            WorkspaceAction.DebugInfo -> debugWorkspace = workspace
                        }
                    },
                )
            }
            Spacer(Modifier.height(KoruSpacing.lg))
        }
    }
    debugWorkspace?.let { workspace ->
        WorkspaceDebugDialog(workspace = workspace, dismiss = { debugWorkspace = null })
    }
    workspaceForRename?.let { workspace ->
        RenameWorkspaceDialog(
            workspace = workspace,
            dismiss = { workspaceForRename = null },
            rename = { title ->
                workspaceForRename = null
                onRename(workspace.id, title)
            },
        )
    }
    workspaceForDelete?.let { workspace ->
        DeleteWorkspaceDialog(
            workspace = workspace,
            dismiss = { workspaceForDelete = null },
            delete = {
                workspaceForDelete = null
                onDelete(workspace.id)
            },
        )
    }
}

@Composable
private fun WorkspaceActionRow(
    label: String,
    icon: ImageVector,
    destructive: Boolean,
    onClick: () -> Unit,
) {
    val color = if (destructive) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface
    ListItem(
        headlineContent = { Text(label, color = color) },
        leadingContent = { Icon(icon, contentDescription = null, tint = color) },
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick),
    )
}

/** The per-Profile Cloud sync switch, with the one line that says what Koru
 *  Cloud is actually doing for this Profile underneath it. A refusal shows its
 *  stable code verbatim — it is what a person reads back to us, so translating
 *  or prettifying it would lose the only thing it is for. */
@Composable
private fun CloudSyncActionRow(
    row: app.koru.mobile.services.CloudSyncRow,
    onChange: (Boolean) -> Unit,
) {
    val detail = when (val value = row.detail) {
        is CloudSyncDetail.Idle -> null
        is CloudSyncDetail.Working -> stringResource(R.string.account_cloud_sync_working)
        is CloudSyncDetail.Complete -> stringResource(R.string.account_cloud_sync_complete)
        is CloudSyncDetail.Partial ->
            stringResource(R.string.account_cloud_sync_partial, value.synced, value.total)
        is CloudSyncDetail.Failed -> value.code
        is CloudSyncDetail.Unattached -> stringResource(R.string.account_cloud_sync_unattached)
    }
    val color = if (row.interactive) {
        MaterialTheme.colorScheme.onSurface
    } else {
        MaterialTheme.colorScheme.onSurfaceVariant
    }
    ListItem(
        headlineContent = { Text(stringResource(R.string.account_cloud_sync), color = color) },
        supportingContent = detail?.let {
            {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = if (row.detail is CloudSyncDetail.Failed) {
                        MaterialTheme.colorScheme.error
                    } else {
                        MaterialTheme.colorScheme.onSurfaceVariant
                    },
                )
            }
        },
        leadingContent = { Icon(Icons.Outlined.CloudQueue, contentDescription = null, tint = color) },
        trailingContent = {
            Switch(checked = row.on, onCheckedChange = onChange, enabled = row.interactive)
        },
        modifier = Modifier.fillMaxWidth(),
    )
}

@Composable
private fun WorkspaceDebugDialog(workspace: Workspace, dismiss: () -> Unit) {
    val context = LocalContext.current
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(workspace.displayTitle) },
        text = {
            Column {
                DebugRow(stringResource(R.string.workspace_id), workspace.id.value)
                DebugRow(stringResource(R.string.profile_id), workspace.profileId.value)
                DebugRow(
                    stringResource(R.string.archived),
                    stringResource(if (workspace.archived) R.string.yes else R.string.no),
                )
                DebugRow(
                    stringResource(R.string.deleted),
                    workspace.deletedAtMillis
                        ?.let { DateUtils.getRelativeTimeSpanString(it).toString() }
                        ?: stringResource(R.string.no),
                )
                DebugRow(
                    stringResource(R.string.modified),
                    if (workspace.modifiedAtMillis > 0) {
                        DateUtils.getRelativeTimeSpanString(workspace.modifiedAtMillis).toString()
                    } else {
                        "—"
                    },
                )
                DebugRow(stringResource(R.string.icon), workspace.icon ?: "—")
                DebugRow(stringResource(R.string.color), workspace.color ?: "—")
            }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text(stringResource(R.string.cancel)) } },
        confirmButton = {
            TextButton(onClick = {
                (context.getSystemService(ClipboardManager::class.java))
                    .setPrimaryClip(ClipData.newPlainText("Koru workspace id", workspace.id.value))
                Toast.makeText(context, R.string.workspace_id_copied, Toast.LENGTH_SHORT).show()
            }) { Text(stringResource(R.string.copy_workspace_id)) }
        },
    )
}

@Composable
private fun DebugRow(label: String, value: String) {
    Row(Modifier.fillMaxWidth().padding(vertical = KoruSpacing.xs)) {
        Text(label, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.weight(1f))
        Text(value, style = MaterialTheme.typography.bodySmall, maxLines = 1, overflow = TextOverflow.Ellipsis)
    }
}

@Composable
private fun RenameProfileDialog(profile: Profile, dismiss: () -> Unit, rename: (String) -> Unit) {
    // Seeded with the raw name, not the display label, so a leading emoji is
    // part of what the person edits rather than something the rename drops.
    var value by remember(profile.id) { mutableStateOf(profile.displayName) }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(R.string.rename_profile)) },
        text = {
            OutlinedTextField(
                value = value,
                onValueChange = { value = it },
                label = { Text(stringResource(R.string.name_or_title)) },
                singleLine = true,
            )
        },
        confirmButton = {
            TextButton(
                onClick = { rename(value) },
                enabled = value.isNotBlank(),
            ) { Text(stringResource(R.string.rename)) }
        },
        dismissButton = { TextButton(onClick = dismiss) { Text(stringResource(R.string.cancel)) } },
    )
}

@Composable
private fun RenameWorkspaceDialog(workspace: Workspace, dismiss: () -> Unit, rename: (String) -> Unit) {
    var value by remember(workspace.id) { mutableStateOf(workspace.title) }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(R.string.rename_workspace)) },
        text = {
            OutlinedTextField(
                value = value,
                onValueChange = { value = it },
                label = { Text(stringResource(R.string.name_or_title)) },
                singleLine = true,
            )
        },
        dismissButton = { TextButton(onClick = dismiss) { Text(stringResource(R.string.cancel)) } },
        confirmButton = {
            TextButton(
                onClick = { rename(value.trim()) },
                enabled = value.isNotBlank() && value.trim() != workspace.title,
            ) { Text(stringResource(R.string.rename)) }
        },
    )
}

@Composable
private fun DeleteWorkspaceDialog(workspace: Workspace, dismiss: () -> Unit, delete: () -> Unit) {
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(R.string.delete_workspace_named, workspace.title)) },
        text = { Text(stringResource(R.string.delete_workspace_message)) },
        dismissButton = { TextButton(onClick = dismiss) { Text(stringResource(R.string.cancel)) } },
        confirmButton = {
            TextButton(
                onClick = delete,
                colors = ButtonDefaults.textButtonColors(contentColor = MaterialTheme.colorScheme.error),
            ) { Text(stringResource(R.string.delete)) }
        },
    )
}

internal enum class WorkspaceAction {
    Rename,
    Archive,
    Unarchive,
    Delete,
    Restore,
    DebugInfo,
}

internal fun workspaceActions(workspace: Workspace): List<WorkspaceAction> = when {
    workspace.deletedAtMillis != null -> listOf(WorkspaceAction.Restore, WorkspaceAction.DebugInfo)
    workspace.archived -> listOf(WorkspaceAction.Rename, WorkspaceAction.Unarchive, WorkspaceAction.Delete, WorkspaceAction.DebugInfo)
    else -> listOf(WorkspaceAction.Rename, WorkspaceAction.Archive, WorkspaceAction.Delete, WorkspaceAction.DebugInfo)
}

@Composable
private fun WorkspaceFilterRow(label: String, checked: Boolean, onCheckedChange: (Boolean) -> Unit) {
    ListItem(
        headlineContent = { Text(label) },
        trailingContent = { Switch(checked = checked, onCheckedChange = onCheckedChange) },
        modifier = Modifier
            .fillMaxWidth()
            .clickable { onCheckedChange(!checked) },
    )
}

@Composable
private fun WorkspaceSortRow(label: String, selected: Boolean, onSelect: () -> Unit) {
    ListItem(
        headlineContent = { Text(label) },
        leadingContent = { RadioButton(selected = selected, onClick = null) },
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onSelect),
    )
}

internal enum class WorkspaceSort {
    LastModified,
    TitleAscending,
    TitleDescending,
}

internal fun sortWorkspaces(rows: List<Workspace>, sort: WorkspaceSort): List<Workspace> {
    val lifecycle = compareBy<Workspace> {
        when {
            it.deletedAtMillis != null -> 2
            it.archived -> 1
            else -> 0
        }
    }
    val titleAscending = compareBy<Workspace, String>(String.CASE_INSENSITIVE_ORDER) { it.title }
        .thenBy { it.title }
        .thenBy { it.id.value }
    val withinLifecycle = when (sort) {
        WorkspaceSort.LastModified -> compareByDescending<Workspace> { it.modifiedAtMillis }
            .then(titleAscending)
        WorkspaceSort.TitleAscending -> titleAscending
        WorkspaceSort.TitleDescending -> titleAscending.reversed()
    }
    return rows.sortedWith(lifecycle.then(withinLifecycle))
}

internal fun workspaceVisible(workspace: Workspace, showArchived: Boolean, showDeleted: Boolean) =
    when {
        workspace.deletedAtMillis != null -> showDeleted
        workspace.archived -> showArchived
        else -> true
    }

internal enum class DocumentProjection { Documents, Files }

internal fun documentVisible(row: DocumentSummary, projection: DocumentProjection): Boolean =
    (projection == DocumentProjection.Files) == (row.kind == "file")

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun DocumentsScreen(
    rows: List<DocumentSummary>,
    transfers: BlobTransferSnapshot?,
    onAllowOnCellular: () -> Unit,
    onClearLocal: () -> Unit = {},
    lastRelease: ReleasedFiles? = null,
    onDismissRelease: () -> Unit = {},
    onBack: () -> Unit,
    onOpen: (DocumentId) -> Unit,
    onOpenNote: (DocumentSummary) -> Unit,
    onRename: (DocumentId, String) -> Unit,
    onTrash: (DocumentId) -> Unit,
    /** The file's details, read once the sheet is open. Null while it loads. */
    fileDetails: suspend (DocumentSummary) -> BlobReference?,
    onOpenFile: (DocumentSummary) -> Unit,
    onShareFile: (DocumentSummary) -> Unit,
) {
    var projection by remember { mutableStateOf(DocumentProjection.Documents) }
    var actionDocument by remember { mutableStateOf<DocumentSummary?>(null) }
    var documentForRename by remember { mutableStateOf<DocumentSummary?>(null) }
    // **Files are newest first; Documents keep the order they arrive in.**
    // A file list is a record of what somebody saved, so the thing they just
    // added is the thing they are looking for. Documents are navigated by name
    // and reordering them under a person would move what they were reaching
    // for. Ties fall back to the title so the order is stable rather than
    // whatever the inventory happened to emit.
    val visible = rows.filter { row -> documentVisible(row, projection) }
        .let { filtered ->
            if (projection == DocumentProjection.Files) {
                filtered.sortedWith(
                    compareByDescending<DocumentSummary> { it.updatedAtMillis }
                        .thenBy { it.displayTitle.ifBlank { it.title } },
                )
            } else {
                filtered
            }
        }
    EntityListScreen(
        stringResource(R.string.documents),
        visible,
        { it.displayTitle.ifBlank { stringResource(R.string.untitled) } },
        { null },
        onOpen = { onOpen(it.id) },
        onCreate = null,
        onBack = onBack,
        contentHeader = {
            PrimaryTabRow(selectedTabIndex = projection.ordinal) {
                Tab(
                    selected = projection == DocumentProjection.Documents,
                    onClick = { projection = DocumentProjection.Documents },
                    text = { Text(stringResource(R.string.documents)) },
                )
                Tab(
                    selected = projection == DocumentProjection.Files,
                    onClick = { projection = DocumentProjection.Files },
                    text = { Text(stringResource(R.string.files)) },
                )
            }
            // **Under the tabs, and only on Files.** It counts files and
            // pictures, so above them it read as a statement about the
            // Workspace and sat over a Documents tab it says nothing about.
            if (projection == DocumentProjection.Files) {
                transfers?.let {
                    BlobTransferPanel(
                        report = blobTransferReport(it),
                        onAllowOnCellular = onAllowOnCellular,
                        onClearLocal = onClearLocal,
                        lastRelease = lastRelease,
                        onDismissRelease = onDismissRelease,
                    )
                }
            }
        },
        leadingContent = { document ->
            BoxedEntityIcon(
                emoji = document.leadingEmoji.takeIf { document.kind != "file" },
                label = document.title,
                fallbackIcon = if (document.kind == "file") {
                    Icons.AutoMirrored.Outlined.InsertDriveFile
                } else {
                    Icons.Outlined.Description
                },
            )
        },
        onLongPress = { actionDocument = it },
        canOpen = { it.kind != "file" },
        rowTrailing = { document ->
            // A file's note opens like any other document; back returns here.
            document.noteId?.let {
                IconButton(onClick = { onOpenNote(document) }) {
                    Icon(
                        Icons.AutoMirrored.Outlined.StickyNote2,
                        contentDescription = stringResource(R.string.open_note),
                    )
                }
            }
        },
    )
    actionDocument?.let { document ->
        ModalBottomSheet(onDismissRequest = { actionDocument = null }) {
            Text(
                document.title,
                modifier = Modifier.padding(horizontal = KoruSpacing.lg, vertical = KoruSpacing.sm),
                style = MaterialTheme.typography.titleLarge,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            if (document.kind == "file") {
                FileSheetDetails(document, transfers, fileDetails, onOpenFile, onShareFile)
            }
            WorkspaceActionRow(
                label = stringResource(R.string.rename_document),
                icon = Icons.Outlined.Edit,
                destructive = false,
                onClick = {
                    actionDocument = null
                    documentForRename = document
                },
            )
            if (document.kind == "file" && document.noteId == null) {
                WorkspaceActionRow(
                    label = stringResource(R.string.add_note),
                    icon = Icons.AutoMirrored.Outlined.NoteAdd,
                    destructive = false,
                    onClick = {
                        actionDocument = null
                        onOpenNote(document)
                    },
                )
            }
            WorkspaceActionRow(
                label = stringResource(R.string.move_to_trash),
                icon = Icons.Outlined.Delete,
                destructive = true,
                onClick = {
                    actionDocument = null
                    onTrash(document.id)
                },
            )
            Spacer(Modifier.height(KoruSpacing.lg))
        }
    }
    documentForRename?.let { document ->
        RenameDocumentDialog(
            document = document,
            dismiss = { documentForRename = null },
            rename = { title ->
                documentForRename = null
                onRename(document.id, title)
            },
        )
    }
}

/**
 * Hand an exported file to whatever app the person has for it.
 *
 * **A `content://` URI under a one-shot grant, never a path.** Koru's storage
 * is private, so a path would be unreadable to the receiver; the provider
 * vends a URI scoped to the exports directory alone, and the grant flag means
 * the chosen app may read that one file and nothing else.
 *
 * @return false when nothing on the phone handles the type, which is a
 * sentence worth saying rather than an exception worth throwing.
 */
private fun openExportedFile(
    context: android.content.Context,
    file: java.io.File,
    mimeType: String,
    share: Boolean,
): Boolean {
    val uri = androidx.core.content.FileProvider.getUriForFile(
        context,
        context.packageName + ".fileprovider",
        file,
    )
    val type = mimeType.ifBlank { "application/octet-stream" }
    val intent = if (share) {
        android.content.Intent(android.content.Intent.ACTION_SEND)
            .setType(type)
            .putExtra(android.content.Intent.EXTRA_STREAM, uri)
    } else {
        android.content.Intent(android.content.Intent.ACTION_VIEW).setDataAndType(uri, type)
    }
    intent.addFlags(android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION)
    val chooser = android.content.Intent.createChooser(intent, null)
        .addFlags(android.content.Intent.FLAG_ACTIVITY_NEW_TASK)
    return runCatching { context.startActivity(chooser) }.isSuccess
}

/**
 * What the desktop's context pane says about a selected file, on a phone.
 *
 * **Details before actions, and the details are facts rather than reassurance.**
 * Type and size come from the blob the file *is*; where the bytes are comes
 * from the same [BlobLoadState] the picture path already produces, so a
 * transfer held back by a metered connection says so here in the same words it
 * says everywhere else rather than looking like a file that failed.
 *
 * The blob is read when the sheet opens, not carried on every row: a list
 * should not pay for a detail one sheet shows.
 */
@Composable
private fun FileSheetDetails(
    document: DocumentSummary,
    transfers: BlobTransferSnapshot?,
    fileDetails: suspend (DocumentSummary) -> BlobReference?,
    onOpenFile: (DocumentSummary) -> Unit,
    onShareFile: (DocumentSummary) -> Unit,
) {
    val reference by produceState<BlobReference?>(null, document.id) {
        value = runCatching { fileDetails(document) }.getOrNull()
    }
    val details = reference
    if (details != null) {
        ListItem(
            headlineContent = {
                Text(
                    stringResource(
                        R.string.file_details,
                        app.koru.mobile.services.resolveMimeType(details.mimeType, document.title)
                            .ifBlank { stringResource(R.string.file_type_unknown) },
                        android.text.format.Formatter.formatShortFileSize(
                            androidx.compose.ui.platform.LocalContext.current,
                            details.size,
                        ),
                    ),
                )
            },
            supportingContent = { FileWhereabouts(transfers) },
            leadingContent = {
                Icon(Icons.AutoMirrored.Outlined.InsertDriveFile, contentDescription = null)
            },
        )
    }
    // Offered while the details are still loading: the export waits for the
    // bytes itself, so tapping early is a slower open rather than a failure.
    WorkspaceActionRow(
        label = stringResource(R.string.open_file),
        icon = Icons.Outlined.OpenInNew,
        destructive = false,
        onClick = { onOpenFile(document) },
    )
    WorkspaceActionRow(
        label = stringResource(R.string.share_file),
        icon = Icons.Outlined.Share,
        destructive = false,
        onClick = { onShareFile(document) },
    )
}

/**
 * Whether opening this file will have to fetch it, and what is holding that up.
 *
 * **Deliberately not `observeBlob`.** That answers with the bytes, so asking it
 * where a video is would download the video to draw a caption — the exact cost
 * `export` exists to avoid. The Workspace's transfer status is integers the
 * engine already holds, and the one thing worth saying before somebody taps
 * *Open* is whether transfers are moving at all.
 *
 * It is a Workspace-wide answer, not a per-file one. Saying "on this device"
 * about *this* blob needs a presence probe the host does not expose yet, and
 * guessing it from a count would be a sentence that is sometimes wrong.
 */
@Composable
private fun FileWhereabouts(transfers: BlobTransferSnapshot?) {
    val hold = transfers?.pendingReason.orEmpty()
    val text = when {
        hold.isEmpty() -> stringResource(R.string.file_opens_from_cloud)
        else -> stringResource(R.string.file_transfers_held, hold)
    }
    Text(text, style = MaterialTheme.typography.bodySmall)
}

@Composable
private fun RenameDocumentDialog(
    document: DocumentSummary,
    dismiss: () -> Unit,
    rename: (String) -> Unit,
) {
    var value by remember(document.id) { mutableStateOf(document.title) }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(R.string.rename_document)) },
        text = {
            OutlinedTextField(
                value = value,
                onValueChange = { value = it },
                label = { Text(stringResource(R.string.name_or_title)) },
                singleLine = true,
            )
        },
        dismissButton = { TextButton(onClick = dismiss) { Text(stringResource(R.string.cancel)) } },
        confirmButton = {
            TextButton(
                onClick = { rename(value.trim()) },
                enabled = value.isNotBlank() && value.trim() != document.title,
            ) { Text(stringResource(R.string.rename)) }
        },
    )
}

@Composable
private fun BoxedEntityIcon(
    emoji: String?,
    label: String,
    fallbackIcon: ImageVector? = null,
    containerColor: Color = MaterialTheme.colorScheme.surfaceVariant,
    contentColor: Color = MaterialTheme.colorScheme.onSurface,
) {
    Surface(
        modifier = Modifier.size(40.dp),
        shape = MaterialTheme.shapes.small,
        color = containerColor,
    ) {
        Box(contentAlignment = Alignment.Center) {
            when {
                emoji != null -> Text(emoji, style = MaterialTheme.typography.titleLarge)
                fallbackIcon != null -> Icon(
                    fallbackIcon,
                    contentDescription = null,
                    tint = contentColor,
                )
                else -> Text(
                    label.trim().firstOrNull()?.uppercase() ?: "?",
                    color = contentColor,
                    fontWeight = FontWeight.SemiBold,
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun <T> EntityListScreen(
    title: String,
    rows: List<T>,
    primary: @Composable (T) -> String,
    secondary: @Composable (T) -> String?,
    onOpen: ((T) -> Unit)?,
    onCreate: ((String) -> Unit)?,
    onBack: (() -> Unit)?,
    trailing: (() -> Unit)? = null,
    trailingLabel: String = stringResource(R.string.developer),
    secondaryTrailing: (() -> Unit)? = null,
    secondaryTrailingLabel: String = "",
    banner: String? = null,
    bannerProgress: Boolean = banner != null,
    headerActions: (@Composable RowScope.() -> Unit)? = null,
    leadingContent: (@Composable (T) -> Unit)? = null,
    rowTrailing: (@Composable (T) -> Unit)? = null,
    onLongPress: ((T) -> Unit)? = null,
    canOpen: (T) -> Boolean = { true },
    contentHeader: (@Composable () -> Unit)? = null,
) {
    var creating by remember { mutableStateOf(false) }
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(title) },
                navigationIcon = { if (onBack != null) TextButton(onClick = onBack) { Text(stringResource(R.string.back)) } },
                actions = {
                    if (trailing != null) TextButton(onClick = trailing) { Text(trailingLabel) }
                    if (secondaryTrailing != null) TextButton(onClick = secondaryTrailing) { Text(secondaryTrailingLabel) }
                    if (onCreate != null) TextButton(onClick = { creating = true }) { Text(stringResource(R.string.create)) }
                    headerActions?.invoke(this)
                },
            )
        },
    ) { padding ->
        Column(Modifier.padding(padding)) {
        contentHeader?.invoke()
        if (banner != null) {
            Surface(color = MaterialTheme.colorScheme.secondaryContainer, modifier = Modifier.fillMaxWidth()) {
                Column(Modifier.padding(KoruSpacing.md)) {
                    Text(banner)
                    if (bannerProgress) {
                        Spacer(Modifier.height(KoruSpacing.sm))
                        LinearProgressIndicator(Modifier.fillMaxWidth())
                    }
                }
            }
        }
        if (rows.isEmpty() && banner == null) {
            EmptyState(Modifier, stringResource(R.string.empty_state))
        } else {
            LazyColumn(contentPadding = PaddingValues(vertical = KoruSpacing.sm)) {
                items(rows) { row ->
                    val supportingText = secondary(row)
                    // The row itself opens — every row, not only ones with a
                    // long-press action. There is no separate "Open" affordance.
                    val rowModifier = Modifier
                        .fillMaxWidth()
                        .combinedClickable(
                            onClick = { if (canOpen(row)) onOpen?.invoke(row) },
                            onLongClick = if (onLongPress != null) {
                                { onLongPress(row) }
                            } else {
                                null
                            },
                        )
                    ListItem(
                        headlineContent = { Text(primary(row), maxLines = 1, overflow = TextOverflow.Ellipsis) },
                        supportingContent = supportingText?.let { text ->
                            { Text(text, maxLines = 1) }
                        },
                        leadingContent = leadingContent?.let { content ->
                            { content(row) }
                        },
                        trailingContent = rowTrailing?.let { content ->
                            { content(row) }
                        },
                        modifier = rowModifier,
                    )
                    HorizontalDivider()
                }
            }
        }
        }
    }
    if (creating && onCreate != null) CreateDialog(title, { creating = false }) { creating = false; onCreate(it) }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun ShareDestinationScreen(
    state: ShareUiState,
    profiles: List<Profile>,
    workspaces: List<Workspace>,
    recentWorkspaces: List<Workspace>,
    onSelectProfile: (ProfileId) -> Unit,
    onNameChange: (Int, String) -> Unit,
    onNoteChange: (Int, String) -> Unit,
    onSave: (WorkspaceId) -> Unit,
    onFinish: () -> Unit,
) {
    BackHandler(onBack = onFinish)
    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.save_to_koru)) },
                navigationIcon = { TextButton(onClick = onFinish) { Text(stringResource(R.string.cancel)) } },
            )
        },
    ) { padding ->
        when {
            state.receiving -> Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }
            state.savedFiles.isNotEmpty() -> Column(
                Modifier.fillMaxSize().padding(padding).padding(KoruSpacing.xl),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(stringResource(R.string.saved_to_files), style = MaterialTheme.typography.headlineSmall)
                Spacer(Modifier.height(KoruSpacing.sm))
                Text(
                    pluralStringResource(R.plurals.saved_file_count, state.savedFiles.size, state.savedFiles.size),
                    style = MaterialTheme.typography.bodyLarge,
                )
                Spacer(Modifier.height(KoruSpacing.lg))
                Button(onClick = onFinish) { Text(stringResource(R.string.done)) }
            }
            state.pending != null -> {
                val files = state.pending.files
                // The recorded destinations that are still real: the workspace
                // exists in the live inventory, is neither archived nor
                // deleted, and the recorded profile still owns it.
                val recents = state.recents.mapNotNull { recent ->
                    val workspace = recentWorkspaces.firstOrNull { it.id == recent.workspaceId }
                        ?: return@mapNotNull null
                    if (workspace.profileId != recent.profileId || workspace.archived || workspace.deletedAtMillis != null) {
                        return@mapNotNull null
                    }
                    val profile = profiles.firstOrNull { it.id == recent.profileId }
                        ?: return@mapNotNull null
                    Triple(profile, workspace, recent)
                }
                LazyColumn(
                    Modifier.fillMaxSize().padding(padding),
                    contentPadding = PaddingValues(KoruSpacing.md),
                    verticalArrangement = Arrangement.spacedBy(KoruSpacing.xs),
                ) {
                    if (recents.isNotEmpty()) {
                        item {
                            Text(stringResource(R.string.recent_destinations), style = MaterialTheme.typography.titleMedium)
                        }
                        items(recents, key = { "recent-${it.third.workspaceId.value}" }) { (profile, workspace, recent) ->
                            // One tap saves — that is the section's point. The
                            // profile is selected first because saving checks
                            // the workspace against it.
                            val save = {
                                onSelectProfile(recent.profileId)
                                onSave(recent.workspaceId)
                            }
                            ListItem(
                                headlineContent = { Text(workspace.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                                supportingContent = { Text(profile.displayLabel, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                                trailingContent = {
                                    Button(onClick = save, enabled = !state.saving) {
                                        Text(stringResource(R.string.save))
                                    }
                                },
                                modifier = Modifier.fillMaxWidth().clickable(enabled = !state.saving) { save() },
                            )
                        }
                        item { Spacer(Modifier.height(KoruSpacing.md)) }
                    }
                    item {
                        Text(
                            pluralStringResource(R.plurals.shared_file_count, files.size, files.size),
                            style = MaterialTheme.typography.titleMedium,
                        )
                    }
                    // Every item's save name and optional note are editable,
                    // prefilled with the reader's defaults. Unkeyed on purpose:
                    // an edit changes the name, and a name key would recreate
                    // the field and drop the keyboard focus mid-word.
                    items(files.size) { index ->
                        OutlinedTextField(
                            value = state.names.getOrElse(index) { files[index].name },
                            onValueChange = { onNameChange(index, it) },
                            label = { Text(stringResource(R.string.name_or_title)) },
                            singleLine = true,
                            modifier = Modifier.fillMaxWidth(),
                        )
                        Spacer(Modifier.height(KoruSpacing.xs))
                        OutlinedTextField(
                            value = state.notes.getOrElse(index) { files[index].note.orEmpty() },
                            onValueChange = { onNoteChange(index, it) },
                            label = { Text(stringResource(R.string.note_optional)) },
                            modifier = Modifier.fillMaxWidth(),
                        )
                    }
                    item {
                        Spacer(Modifier.height(KoruSpacing.md))
                        Text(stringResource(R.string.choose_profile), style = MaterialTheme.typography.titleMedium)
                    }
                    items(profiles, key = { it.id.value }) { profile ->
                        ListItem(
                            headlineContent = { Text(profile.displayName) },
                            leadingContent = { RadioButton(state.selectedProfileId == profile.id, { onSelectProfile(profile.id) }) },
                            modifier = Modifier.fillMaxWidth(),
                            trailingContent = { TextButton(onClick = { onSelectProfile(profile.id) }) { Text(stringResource(R.string.select)) } },
                        )
                    }
                    if (profiles.isEmpty()) item { Text(stringResource(R.string.no_profiles_for_share)) }
                    if (state.selectedProfileId != null) {
                        item {
                            Spacer(Modifier.height(KoruSpacing.md))
                            Text(stringResource(R.string.choose_workspace), style = MaterialTheme.typography.titleMedium)
                        }
                        items(workspaces, key = { it.id.value }) { workspace ->
                            ListItem(
                                headlineContent = { Text(workspace.title) },
                                trailingContent = {
                                    Button(onClick = { onSave(workspace.id) }, enabled = !state.saving) {
                                        Text(stringResource(R.string.save))
                                    }
                                },
                            )
                        }
                        if (workspaces.isEmpty()) item { Text(stringResource(R.string.no_workspaces_for_share)) }
                        if (state.saving) item {
                            Box(Modifier.fillMaxWidth().padding(KoruSpacing.md), contentAlignment = Alignment.Center) {
                                CircularProgressIndicator()
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable private fun EmptyState(modifier: Modifier, message: String) {
    Column(modifier.fillMaxSize().padding(KoruSpacing.xl), verticalArrangement = Arrangement.Center, horizontalAlignment = Alignment.CenterHorizontally) {
        Text(message, style = MaterialTheme.typography.bodyLarge)
    }
}

@Composable private fun CreateDialog(title: String, dismiss: () -> Unit, create: (String) -> Unit) {
    var value by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = dismiss,
        title = { Text(stringResource(R.string.create_named, title)) },
        text = { OutlinedTextField(value, { value = it }, label = { Text(stringResource(R.string.name_or_title)) }, singleLine = true) },
        dismissButton = { TextButton(onClick = dismiss) { Text(stringResource(R.string.cancel)) } },
        confirmButton = { Button(onClick = { create(value) }, enabled = value.isNotBlank()) { Text(stringResource(R.string.create)) } },
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun DiagnosticsScreen(state: AppUiState, implementation: String, account: app.koru.mobile.services.AccountDiagnostics, onBack: () -> Unit) {
    val configuration = LocalConfiguration.current
    Scaffold(topBar = { TopAppBar(title = { Text(stringResource(R.string.developer)) }, navigationIcon = { TextButton(onClick = onBack) { Text(stringResource(R.string.back)) } }) }) { padding ->
        val values = listOf(
            stringResource(R.string.platform) to "Android ${android.os.Build.VERSION.RELEASE}",
            stringResource(R.string.app_version) to "${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})",
            stringResource(R.string.contract_version) to "koru.v1",
            stringResource(R.string.active_profile) to (state.activeProfileId?.value ?: "—"),
            stringResource(R.string.active_workspace) to (state.activeWorkspaceId?.value ?: "—"),
            stringResource(R.string.active_document) to (state.activeDocumentId?.value ?: "—"),
            stringResource(R.string.locale) to configuration.locales[0].toLanguageTag(),
            stringResource(R.string.service_implementation) to implementation,
            stringResource(R.string.account_worker) to listOfNotNull(account.stage, account.enrollment, account.profileId,
                account.failure, account.retryAtMillis?.toString(), account.enrollmentFailure, account.enrollmentRetryAtMillis?.toString(),
                account.enrollments.takeIf { it.isNotEmpty() }?.joinToString("; ") { row ->
                    listOfNotNull(row.profileId, row.stage, row.failure, row.retryAtMillis?.toString()).joinToString(" · ") },
                if (account.stage == "signed_out") stringResource(R.string.account_sign_in_again) else null).joinToString(" · "),
        )
        LazyColumn(Modifier.padding(padding)) { items(values) { (label, value) -> ListItem(headlineContent = { Text(label) }, supportingContent = { Text(value) }); HorizontalDivider() } }
    }
}

@Composable private fun localizedFailure(failure: AppFailure) = when (failure.code) {
    AppErrorCode.BlobHeld -> failure.message.orEmpty().let { code ->
        stringResource(app.koru.mobile.services.blobHeldMessage(code), code)
    }
    else -> localizedError(failure.code)
}

@Composable private fun localizedError(code: AppErrorCode) = when (code) {
    AppErrorCode.NotFound -> stringResource(R.string.error_not_found)
    AppErrorCode.PermissionDenied -> stringResource(R.string.error_permission)
    AppErrorCode.Unavailable -> stringResource(R.string.error_unavailable)
    AppErrorCode.InvalidData -> stringResource(R.string.error_invalid)
    AppErrorCode.Unsupported -> stringResource(R.string.error_unsupported)
    AppErrorCode.Unknown -> stringResource(R.string.error_unknown)
    AppErrorCode.BlobHeld -> stringResource(R.string.error_unavailable)
}
