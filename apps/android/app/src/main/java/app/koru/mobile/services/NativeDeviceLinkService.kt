// SPDX-License-Identifier: MPL-2.0

package app.koru.mobile.services

import android.content.Context
import app.koru.contract.api.v1.DeviceList
import app.koru.contract.api.v1.DeviceHistory as WireDeviceHistory
import app.koru.contract.api.v1.DocumentProjection as WireDocumentProjection
import app.koru.contract.api.v1.HostStatus
import app.koru.contract.api.v1.Inventory as WireInventory
import app.koru.contract.api.v1.LinkedDevice as WireLinkedDevice
import app.koru.contract.api.v1.MutationResult
import app.koru.contract.api.v1.RelayState as WireRelayState
import app.koru.contract.api.v1.StateEvent as WireStateEvent
import app.koru.mobile.BuildConfig
import app.koru.mobile.domain.*
import app.koru.mobile.logging.AppLogger
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.withContext
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.json.JSONObject

/** The SharedPreferences key holding the device-local relay on/off value. */
private const val RELAY_ENABLED_KEY = "relay_enabled"

/** Thin JNI boundary over the embedded Rust identity, Store, and iroh host. */
class NativeDeviceLinkService(
    context: Context,
    private val logger: AppLogger,
) : DeviceLinkService {
    private val root = context.filesDir.resolve("koru").absolutePath
    private val secrets = AndroidSecretVault(context).loadOrCreate()
    private val loaded = runCatching { System.loadLibrary("koru_mobile") }.isSuccess
    // SharedPreferences is this device's owner of the relay on/off value
    // (iroh-relay roadmap, decision №7: platform-local, never synced). It is
    // read here so the host opens under the stored preference; a later
    // toggle goes through `set_relay`, which applies live.
    private val relayStore = context.getSharedPreferences("device", Context.MODE_PRIVATE)
    private val mutableRelayEnabled = MutableStateFlow(relayStore.getBoolean(RELAY_ENABLED_KEY, true))
    override val relayEnabled: StateFlow<Boolean> = mutableRelayEnabled.asStateFlow()
    private val api = KoruApiNative(root, secrets, relayEnabled = mutableRelayEnabled.value)
    private val mutableState = MutableStateFlow<DeviceLinkState>(
        if (loaded) runCatching { api.status().toState() }
            .getOrElse { DeviceLinkState.Unavailable(it.message ?: "native_status") }
        else DeviceLinkState.Unavailable("native_library_missing")
    )
    override val state: StateFlow<DeviceLinkState> = mutableState.asStateFlow()
    private val mutableDevices = MutableStateFlow<List<LinkedDevice>>(emptyList())
    override val devices: StateFlow<List<LinkedDevice>> = mutableDevices.asStateFlow()
    private val mutableThisDevice = MutableStateFlow<LinkedDevice?>(null)
    override val thisDevice: StateFlow<LinkedDevice?> = mutableThisDevice.asStateFlow()
    private val mutableRelinkAvailable = MutableStateFlow<Boolean?>(null)
    override val relinkAvailable: StateFlow<Boolean?> = mutableRelinkAvailable.asStateFlow()
    private val mutableSyncEvent = MutableStateFlow<NativeSyncEvent?>(null)
    internal val syncEvent: StateFlow<NativeSyncEvent?> = mutableSyncEvent.asStateFlow()
    private val mutableRelayStatus = MutableStateFlow<RelayStatus?>(null)
    override val relayStatus: StateFlow<RelayStatus?> = mutableRelayStatus.asStateFlow()
    private val mutableAccountDiagnostics = MutableStateFlow(AccountDiagnostics())
    override val accountDiagnostics: StateFlow<AccountDiagnostics> = mutableAccountDiagnostics.asStateFlow()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    init {
        if (loaded) scope.launch {
            // Tell linked devices what this one runs, before the list is
            // first read. The native host writes a record only when the
            // description changed, so a plain relaunch costs nothing.
            runCatching { api.advertise(buildPayload()) }
                .onSuccess { logger.event("build_advertised", mapOf("result" to it)) }
                .onFailure { logger.event("build_advertised", mapOf("error" to (it.message ?: "?"))) }
            var previous = emptyList<LinkedDevice>()
            var previousSyncSequence = 0L
            while (isActive) {
                // Status only. Native SDK/vault prompts are never fetched by diagnostics.
                runCatching { api.accountPoll(app.koru.contract.api.v1.AccountPollRequest.newBuilder()
                    .setVersion(1).setRequester(1).build()) }.onSuccess { reply ->
                    if (reply.hasStatus()) {
                        val diagnostic = AccountDiagnostics.from(reply.status)
                        if (AppLogger.accountEnabled && diagnostic != mutableAccountDiagnostics.value) {
                            logger.event("account_worker", mapOf("stage" to diagnostic.stage,
                                "enrollment" to diagnostic.enrollment, "failure_code" to diagnostic.failure,
                                "retry_at_millis" to diagnostic.retryAtMillis,
                                "enrollment_failure_code" to diagnostic.enrollmentFailure,
                                "enrollment_retry_at_millis" to diagnostic.enrollmentRetryAtMillis,
                                "enrollments" to diagnostic.enrollments.map { mapOf(
                                    "stage" to it.stage, "failure_code" to it.failure, "retry_at_millis" to it.retryAtMillis) }))
                        }
                        mutableAccountDiagnostics.value = diagnostic
                    }
                }
                runCatching { api.status().toState() }
                    .onSuccess { status ->
                        if (status != mutableState.value && mutableState.value !is DeviceLinkState.Linking) {
                            mutableState.value = status
                        }
                    }
                runCatching { api.devices().toSnapshot() }.onSuccess { snapshot ->
                    mutableDevices.value = snapshot.devices
                    mutableThisDevice.value = snapshot.thisDevice
                    mutableRelinkAvailable.value = snapshot.relinkAvailable
                    mutableRelayStatus.value = snapshot.relay
                    if (snapshot.devices != previous) {
                        snapshot.devices.forEach { device -> logger.event("sync_device_status", mapOf(
                            "device" to device.displayName,
                            "device_id" to device.deviceId,
                            "state" to if (device.connected) "connected" else "disconnected",
                            "path" to (device.connection ?: "-"),
                            "last_seen_millis" to (device.lastSeenMillis?.toString() ?: "never"),
                            "build" to (device.buildLabel ?: "unknown"),
                            "build_drift" to (device.buildDrift ?: "-"),
                            "update_needed_on" to (device.updateNeededOn ?: "-"),
                        )) }
                        previous = snapshot.devices
                    }
                    snapshot.syncEvent?.let { mutableSyncEvent.value = it }
                    snapshot.syncEvent?.takeIf { it.sequence != previousSyncSequence }?.let { event ->
                        previousSyncSequence = event.sequence
                        logger.event("sync_progress", mapOf(
                            "at_millis" to event.atMillis.toString(),
                            "sequence" to event.sequence.toString(),
                            "direction" to event.direction,
                            "peer" to event.peer,
                            "stage" to event.stage,
                            "workspace_id" to (event.workspaceId ?: "-"),
                            "items" to (event.items?.toString() ?: "-"),
                            "error_code" to (event.errorCode ?: "-"),
                        ))
                    }
                }
                delay(1_000)
            }
        }
    }

    override suspend fun link(code: String) = withContext(Dispatchers.IO) {
        require(code.isNotBlank()) { "invalid_link_code" }
        check(loaded) { "native_library_missing" }
        mutableState.value = DeviceLinkState.Linking()
        val result = api.link(code)
        mutableState.value = result.toState()
        logger.event("device_link", mapOf("state" to result.state, "code" to result.errorCodeOrNull().orEmpty()))
        if (result.state == "failed") error(result.errorCodeOrNull() ?: "pairing_failed")
    }

    internal suspend fun configureAccount(configuration: AndroidAccountConfig) = withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        api.configureAccount(configuration)
    }
    internal suspend fun accountCommand(request: app.koru.contract.api.v1.AccountCommandRequest) = withContext(Dispatchers.IO) { api.accountCommand(request) }
    internal suspend fun accountPoll(request: app.koru.contract.api.v1.AccountPollRequest) = withContext(Dispatchers.IO) { api.accountPoll(request) }
    internal suspend fun accountPlatform(request: app.koru.contract.api.v1.AccountPlatformRequest) = withContext(Dispatchers.IO) { api.accountPlatform(request) }

    override suspend fun startPairing(): PairingOffer = withContext(Dispatchers.IO) {
        var stage = "native_call"
        var responseShape = "unavailable"
        runCatching {
            check(loaded) { "native_library_missing" }
            val value = api.startPairing()
            stage = "decode_response"
            stage = "validate_response"
            responseShape = pairingResponseShape(value)
            value.errorCodeOrNull()?.let { code ->
                throw IllegalStateException(code)
            }
            val payload = value.qrPayload.takeIf { value.hasQrPayload() }
            check(!payload.isNullOrBlank()) { "native_pairing_missing_qr_payload" }
            val expires = value.expiresInSeconds
            check(expires > 0) { "native_pairing_invalid_expiry" }
            PairingOffer(payload, expires)
        }.onSuccess { offer ->
            logger.event("pairing_offer_created", mapOf("payload_bytes" to offer.payload.length.toString()))
        }.onFailure { error ->
            logger.event("pairing_offer_failed", mapOf(
                "stage" to stage,
                "type" to error::class.java.simpleName,
                "message" to (error.message ?: error.toString()),
                "cause" to (error.cause?.toString() ?: "none"),
                "response" to responseShape,
            ))
        }.getOrThrow()
    }

    override suspend fun cancelPairing() = withContext(Dispatchers.IO) {
        if (loaded) api.cancelPairing().requireOk()
        Unit
    }

    override suspend fun unlink(deviceId: String) = withContext(Dispatchers.IO) {
        api.unlink(deviceId).requireOk()
        val snapshot = api.devices().toSnapshot()
        mutableDevices.value = snapshot.devices
        mutableRelinkAvailable.value = snapshot.relinkAvailable
        mutableState.value = api.status().toState()
        logger.event("device_unlinked", mapOf("device_id" to deviceId))
    }

    override suspend fun setRelayEnabled(enabled: Boolean) = withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        // Apply live first, then persist: what the store says is what the
        // running host is doing (decision №6 — the change needs no restart).
        api.setRelay(enabled).requireOk()
        relayStore.edit().putBoolean(RELAY_ENABLED_KEY, enabled).apply()
        mutableRelayEnabled.value = enabled
        logger.event("relay_toggled", mapOf("enabled" to enabled.toString()))
    }

    override suspend fun rename(name: String) = withContext(Dispatchers.IO) {
        api.rename(name.trim()).requireOk()
        val snapshot = api.devices().toSnapshot()
        mutableDevices.value = snapshot.devices
        mutableThisDevice.value = snapshot.thisDevice
        mutableRelinkAvailable.value = snapshot.relinkAvailable
        logger.event("device_renamed", emptyMap())
    }

    // Content is never touched: the native host moves `device.json` and
    // `trust.json` aside and mints a descriptor from the seeds this device
    // does hold. It refuses with `identity_intact` on a healthy device, so the
    // guard is the host's and not only this screen's.
    override suspend fun resetIdentity() = withContext(Dispatchers.IO) {
        api.resetIdentity().requireOk()
        // The fresh identity should say what it runs before anyone sees it,
        // and neither of the two reads below is allowed to fail the reset that
        // has already happened — the poll would repair them a second later.
        runCatching { api.advertise(buildPayload()) }
        runCatching { api.devices().toSnapshot() }.onSuccess { snapshot ->
            mutableDevices.value = snapshot.devices
            mutableThisDevice.value = snapshot.thisDevice
        }
        runCatching { api.status().toState() }.onSuccess { mutableState.value = it }
        logger.event("device_identity_reset", emptyMap())
    }

    override suspend fun history(deviceId: String, before: Long?): DeviceHistoryPage =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.history(deviceId, before).toPage()
        }

    internal suspend fun inventory(): NativeInventory = withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        api.inventory().toInventory()
    }

    private val documentReads = Mutex()

    /** Subscribe before the initial read. JNI only enqueues a conflated wake;
     * cancellation releases the native observer, including during registration. */
    internal fun documentChanges(id: DocumentId): Flow<Unit> = documentInvalidations(
        register = { callback ->
            check(loaded) { "native_library_missing" }
            api.observeDocument(id.value, callback)
        },
        release = api::closeDocumentObserver,
    ).flowOn(Dispatchers.IO)
    private var documentReadSequence = 0L
    private var lastDocumentRead: DocumentSnapshot? = null

    internal suspend fun document(id: DocumentId): DocumentSnapshot = documentReads.withLock { withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        val read = api.document(id.value).toSnapshot()
        val previous = lastDocumentRead
        if (previous?.id == read.id && previous.revision == read.revision) previous
        else read.copy(observation = ++documentReadSequence).also { lastDocumentRead = it }
    } }

    internal suspend fun editBlockText(id: DocumentId, edit: DocumentCommand.EditBlockText) = withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        api.editBlockText(id.value, edit).requireOk()
    }

    internal suspend fun run(id: DocumentId): RunSnapshot = withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        api.run(id.value).toSnapshot()
    }

    internal suspend fun runTimeline(id: DocumentId, newestFirstFrom: Int, len: Int): RunTimelinePage =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.runTimeline(id.value, newestFirstFrom, len).toPage()
        }

    internal suspend fun blob(workspace: WorkspaceId, id: BlobId): ByteArray =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.blob(workspace.value, id.value).let {
                it.errorCodeOrNull()?.let(::error)
                it.value.toByteArray()
            }
        }

    /** The blob a file document is: hash, MIME and size, from the manifest
     *  and one node. Opening the document itself answers `document_is_file`. */
    internal suspend fun fileBlob(documentId: DocumentId): Triple<String, String, Long> =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.fileBlob(documentId.value).let {
                it.errorCodeOrNull()?.let(::error)
                Triple(it.hash, it.mimeType, it.size)
            }
        }

    /** Write one blob to [destination], streamed by the host.
     *
     *  Deliberately not [blob]: that returns the payload through JNI and into
     *  the JVM heap, which a video does not survive. The host copies Store to
     *  disk and this learns only how many bytes arrived. */
    internal suspend fun exportBlob(workspace: WorkspaceId, id: BlobId, destination: String): Long =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.exportBlob(workspace.value, id.value, destination).let {
                it.errorCodeOrNull()?.let(::error)
                it.size
            }
        }

    // ---- blob transfer (`cloud-blob-object-v1` §10, Plan B5) ----
    //
    // Every one of these is a call across the shim and nothing else. What a
    // pause means, what an explicit request overrides and whether a download
    // fits are the shared engine's decisions, on this platform exactly as on
    // the desktop.

    internal suspend fun blobStatus(workspace: String): BlobTransferSnapshot =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            BlobTransferSnapshot.from(api.blobStatus(workspace))
        }

    // ---- Cloud sync progress: the desktop's two Manage Profiles reads ----

    /** One Workspace's Cloud sync state, asked of its manifest by the host.
     *  `null` when the host could not answer about it. */
    internal suspend fun cloudSyncStatus(workspace: WorkspaceId): CloudWorkspaceStatus? =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            CloudWorkspaceStatus.from(api.cloudSyncStatus(workspace.value))
        }

    /** The documents this Workspace waits on another device for. `null` when
     *  the list could not be read — no blob engine, or the Store refused. */
    internal suspend fun cloudWaiting(workspace: WorkspaceId): List<CloudWaitingEntry>? =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            CloudWaitingEntry.listFrom(api.cloudWaiting(workspace.value))
        }

    internal suspend fun setBlobPolicy(policy: app.koru.contract.api.v1.BlobPolicy) =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.setBlobPolicy(policy).requireOk()
        }

    /** Somebody is looking at a placeholder. Carries through a metered pause. */
    internal suspend fun requestBlob(workspace: String, hash: String) =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.requestBlob(workspace, hash).requireOk()
        }

    /** Drop this phone's copies of the files Koru Cloud holds, keeping every
     *  reference; the engine checks each file against the control plane first. */
    internal suspend fun releaseBlobs(workspace: String): ReleasedFiles =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            ReleasedFiles.from(api.releaseBlobs(workspace))
        }

    /** Resume what a killed process left behind. WorkManager's entry point. */
    internal suspend fun recoverBlobs(workspace: String) = withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        api.recoverBlobs(workspace).requireOk()
    }

    /** The §11 announcement: what became durable since `afterSequence`. */
    internal suspend fun blobArrivals(workspace: String, afterSequence: Long) =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.blobArrivals(workspace, afterSequence)
        }

    internal suspend fun mutateProfile(id: String, action: String, value: String = "") =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.mutateProfile(id, action, value).requireOk()
        }

    internal suspend fun mutateWorkspace(id: String, action: String, title: String = "") =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.mutateWorkspace(id, action, title).requireOk()
        }

    internal suspend fun mutateDocument(id: String, action: String, title: String = "") =
        withContext(Dispatchers.IO) {
            check(loaded) { "native_library_missing" }
            api.mutateDocument(id, action, title).requireOk()
        }

    internal suspend fun importFile(
        workspaceId: String,
        name: String,
        mimeType: String,
        path: String,
        note: String? = null,
    ): String = withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        val value = api.importFile(workspaceId, name, mimeType, path, note)
        value.errorCodeOrNull()?.let(::error)
        check(value.hasDocumentId()) { "import_missing_document_id" }
        value.documentId
    }

    internal suspend fun ensureFileNote(documentId: String): String = withContext(Dispatchers.IO) {
        check(loaded) { "native_library_missing" }
        val value = api.ensureFileNote(documentId)
        value.errorCodeOrNull()?.let(::error)
        check(value.hasDocumentId()) { "note_missing_document_id" }
        value.documentId
    }
}

/**
 * What this app tells linked devices it runs. The version is Gradle's — what
 * the APK says — rather than anything the native library knows, because the
 * library has no build info of its own. Keys match `koru_device::BuildPayload`.
 */
internal fun buildPayload() = app.koru.contract.api.v1.BuildPayload.newBuilder().apply {
    appVersion = "${BuildConfig.VERSION_NAME} (${BuildConfig.VERSION_CODE})"
    channel = BuildConfig.BUILD_TYPE
    BuildConfig.GIT_BRANCH.takeIf { it.isNotBlank() }?.let(::setBranch)
    BuildConfig.GIT_COMMIT.takeIf { it.isNotBlank() }?.let(::setCommit)
    os = "Android ${android.os.Build.VERSION.RELEASE}"
    osVersion = "${android.os.Build.VERSION.RELEASE} (API ${android.os.Build.VERSION.SDK_INT})"
}.build()

private fun JSONObject.optionalString(name: String): String? {
    val value = opt(name)
    return when (value) {
        null, JSONObject.NULL -> null
        is String -> value.takeIf { it.isNotBlank() }
        else -> value.toString().takeIf { it.isNotBlank() }
    }
}

/** Field presence and types only; the QR payload and secrets never reach logs. */
private fun pairingResponseShape(value: JSONObject): String = listOf(
    "state=${value.optionalString("state") ?: "missing"}",
    "qr_payload=${if (value.opt("qr_payload") is String) "string" else "missing"}",
    "expires=${value.opt("expires_in_seconds")?.javaClass?.simpleName ?: "missing"}",
    "error_code=${value.optionalString("error_code") ?: "none"}",
).joinToString(",")

private fun pairingResponseShape(value: app.koru.contract.api.v1.PairingOffer): String = listOf(
    "state=${value.state}",
    "qr_payload=${if (value.hasQrPayload()) "string" else "missing"}",
    "expires=${value.expiresInSeconds}",
    "error_code=${value.errorCodeOrNull() ?: "none"}",
).joinToString(",")

private fun HostStatus.errorCodeOrNull() = if (hasErrorCode()) errorCode else null
private fun app.koru.contract.api.v1.LinkDeviceResult.errorCodeOrNull() =
    if (hasErrorCode()) errorCode else null
private fun app.koru.contract.api.v1.PairingOffer.errorCodeOrNull() =
    if (hasErrorCode()) errorCode else null
private fun app.koru.contract.api.v1.BlobBytes.errorCodeOrNull() =
    if (hasErrorCode()) errorCode else null
private fun app.koru.contract.api.v1.FileBlob.errorCodeOrNull() =
    if (hasErrorCode()) errorCode else null
private fun app.koru.contract.api.v1.ExportedBlob.errorCodeOrNull() =
    if (hasErrorCode()) errorCode else null
private fun app.koru.contract.api.v1.DocumentIdResult.errorCodeOrNull() =
    if (hasErrorCode()) errorCode else null

private fun MutationResult.requireOk() {
    if (hasErrorCode()) error(errorCode)
    check(ok) { state.ifBlank { "native_mutation_failed" } }
}

/**
 * The one error code that names a *state* rather than a failed attempt: the
 * host could not open this device's identity at all.
 *
 * It arrives under two different `state` values — `status` answers
 * `"unavailable"`, while `link_device` and `start_pairing` answer `"failed"` —
 * and it means the same thing under both, so the code is what it is read
 * from. Keying on the state instead is how the row it leads to becomes
 * unreachable on the polled path, which is the whole of the failure this
 * screen was built to stop.
 */
internal const val DEVICE_IDENTITY_MISMATCH = "device_identity_mismatch"

internal fun HostStatus.toState(): DeviceLinkState = state.toLinkState(
    if (hasPeerName()) peerName else null,
    errorCodeOrNull(),
)

internal fun app.koru.contract.api.v1.LinkDeviceResult.toState(): DeviceLinkState =
    state.toLinkState(if (hasPeerName()) peerName else null, errorCodeOrNull())

private fun String.toLinkState(peerName: String?, errorCode: String?): DeviceLinkState = when {
    errorCode == DEVICE_IDENTITY_MISMATCH -> DeviceLinkState.IdentityMismatch
    this == "unlinked" -> DeviceLinkState.Unlinked
    this == "linking" -> DeviceLinkState.Linking(peerName)
    this == "linked" -> DeviceLinkState.Linked(peerName)
    this == "failed" -> DeviceLinkState.Failed(errorCode ?: "pairing_failed")
    else -> DeviceLinkState.Unavailable(errorCode ?: "native_status")
}

internal fun WireInventory.toInventory(): NativeInventory {
    if (hasErrorCode()) error(errorCode)
    return NativeInventory(
        profilesList.map { NativeInventoryProfile(
            it.id, it.name, it.locale, it.icon.takeIf { _ -> it.hasIcon() },
            it.leadingEmoji.takeIf { _ -> it.hasLeadingEmoji() }, it.displayName,
            it.color.takeIf { _ -> it.hasColor() },
            it.accountBinding,
        ) },
        workspacesList.map { NativeInventoryWorkspace(
            it.id, it.profileId, it.name, it.icon.takeIf { _ -> it.hasIcon() },
            it.color.takeIf { _ -> it.hasColor() },
            it.leadingEmoji.takeIf { _ -> it.hasLeadingEmoji() }, it.displayName,
            it.archived, it.deletedAtMillis.takeIf { _ -> it.hasDeletedAtMillis() },
            it.modifiedAtMillis,
        ) },
        documentsList.map { NativeInventoryDocument(
            it.id, it.workspaceId, it.title, it.displayTitle,
            it.leadingEmoji.takeIf { _ -> it.hasLeadingEmoji() }, it.kind,
            it.mime.takeIf { _ -> it.hasMime() },
            it.noteId.takeIf { _ -> it.hasNoteId() },
            it.updatedAtMillis,
        ) },
        deletedWorkspaces, trashedDocuments, failedWorkspaces, hiddenRuns,
    )
}

private fun WireDocumentProjection.toSnapshot(): DocumentSnapshot {
    if (hasErrorCode()) error(errorCode)
    val workspace = WorkspaceId(workspaceId)
    return DocumentSnapshot(
        DocumentId(id), workspace, title, revision,
        blocksList.map { nativeBlock(it, workspace) },
    )
}

private fun nativeBlock(value: app.koru.contract.api.v1.Block, workspace: WorkspaceId): Block {
    val properties = value.properties.let {
        BlockProperties(
            name = it.name.takeIf { _ -> it.hasName() },
            icon = it.icon.takeIf { _ -> it.hasIcon() },
            color = it.color.takeIf { _ -> it.hasColor() },
            done = it.done,
            alignment = when (it.alignment) {
                "center" -> Alignment.Center
                "end" -> Alignment.End
                "justify" -> Alignment.Justify
                else -> Alignment.Start
            },
            headingLevel = it.headingLevel.takeIf { _ -> it.hasHeadingLevel() }?.toInt(),
            listStyle = if (!it.hasListStyle()) null else when (it.listStyle) {
                "bullet" -> ListStyle.Bullet
                "checkbox" -> ListStyle.Checkbox
                "number", "numbered" -> ListStyle.Numbered
                "letter" -> ListStyle.Letter
                "roman" -> ListStyle.Roman
                else -> null
            },
            imageWidth = it.imageWidth.takeIf { _ -> it.hasImageWidth() }?.toInt(),
            imageHeight = it.imageHeight.takeIf { _ -> it.hasImageHeight() }?.toInt(),
            language = it.language.takeIf { _ -> it.hasLanguage() },
        )
    }
    val rich = value.richText.takeIf { value.hasRichText() }?.let { wire ->
        RichText(
            wire.text,
            wire.marksList.map {
                TextMark(
                    TextRange(it.startUtf8, it.endUtf8),
                    MarkKind.entries.firstOrNull { kind -> kind.wireName == it.kind } ?: MarkKind.Unknown,
                    it.value.takeIf { _ -> it.hasValue() }?.encodeToByteArray() ?: byteArrayOf(),
                    it.kind,
                )
            },
            wire.inlineObjectsList.map {
                InlineObject(
                    InlineObjectId(it.id), it.anchorUtf8, it.role,
                    nativeReference(it.reference), it.label.takeIf { _ -> it.hasLabel() },
                )
            },
        )
    }
    val blob = value.blob.takeIf { value.hasBlob() }?.let {
        BlobReference(
            BlobId(it.id), it.mimeType, it.size,
            it.width.takeIf { _ -> it.hasWidth() }?.toInt(),
            it.height.takeIf { _ -> it.hasHeight() }?.toInt(), workspace,
        )
    }
    val reference = value.reference.takeIf { value.hasReference() }?.let(::nativeReference)
    val content = when (value.kind) {
        "paragraph" -> BlockContent.Paragraph(requireNotNull(rich))
        "quote" -> BlockContent.Quote(requireNotNull(rich))
        "callout" -> BlockContent.Callout
        "task" -> BlockContent.Task(requireNotNull(rich))
        "listblock" -> BlockContent.ListContainer(properties.listStyle ?: ListStyle.Bullet)
        "image" -> BlockContent.Image(requireNotNull(blob))
        "canvas" -> BlockContent.Canvas(requireNotNull(blob))
        "code" -> BlockContent.Code(requireNotNull(rich))
        "table" -> BlockContent.Table(requireNotNull(rich))
        "divider" -> BlockContent.Divider
        "embed" -> BlockContent.Embed(requireNotNull(reference))
        "contact" -> BlockContent.Contact(requireNotNull(rich))
        else -> BlockContent.Unsupported(0, "koru.native/${value.kind}", byteArrayOf())
    }
    return Block(
        BlockId(value.id), value.parentId.takeIf { value.hasParentId() }?.let(::BlockId),
        properties, content,
    )
}

private fun nativeReference(value: app.koru.contract.api.v1.EntityReference): EntityReference =
    when (value.kind) {
        "document" -> EntityReference.Document(DocumentId(value.documentId))
        "block" -> EntityReference.Block(
            BlockId(value.blockId),
            value.documentId.takeIf { value.hasDocumentId() }?.let(::DocumentId),
        )
        "url" -> EntityReference.Url(value.url)
        "label" -> EntityReference.Label(value.value)
        "run" -> EntityReference.Run(DocumentId(value.documentId))
        else -> EntityReference.Label(value.value.ifBlank { "Reference" })
    }

private fun WireStateEvent.toDomain() = DeviceStateEvent(
    eventId = eventId.takeIf { hasEventId() }, atMillis = atMillis, kind = kind,
    profileId = profileId.takeIf { hasProfileId() },
    workspaceId = workspaceId.takeIf { hasWorkspaceId() },
    documentId = documentId.takeIf { hasDocumentId() },
    documentName = documentName.takeIf { hasDocumentName() },
    blobHash = blobHash.takeIf { hasBlobHash() }, detail = detail.takeIf { hasDetail() },
    items = items.takeIf { hasItems() }?.toInt(),
)

private fun WireLinkedDevice.toDomain() = LinkedDevice(
    deviceId, displayName, linkedAtMillis, connected,
    lastSeenMillis.takeIf { hasLastSeenMillis() },
    introducedBy.takeIf { hasIntroducedBy() },
    buildLabel = buildLabel.takeIf { hasBuildLabel() },
    buildDrift = buildDrift.takeIf { hasBuildDrift() },
    buildSeverity = buildSeverity.takeIf { hasBuildSeverity() },
    updateNeededOn = updateNeededOn.takeIf { hasUpdateNeededOn() },
    connection = connection.takeIf { hasConnection() },
    lastSyncMillis = lastSyncMillis.takeIf { hasLastSyncMillis() },
    history = historyList.map(WireStateEvent::toDomain),
)

private fun DeviceList.toSnapshot(): NativeDeviceSnapshot {
    if (hasErrorCode()) error(errorCode)
    return NativeDeviceSnapshot(
        devicesList.map(WireLinkedDevice::toDomain),
        thisDevice.takeIf { hasThisDevice() }?.toDomain(),
        syncEvent.takeIf { hasSyncEvent() }?.let {
            NativeSyncEvent(
                it.sequence.toLong(), it.atMillis, it.direction, it.peer, it.stage,
                it.workspaceId.takeIf { _ -> it.hasWorkspaceId() },
                it.items.takeIf { _ -> it.hasItems() }?.toInt(),
                it.errorCode.takeIf { _ -> it.hasErrorCode() },
            )
        },
        // UNSPECIFIED (and any value this build does not know) is "nothing
        // to claim" — an old host simply shows no status line.
        relay.takeIf { hasRelay() }?.let {
            when (it.state) {
                WireRelayState.RELAY_STATE_OFF -> RelayStatus(RelayState.Off, it.host)
                WireRelayState.RELAY_STATE_CONNECTING -> RelayStatus(RelayState.Connecting, it.host)
                WireRelayState.RELAY_STATE_CONNECTED -> RelayStatus(RelayState.Connected, it.host)
                WireRelayState.RELAY_STATE_DISCONNECTED -> RelayStatus(RelayState.Disconnected, it.host)
                else -> null
            }
        },
        relinkAvailable,
    )
}

private fun WireDeviceHistory.toPage(): DeviceHistoryPage {
    if (hasErrorCode()) error(errorCode)
    return DeviceHistoryPage(deviceId, eventsList.map(WireStateEvent::toDomain), nextBefore.takeIf { hasNextBefore() })
}

data class NativeLinkResult(
    val state: String,
    val peerName: String?,
    val errorCode: String?,
)

data class NativeInventoryProfile(
    val id: String,
    val name: String,
    val locale: String,
    val icon: String?,
    val leadingEmoji: String?,
    val displayName: String,
    val color: String?,
    val accountBinding: String = "",
)
data class NativeInventoryWorkspace(
    val id: String,
    val profileId: String,
    val name: String,
    val icon: String?,
    val color: String?,
    val leadingEmoji: String?,
    val displayName: String,
    val archived: Boolean,
    val deletedAtMillis: Long?,
    val modifiedAtMillis: Long,
)
data class NativeInventoryDocument(
    val id: String,
    val workspaceId: String,
    val title: String,
    val displayTitle: String,
    val leadingEmoji: String?,
    val kind: String,
    val mime: String?,
    val noteId: String?,
    /** When this last changed, so a list can put the newest first. */
    val updatedAtMillis: Long = 0,
)
data class NativeInventory(
    val profiles: List<NativeInventoryProfile> = emptyList(),
    val workspaces: List<NativeInventoryWorkspace> = emptyList(),
    val documents: List<NativeInventoryDocument> = emptyList(),
    val deletedWorkspaces: Int = 0,
    val trashedDocuments: Int = 0,
    val failedWorkspaces: Int = 0,
    /** Run and schedule documents the host hid from the listing (P1.0 F6). */
    val hiddenRuns: Int = 0,
)

// The JSON twin of the Protobuf shape above. It delegates rather than
// restating the arms: a second copy of this mapping is what let the mismatch
// mean two different things on one platform.
private fun NativeLinkResult.toState(): DeviceLinkState =
    state.toLinkState(peerName, errorCode)

private fun parse(value: String): NativeLinkResult = JSONObject(value).let {
    NativeLinkResult(
        state = it.optString("state", "failed"),
        peerName = it.optionalString("peer_name"),
        errorCode = it.optionalString("error_code"),
    )
}

internal fun parseInventory(value: String): NativeInventory = JSONObject(value).let { root ->
    root.optionalString("error_code")?.let(::error)
    NativeInventory(
        profiles = root.getJSONArray("profiles").let { rows ->
            List(rows.length()) { index ->
                rows.getJSONObject(index).let {
                    NativeInventoryProfile(
                        it.getString("id"),
                        it.getString("name"),
                        it.getString("locale"),
                        it.optionalString("icon"),
                        it.optionalString("leading_emoji"),
                        it.optString("display_name", it.getString("name")),
                        it.optionalString("color"),
                        it.optString("account_binding", ""),
                    )
                }
            }
        },
        workspaces = root.getJSONArray("workspaces").let { rows ->
            List(rows.length()) { index ->
                rows.getJSONObject(index).let {
                    NativeInventoryWorkspace(
                        it.getString("id"),
                        it.getString("profile_id"),
                        it.getString("name"),
                        it.optionalString("icon"),
                        it.optionalString("color"),
                        it.optionalString("leading_emoji"),
                        it.optString("display_name", it.getString("name")),
                        it.optBoolean("archived", false),
                        if (it.isNull("deleted_at_millis")) null else it.getLong("deleted_at_millis"),
                        it.optLong("modified_at_millis", 0),
                    )
                }
            }
        },
        documents = root.getJSONArray("documents").let { rows ->
            List(rows.length()) { index ->
                rows.getJSONObject(index).let {
                    NativeInventoryDocument(
                        it.getString("id"),
                        it.getString("workspace_id"),
                        it.getString("title"),
                        it.optString("display_title", it.getString("title")),
                        it.optionalString("leading_emoji"),
                        it.optString("kind", "document"),
                        it.optionalString("mime"),
                        it.optionalString("note_id"),
                    )
                }
            }
        },
        deletedWorkspaces = root.optInt("deleted_workspaces", 0),
        trashedDocuments = root.optInt("trashed_documents", 0),
        failedWorkspaces = root.optInt("failed_workspaces", 0),
        hiddenRuns = root.optInt("hidden_runs", 0),
    )
}

internal fun parseNativeDocument(value: String): DocumentSnapshot = JSONObject(value).let { root ->
    root.optionalString("error_code")?.let(::error)
    val document = root.getJSONObject("document")
    val workspace = WorkspaceId(document.getString("workspace_id"))
    DocumentSnapshot(
        id = DocumentId(document.getString("id")),
        workspaceId = workspace,
        title = document.getString("title"),
        revision = document.getLong("revision"),
        blocks = document.getJSONArray("blocks").let { rows ->
            List(rows.length()) { index -> nativeBlock(rows.getJSONObject(index), workspace) }
        },
    )
}

private fun nativeBlock(value: JSONObject, workspace: WorkspaceId): Block {
    val properties = nativeProperties(value.getJSONObject("properties"))
    val kind = value.getString("kind")
    val rich = value.optJSONObject("rich_text")?.let(::nativeRichText)
    val blob = value.optJSONObject("blob")?.let { nativeBlob(it, workspace) }
    val reference = value.optJSONObject("reference")?.let(::nativeReference)
    val content = when (kind) {
        "paragraph" -> BlockContent.Paragraph(requireNotNull(rich))
        "quote" -> BlockContent.Quote(requireNotNull(rich))
        "callout" -> BlockContent.Callout
        "task" -> BlockContent.Task(requireNotNull(rich))
        "listblock" -> BlockContent.ListContainer(properties.listStyle ?: ListStyle.Bullet)
        "image" -> BlockContent.Image(requireNotNull(blob))
        "canvas" -> BlockContent.Canvas(requireNotNull(blob))
        "code" -> BlockContent.Code(requireNotNull(rich))
        "table" -> BlockContent.Table(requireNotNull(rich))
        "divider" -> BlockContent.Divider
        "embed" -> BlockContent.Embed(requireNotNull(reference))
        "contact" -> BlockContent.Contact(requireNotNull(rich))
        else -> BlockContent.Unsupported(0, "koru.native/$kind", byteArrayOf())
    }
    return Block(
        id = BlockId(value.getString("id")),
        parentId = value.optionalString("parent_id")?.let(::BlockId),
        properties = properties,
        content = content,
    )
}

private fun nativeProperties(value: JSONObject) = BlockProperties(
    name = value.optionalString("name"),
    icon = value.optionalString("icon"),
    color = value.optionalString("color"),
    done = value.optBoolean("done", false),
    alignment = when (value.optString("alignment", "start")) {
        "center" -> Alignment.Center
        "end" -> Alignment.End
        "justify" -> Alignment.Justify
        else -> Alignment.Start
    },
    headingLevel = value.optInt("heading_level", 0).takeIf { it > 0 },
    listStyle = when (value.optionalString("list_style")) {
        "bullet" -> ListStyle.Bullet
        "checkbox" -> ListStyle.Checkbox
        "number", "numbered" -> ListStyle.Numbered
        "letter" -> ListStyle.Letter
        "roman" -> ListStyle.Roman
        else -> null
    },
    imageWidth = value.optInt("image_width", 0).takeIf { it > 0 },
    imageHeight = value.optInt("image_height", 0).takeIf { it > 0 },
    language = value.optionalString("language"),
)

private fun nativeRichText(value: JSONObject) = RichText(
    text = value.getString("text"),
    marks = value.getJSONArray("marks").let { rows ->
        List(rows.length()) { index ->
            rows.getJSONObject(index).let { mark ->
                val wire = mark.getString("kind")
                TextMark(
                    range = TextRange(mark.getInt("start_utf8"), mark.getInt("end_utf8")),
                    kind = MarkKind.entries.firstOrNull { it.wireName == wire } ?: MarkKind.Unknown,
                    value = mark.optionalString("value")?.encodeToByteArray() ?: byteArrayOf(),
                    wireKind = wire,
                )
            }
        }
    },
    inlineObjects = value.getJSONArray("inline_objects").let { rows ->
        List(rows.length()) { index ->
            rows.getJSONObject(index).let { item ->
                InlineObject(
                    id = InlineObjectId(item.getString("id")),
                    anchorUtf8 = item.getInt("anchor_utf8"),
                    role = item.getString("role"),
                    reference = nativeReference(item.getJSONObject("reference")),
                    label = item.optionalString("label"),
                )
            }
        }
    },
)

private fun nativeBlob(value: JSONObject, workspace: WorkspaceId) = BlobReference(
    id = BlobId(value.getString("id")),
    mimeType = value.getString("mime_type"),
    size = value.getLong("size"),
    width = value.optInt("width", 0).takeIf { it > 0 },
    height = value.optInt("height", 0).takeIf { it > 0 },
    workspaceId = workspace,
)

private fun nativeReference(value: JSONObject): EntityReference = when (value.getString("kind")) {
    "document" -> EntityReference.Document(DocumentId(value.getString("document_id")))
    "block" -> EntityReference.Block(
        BlockId(value.getString("block_id")),
        value.optionalString("document_id")?.let(::DocumentId),
    )
    "url" -> EntityReference.Url(value.getString("url"))
    "label" -> EntityReference.Label(value.getString("value"))
    else -> EntityReference.Label(value.optString("value", "Reference"))
}

internal data class NativeSyncEvent(val sequence: Long, val atMillis: Long, val direction: String, val peer: String, val stage: String, val workspaceId: String?, val items: Int?, val errorCode: String?)
internal data class NativeDeviceSnapshot(
    val devices: List<LinkedDevice>,
    val thisDevice: LinkedDevice?,
    val syncEvent: NativeSyncEvent?,
    val relay: RelayStatus? = null,
    val relinkAvailable: Boolean = false,
)

private fun parseStateEvent(event: JSONObject) = DeviceStateEvent(
    eventId = if (event.isNull("event_id")) null else event.getLong("event_id"),
    atMillis = event.getLong("at_millis"),
    kind = event.getString("kind"),
    profileId = event.optionalString("profile_id"),
    workspaceId = event.optionalString("workspace_id"),
    documentId = event.optionalString("document_id"),
    documentName = event.optionalString("document_name"),
    blobHash = event.optionalString("blob_hash"),
    detail = event.optionalString("detail"),
    items = if (event.isNull("items")) null else event.getInt("items"),
)

private fun parseDevice(value: JSONObject): LinkedDevice = LinkedDevice(
    value.getString("device_id"),
    value.getString("display_name"),
    value.getLong("linked_at_millis"),
    value.getBoolean("connected"),
    if (value.isNull("last_seen_millis")) null else value.getLong("last_seen_millis"),
    value.optionalString("introduced_by"),
    buildLabel = value.optionalString("build_label"),
    buildDrift = value.optionalString("build_drift"),
    buildSeverity = value.optionalString("build_severity"),
    updateNeededOn = value.optionalString("update_needed_on"),
    connection = value.optionalString("connection"),
    lastSyncMillis = if (value.isNull("last_sync_millis")) null else value.getLong("last_sync_millis"),
    history = value.optJSONArray("history")?.let { rows ->
        List(rows.length()) { index ->
            parseStateEvent(rows.getJSONObject(index))
        }
    } ?: emptyList(),
)

internal fun parseDevices(value: String): NativeDeviceSnapshot = JSONObject(value).let { root ->
    root.optionalString("error_code")?.let(::error)
    val devices = root.getJSONArray("devices").let { rows ->
        List(rows.length()) { index -> parseDevice(rows.getJSONObject(index)) }
    }
    val thisDevice = root.optJSONObject("this_device")?.let(::parseDevice)
    val sync = root.optJSONObject("sync_event")?.let {
        NativeSyncEvent(
            it.getLong("sequence"),
            it.getLong("at_millis"),
            it.getString("direction"),
            it.getString("peer"),
            it.getString("stage"),
            it.optionalString("workspace_id"),
            if (it.isNull("items")) null else it.getInt("items"),
            it.optionalString("error_code"),
        )
    }
    val relay = root.optJSONObject("relay")?.let {
        val state = when (it.optString("state")) {
            "off" -> RelayState.Off
            "connecting" -> RelayState.Connecting
            "connected" -> RelayState.Connected
            "disconnected" -> RelayState.Disconnected
            else -> null
        }
        state?.let { known -> RelayStatus(known, it.optString("host")) }
    }
    NativeDeviceSnapshot(
        devices,
        thisDevice,
        sync,
        relay,
        root.optBoolean("relink_available", false),
    )
}

internal fun parseDeviceHistory(value: String): DeviceHistoryPage = JSONObject(value).let { root ->
    root.optionalString("error_code")?.let(::error)
    val events = root.getJSONArray("events").let { rows ->
        List(rows.length()) { index -> parseStateEvent(rows.getJSONObject(index)) }
    }
    DeviceHistoryPage(
        deviceId = root.getString("device_id"),
        events = events,
        nextBefore = if (root.isNull("next_before")) null else root.getLong("next_before"),
    )
}

/**
 * The Cloud sync progress screen's reads, over the embedded host.
 *
 * The Workspaces are the Profile's live ones — deleted rows left out, as the
 * desktop's section counts them — from the inventory every other list on
 * this phone reads, so the progress screen and the Workspaces screen are
 * about the same set.
 */
internal class NativeCloudProgressSource(
    private val native: NativeDeviceLinkService,
    private val catalog: WorkspaceService,
) : CloudProgressSource {
    override suspend fun workspaces(profile: ProfileId): List<Pair<WorkspaceId, String>> =
        catalog.observeWorkspaces(profile).first()
            .filter { it.deletedAtMillis == null }
            .map { it.id to it.title }

    override suspend fun status(workspace: WorkspaceId): CloudWorkspaceStatus? = answered {
        native.cloudSyncStatus(workspace)
    }

    override suspend fun waiting(workspace: WorkspaceId): List<CloudWaitingEntry>? = answered {
        native.cloudWaiting(workspace)
    }

    /** A host that throws has not answered; a cancelled read is not a failure. */
    private suspend fun <T> answered(read: suspend () -> T?): T? = try {
        read()
    } catch (cancelled: kotlinx.coroutines.CancellationException) {
        throw cancelled
    } catch (_: Exception) {
        null
    }
}
