// SPDX-License-Identifier: MPL-2.0

//! Bounded in-process AI worker and deterministic vertical-slice service.

use serde_json::json;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use uuid::Uuid;

use koru_ai_api::{
    AiResponseStream, AiScope, AiServerPort, ModelPullResponseStream, RequestContext,
    Result as ApiResult,
};
use koru_ai_core::{
    AiError, AiErrorClass, AiEvent, AiIntent, AiPersistence, AiProviderId, AiRequest,
    AiRequestMessage, AiRequestOptions, AiSearchQuery, AiSettings, AiToolCall, AiToolDefinition,
    AiToolExecutionContext, AiToolExecutor, AiToolLoop, AiToolLoopLimits, AiToolResult, AiUsage,
    AttemptStatus, BranchCopy, BranchOrigin, Cancellation, Chat, ChatAccessLens, ChatId, ChatRole,
    ContextCommit, ContextItem, ContextRef, DocumentExport, DocumentExportId, DocumentId,
    DocumentRelation, ExportStatus, FeedbackEvent, InferenceAttempt, InferenceAttemptId, Message,
    MessageBlockContext, MessageBlockId, MessageBlockRef, MessageDocumentRef, MessageId,
    MessageState, PendingRetry, PendingTurn, PinnedDocument, ReasoningChoice, ReasoningControl,
    RecoveryCounts, RequestMeasurements, StoreContextPort, Timestamp, ToolActivity,
    ToolActivityStatus, ToolApproval, ToolErrorCode, ToolInvocationId, ToolMode, UsageSource,
    WorkspaceId, finalize_markdown,
};
use koru_ai_provider::{
    CredentialStore, DeterministicMockProvider, KORU_MODEL, ProviderRegistry, ProviderRuntime,
    koru_model, openai_model, validate_reasoning,
};
use koru_ai_sqlite::Repository;
use koru_ai_wire::{
    AI_STREAM_VERSION, AI_WIRE_VERSION, AiSearchRequest, AiSearchResponse, AttemptResponse,
    BranchChatRequest, BranchChatResponse, CancelAttemptResponse, CancelModelPullResponse,
    ChatToolPolicyResponse, ChatTranscript, ClearHistoryResponse, ClearUsageResponse,
    CreateChatRequest, CreateChatResponse, DeleteChatResponse, ExportChatRequest,
    ExportChatResponse, Failure, FeedbackRequest, FeedbackResponse, ListChatsResponse,
    ListChatsToolRequest, ListChatsToolResponse, MessageSources, MessageUsage, ModelPullEvent,
    OllamaSettingsView, OpenAiSettingsView, ProviderSettingsResponse, ProviderValidationResponse,
    ProviderValidationStatus, PublicAiEvent, PullOllamaModelAccepted, PullOllamaModelRequest,
    ReadChatToolRequest, ReadChatToolResponse, RenameChatRequest, RenameChatResponse,
    RetryTurnRequest, SearchChatsToolRequest, SearchChatsToolResponse, SelectProviderRequest,
    SendTurnAccepted, SendTurnRequest, StreamEvent, ToolDecisionResponse, UpdateChatModelRequest,
    UpdateChatModelResponse, UpdateChatPrivacyRequest, UpdateChatPrivacyResponse,
    UpdateChatToolPolicyRequest, UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest,
    UsageResponse,
};

pub const WORKER_COMMAND_CAPACITY: usize = 64;
pub const PROVIDER_QUEUE_CAPACITY: usize = 8;
pub const PROVIDER_ADMIN_QUEUE_CAPACITY: usize = 8;
pub const CONTEXT_QUEUE_CAPACITY: usize = 8;
pub const EXPORT_QUEUE_CAPACITY: usize = 8;
pub const TOOL_QUEUE_CAPACITY: usize = 8;
pub const STREAM_CAPACITY: usize = 32;
mod agents;

use agents::{AttemptWork, Clearing, ClosingChat, agent_validation};
use koru_ai_core::AgentToolKind;
use koru_ai_provider_agent::{AgentSessions, EndOutcome, is_agent_provider};

const PROVIDER_THREADS: usize = 2;
const TOOL_THREADS: usize = 2;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const LOOP_INTERVAL: Duration = Duration::from_millis(20);
const COALESCE_INTERVAL: Duration = Duration::from_millis(16);
const COALESCE_BYTES: usize = 256;
/// One number, on the wire: the composer caps what it sends by the same one.
const MAX_CONTEXT_REFS: usize = koru_ai_wire::MAX_CONTEXT_REFS;
const MAX_CONTEXT_PAYLOAD_BYTES: u64 = 4 * 1024 * 1024;
const PROMPT_TEMPLATE_VERSION: &str = "koru-chat-v1";
const CHAT_PROMPT: &str = include_str!("../prompts/chat-v1.md");

pub struct WorkerConfig {
    root: PathBuf,
    channel: String,
    providers: ProviderRegistry,
    provider_runtime: Option<ProviderRuntime>,
    store_context: Option<Arc<dyn StoreContextPort>>,
    tool_executor: Option<Arc<dyn AiToolExecutor>>,
    provider_threads: usize,
    /// The session host's endpoint, when this worker may drive local agents
    /// through it. `None` (phones, rooted test harnesses without a host)
    /// registers no agent provider at all.
    session_host: Option<koru_ipc::Endpoint>,
    #[cfg(test)]
    panic_after_open: bool,
}

impl WorkerConfig {
    pub fn new(root: PathBuf, channel: impl Into<String>, providers: ProviderRegistry) -> Self {
        Self {
            root,
            channel: channel.into(),
            providers,
            provider_runtime: None,
            store_context: None,
            tool_executor: None,
            provider_threads: PROVIDER_THREADS,
            session_host: None,
            #[cfg(test)]
            panic_after_open: false,
        }
    }

    pub fn production(root: PathBuf, channel: impl Into<String>) -> Result<Self, AiError> {
        let channel = channel.into();
        let runtime = ProviderRuntime::with_os_vault(&channel)?;
        Ok(Self {
            root,
            channel,
            providers: runtime.registry(),
            provider_runtime: Some(runtime),
            store_context: None,
            tool_executor: None,
            provider_threads: PROVIDER_THREADS,
            session_host: None,
            #[cfg(test)]
            panic_after_open: false,
        })
    }

    pub fn production_with_credentials(
        root: PathBuf,
        channel: impl Into<String>,
        credentials: Arc<dyn CredentialStore>,
        openai_account: impl Into<String>,
    ) -> Result<Self, AiError> {
        let channel = channel.into();
        let runtime = ProviderRuntime::new(None, credentials, openai_account)?;
        Ok(Self {
            root,
            channel,
            providers: runtime.registry(),
            provider_runtime: Some(runtime),
            store_context: None,
            tool_executor: None,
            provider_threads: PROVIDER_THREADS,
            session_host: None,
            #[cfg(test)]
            panic_after_open: false,
        })
    }

    pub fn deterministic_mock(root: PathBuf, channel: impl Into<String>) -> Self {
        Self::deterministic_mock_with_delay(root, channel, Duration::ZERO)
    }

    /// Deterministic provider configuration for process-boundary integration
    /// tests that need time to interrupt an in-flight response.
    pub fn deterministic_mock_with_delay(
        root: PathBuf,
        channel: impl Into<String>,
        chunk_delay: Duration,
    ) -> Self {
        let mut providers = ProviderRegistry::default();
        providers.insert(
            AiProviderId::parse("mock").expect("static provider id"),
            Arc::new(DeterministicMockProvider::default().with_chunk_delay(chunk_delay)),
        );
        Self::new(root, channel, providers)
    }

    pub fn with_provider_threads(mut self, threads: usize) -> Self {
        self.provider_threads = threads.max(1);
        self
    }

    pub fn with_store_context(mut self, store_context: Arc<dyn StoreContextPort>) -> Self {
        self.store_context = Some(store_context);
        self
    }

    pub fn with_tool_executor(mut self, tool_executor: Arc<dyn AiToolExecutor>) -> Self {
        self.tool_executor = Some(tool_executor);
        self
    }

    /// Let this worker drive local coding agents as chat providers through
    /// the session host at `endpoint` (one `agent-<kind>` provider per
    /// managed agent the host registers).
    pub fn with_session_host(mut self, endpoint: koru_ipc::Endpoint) -> Self {
        self.session_host = Some(endpoint);
        self
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    pub fn channel(&self) -> &str {
        &self.channel
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiWorkerInfo {
    pub api_version: u32,
    pub schema_version: i64,
    pub capabilities: Vec<String>,
    pub providers: Vec<String>,
    pub channel: String,
    pub started_at: Timestamp,
    pub recovery: RecoveryCounts,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerStatus {
    Starting,
    Ready(AiWorkerInfo),
    Stopping,
    Stopped,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerError {
    Saturated,
    Stopped,
    TimedOut,
    Startup(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saturated => formatter.write_str("the AI worker command queue is saturated"),
            Self::Stopped => formatter.write_str("the AI worker has stopped"),
            Self::TimedOut => formatter.write_str("the AI worker did not answer in time"),
            Self::Startup(message) => write!(formatter, "the AI worker could not start: {message}"),
        }
    }
}

impl std::error::Error for WorkerError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkerExit {
    Clean,
    Failed(String),
    Panicked,
}

pub struct AiWorker {
    config: WorkerConfig,
}

impl AiWorker {
    pub fn new(config: WorkerConfig) -> Self {
        Self { config }
    }

    pub fn spawn(self) -> Result<WorkerHandle, WorkerError> {
        let (sender, receiver) = sync_channel(WORKER_COMMAND_CAPACITY);
        let (ready_tx, ready_rx) = sync_channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let status = Arc::new(Mutex::new(WorkerStatus::Starting));
        let thread_status = Arc::clone(&status);
        let provider_output = sender.clone();
        let join = std::thread::Builder::new()
            .name("koru-ai-server worker".into())
            .spawn(move || {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    run_worker(
                        self.config,
                        receiver,
                        provider_output,
                        Arc::clone(&thread_shutdown),
                        ready_tx,
                    )
                }));
                let exit = match outcome {
                    Ok(Ok(())) => WorkerExit::Clean,
                    Ok(Err(message)) => WorkerExit::Failed(message),
                    Err(_) => WorkerExit::Panicked,
                };
                set_status(
                    &thread_status,
                    match &exit {
                        WorkerExit::Clean => WorkerStatus::Stopped,
                        WorkerExit::Failed(message) => WorkerStatus::Failed(message.clone()),
                        WorkerExit::Panicked => WorkerStatus::Failed("worker panicked".into()),
                    },
                );
                exit
            })
            .map_err(|error| WorkerError::Startup(error.to_string()))?;

        match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(info)) => {
                set_status(&status, WorkerStatus::Ready(info));
                Ok(WorkerHandle {
                    sender,
                    shutdown,
                    status,
                    join: Some(join),
                })
            }
            Ok(Err(message)) => {
                let _ = join.join();
                Err(WorkerError::Startup(message))
            }
            Err(RecvTimeoutError::Timeout) => {
                shutdown.store(true, Ordering::Release);
                Err(WorkerError::TimedOut)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let exit = join.join().unwrap_or(WorkerExit::Panicked);
                Err(WorkerError::Startup(match exit {
                    WorkerExit::Failed(message) => message,
                    WorkerExit::Panicked => "worker panicked".into(),
                    WorkerExit::Clean => "worker exited before readiness".into(),
                }))
            }
        }
    }
}

#[derive(Debug)]
pub struct WorkerHandle {
    sender: SyncSender<Envelope>,
    shutdown: Arc<AtomicBool>,
    status: Arc<Mutex<WorkerStatus>>,
    join: Option<JoinHandle<WorkerExit>>,
}

impl WorkerHandle {
    pub fn client(&self) -> AiWorkerClient {
        AiWorkerClient {
            sender: self.sender.clone(),
        }
    }

    pub fn status(&self) -> WorkerStatus {
        lock_status(&self.status).clone()
    }

    pub fn ping(&self) -> Result<(), WorkerError> {
        let (reply, response) = sync_channel(1);
        self.try_send(Envelope::Command(WorkerCommand::Ping(reply)))?;
        response
            .recv_timeout(Duration::from_secs(5))
            .map_err(map_recv)
    }

    pub fn info(&self) -> Result<AiWorkerInfo, WorkerError> {
        let (reply, response) = sync_channel(1);
        self.try_send(Envelope::Command(WorkerCommand::Info(reply)))?;
        response
            .recv_timeout(Duration::from_secs(5))
            .map_err(map_recv)
    }

    pub fn shutdown(&self) {
        set_status(&self.status, WorkerStatus::Stopping);
        self.shutdown.store(true, Ordering::Release);
    }

    pub fn join(mut self) -> WorkerExit {
        self.join
            .take()
            .expect("AI worker join handle")
            .join()
            .unwrap_or(WorkerExit::Panicked)
    }

    fn try_send(&self, envelope: Envelope) -> Result<(), WorkerError> {
        self.sender.try_send(envelope).map_err(map_try_send)
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        if self.join.is_some() {
            self.shutdown.store(true, Ordering::Release);
        }
    }
}

#[derive(Clone)]
pub struct AiWorkerClient {
    sender: SyncSender<Envelope>,
}

impl AiWorkerClient {
    fn send(&self, command: WorkerCommand) -> ApiResult<()> {
        self.sender
            .try_send(Envelope::Command(command))
            .map_err(queue_failure)
    }
}

impl AiServerPort for AiWorkerClient {
    fn provider_settings(&self, context: RequestContext) -> ApiResult<ProviderSettingsResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ProviderSettings { context, reply })?;
        receive_api(response)
    }

    fn select_provider(
        &self,
        context: RequestContext,
        request: SelectProviderRequest,
    ) -> ApiResult<ProviderSettingsResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::SelectProvider {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn update_openai_settings(
        &self,
        context: RequestContext,
        request: UpdateOpenAiSettingsRequest,
    ) -> ApiResult<OpenAiSettingsView> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::UpdateOpenAiSettings {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn update_ollama_settings(
        &self,
        context: RequestContext,
        request: UpdateOllamaSettingsRequest,
    ) -> ApiResult<OllamaSettingsView> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::UpdateOllamaSettings {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn validate_provider(
        &self,
        context: RequestContext,
        provider: AiProviderId,
    ) -> ApiResult<ProviderValidationResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ValidateProvider {
            context,
            provider,
            reply,
        })?;
        receive_api(response)
    }

    fn discover_agent_models(
        &self,
        context: RequestContext,
        provider: AiProviderId,
    ) -> ApiResult<koru_ai_wire::AgentModelsResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::DiscoverAgentModels {
            context,
            provider,
            reply,
        })?;
        receive_api(response)
    }

    fn pull_ollama_model(
        &self,
        context: RequestContext,
        request: PullOllamaModelRequest,
    ) -> ApiResult<ModelPullResponseStream> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::PullOllamaModel {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn cancel_ollama_pull(&self, context: RequestContext) -> ApiResult<CancelModelPullResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::CancelOllamaPull { context, reply })?;
        receive_api(response)
    }

    fn create_chat(
        &self,
        context: RequestContext,
        request: CreateChatRequest,
    ) -> ApiResult<CreateChatResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::CreateChat {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn list_chats(&self, context: RequestContext) -> ApiResult<ListChatsResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ListChats { context, reply })?;
        receive_api(response)
    }

    fn read_chat(&self, context: RequestContext, chat: ChatId) -> ApiResult<ChatTranscript> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ReadChat {
            context,
            chat,
            reply,
        })?;
        receive_api(response)
    }

    fn rename_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: RenameChatRequest,
    ) -> ApiResult<RenameChatResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::RenameChat {
            context,
            chat,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn update_chat_model(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatModelRequest,
    ) -> ApiResult<UpdateChatModelResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::UpdateChatModel {
            context,
            chat,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn update_chat_privacy(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatPrivacyRequest,
    ) -> ApiResult<UpdateChatPrivacyResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::UpdateChatPrivacy {
            context,
            chat,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn update_chat_folder(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::UpdateChatFolderRequest,
    ) -> ApiResult<koru_ai_wire::UpdateChatFolderResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::UpdateChatFolder {
            context,
            chat,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn tool_list_chats(
        &self,
        context: RequestContext,
        request: ListChatsToolRequest,
    ) -> ApiResult<ListChatsToolResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ToolListChats {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn tool_search_chats(
        &self,
        context: RequestContext,
        request: SearchChatsToolRequest,
    ) -> ApiResult<SearchChatsToolResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ToolSearchChats {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn tool_read_chat(
        &self,
        context: RequestContext,
        request: ReadChatToolRequest,
    ) -> ApiResult<ReadChatToolResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ToolReadChat {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn read_chat_tool_policy(
        &self,
        context: RequestContext,
        chat: ChatId,
    ) -> ApiResult<ChatToolPolicyResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ReadChatToolPolicy {
            context,
            chat,
            reply,
        })?;
        receive_api(response)
    }

    fn update_chat_tool_policy(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatToolPolicyRequest,
    ) -> ApiResult<ChatToolPolicyResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::UpdateChatToolPolicy {
            context,
            chat,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn usage(&self, context: RequestContext, chat: Option<ChatId>) -> ApiResult<UsageResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::Usage {
            context,
            chat,
            reply,
        })?;
        receive_api(response)
    }

    fn delete_chat(&self, context: RequestContext, chat: ChatId) -> ApiResult<DeleteChatResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::DeleteChat {
            context,
            chat,
            reply,
        })?;
        receive_api(response)
    }

    fn clear_history(&self, context: RequestContext) -> ApiResult<ClearHistoryResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ClearHistory { context, reply })?;
        receive_api(response)
    }

    fn clear_usage(&self, context: RequestContext) -> ApiResult<ClearUsageResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ClearUsage { context, reply })?;
        receive_api(response)
    }

    fn search(
        &self,
        context: RequestContext,
        request: AiSearchRequest,
    ) -> ApiResult<AiSearchResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::Search {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn feedback(
        &self,
        context: RequestContext,
        message: MessageId,
        request: FeedbackRequest,
    ) -> ApiResult<FeedbackResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::Feedback {
            context,
            message,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn branch_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: BranchChatRequest,
    ) -> ApiResult<BranchChatResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::BranchChat {
            context,
            chat,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn send_turn(
        &self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> ApiResult<AiResponseStream> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::SendTurn {
            context,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn read_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> ApiResult<AttemptResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ReadAttempt {
            context,
            attempt,
            reply,
        })?;
        receive_api(response)
    }

    fn cancel_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> ApiResult<CancelAttemptResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::CancelAttempt {
            context,
            attempt,
            reply,
        })?;
        receive_api(response)
    }

    fn approve_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        choice: Option<String>,
    ) -> ApiResult<ToolDecisionResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ApproveToolCall {
            context,
            tool,
            choice,
            reply,
        })?;
        receive_api(response)
    }

    fn deny_tool_call(
        &self,
        context: RequestContext,
        tool: ToolInvocationId,
        choice: Option<String>,
    ) -> ApiResult<ToolDecisionResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::DenyToolCall {
            context,
            tool,
            choice,
            reply,
        })?;
        receive_api(response)
    }

    fn retry_turn(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: RetryTurnRequest,
    ) -> ApiResult<AiResponseStream> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::RetryTurn {
            context,
            chat,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn export_chat(
        &self,
        context: RequestContext,
        chat: ChatId,
        request: ExportChatRequest,
    ) -> ApiResult<ExportChatResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ExportChat {
            context,
            chat,
            request,
            reply,
        })?;
        receive_api(response)
    }

    fn read_export(
        &self,
        context: RequestContext,
        export: DocumentExportId,
    ) -> ApiResult<ExportChatResponse> {
        let (reply, response) = sync_channel(1);
        self.send(WorkerCommand::ReadExport {
            context,
            export,
            reply,
        })?;
        receive_api(response)
    }
}

enum Envelope {
    Command(WorkerCommand),
    Context(ContextResolution),
    Provider(AiEvent),
    /// A provider job's thread has returned, whatever it produced: the
    /// work registry counts it back, and nothing else infers it.
    ProviderReturned(InferenceAttemptId),
    Tools(ToolResolution),
    PullFinished(Uuid),
    Export(ExportResolution),
    /// The session-control thread reported on a chat's sessions.
    ChatSessionsEnded {
        chat: ChatId,
        outcome: EndOutcome,
    },
}

enum WorkerCommand {
    Ping(SyncSender<()>),
    Info(SyncSender<AiWorkerInfo>),
    ProviderSettings {
        context: RequestContext,
        reply: SyncSender<ApiResult<ProviderSettingsResponse>>,
    },
    SelectProvider {
        context: RequestContext,
        request: SelectProviderRequest,
        reply: SyncSender<ApiResult<ProviderSettingsResponse>>,
    },
    UpdateOpenAiSettings {
        context: RequestContext,
        request: UpdateOpenAiSettingsRequest,
        reply: SyncSender<ApiResult<OpenAiSettingsView>>,
    },
    UpdateOllamaSettings {
        context: RequestContext,
        request: UpdateOllamaSettingsRequest,
        reply: SyncSender<ApiResult<OllamaSettingsView>>,
    },
    ValidateProvider {
        context: RequestContext,
        provider: AiProviderId,
        reply: SyncSender<ApiResult<ProviderValidationResponse>>,
    },
    DiscoverAgentModels {
        context: RequestContext,
        provider: AiProviderId,
        reply: SyncSender<ApiResult<koru_ai_wire::AgentModelsResponse>>,
    },
    PullOllamaModel {
        context: RequestContext,
        request: PullOllamaModelRequest,
        reply: SyncSender<ApiResult<ModelPullResponseStream>>,
    },
    CancelOllamaPull {
        context: RequestContext,
        reply: SyncSender<ApiResult<CancelModelPullResponse>>,
    },
    CreateChat {
        context: RequestContext,
        request: CreateChatRequest,
        reply: SyncSender<ApiResult<CreateChatResponse>>,
    },
    ListChats {
        context: RequestContext,
        reply: SyncSender<ApiResult<ListChatsResponse>>,
    },
    ReadChat {
        context: RequestContext,
        chat: ChatId,
        reply: SyncSender<ApiResult<ChatTranscript>>,
    },
    RenameChat {
        context: RequestContext,
        chat: ChatId,
        request: RenameChatRequest,
        reply: SyncSender<ApiResult<RenameChatResponse>>,
    },
    UpdateChatModel {
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatModelRequest,
        reply: SyncSender<ApiResult<UpdateChatModelResponse>>,
    },
    UpdateChatPrivacy {
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatPrivacyRequest,
        reply: SyncSender<ApiResult<UpdateChatPrivacyResponse>>,
    },
    UpdateChatFolder {
        context: RequestContext,
        chat: ChatId,
        request: koru_ai_wire::UpdateChatFolderRequest,
        reply: SyncSender<ApiResult<koru_ai_wire::UpdateChatFolderResponse>>,
    },
    ToolListChats {
        context: RequestContext,
        request: ListChatsToolRequest,
        reply: SyncSender<ApiResult<ListChatsToolResponse>>,
    },
    ToolSearchChats {
        context: RequestContext,
        request: SearchChatsToolRequest,
        reply: SyncSender<ApiResult<SearchChatsToolResponse>>,
    },
    ToolReadChat {
        context: RequestContext,
        request: ReadChatToolRequest,
        reply: SyncSender<ApiResult<ReadChatToolResponse>>,
    },
    UpdateChatToolPolicy {
        context: RequestContext,
        chat: ChatId,
        request: UpdateChatToolPolicyRequest,
        reply: SyncSender<ApiResult<ChatToolPolicyResponse>>,
    },
    ReadChatToolPolicy {
        context: RequestContext,
        chat: ChatId,
        reply: SyncSender<ApiResult<ChatToolPolicyResponse>>,
    },
    Usage {
        context: RequestContext,
        chat: Option<ChatId>,
        reply: SyncSender<ApiResult<UsageResponse>>,
    },
    DeleteChat {
        context: RequestContext,
        chat: ChatId,
        reply: SyncSender<ApiResult<DeleteChatResponse>>,
    },
    ClearHistory {
        context: RequestContext,
        reply: SyncSender<ApiResult<ClearHistoryResponse>>,
    },
    ClearUsage {
        context: RequestContext,
        reply: SyncSender<ApiResult<ClearUsageResponse>>,
    },
    Search {
        context: RequestContext,
        request: AiSearchRequest,
        reply: SyncSender<ApiResult<AiSearchResponse>>,
    },
    Feedback {
        context: RequestContext,
        message: MessageId,
        request: FeedbackRequest,
        reply: SyncSender<ApiResult<FeedbackResponse>>,
    },
    BranchChat {
        context: RequestContext,
        chat: ChatId,
        request: BranchChatRequest,
        reply: SyncSender<ApiResult<BranchChatResponse>>,
    },
    SendTurn {
        context: RequestContext,
        request: SendTurnRequest,
        reply: SyncSender<ApiResult<AiResponseStream>>,
    },
    ReadAttempt {
        context: RequestContext,
        attempt: InferenceAttemptId,
        reply: SyncSender<ApiResult<AttemptResponse>>,
    },
    CancelAttempt {
        context: RequestContext,
        attempt: InferenceAttemptId,
        reply: SyncSender<ApiResult<CancelAttemptResponse>>,
    },
    ApproveToolCall {
        context: RequestContext,
        tool: ToolInvocationId,
        choice: Option<String>,
        reply: SyncSender<ApiResult<ToolDecisionResponse>>,
    },
    DenyToolCall {
        context: RequestContext,
        tool: ToolInvocationId,
        choice: Option<String>,
        reply: SyncSender<ApiResult<ToolDecisionResponse>>,
    },
    RetryTurn {
        context: RequestContext,
        chat: ChatId,
        request: RetryTurnRequest,
        reply: SyncSender<ApiResult<AiResponseStream>>,
    },
    ExportChat {
        context: RequestContext,
        chat: ChatId,
        request: ExportChatRequest,
        reply: SyncSender<ApiResult<ExportChatResponse>>,
    },
    ReadExport {
        context: RequestContext,
        export: DocumentExportId,
        reply: SyncSender<ApiResult<ExportChatResponse>>,
    },
    #[cfg(test)]
    Hold {
        entered: SyncSender<()>,
        release: Receiver<()>,
    },
    #[cfg(test)]
    Panic(SyncSender<()>),
}

impl WorkerCommand {
    fn diagnostic_name(&self) -> &'static str {
        match self {
            Self::Ping(_) => "ping",
            Self::Info(_) => "info",
            Self::ProviderSettings { .. } => "provider_settings",
            Self::SelectProvider { .. } => "select_provider",
            Self::UpdateOpenAiSettings { .. } => "update_openai_settings",
            Self::UpdateOllamaSettings { .. } => "update_ollama_settings",
            Self::ValidateProvider { .. } => "validate_provider",
            Self::DiscoverAgentModels { .. } => "discover_agent_models",
            Self::PullOllamaModel { .. } => "pull_ollama_model",
            Self::CancelOllamaPull { .. } => "cancel_ollama_pull",
            Self::CreateChat { .. } => "create_chat",
            Self::ListChats { .. } => "list_chats",
            Self::ReadChat { .. } => "read_chat",
            Self::RenameChat { .. } => "rename_chat",
            Self::UpdateChatModel { .. } => "update_chat_model",
            Self::UpdateChatPrivacy { .. } => "update_chat_privacy",
            Self::UpdateChatFolder { .. } => "update_chat_folder",
            Self::ToolListChats { .. } => "tool_list_chats",
            Self::ToolSearchChats { .. } => "tool_search_chats",
            Self::ToolReadChat { .. } => "tool_read_chat",
            Self::UpdateChatToolPolicy { .. } => "update_chat_tool_policy",
            Self::ReadChatToolPolicy { .. } => "read_chat_tool_policy",
            Self::Usage { .. } => "usage",
            Self::DeleteChat { .. } => "delete_chat",
            Self::ClearHistory { .. } => "clear_history",
            Self::ClearUsage { .. } => "clear_usage",
            Self::Search { .. } => "search",
            Self::Feedback { .. } => "feedback",
            Self::BranchChat { .. } => "branch_chat",
            Self::SendTurn { .. } => "send_turn",
            Self::ReadAttempt { .. } => "read_attempt",
            Self::CancelAttempt { .. } => "cancel_attempt",
            Self::ApproveToolCall { .. } => "approve_tool_call",
            Self::DenyToolCall { .. } => "deny_tool_call",
            Self::RetryTurn { .. } => "retry_turn",
            Self::ExportChat { .. } => "export_chat",
            Self::ReadExport { .. } => "read_export",
            #[cfg(test)]
            Self::Hold { .. } => "hold",
            #[cfg(test)]
            Self::Panic(_) => "panic",
        }
    }
}

struct StreamState {
    sender: SyncSender<StreamEvent>,
    next_sequence: u64,
}

struct WorkerState {
    repository: Repository,
    /// The worker's stop flag, the one the pools' threads watch too.
    shutdown: Arc<AtomicBool>,
    provider_runtime: Option<ProviderRuntime>,
    context_pool: ContextPool,
    export_pool: ExportPool,
    provider_pool: ProviderPool,
    tool_pool: ToolPool,
    tool_definitions: Vec<AiToolDefinition>,
    tool_loops: HashMap<InferenceAttemptId, AiToolLoop>,
    tool_batches: HashMap<InferenceAttemptId, PendingToolBatch>,
    tool_attempts: HashMap<ToolInvocationId, InferenceAttemptId>,
    provider_admin: Option<ProviderAdminPool>,
    ollama_pull: Option<(Uuid, String, Cancellation)>,
    streams: HashMap<InferenceAttemptId, StreamState>,
    cancellations: HashMap<InferenceAttemptId, Cancellation>,
    info: AiWorkerInfo,
    /// Local agents, when the worker has a session host to drive them
    /// through.
    agents: Option<AgentSessions>,
    /// The work registry: what deletion and clearing read (never `streams`).
    active_work: HashMap<InferenceAttemptId, AttemptWork>,
    closing: HashMap<ChatId, ClosingChat>,
    clearing: Option<Clearing>,
    /// An agent's live wording per open row, for the length of its turn.
    agent_live: HashMap<ToolInvocationId, koru_ai_core::AgentActivityLive>,
    agent_rows: HashMap<InferenceAttemptId, Vec<ToolInvocationId>>,
    /// The dispatcher's own inbox, for reports that arrive from other threads.
    output: SyncSender<Envelope>,
    /// The agents this worker offers, as the settings view lists them.
    agent_views: Vec<koru_ai_wire::AgentProviderView>,
    /// Each Ollama model's reasoning control, as its last inspection told
    /// it; a model not inspected in this process shows the chip no slider.
    ollama_controls: HashMap<String, ReasoningControl>,
}

fn run_worker(
    config: WorkerConfig,
    receiver: Receiver<Envelope>,
    provider_output: SyncSender<Envelope>,
    shutdown: Arc<AtomicBool>,
    ready: SyncSender<Result<AiWorkerInfo, String>>,
) -> Result<(), String> {
    let database = config.root.join(koru_ai_sqlite::DATABASE_FILE);
    let mut repository = Repository::open(&database).map_err(|error| error.to_string())?;
    let recovery = repository
        .recover_interrupted(Timestamp::now())
        .map_err(|error| error.public_message)?;
    if let Some(runtime) = &config.provider_runtime {
        let mut settings = repository
            .settings()
            .map_err(|error| error.public_message)?;
        // Credential presence is secure-storage state, not durable AI data.
        // Clear the legacy lookup marker once, then hydrate only the runtime
        // copy by probing the OS vault as this worker starts.
        if let Some(settings) = &mut settings
            && settings.openai.key_account.take().is_some()
        {
            repository
                .save_settings(settings)
                .map_err(|error| error.public_message)?;
        }
        runtime.replace_settings(settings.map(|settings| runtime_settings(runtime, settings)));
    }
    #[cfg(test)]
    if config.panic_after_open {
        panic!("requested AI worker startup panic");
    }

    let mut registry = config.providers;
    let mut agent_views = Vec::new();
    let agents = match config.session_host {
        Some(endpoint) => match AgentSessions::spawn(endpoint) {
            Ok(agents) => {
                for (id, provider) in agents.providers() {
                    registry.insert(id, provider);
                }
                agent_views = koru_ai_provider_agent::agent_provider_kinds()
                    .iter()
                    .map(|kind| koru_ai_wire::AgentProviderView {
                        id: kind.id.clone(),
                        label: koru_ai_provider_agent::brand_label(kind),
                    })
                    .collect();
                Some(agents)
            }
            Err(error) => {
                tracing::warn!(%error, "local agents are unavailable: the agent control thread did not start");
                None
            }
        },
        None => None,
    };
    let mut providers: Vec<String> = registry.ids().map(ToString::to_string).collect();
    providers.sort();
    let capabilities = if registry.is_empty() {
        Vec::new()
    } else {
        vec![
            "ai.chat".into(),
            "ai.search".into(),
            "ai.streaming".into(),
            "ai.usage".into(),
        ]
    };
    let provider_pool = ProviderPool::spawn(
        Arc::new(registry),
        provider_output.clone(),
        Arc::clone(&shutdown),
        config.provider_threads,
    )?;
    let tool_definitions = config
        .tool_executor
        .as_ref()
        .map(|executor| executor.definitions())
        .unwrap_or_default();
    let tool_pool = ToolPool::spawn(
        config.tool_executor,
        provider_output.clone(),
        Arc::clone(&shutdown),
        TOOL_THREADS,
    )?;
    let store_context = config.store_context;
    let context_pool = ContextPool::spawn(
        store_context.clone(),
        provider_output.clone(),
        Arc::clone(&shutdown),
    )?;
    let export_pool = ExportPool::spawn(
        store_context,
        provider_output.clone(),
        Arc::clone(&shutdown),
    )?;
    let provider_admin = config
        .provider_runtime
        .as_ref()
        .map(|runtime| {
            ProviderAdminPool::spawn(
                runtime.clone(),
                agents.clone(),
                provider_output.clone(),
                Arc::clone(&shutdown),
            )
        })
        .transpose()?;
    let info = AiWorkerInfo {
        api_version: AI_WIRE_VERSION,
        schema_version: repository
            .schema_version()
            .map_err(|error| error.to_string())?,
        capabilities,
        providers,
        channel: config.channel,
        started_at: Timestamp::now(),
        recovery,
    };
    tracing::info!(
        api_version = info.api_version,
        schema_version = info.schema_version,
        recovered_attempts = info.recovery.attempts,
        recovered_messages = info.recovery.messages,
        provider_count = info.providers.len(),
        "AI worker ready"
    );
    ready
        .send(Ok(info.clone()))
        .map_err(|_| "AI worker readiness receiver disappeared".to_owned())?;

    let mut state = WorkerState {
        repository,
        shutdown: Arc::clone(&shutdown),
        provider_runtime: config.provider_runtime,
        context_pool,
        export_pool,
        provider_pool,
        tool_pool,
        tool_definitions,
        tool_loops: HashMap::new(),
        tool_batches: HashMap::new(),
        tool_attempts: HashMap::new(),
        provider_admin,
        ollama_pull: None,
        streams: HashMap::new(),
        cancellations: HashMap::new(),
        info,
        agents,
        active_work: HashMap::new(),
        closing: HashMap::new(),
        clearing: None,
        agent_live: HashMap::new(),
        agent_rows: HashMap::new(),
        output: provider_output.clone(),
        agent_views,
        ollama_controls: HashMap::new(),
    };
    while !shutdown.load(Ordering::Acquire) {
        match receiver.recv_timeout(LOOP_INTERVAL) {
            Ok(Envelope::Command(command)) => {
                let command_name = command.diagnostic_name();
                let started = Instant::now();
                let outcome = catch_unwind(AssertUnwindSafe(|| state.handle_command(command)));
                if outcome.is_err() {
                    tracing::error!(
                        command = command_name,
                        "AI worker command panicked; worker remains available"
                    );
                }
                tracing::debug!(
                    target: "koru_ai::worker",
                    command = command_name,
                    outcome = if outcome.is_ok() { "ok" } else { "panic" },
                    elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                    "AI command dispatched"
                );
            }
            Ok(Envelope::Context(resolution)) => state.handle_context_resolution(resolution),
            Ok(Envelope::Provider(event)) => state.handle_provider_event(event),
            Ok(Envelope::ProviderReturned(attempt)) => state.work_returned(attempt),
            Ok(Envelope::Tools(resolution)) => state.handle_tool_resolution(resolution),
            Ok(Envelope::ChatSessionsEnded { chat, outcome }) => {
                state.on_chat_sessions_ended(chat, outcome)
            }
            Ok(Envelope::PullFinished(id)) => {
                if state
                    .ollama_pull
                    .as_ref()
                    .is_some_and(|(active, ..)| *active == id)
                {
                    state.ollama_pull = None;
                }
            }
            Ok(Envelope::Export(resolution)) => state.handle_export_resolution(resolution),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    state.cancel_active_for_shutdown();
    if let Some(admin) = &mut state.provider_admin {
        admin.stop();
    }
    state.context_pool.stop();
    state.export_pool.stop();
    state.tool_pool.stop();
    state.provider_pool.stop();
    tracing::info!("AI worker stopped");
    Ok(())
}

fn transcript_usage(messages: &[Message], attempts: Vec<InferenceAttempt>) -> Vec<MessageUsage> {
    let mut usage_by_message = attempts
        .into_iter()
        .filter_map(|attempt| {
            let usage = attempt.usage;
            let reported = usage.prompt_tokens.is_some()
                || usage.cached_prompt_tokens.is_some()
                || usage.completion_tokens.is_some()
                || usage.reasoning_tokens.is_some()
                || usage.total_tokens.is_some();
            attempt
                .assistant_message_id
                .filter(|_| reported)
                .map(|message_id| (message_id, usage))
        })
        .collect::<HashMap<_, _>>();
    messages
        .iter()
        .filter_map(|message| {
            usage_by_message
                .remove(&message.id)
                .map(|usage| MessageUsage {
                    message_id: message.id,
                    usage,
                })
        })
        .collect()
}

impl WorkerState {
    fn handle_command(&mut self, command: WorkerCommand) {
        match command {
            WorkerCommand::Ping(reply) => {
                let _ = reply.send(());
            }
            WorkerCommand::Info(reply) => {
                let _ = reply.send(self.info.clone());
            }
            WorkerCommand::ProviderSettings { context, reply } => {
                let _ = reply.send(self.provider_settings(context));
            }
            WorkerCommand::SelectProvider {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.select_provider(context, request));
            }
            WorkerCommand::UpdateOpenAiSettings {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.update_openai_settings(context, request));
            }
            WorkerCommand::UpdateOllamaSettings {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.update_ollama_settings(context, request));
            }
            WorkerCommand::ValidateProvider {
                context,
                provider,
                reply,
            } => self.validate_provider(context, provider, reply),
            WorkerCommand::DiscoverAgentModels {
                context,
                provider,
                reply,
            } => self.discover_agent_models(context, provider, reply),
            WorkerCommand::PullOllamaModel {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.pull_ollama_model(context, request));
            }
            WorkerCommand::CancelOllamaPull { context, reply } => {
                let _ = reply.send(self.cancel_ollama_pull(context));
            }
            WorkerCommand::CreateChat {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.create_chat(context, request));
            }
            WorkerCommand::ListChats { context, reply } => {
                let _ = reply.send(self.list_chats(context));
            }
            WorkerCommand::ReadChat {
                context,
                chat,
                reply,
            } => {
                let _ = reply.send(self.read_chat(context, chat));
            }
            WorkerCommand::RenameChat {
                context,
                chat,
                request,
                reply,
            } => {
                let _ = reply.send(self.rename_chat(context, chat, request));
            }
            WorkerCommand::UpdateChatModel {
                context,
                chat,
                request,
                reply,
            } => {
                let _ = reply.send(self.update_chat_model(context, chat, request));
            }
            WorkerCommand::UpdateChatPrivacy {
                context,
                chat,
                request,
                reply,
            } => {
                let _ = reply.send(self.update_chat_privacy(context, chat, request));
            }
            WorkerCommand::UpdateChatFolder {
                context,
                chat,
                request,
                reply,
            } => {
                let _ = reply.send(self.update_chat_folder(context, chat, request));
            }
            WorkerCommand::ToolListChats {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.tool_list_chats(context, request));
            }
            WorkerCommand::ToolSearchChats {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.tool_search_chats(context, request));
            }
            WorkerCommand::ToolReadChat {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.tool_read_chat(context, request));
            }
            WorkerCommand::UpdateChatToolPolicy {
                context,
                chat,
                request,
                reply,
            } => {
                let _ = reply.send(self.update_chat_tool_policy(context, chat, request));
            }
            WorkerCommand::ReadChatToolPolicy {
                context,
                chat,
                reply,
            } => {
                let _ = reply.send(self.read_chat_tool_policy(context, chat));
            }
            WorkerCommand::Usage {
                context,
                chat,
                reply,
            } => {
                let _ = reply.send(self.usage(context, chat));
            }
            WorkerCommand::DeleteChat {
                context,
                chat,
                reply,
            } => self.begin_delete_chat(context, chat, reply),
            WorkerCommand::ClearHistory { context, reply } => {
                self.begin_clear_history(context, reply)
            }
            WorkerCommand::ClearUsage { context, reply } => {
                let _ = reply.send(self.clear_usage(context));
            }
            WorkerCommand::Search {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.search(context, request));
            }
            WorkerCommand::Feedback {
                context,
                message,
                request,
                reply,
            } => {
                let _ = reply.send(self.feedback(context, message, request));
            }
            WorkerCommand::BranchChat {
                context,
                chat,
                request,
                reply,
            } => {
                let _ = reply.send(self.branch_chat(context, chat, request));
            }
            WorkerCommand::SendTurn {
                context,
                request,
                reply,
            } => {
                let _ = reply.send(self.send_turn(context, request));
            }
            WorkerCommand::ReadAttempt {
                context,
                attempt,
                reply,
            } => {
                let _ = reply.send(self.read_attempt(context, attempt));
            }
            WorkerCommand::CancelAttempt {
                context,
                attempt,
                reply,
            } => {
                let _ = reply.send(self.cancel_attempt(context, attempt));
            }
            WorkerCommand::ApproveToolCall {
                context,
                tool,
                choice,
                reply,
            } => {
                let _ = reply.send(self.approve_tool_call(context, tool, choice));
            }
            WorkerCommand::DenyToolCall {
                context,
                tool,
                choice,
                reply,
            } => {
                let _ = reply.send(self.deny_tool_call(context, tool, choice));
            }
            WorkerCommand::RetryTurn {
                context,
                chat,
                request,
                reply,
            } => {
                let _ = reply.send(self.retry_turn(context, chat, request));
            }
            WorkerCommand::ExportChat {
                context,
                chat,
                request,
                reply,
            } => {
                let _ = reply.send(self.export_chat(context, chat, request));
            }
            WorkerCommand::ReadExport {
                context,
                export,
                reply,
            } => {
                let _ = reply.send(self.read_export(context, export));
            }
            #[cfg(test)]
            WorkerCommand::Hold { entered, release } => {
                let _ = entered.send(());
                let _ = release.recv();
            }
            #[cfg(test)]
            WorkerCommand::Panic(entered) => {
                let _ = entered.send(());
                panic!("requested AI worker command panic");
            }
        }
    }

    fn provider_settings(&self, context: RequestContext) -> ApiResult<ProviderSettingsResponse> {
        authorize(&context, AiScope::ManageProviders)?;
        self.require_provider_runtime()?;
        Ok(settings_view(&self.current_settings()?, &self.agent_views))
    }

    fn select_provider(
        &mut self,
        context: RequestContext,
        request: SelectProviderRequest,
    ) -> ApiResult<ProviderSettingsResponse> {
        authorize(&context, AiScope::ManageProviders)?;
        self.require_provider_runtime()?;
        if !matches!(
            request.provider.as_str(),
            AiProviderId::OPENAI | AiProviderId::OLLAMA | AiProviderId::KORU
        ) && !self
            .agent_views
            .iter()
            .any(|agent| agent.id == request.provider)
        {
            return Err(failure(
                "invalid_request",
                "The selected AI provider is not supported.",
                false,
            ));
        }
        let mut settings = self.current_settings()?;
        settings.selected_provider = Some(request.provider);
        settings.updated_at = Timestamp::now();
        let settings = self.save_settings(settings)?;
        Ok(settings_view(&settings, &self.agent_views))
    }

    fn update_openai_settings(
        &mut self,
        context: RequestContext,
        request: UpdateOpenAiSettingsRequest,
    ) -> ApiResult<OpenAiSettingsView> {
        authorize(&context, AiScope::ManageProviders)?;
        self.require_provider_runtime()?;
        if request.model.as_deref().is_some_and(invalid_provider_text) {
            return Err(failure(
                "invalid_request",
                "A valid OpenAI model is required.",
                false,
            ));
        }
        if let Some(model) = request.model.as_deref() {
            let descriptor = openai_model(model);
            validate_reasoning(&descriptor.capabilities.reasoning, request.reasoning)
                .map_err(ai_failure)?;
        }
        let mut settings = self.current_settings()?;
        settings.openai.model = request.model;
        settings.openai.reasoning = request.reasoning;
        settings.selected_provider =
            Some(AiProviderId::parse(AiProviderId::OPENAI).expect("built-in provider id"));
        settings.updated_at = Timestamp::now();
        let settings = self.save_settings(settings)?;
        Ok(openai_settings_view(&settings))
    }

    fn update_ollama_settings(
        &mut self,
        context: RequestContext,
        request: UpdateOllamaSettingsRequest,
    ) -> ApiResult<OllamaSettingsView> {
        authorize(&context, AiScope::ManageProviders)?;
        self.require_provider_runtime()?;
        let endpoint = url::Url::parse(&request.endpoint)
            .map_err(|_| failure("invalid_request", "The Ollama endpoint is invalid.", false))?;
        if !valid_ollama_endpoint(&endpoint) {
            return Err(failure(
                "invalid_request",
                "The Ollama endpoint must be an HTTP or HTTPS origin without credentials, query, or fragment.",
                false,
            ));
        }
        if request.model.as_deref().is_some_and(invalid_provider_text) {
            return Err(failure(
                "invalid_request",
                "The Ollama model is invalid.",
                false,
            ));
        }
        let mut settings = self.current_settings()?;
        settings.ollama.endpoint = endpoint;
        settings.ollama.model = request.model;
        settings.ollama.reasoning = request.reasoning;
        settings.selected_provider =
            Some(AiProviderId::parse(AiProviderId::OLLAMA).expect("built-in provider id"));
        settings.updated_at = Timestamp::now();
        let settings = self.save_settings(settings)?;
        Ok(ollama_settings_view(&settings))
    }

    fn validate_provider(
        &self,
        context: RequestContext,
        provider: AiProviderId,
        reply: SyncSender<ApiResult<ProviderValidationResponse>>,
    ) {
        if let Err(error) = authorize(&context, AiScope::ManageProviders) {
            let _ = reply.send(Err(error));
            return;
        }
        let Some(admin) = &self.provider_admin else {
            let _ = reply.send(Err(failure(
                "ai_provider_management_unavailable",
                "Provider management is unavailable in this AI service.",
                false,
            )));
            return;
        };
        if let Err(error) = admin.try_submit(ProviderAdminJob::Validate { provider, reply }) {
            if let ProviderAdminJob::Validate { reply, .. } = error.job {
                let _ = reply.send(Err(error.failure));
            }
        }
    }

    /// Listing an agent's models can take the agent seconds, so it runs on
    /// a thread of its own and answers through the held reply; the
    /// dispatcher never waits on it.
    fn discover_agent_models(
        &self,
        context: RequestContext,
        provider: AiProviderId,
        reply: SyncSender<ApiResult<koru_ai_wire::AgentModelsResponse>>,
    ) {
        if let Err(error) = authorize(&context, AiScope::ManageProviders) {
            let _ = reply.send(Err(error));
            return;
        }
        let Some(agents) = self.agents.clone() else {
            let _ = reply.send(Err(failure(
                "agent_service_unavailable",
                "Local agents are not available in this AI service.",
                false,
            )));
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("koru-ai-agent-models".into())
            .spawn(move || {
                let _ = reply.send(agents::agent_models(&agents, provider));
            });
        if spawned.is_err() {
            tracing::warn!("could not start the agent model listing thread");
        }
    }

    fn pull_ollama_model(
        &mut self,
        context: RequestContext,
        request: PullOllamaModelRequest,
    ) -> ApiResult<ModelPullResponseStream> {
        authorize(&context, AiScope::ManageProviders)?;
        if invalid_provider_text(&request.model) {
            return Err(failure(
                "invalid_request",
                "A valid Ollama model is required.",
                false,
            ));
        }
        if self.ollama_pull.is_some() {
            return Err(failure(
                "model_pull_active",
                "An Ollama model pull is already active.",
                true,
            ));
        }
        let admin = self.provider_admin.as_ref().ok_or_else(|| {
            failure(
                "ai_provider_management_unavailable",
                "Provider management is unavailable in this AI service.",
                false,
            )
        })?;
        let cancellation = Cancellation::default();
        let model = request.model;
        let pull_id = Uuid::new_v4();
        let (events, receiver) = sync_channel(STREAM_CAPACITY);
        admin
            .try_submit(ProviderAdminJob::Pull {
                pull_id,
                model: model.clone(),
                cancellation: cancellation.clone(),
                events,
            })
            .map_err(|error| error.failure)?;
        self.ollama_pull = Some((pull_id, model.clone(), cancellation));
        Ok(ModelPullResponseStream {
            accepted: PullOllamaModelAccepted { model },
            events: receiver,
        })
    }

    fn cancel_ollama_pull(
        &mut self,
        context: RequestContext,
    ) -> ApiResult<CancelModelPullResponse> {
        authorize(&context, AiScope::ManageProviders)?;
        let cancelled = self.ollama_pull.take().is_some_and(|(_, _, cancellation)| {
            cancellation.cancel();
            true
        });
        Ok(CancelModelPullResponse { cancelled })
    }

    fn current_settings(&self) -> ApiResult<AiSettings> {
        let settings = self
            .repository
            .settings()
            .map_err(ai_failure)
            .map(|settings| {
                settings.unwrap_or_else(|| AiSettings::unconfigured(Timestamp::now()))
            })?;
        Ok(match &self.provider_runtime {
            Some(runtime) => {
                let mut settings = settings;
                settings.openai.key_account = runtime
                    .settings()
                    .and_then(|runtime_settings| runtime_settings.openai.key_account);
                settings
            }
            None => settings,
        })
    }

    fn save_settings(&mut self, settings: AiSettings) -> ApiResult<AiSettings> {
        let runtime = self.require_provider_runtime()?.clone();
        let settings = runtime_settings(&runtime, settings);
        let mut persisted = settings.clone();
        persisted.openai.key_account = None;
        self.repository
            .save_settings(&persisted)
            .map_err(ai_failure)?;
        runtime.replace_settings(Some(settings.clone()));
        Ok(settings)
    }

    fn require_provider_runtime(&self) -> ApiResult<&ProviderRuntime> {
        self.provider_runtime.as_ref().ok_or_else(|| {
            failure(
                "ai_provider_management_unavailable",
                "Provider management is unavailable in this AI service.",
                false,
            )
        })
    }

    fn create_chat(
        &mut self,
        context: RequestContext,
        request: CreateChatRequest,
    ) -> ApiResult<CreateChatResponse> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(None)?;
        if request.title.trim().is_empty() || request.model.requested_model.trim().is_empty() {
            return Err(failure(
                "invalid_request",
                "A chat title and model are required.",
                false,
            ));
        }
        let now = Timestamp::now();
        let chat = Chat {
            id: request.chat_id,
            title: request.title,
            private: false,
            current_model: request.model,
            branched_from: None,
            agent_working_folder: None,
            created_at: now,
            updated_at: now,
        };
        if let Some(existing) = self.repository.chat(chat.id).map_err(ai_failure)? {
            if existing.title == chat.title && existing.current_model == chat.current_model {
                return Ok(CreateChatResponse { chat: existing });
            }
            return Err(failure(
                "idempotency_conflict",
                "This chat identity was already used for different content.",
                false,
            ));
        }
        self.repository.create_chat(&chat).map_err(ai_failure)?;
        Ok(CreateChatResponse { chat })
    }

    fn read_chat(&self, context: RequestContext, chat_id: ChatId) -> ApiResult<ChatTranscript> {
        authorize(&context, AiScope::Chat)?;
        let chat = self
            .repository
            .chat(chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let messages = self.repository.messages(chat_id).map_err(ai_failure)?;
        let usage = transcript_usage(
            &messages,
            self.repository
                .attempts_for_chat(chat_id)
                .map_err(ai_failure)?,
        );
        let mut sources = Vec::<MessageSources>::new();
        for (message_id, source) in self.repository.citations(chat_id).map_err(ai_failure)? {
            if let Some(group) = sources
                .iter_mut()
                .find(|group| group.message_id == message_id)
            {
                group.sources.push(source);
            } else {
                sources.push(MessageSources {
                    message_id,
                    sources: vec![source],
                });
            }
        }
        let reasoning_control = self.reasoning_control(&chat.current_model);
        Ok(ChatTranscript {
            chat,
            tool_policy: self
                .repository
                .chat_tool_policy(chat_id)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?,
            tool_activities: {
                let mut activities = self
                    .repository
                    .tool_activities(chat_id)
                    .map_err(ai_failure)?;
                self.decorate_live(&mut activities);
                activities
            },
            messages,
            blocks: self.repository.blocks(chat_id).map_err(ai_failure)?,
            pinned_documents: self
                .repository
                .pinned_documents(chat_id)
                .map_err(ai_failure)?,
            feedback: self.repository.feedback(chat_id).map_err(ai_failure)?,
            usage,
            sources,
            reasoning_control,
        })
    }

    fn rename_chat(
        &mut self,
        context: RequestContext,
        chat_id: ChatId,
        request: RenameChatRequest,
    ) -> ApiResult<RenameChatResponse> {
        authorize(&context, AiScope::Chat)?;
        let title = request.title.trim();
        if title.is_empty()
            || title.len() > 240
            || title
                .chars()
                .any(|ch| ch == '\0' || ch == '\n' || ch == '\r')
        {
            return Err(failure(
                "invalid_request",
                "A chat title must be one non-empty line of at most 240 bytes.",
                false,
            ));
        }
        if !self
            .repository
            .rename_chat(chat_id, title, Timestamp::now())
            .map_err(ai_failure)?
        {
            return Err(not_found());
        }
        let chat = self
            .repository
            .chat(chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        Ok(RenameChatResponse { chat })
    }

    fn update_chat_model(
        &mut self,
        context: RequestContext,
        chat_id: ChatId,
        request: UpdateChatModelRequest,
    ) -> ApiResult<UpdateChatModelResponse> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(Some(chat_id))?;
        if self.chat_has_active_attempt(chat_id)? {
            return Err(active_history_conflict());
        }
        let mut model = request.model;
        model.requested_model = model.requested_model.trim().to_owned();
        if model.requested_model.is_empty() || invalid_provider_text(&model.requested_model) {
            return Err(failure(
                "invalid_request",
                "A valid AI model is required.",
                false,
            ));
        }
        match model.provider.as_str() {
            AiProviderId::OPENAI => {
                let runtime = self.require_provider_runtime()?;
                if !runtime.openai_credential_configured() {
                    return Err(failure(
                        "ai_provider_unconfigured",
                        "OpenAI credentials are not configured.",
                        false,
                    ));
                }
                let models = runtime.openai_models().map_err(ai_failure)?;
                if !models
                    .iter()
                    .any(|available| available == &model.requested_model)
                {
                    return Err(failure(
                        "ai_model_unavailable",
                        "That OpenAI model is not available to this project.",
                        false,
                    ));
                }
                let descriptor = openai_model(&model.requested_model);
                validate_reasoning(&descriptor.capabilities.reasoning, model.default_reasoning)
                    .map_err(ai_failure)?;
            }
            AiProviderId::OLLAMA => {
                let runtime = self.require_provider_runtime()?;
                let models = runtime.ollama_models().map_err(ai_failure)?;
                if !models
                    .iter()
                    .any(|available| available == &model.requested_model)
                {
                    return Err(failure(
                        "ai_model_unavailable",
                        "That Ollama model is not installed.",
                        false,
                    ));
                }
                let descriptor = runtime
                    .validate_ollama(&model.requested_model, model.default_reasoning)
                    .map_err(ai_failure)?;
                self.ollama_controls.insert(
                    model.requested_model.clone(),
                    descriptor.capabilities.reasoning,
                );
            }
            AiProviderId::KORU => {
                let runtime = self.require_provider_runtime()?;
                if model.requested_model != KORU_MODEL {
                    return Err(failure(
                        "ai_model_unavailable",
                        "The hosted Koru AI model is not available.",
                        false,
                    ));
                }
                runtime
                    .validate_koru(model.default_reasoning)
                    .map_err(ai_failure)?;
            }
            _ => {
                if !self
                    .info
                    .providers
                    .iter()
                    .any(|provider| provider == model.provider.as_str())
                {
                    return Err(failure(
                        "invalid_request",
                        "The selected AI provider is not supported.",
                        false,
                    ));
                }
            }
        }
        if !self
            .repository
            .update_chat_model(chat_id, &model, Timestamp::now())
            .map_err(ai_failure)?
        {
            return Err(not_found());
        }
        let chat = self
            .repository
            .chat(chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let reasoning_control = self.reasoning_control(&chat.current_model);
        Ok(UpdateChatModelResponse {
            chat,
            reasoning_control,
        })
    }

    fn update_chat_privacy(
        &mut self,
        context: RequestContext,
        chat_id: ChatId,
        request: UpdateChatPrivacyRequest,
    ) -> ApiResult<UpdateChatPrivacyResponse> {
        authorize(&context, AiScope::Chat)?;
        let chat = self
            .repository
            .set_chat_private(chat_id, request.private, Timestamp::now())
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        Ok(UpdateChatPrivacyResponse { chat })
    }

    /// What a chat's model offers as an effort control, from what the
    /// service already holds: the static OpenAI and Koru tables, and the
    /// Ollama control kept from the model's last inspection. A local agent
    /// advertises none (I4: a launch-only route gets no slider).
    fn reasoning_control(&self, model: &koru_ai_core::ModelSelection) -> Option<ReasoningControl> {
        match model.provider.as_str() {
            AiProviderId::OPENAI => {
                Some(openai_model(&model.requested_model).capabilities.reasoning)
            }
            AiProviderId::KORU => Some(koru_model().capabilities.reasoning),
            AiProviderId::OLLAMA => self.ollama_controls.get(&model.requested_model).cloned(),
            _ => None,
        }
    }

    /// The folder a chat's agent works in. Only an agent chat with no
    /// message yet may choose one: the agent starts in the folder, and a
    /// conversation that already happened elsewhere would be misdescribed.
    fn update_chat_folder(
        &mut self,
        context: RequestContext,
        chat_id: ChatId,
        request: koru_ai_wire::UpdateChatFolderRequest,
    ) -> ApiResult<koru_ai_wire::UpdateChatFolderResponse> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(Some(chat_id))?;
        let chat = self
            .repository
            .chat(chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if !is_agent_provider(&chat.current_model.provider) {
            return Err(failure(
                "invalid_request",
                "Only a chat with a local agent has a working folder.",
                false,
            ));
        }
        if !self
            .repository
            .messages(chat_id)
            .map_err(ai_failure)?
            .is_empty()
        {
            return Err(failure(
                "folder_locked",
                "The agent already started in its folder. Start a new chat to work somewhere else.",
                false,
            ));
        }
        let folder = match request.folder.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(folder) => {
                let path = std::path::Path::new(folder);
                if !path.is_absolute() || !path.is_dir() {
                    return Err(failure(
                        "invalid_request",
                        "The working folder must be an existing folder on this computer.",
                        false,
                    ));
                }
                Some(folder)
            }
        };
        let chat = self
            .repository
            .set_chat_working_folder(chat_id, folder, Timestamp::now())
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        Ok(koru_ai_wire::UpdateChatFolderResponse { chat })
    }

    fn tool_list_chats(
        &self,
        context: RequestContext,
        request: ListChatsToolRequest,
    ) -> ApiResult<ListChatsToolResponse> {
        authorize(&context, AiScope::Chat)?;
        koru_ai_core::chat_tools::list_chats(
            &self.repository,
            ChatAccessLens::ToolSurface { calling_chat: None },
            request,
        )
        .map_err(ai_failure)
    }

    fn tool_search_chats(
        &self,
        context: RequestContext,
        request: SearchChatsToolRequest,
    ) -> ApiResult<SearchChatsToolResponse> {
        authorize(&context, AiScope::Chat)?;
        koru_ai_core::chat_tools::search_chats(
            &self.repository,
            ChatAccessLens::ToolSurface { calling_chat: None },
            request,
        )
        .map_err(ai_failure)
    }

    fn tool_read_chat(
        &self,
        context: RequestContext,
        request: ReadChatToolRequest,
    ) -> ApiResult<ReadChatToolResponse> {
        authorize(&context, AiScope::Chat)?;
        koru_ai_core::chat_tools::read_chat(
            &self.repository,
            ChatAccessLens::ToolSurface { calling_chat: None },
            request,
        )
        .map_err(|error| {
            if error.public_message == "Chat not found." {
                failure("chat_not_found", "Chat not found.", false)
            } else {
                ai_failure(error)
            }
        })
    }

    fn update_chat_tool_policy(
        &mut self,
        context: RequestContext,
        chat_id: ChatId,
        request: UpdateChatToolPolicyRequest,
    ) -> ApiResult<ChatToolPolicyResponse> {
        authorize(&context, AiScope::Chat)?;
        if self.chat_has_active_attempt(chat_id)? {
            return Err(active_history_conflict());
        }
        let policy = self
            .repository
            .replace_chat_tool_policy(
                chat_id,
                request.mode,
                &request.workspace_ids,
                Timestamp::now(),
            )
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        Ok(ChatToolPolicyResponse { policy })
    }

    fn read_chat_tool_policy(
        &self,
        context: RequestContext,
        chat_id: ChatId,
    ) -> ApiResult<ChatToolPolicyResponse> {
        authorize(&context, AiScope::Chat)?;
        let policy = self
            .repository
            .chat_tool_policy(chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        Ok(ChatToolPolicyResponse { policy })
    }

    fn usage(&self, context: RequestContext, chat_id: Option<ChatId>) -> ApiResult<UsageResponse> {
        authorize(&context, AiScope::ReadUsage)?;
        if let Some(chat_id) = chat_id
            && self.repository.chat(chat_id).map_err(ai_failure)?.is_none()
        {
            return Err(not_found());
        }
        Ok(UsageResponse {
            report: self.repository.usage(chat_id).map_err(ai_failure)?,
        })
    }

    fn clear_usage(&mut self, context: RequestContext) -> ApiResult<ClearUsageResponse> {
        authorize(&context, AiScope::ReadUsage)?;
        if !self.active_work.is_empty() {
            return Err(active_history_conflict());
        }
        let cleared = self.repository.clear_usage().map_err(ai_failure)?;
        Ok(ClearUsageResponse {
            attempts_deleted: cleared.attempts,
            feedback_events_deleted: cleared.feedback_events,
        })
    }

    fn chat_has_active_attempt(&self, chat_id: ChatId) -> ApiResult<bool> {
        for attempt_id in self.active_work.keys().copied() {
            if self
                .repository
                .attempt(attempt_id)
                .map_err(ai_failure)?
                .is_some_and(|attempt| attempt.chat_id == Some(chat_id))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn search(
        &self,
        context: RequestContext,
        request: AiSearchRequest,
    ) -> ApiResult<AiSearchResponse> {
        authorize(&context, AiScope::Chat)?;
        if request.text.len() > 4_096 || request.text.chars().any(char::is_control) {
            return Err(failure(
                "invalid_request",
                "The AI search query is invalid.",
                false,
            ));
        }
        if request.limit == 0 || request.limit > 200 {
            return Err(failure(
                "invalid_request",
                "The AI search limit must be between 1 and 200.",
                false,
            ));
        }
        if request
            .cursor
            .as_deref()
            .is_some_and(|cursor| cursor.parse::<usize>().is_err())
        {
            return Err(failure(
                "invalid_cursor",
                "The AI search cursor is invalid.",
                false,
            ));
        }
        let page = self
            .repository
            .search(&AiSearchQuery {
                text: request.text,
                limit: request.limit,
                cursor: request.cursor,
                visibility: ChatAccessLens::UserSurface,
            })
            .map_err(ai_failure)?;
        Ok(AiSearchResponse {
            page,
            request_seq: request.request_seq,
        })
    }

    fn feedback(
        &mut self,
        context: RequestContext,
        message_id: MessageId,
        request: FeedbackRequest,
    ) -> ApiResult<FeedbackResponse> {
        authorize(&context, AiScope::Chat)?;
        let message = self
            .repository
            .message(message_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if message.role != ChatRole::Assistant || !message.state.is_terminal() {
            return Err(failure(
                "invalid_request",
                "Only a finished AI response can be rated.",
                false,
            ));
        }
        let event = FeedbackEvent {
            id: request.event_id,
            chat_id: Some(message.chat_id),
            message_id: Some(message.id),
            attempt_id: self
                .repository
                .attempt_for_message(message.id)
                .map_err(ai_failure)?
                .map(|attempt| attempt.id),
            value: request.value,
            surface: request.surface,
            created_at: Timestamp::now(),
        };
        let stored = self
            .repository
            .append_feedback(&event)
            .map_err(ai_failure)?;
        if stored.message_id != event.message_id
            || stored.chat_id != event.chat_id
            || stored.attempt_id != event.attempt_id
            || stored.value != event.value
            || stored.surface != event.surface
        {
            return Err(failure(
                "idempotency_conflict",
                "This feedback identity was already used for another rating.",
                false,
            ));
        }
        Ok(FeedbackResponse { event: stored })
    }

    fn branch_chat(
        &mut self,
        context: RequestContext,
        source_chat: ChatId,
        request: BranchChatRequest,
    ) -> ApiResult<BranchChatResponse> {
        authorize(&context, AiScope::Chat)?;
        if request.title.trim().is_empty() {
            return Err(failure(
                "invalid_request",
                "A branch title is required.",
                false,
            ));
        }
        if let Some(existing) = self
            .repository
            .chat(request.branch_chat_id)
            .map_err(ai_failure)?
        {
            let expected = BranchOrigin {
                chat_id: source_chat,
                through_message_id: request.through_message_id,
            };
            if existing.branched_from.as_ref() != Some(&expected) || existing.title != request.title
            {
                return Err(failure(
                    "idempotency_conflict",
                    "This chat identity was already used for another branch.",
                    false,
                ));
            }
            return self
                .read_chat(context, existing.id)
                .map(|transcript| BranchChatResponse { transcript });
        }
        let source = self
            .repository
            .chat(source_chat)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let source_messages = self.repository.messages(source_chat).map_err(ai_failure)?;
        let through = source_messages
            .iter()
            .find(|message| message.id == request.through_message_id)
            .ok_or_else(not_found)?;
        if !through.state.is_terminal() {
            return Err(failure(
                "invalid_request",
                "A branch must end at a finished message.",
                false,
            ));
        }
        let through_ordinal = through.ordinal;
        let now = Timestamp::now();
        let chat = Chat {
            id: request.branch_chat_id,
            title: request.title,
            private: source.private,
            current_model: source.current_model,
            branched_from: Some(BranchOrigin {
                chat_id: source_chat,
                through_message_id: through.id,
            }),
            agent_working_folder: source.agent_working_folder.clone(),
            created_at: now,
            updated_at: now,
        };
        let prefix: Vec<_> = source_messages
            .into_iter()
            .filter(|message| message.ordinal <= through_ordinal)
            .collect();
        let mut message_ids = HashMap::new();
        let messages: Vec<_> = prefix
            .into_iter()
            .map(|mut message| {
                let origin = message.id;
                message.id = MessageId::new();
                message.chat_id = chat.id;
                message.origin_message_id = Some(origin);
                message_ids.insert(origin, message.id);
                message
            })
            .collect();
        let blocks = self
            .repository
            .blocks(source_chat)
            .map_err(ai_failure)?
            .into_iter()
            .filter_map(|mut block| {
                let message = message_ids.get(&block.message_id).copied()?;
                block.id = MessageBlockId::new();
                block.message_id = message;
                Some(block)
            })
            .collect();
        let citations = self
            .repository
            .citations(source_chat)
            .map_err(ai_failure)?
            .into_iter()
            .filter_map(|(message, source)| {
                message_ids
                    .get(&message)
                    .copied()
                    .map(|message| (message, source))
            })
            .collect();
        let pinned_documents = self
            .repository
            .pinned_documents(source_chat)
            .map_err(ai_failure)?
            .into_iter()
            .map(|mut pin| {
                pin.chat_id = chat.id;
                pin
            })
            .collect();
        let mut tool_policy = self
            .repository
            .chat_tool_policy(source_chat)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        tool_policy.chat_id = chat.id;
        self.repository
            .create_branch(&BranchCopy {
                chat,
                tool_policy,
                messages,
                blocks,
                citations,
                pinned_documents,
            })
            .map_err(ai_failure)?;
        self.read_chat(context, request.branch_chat_id)
            .map(|transcript| BranchChatResponse { transcript })
    }

    fn export_chat(
        &mut self,
        context: RequestContext,
        chat_id: ChatId,
        request: ExportChatRequest,
    ) -> ApiResult<ExportChatResponse> {
        authorize(&context, AiScope::Chat)?;
        if request.destination.title.trim().is_empty() {
            return Err(failure(
                "invalid_request",
                "A document title is required.",
                false,
            ));
        }
        let chat = self
            .repository
            .chat(chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let messages = self.repository.messages(chat_id).map_err(ai_failure)?;
        let through = messages
            .iter()
            .find(|message| message.id == request.through_message_id)
            .ok_or_else(not_found)?;
        if through.role != ChatRole::Assistant || through.state != MessageState::Complete {
            return Err(failure(
                "invalid_request",
                "Only a completed response can be stored as a document.",
                false,
            ));
        }
        let blocks = self.repository.blocks(chat_id).map_err(ai_failure)?;
        let first_block = blocks
            .iter()
            .find(|block| block.message_id == through.id)
            .ok_or_else(|| {
                failure(
                    "invalid_request",
                    "The response has no finalized block to link.",
                    false,
                )
            })?;
        let now = Timestamp::now();
        let requested = DocumentExport {
            id: request.export_id,
            idempotency_key: request.idempotency_key,
            chat_id,
            through_message_id: through.id,
            destination: request.destination,
            status: ExportStatus::Pending,
            document_id: None,
            error_class: None,
            created_at: now,
            completed_at: None,
        };
        let stored = self
            .repository
            .begin_export(&requested)
            .map_err(ai_failure)?;
        if stored.id != requested.id
            || stored.chat_id != requested.chat_id
            || stored.through_message_id != requested.through_message_id
            || stored.destination != requested.destination
        {
            return Err(failure(
                "idempotency_conflict",
                "This export key was already used for another destination.",
                false,
            ));
        }
        if stored.status == ExportStatus::Pending {
            let markdown = export_markdown(&messages, through.ordinal, chat.id, first_block.id);
            self.export_pool
                .try_submit(ExportJob {
                    export: stored.clone(),
                    markdown,
                })
                .map_err(ai_failure)?;
        }
        Ok(ExportChatResponse { export: stored })
    }

    fn read_export(
        &self,
        context: RequestContext,
        export: DocumentExportId,
    ) -> ApiResult<ExportChatResponse> {
        authorize(&context, AiScope::Chat)?;
        Ok(ExportChatResponse {
            export: self
                .repository
                .export(export)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?,
        })
    }

    fn handle_export_resolution(&mut self, resolution: ExportResolution) {
        let completed_at = Timestamp::now();
        let (outcome, error_class) = match &resolution.result {
            Ok(_) => ("complete", "none"),
            Err(error) => ("failed", error_code(error.class)),
        };
        let result = match resolution.result {
            Ok(document) => self.repository.finish_export(
                resolution.export_id,
                Some(document),
                None,
                completed_at,
            ),
            Err(error) => self.repository.finish_export(
                resolution.export_id,
                None,
                Some(error.class),
                completed_at,
            ),
        };
        if result.is_err() {
            tracing::error!(export = %resolution.export_id, "AI document export completion could not be saved");
        } else {
            tracing::info!(
                export = %resolution.export_id,
                outcome,
                error_class,
                elapsed_ms = resolution.elapsed.as_secs_f64() * 1_000.0,
                "AI document export finished"
            );
        }
    }

    fn list_chats(&self, context: RequestContext) -> ApiResult<ListChatsResponse> {
        authorize(&context, AiScope::Chat)?;
        Ok(ListChatsResponse {
            chats: self.repository.chats().map_err(ai_failure)?,
        })
    }

    fn send_turn(
        &mut self,
        context: RequestContext,
        request: SendTurnRequest,
    ) -> ApiResult<AiResponseStream> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(Some(request.chat_id))?;
        if let Some(existing) = self
            .repository
            .attempt_by_operation(request.operation_id)
            .map_err(ai_failure)?
        {
            return replay_stream(&self.repository, existing);
        }
        if request.markdown.trim().is_empty() {
            return Err(failure("invalid_request", "A message is required.", false));
        }
        if request.explicit_refs.len() > MAX_CONTEXT_REFS {
            return Err(failure(
                "invalid_request",
                "Too many context references were selected.",
                false,
            ));
        }
        let system_prompt_override = request
            .system_prompt_override
            .as_deref()
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty());
        if system_prompt_override.is_some()
            && !koru_env::switch(koru_env::name::KORU_AI_PAYLOAD_DEBUG)
        {
            return Err(failure(
                "invalid_request",
                "System prompt overrides require KORU_AI_PAYLOAD_DEBUG.",
                false,
            ));
        }
        if system_prompt_override
            .is_some_and(|prompt| prompt.len() > 32 * 1024 || prompt.chars().any(|ch| ch == '\0'))
        {
            return Err(failure(
                "invalid_request",
                "The system prompt override is invalid or too large.",
                false,
            ));
        }
        let prompt_template_version = if system_prompt_override.is_some() {
            "debug-override-v1"
        } else {
            PROMPT_TEMPLATE_VERSION
        };
        let system_prompt = system_prompt_override.unwrap_or(CHAT_PROMPT).to_owned();
        let chat = self
            .repository
            .chat(request.chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let existing_messages = self
            .repository
            .messages(request.chat_id)
            .map_err(ai_failure)?;
        if existing_messages.iter().any(|message| {
            message.role == ChatRole::Assistant && message.state == MessageState::Pending
        }) {
            return Err(failure(
                "chat_busy",
                "This chat already has a response in progress.",
                true,
            ));
        }
        let user_ordinal = existing_messages.last().map_or(Ok(0), |message| {
            message.ordinal.checked_add(1).ok_or_else(invariant_failure)
        })?;
        let assistant_ordinal = user_ordinal.checked_add(1).ok_or_else(invariant_failure)?;
        let now = Timestamp::now();
        let user_id = MessageId::new();
        let assistant_id = MessageId::new();
        let attempt_id = InferenceAttemptId::new();
        let reasoning = request
            .reasoning_override
            .unwrap_or(chat.current_model.default_reasoning);
        validate_web_search(&chat, request.web_search)?;
        let user_message = Message {
            id: user_id,
            chat_id: chat.id,
            ordinal: user_ordinal,
            role: ChatRole::User,
            state: MessageState::Complete,
            markdown: request.markdown.clone(),
            origin_message_id: None,
            provider: None,
            requested_model: None,
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: Some(now),
        };
        let assistant_message = Message {
            id: assistant_id,
            chat_id: chat.id,
            ordinal: assistant_ordinal,
            role: ChatRole::Assistant,
            state: MessageState::Pending,
            markdown: String::new(),
            origin_message_id: None,
            provider: Some(chat.current_model.provider.clone()),
            requested_model: Some(chat.current_model.requested_model.clone()),
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: None,
        };
        let mut provider_messages: Vec<AiRequestMessage> = existing_messages
            .iter()
            .filter(|message| message.state == MessageState::Complete)
            .map(|message| AiRequestMessage {
                role: message.role,
                markdown: message.markdown.clone(),
            })
            .collect();
        provider_messages.push(AiRequestMessage {
            role: ChatRole::User,
            markdown: request.markdown,
        });
        let attempt = InferenceAttempt {
            id: attempt_id,
            operation_id: request.operation_id,
            chat_id: Some(chat.id),
            assistant_message_id: Some(assistant_id),
            surface: request.surface,
            intent: AiIntent::Chat,
            provider: chat.current_model.provider.clone(),
            requested_model: chat.current_model.requested_model.clone(),
            resolved_model: None,
            requested_reasoning: reasoning,
            effective_reasoning: None,
            prompt_template_version: prompt_template_version.into(),
            status: AttemptStatus::Pending,
            queued_at: now,
            context_started_at: None,
            context_completed_at: None,
            provider_started_at: None,
            first_token_at: None,
            completed_at: None,
            request_payload_bytes: None,
            web_search_requested: request.web_search,
            web_search_calls: 0,
            context_payload_bytes: 0,
            message_count: provider_messages.len().try_into().unwrap_or(u32::MAX),
            context_document_count: 0,
            context_message_block_count: 0,
            usage: Default::default(),
            stop_reason: None,
            error_class: None,
            provider_response_id: None,
        };
        self.repository
            .begin_turn(&PendingTurn {
                user_message,
                assistant_message,
                attempt: attempt.clone(),
            })
            .map_err(ai_failure)?;

        let (stream_sender, events) = sync_channel(STREAM_CAPACITY);
        self.streams.insert(
            attempt_id,
            StreamState {
                sender: stream_sender,
                next_sequence: 1,
            },
        );
        let cancellation = Cancellation::default();
        self.cancellations.insert(attempt_id, cancellation.clone());
        let base_request = AiRequest {
            attempt_id,
            provider: attempt.provider.clone(),
            model: attempt.requested_model.clone(),
            reasoning,
            prompt_template_version: prompt_template_version.into(),
            system: system_prompt,
            messages: provider_messages,
            context: Vec::new(),
            tools: self.tool_definitions.clone(),
            tool_history: Vec::new(),
            options: AiRequestOptions {
                web_search: request.web_search,
                chat_id: Some(chat.id),
                working_folder: chat.agent_working_folder.clone(),
                ..AiRequestOptions::default()
            },
        };
        self.work_started(attempt_id);
        let context_started_at = Timestamp::now();
        let context_job = self.prepare_context_job(
            attempt_id,
            user_id,
            chat.id,
            base_request,
            request.explicit_refs,
            request.replace_pinned_documents,
            context_started_at,
            cancellation,
        );
        match context_job {
            Ok(job) => match self.context_pool.try_submit(job) {
                Ok(()) => self.work_admitted(attempt_id),
                Err(error) => self.fail_context(attempt_id, context_started_at, error),
            },
            Err(error) => self.fail_context(attempt_id, context_started_at, error),
        }
        tracing::info!(attempt = %attempt_id, "AI attempt queued");
        Ok(AiResponseStream {
            accepted: SendTurnAccepted {
                attempt_id,
                user_message_id: user_id,
                assistant_message_id: assistant_id,
                replayed: false,
            },
            events,
        })
    }

    fn retry_turn(
        &mut self,
        context: RequestContext,
        chat_id: ChatId,
        request: RetryTurnRequest,
    ) -> ApiResult<AiResponseStream> {
        authorize(&context, AiScope::Chat)?;
        if let Some(existing) = self
            .repository
            .attempt_by_operation(request.operation_id)
            .map_err(ai_failure)?
        {
            return replay_stream(&self.repository, existing);
        }
        if request.explicit_refs.len() > MAX_CONTEXT_REFS {
            return Err(failure(
                "invalid_request",
                "Too many context references were selected.",
                false,
            ));
        }
        let chat = self
            .repository
            .chat(chat_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        let messages = self.repository.messages(chat_id).map_err(ai_failure)?;
        if messages.iter().any(|message| {
            message.role == ChatRole::Assistant && message.state == MessageState::Pending
        }) {
            return Err(failure(
                "chat_busy",
                "This chat already has a response in progress.",
                true,
            ));
        }
        let failed = messages
            .iter()
            .find(|message| message.id == request.message_id)
            .ok_or_else(not_found)?;
        if failed.role != ChatRole::Assistant
            || !matches!(
                failed.state,
                MessageState::Failed | MessageState::Interrupted
            )
        {
            return Err(failure(
                "invalid_request",
                "Only a failed or interrupted response can be retried.",
                false,
            ));
        }
        let user = messages
            .iter()
            .rev()
            .find(|message| message.ordinal < failed.ordinal && message.role == ChatRole::User)
            .ok_or_else(|| {
                failure(
                    "invalid_request",
                    "The response has no user message to retry.",
                    false,
                )
            })?;
        let ordinal = messages.last().map_or(Ok(0), |message| {
            message.ordinal.checked_add(1).ok_or_else(invariant_failure)
        })?;
        let now = Timestamp::now();
        let assistant_id = MessageId::new();
        let attempt_id = InferenceAttemptId::new();
        let reasoning = request
            .reasoning_override
            .unwrap_or(chat.current_model.default_reasoning);
        validate_web_search(&chat, request.web_search)?;
        let assistant_message = Message {
            id: assistant_id,
            chat_id,
            ordinal,
            role: ChatRole::Assistant,
            state: MessageState::Pending,
            markdown: String::new(),
            origin_message_id: Some(failed.id),
            provider: Some(chat.current_model.provider.clone()),
            requested_model: Some(chat.current_model.requested_model.clone()),
            resolved_model: None,
            error_class: None,
            created_at: now,
            completed_at: None,
        };
        let provider_messages: Vec<_> = messages
            .iter()
            .filter(|message| message.state == MessageState::Complete)
            .map(|message| AiRequestMessage {
                role: message.role,
                markdown: message.markdown.clone(),
            })
            .collect();
        let attempt = InferenceAttempt {
            id: attempt_id,
            operation_id: request.operation_id,
            chat_id: Some(chat_id),
            assistant_message_id: Some(assistant_id),
            surface: request.surface,
            intent: AiIntent::Chat,
            provider: chat.current_model.provider.clone(),
            requested_model: chat.current_model.requested_model.clone(),
            resolved_model: None,
            requested_reasoning: reasoning,
            effective_reasoning: None,
            prompt_template_version: PROMPT_TEMPLATE_VERSION.into(),
            status: AttemptStatus::Pending,
            queued_at: now,
            context_started_at: None,
            context_completed_at: None,
            provider_started_at: None,
            first_token_at: None,
            completed_at: None,
            request_payload_bytes: None,
            web_search_requested: request.web_search,
            web_search_calls: 0,
            context_payload_bytes: 0,
            message_count: provider_messages.len().try_into().unwrap_or(u32::MAX),
            context_document_count: 0,
            context_message_block_count: 0,
            usage: Default::default(),
            stop_reason: None,
            error_class: None,
            provider_response_id: None,
        };
        self.repository
            .begin_retry(&PendingRetry {
                assistant_message,
                attempt: attempt.clone(),
            })
            .map_err(ai_failure)?;
        let (stream_sender, events) = sync_channel(STREAM_CAPACITY);
        self.streams.insert(
            attempt_id,
            StreamState {
                sender: stream_sender,
                next_sequence: 1,
            },
        );
        let cancellation = Cancellation::default();
        self.cancellations.insert(attempt_id, cancellation.clone());
        let base_request = AiRequest {
            attempt_id,
            provider: attempt.provider.clone(),
            model: attempt.requested_model.clone(),
            reasoning,
            prompt_template_version: PROMPT_TEMPLATE_VERSION.into(),
            system: CHAT_PROMPT.into(),
            messages: provider_messages,
            context: Vec::new(),
            tools: self.tool_definitions.clone(),
            tool_history: Vec::new(),
            options: AiRequestOptions {
                web_search: request.web_search,
                chat_id: Some(chat_id),
                working_folder: chat.agent_working_folder.clone(),
                ..AiRequestOptions::default()
            },
        };
        let context_started_at = Timestamp::now();
        match self.prepare_context_job(
            attempt_id,
            user.id,
            chat_id,
            base_request,
            request.explicit_refs,
            request.replace_pinned_documents,
            context_started_at,
            cancellation,
        ) {
            Ok(job) => {
                if let Err(error) = self.context_pool.try_submit(job) {
                    self.fail_context(attempt_id, context_started_at, error);
                }
            }
            Err(error) => self.fail_context(attempt_id, context_started_at, error),
        }
        Ok(AiResponseStream {
            accepted: SendTurnAccepted {
                attempt_id,
                user_message_id: user.id,
                assistant_message_id: assistant_id,
                replayed: false,
            },
            events,
        })
    }

    fn prepare_context_job(
        &self,
        attempt_id: InferenceAttemptId,
        user_message_id: MessageId,
        chat_id: ChatId,
        request: AiRequest,
        explicit_refs: Vec<ContextRef>,
        replace_pinned_documents: bool,
        started_at: Timestamp,
        cancellation: Cancellation,
    ) -> Result<ContextJob, AiError> {
        let pinned = if replace_pinned_documents {
            Vec::new()
        } else {
            self.repository.pinned_documents(chat_id)?
        };
        let mut known_documents: HashMap<_, _> = pinned
            .iter()
            .cloned()
            .map(|document| (document.document_id, document))
            .collect();
        let mut seen_documents = HashSet::new();
        let mut seen_blocks = HashSet::new();
        let mut refs = Vec::new();

        for document in &pinned {
            if seen_documents.insert(document.document_id) {
                refs.push(PreparedContextRef::Document(document.document_id));
            }
        }
        for reference in explicit_refs {
            match reference {
                ContextRef::Document { document_id } => {
                    known_documents
                        .entry(document_id)
                        .or_insert(PinnedDocument {
                            chat_id,
                            document_id,
                            display_title: String::new(),
                            pinned: true,
                            first_used_at: started_at,
                            last_used_at: started_at,
                        });
                    if seen_documents.insert(document_id) {
                        refs.push(PreparedContextRef::Document(document_id));
                    }
                }
                ContextRef::MessageBlock { block_id } => {
                    if seen_blocks.insert(block_id) {
                        let block = self
                            .repository
                            .message_block_context(block_id)?
                            .ok_or_else(|| {
                                AiError::new(
                                    AiErrorClass::Storage,
                                    "The selected AI message block is not available.",
                                    false,
                                )
                            })?;
                        if block.chat_private && block.chat_id != chat_id {
                            return Err(AiError::new(
                                AiErrorClass::Storage,
                                "The selected AI message block is not available.",
                                false,
                            ));
                        }
                        refs.push(PreparedContextRef::MessageBlock(block));
                    }
                }
            }
        }
        if refs.len() > MAX_CONTEXT_REFS {
            return Err(AiError::new(
                AiErrorClass::ContextTooLarge,
                "Too many pinned and selected context items were included.",
                false,
            ));
        }
        Ok(ContextJob {
            attempt_id,
            user_message_id,
            chat_id,
            request,
            refs,
            known_documents,
            replace_pinned_documents,
            started_at,
            cancellation,
        })
    }

    fn fail_context(
        &mut self,
        attempt_id: InferenceAttemptId,
        started_at: Timestamp,
        error: AiError,
    ) {
        let completed_at = Timestamp::now();
        let message_count = self
            .repository
            .attempt(attempt_id)
            .ok()
            .flatten()
            .map_or(0, |attempt| attempt.message_count);
        let _ = self.repository.commit_context(&ContextCommit {
            attempt_id,
            started_at,
            completed_at,
            measurements: RequestMeasurements {
                message_count,
                ..RequestMeasurements::default()
            },
            pinned_documents: Vec::new(),
            document_refs: Vec::new(),
            message_block_refs: Vec::new(),
            replace_pinned_documents: false,
        });
        self.handle_provider_event(AiEvent::Failed {
            attempt_id,
            error,
            at: completed_at,
        });
    }

    fn handle_context_resolution(&mut self, resolution: ContextResolution) {
        let attempt_id = resolution.commit.attempt_id;
        self.work_returned(attempt_id);
        let outcome = if resolution.result.is_ok() {
            "ready"
        } else {
            "failed"
        };
        let updated = match self.repository.commit_context(&resolution.commit) {
            Ok(updated) => updated,
            Err(error) => {
                self.handle_provider_event(AiEvent::Failed {
                    attempt_id,
                    error,
                    at: Timestamp::now(),
                });
                return;
            }
        };
        tracing::debug!(
            target: "koru_ai::context",
            attempt = %attempt_id,
            outcome,
            message_count = resolution.commit.measurements.message_count,
            document_count = resolution.commit.measurements.context_document_count,
            message_block_count = resolution
                .commit
                .measurements
                .context_message_block_count,
            context_bytes = resolution.commit.measurements.context_payload_bytes,
            elapsed_ms = timestamp_delta_ms(
                resolution.commit.started_at,
                resolution.commit.completed_at,
            ),
            "AI context resolution finished"
        );
        if updated.status.is_terminal() {
            return;
        }
        match resolution.result {
            Ok(request) => {
                let Some(cancellation) = self.cancellations.get(&attempt_id).cloned() else {
                    return;
                };
                let tool_loop = match AiToolLoop::new(request, AiToolLoopLimits::default()) {
                    Ok(tool_loop) => tool_loop,
                    Err(error) => {
                        self.handle_provider_event(AiEvent::Failed {
                            attempt_id,
                            error,
                            at: Timestamp::now(),
                        });
                        return;
                    }
                };
                let request = tool_loop.provider_request();
                self.tool_loops.insert(attempt_id, tool_loop);
                match self.provider_pool.try_submit(ProviderJob {
                    request,
                    cancellation,
                    queued_at: Instant::now(),
                }) {
                    Ok(()) => self.work_admitted(attempt_id),
                    Err(error) => self.handle_provider_event(AiEvent::Failed {
                        attempt_id,
                        error,
                        at: Timestamp::now(),
                    }),
                }
            }
            Err(error) => self.handle_provider_event(AiEvent::Failed {
                attempt_id,
                error,
                at: Timestamp::now(),
            }),
        }
    }

    fn read_attempt(
        &self,
        context: RequestContext,
        attempt: InferenceAttemptId,
    ) -> ApiResult<AttemptResponse> {
        authorize(&context, AiScope::Chat)?;
        Ok(AttemptResponse {
            attempt: self
                .repository
                .attempt(attempt)
                .map_err(ai_failure)?
                .ok_or_else(not_found)?,
        })
    }

    fn cancel_attempt(
        &mut self,
        context: RequestContext,
        attempt_id: InferenceAttemptId,
    ) -> ApiResult<CancelAttemptResponse> {
        authorize(&context, AiScope::Chat)?;
        let attempt = self
            .repository
            .attempt(attempt_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if attempt.status.is_terminal() {
            return Ok(CancelAttemptResponse {
                attempt_id,
                status: attempt.status,
            });
        }
        if let Some(cancellation) = self.cancellations.get(&attempt_id) {
            cancellation.cancel();
        }
        let event = AiEvent::Cancelled {
            attempt_id,
            at: Timestamp::now(),
        };
        let updated = self
            .repository
            .apply_event(&event, &[])
            .map_err(ai_failure)?;
        self.cancel_pending_tools(attempt_id);
        self.finish_agent_rows(attempt_id);
        self.publish(event);
        self.cancellations.remove(&attempt_id);
        self.tool_loops.remove(&attempt_id);
        self.work_terminal(attempt_id);
        log_terminal_attempt(&updated);
        tracing::info!(attempt = %attempt_id, status = "cancelled", "AI attempt finished");
        Ok(CancelAttemptResponse {
            attempt_id,
            status: updated.status,
        })
    }

    fn handle_provider_event(&mut self, mut event: AiEvent) {
        let attempt_id = event.attempt_id();
        // Once the worker is stopping, every pool refuses new work — their
        // threads watch the same flag — while this loop may still be finishing
        // the envelope it took before the flag was set. A failure here is the
        // stop, not the provider: leave the attempt to the shutdown pass, which
        // records it Cancelled, rather than calling it a provider failure.
        if matches!(event, AiEvent::Failed { .. })
            && self.shutdown.load(Ordering::Acquire)
            && self.cancellations.contains_key(&attempt_id)
        {
            return;
        }
        let Ok(Some(before)) = self.repository.attempt(attempt_id) else {
            return;
        };
        if before.status.is_terminal() {
            return;
        }
        if let AiEvent::AgentActivity { activity, at, .. } = event {
            self.apply_agent_activity(&before, activity, at);
            return;
        }
        if let AiEvent::Notice { .. } = &event {
            self.publish(event);
            return;
        }
        if let AiEvent::Usage { usage, .. } = &mut event {
            *usage = accumulated_usage(&before.usage, usage);
        }
        if let AiEvent::ToolCalls { calls, .. } = &event {
            let calls = calls.clone();
            if let Err(error) = self
                .tool_loops
                .get(&attempt_id)
                .ok_or_else(tool_loop_missing)
                .and_then(|tool_loop| tool_loop.accept_calls(&calls))
            {
                self.handle_provider_event(AiEvent::Failed {
                    attempt_id,
                    error,
                    at: Timestamp::now(),
                });
                return;
            }
            match self.repository.apply_event(&event, &[]) {
                Ok(_) => {}
                Err(_) => {
                    self.fail_event_persistence(attempt_id);
                    return;
                }
            }
            if let Err(error) = self.begin_tool_batch(attempt_id, calls) {
                self.handle_provider_event(AiEvent::Failed {
                    attempt_id,
                    error,
                    at: Timestamp::now(),
                });
            }
            return;
        }
        let markdown_started = Instant::now();
        let finalized = if matches!(event, AiEvent::Completed { .. }) {
            before
                .chat_id
                .zip(before.assistant_message_id)
                .and_then(|(chat_id, message_id)| {
                    self.repository.messages(chat_id).ok().and_then(|messages| {
                        messages
                            .into_iter()
                            .find(|message| message.id == message_id)
                    })
                })
                .map(|message| finalize_markdown(message.id, &message.markdown))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if matches!(event, AiEvent::Completed { .. }) {
            tracing::debug!(
                target: "koru_ai::markdown",
                attempt = %attempt_id,
                block_count = finalized.len(),
                elapsed_ms = markdown_started.elapsed().as_secs_f64() * 1_000.0,
                "AI response Markdown finalized"
            );
        }
        match self.repository.apply_event(&event, &finalized) {
            Ok(updated) => {
                if updated.status.is_terminal() {
                    self.cancel_pending_tools(attempt_id);
                    self.finish_agent_rows(attempt_id);
                }
                self.publish(event);
                if updated.status.is_terminal() {
                    self.cancellations.remove(&attempt_id);
                    self.tool_loops.remove(&attempt_id);
                    self.work_terminal(attempt_id);
                    log_terminal_attempt(&updated);
                    tracing::info!(
                        attempt = %attempt_id,
                        status = attempt_status_name(updated.status),
                        "AI attempt finished"
                    );
                }
            }
            Err(_) => {
                self.fail_event_persistence(attempt_id);
            }
        }
    }

    fn handle_tool_resolution(&mut self, resolution: ToolResolution) {
        let attempt_id = resolution.attempt_id;
        self.work_returned(attempt_id);
        let Ok(Some(attempt)) = self.repository.attempt(attempt_id) else {
            return;
        };
        if attempt.status.is_terminal() {
            return;
        }
        let result = match resolution.result {
            Ok(result) => result,
            Err(error) => {
                if error.class == AiErrorClass::Cancelled {
                    return;
                }
                AiToolResult {
                    call_id: resolution.call.call_id.clone(),
                    name: resolution.call.name.clone(),
                    content: json!({
                        "error": {
                            "class": error_code(error.class),
                            "message": error.public_message,
                            "retryable": error.retryable,
                        }
                    }),
                    is_error: true,
                }
            }
        };
        if let Err(error) = self.finish_tool_call(attempt_id, resolution.invocation_id, result) {
            self.handle_provider_event(AiEvent::Failed {
                attempt_id,
                error,
                at: Timestamp::now(),
            });
        }
    }

    fn begin_tool_batch(
        &mut self,
        attempt_id: InferenceAttemptId,
        calls: Vec<AiToolCall>,
    ) -> Result<(), AiError> {
        if self.tool_batches.contains_key(&attempt_id) {
            return Err(tool_protocol_failure(
                "The AI provider requested another tool batch before the prior batch completed.",
            ));
        }
        let attempt = self
            .repository
            .attempt(attempt_id)?
            .ok_or_else(tool_loop_missing)?;
        let chat_id = attempt.chat_id.ok_or_else(tool_loop_missing)?;
        let assistant_message_id = attempt.assistant_message_id.ok_or_else(tool_loop_missing)?;
        let policy = self
            .repository
            .chat_tool_policy(chat_id)?
            .ok_or_else(tool_loop_missing)?;
        let cancellation = self
            .cancellations
            .get(&attempt_id)
            .cloned()
            .ok_or_else(tool_loop_missing)?;
        let mut pending = PendingToolBatch {
            calls: calls.clone(),
            slots: Vec::with_capacity(calls.len()),
        };
        let mut automatic = Vec::new();
        for call in calls {
            let definition = self
                .tool_definitions
                .iter()
                .find(|definition| definition.name == call.name)
                .ok_or_else(|| {
                    tool_protocol_failure("The AI provider requested an unavailable tool.")
                })?;
            let approval_required = policy.mode == ToolMode::Manual && !definition.safety.read_only;
            let now = Timestamp::now();
            let (target_workspace_id, target_document_id) = tool_activity_targets(&call);
            let mut activity = ToolActivity {
                id: ToolInvocationId::new(),
                attempt_id,
                chat_id,
                assistant_message_id,
                tool_name: call.name.clone(),
                read_only: definition.safety.read_only,
                status: ToolActivityStatus::Proposed,
                approval: if approval_required {
                    ToolApproval::Pending
                } else {
                    ToolApproval::NotRequired
                },
                proposed_at: now,
                started_at: None,
                completed_at: None,
                result_bytes: None,
                error_code: None,
                target_workspace_id,
                target_document_id,
                error_message: None,
                agent_label: None,
                agent_summary: None,
                agent_input: None,
                agent_output: None,
                display_target: None,
                live: None,
            };
            self.repository.create_tool_activity(&activity)?;
            self.publish_tool_activity(activity.clone());
            activity.status = if approval_required {
                ToolActivityStatus::ApprovalRequired
            } else {
                ToolActivityStatus::Running
            };
            if !approval_required {
                activity.started_at = Some(Timestamp::now());
            }
            self.repository.update_tool_activity(&activity)?;
            self.publish_tool_activity(activity.clone());
            self.tool_attempts.insert(activity.id, attempt_id);
            if !approval_required {
                automatic.push((activity.id, call.clone()));
            }
            pending.slots.push(PendingToolCall {
                activity,
                result: None,
            });
        }
        self.tool_batches.insert(attempt_id, pending);
        for (invocation_id, call) in automatic {
            match self.tool_pool.try_submit(ToolJob {
                context: AiToolExecutionContext {
                    chat_id,
                    attempt_id,
                    invocation_id,
                    workspace_ids: policy.workspace_ids.clone(),
                },
                call: call.clone(),
                cancellation: cancellation.clone(),
            }) {
                Ok(()) => self.work_admitted(attempt_id),
                Err(error) => {
                    self.finish_tool_call(
                        attempt_id,
                        invocation_id,
                        tool_error_result(&call, error),
                    )?;
                }
            }
        }
        Ok(())
    }

    fn finish_tool_call(
        &mut self,
        attempt_id: InferenceAttemptId,
        invocation_id: ToolInvocationId,
        result: AiToolResult,
    ) -> Result<(), AiError> {
        let activity = {
            let batch = self
                .tool_batches
                .get_mut(&attempt_id)
                .ok_or_else(tool_loop_missing)?;
            let slot = batch
                .slots
                .iter_mut()
                .find(|slot| slot.activity.id == invocation_id)
                .ok_or_else(tool_loop_missing)?;
            if slot.result.is_some() {
                return Ok(());
            }
            if slot.activity.tool_name != result.name {
                return Err(tool_protocol_failure(
                    "The workspace tool result did not match its durable invocation.",
                ));
            }
            slot.activity.status = if result.is_error {
                ToolActivityStatus::Failed
            } else {
                ToolActivityStatus::Complete
            };
            slot.activity.completed_at = Some(Timestamp::now());
            slot.activity.result_bytes = Some(tool_result_content_bytes(&result));
            slot.activity.error_code = result.is_error.then_some(ToolErrorCode::ExecutionFailed);
            slot.activity.error_message =
                result.is_error.then(|| tool_result_error_message(&result));
            slot.result = Some(result);
            slot.activity.clone()
        };
        self.repository.update_tool_activity(&activity)?;
        self.publish_tool_activity(activity);
        self.continue_after_tools(attempt_id)
    }

    fn continue_after_tools(&mut self, attempt_id: InferenceAttemptId) -> Result<(), AiError> {
        let Some(batch) = self.tool_batches.get(&attempt_id) else {
            return Ok(());
        };
        if batch.slots.iter().any(|slot| slot.result.is_none()) {
            return Ok(());
        }
        let batch = self
            .tool_batches
            .remove(&attempt_id)
            .ok_or_else(tool_loop_missing)?;
        let results = batch
            .slots
            .into_iter()
            .map(|slot| {
                self.tool_attempts.remove(&slot.activity.id);
                slot.result.ok_or_else(tool_loop_missing)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let tool_loop = self
            .tool_loops
            .get_mut(&attempt_id)
            .ok_or_else(tool_loop_missing)?;
        tool_loop.append_results(batch.calls, results)?;
        let request = tool_loop.provider_request();
        let cancellation = self
            .cancellations
            .get(&attempt_id)
            .cloned()
            .ok_or_else(tool_loop_missing)?;
        self.provider_pool.try_submit(ProviderJob {
            request,
            cancellation,
            queued_at: Instant::now(),
        })
    }

    fn publish_tool_activity(&mut self, activity: ToolActivity) {
        self.publish(AiEvent::ToolActivity {
            attempt_id: activity.attempt_id,
            activity,
        });
    }

    fn cancel_pending_tools(&mut self, attempt_id: InferenceAttemptId) {
        let Some(mut batch) = self.tool_batches.remove(&attempt_id) else {
            return;
        };
        for slot in &mut batch.slots {
            self.tool_attempts.remove(&slot.activity.id);
            if slot.activity.status.is_terminal() {
                continue;
            }
            slot.activity.status = ToolActivityStatus::Cancelled;
            if slot.activity.approval == ToolApproval::Pending {
                slot.activity.approval = ToolApproval::Denied;
            }
            slot.activity.completed_at = Some(Timestamp::now());
            slot.activity.error_code = Some(ToolErrorCode::Cancelled);
            slot.activity.error_message = Some("The workspace tool was cancelled.".into());
            if self.repository.update_tool_activity(&slot.activity).is_ok() {
                self.publish_tool_activity(slot.activity.clone());
            }
        }
    }

    fn approve_tool_call(
        &mut self,
        context: RequestContext,
        invocation_id: ToolInvocationId,
        choice: Option<String>,
    ) -> ApiResult<ToolDecisionResponse> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(None)?;
        let mut activity = self
            .repository
            .tool_activity(invocation_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if AgentToolKind::is_agent_token(&activity.tool_name) {
            if activity.status != ToolActivityStatus::ApprovalRequired
                || activity.approval != ToolApproval::Pending
            {
                return Err(tool_decision_conflict());
            }
            return self.answer_agent_approval(&activity, choice, true);
        }
        if activity.approval == ToolApproval::Approved {
            return Ok(ToolDecisionResponse { activity });
        }
        if activity.status != ToolActivityStatus::ApprovalRequired
            || activity.approval != ToolApproval::Pending
        {
            return Err(tool_decision_conflict());
        }
        let attempt_id = self
            .tool_attempts
            .get(&invocation_id)
            .copied()
            .ok_or_else(tool_not_resumable)?;
        let call = self
            .tool_batches
            .get(&attempt_id)
            .and_then(|batch| {
                batch
                    .slots
                    .iter()
                    .position(|slot| slot.activity.id == invocation_id)
                    .map(|index| batch.calls[index].clone())
            })
            .ok_or_else(tool_not_resumable)?;
        activity.status = ToolActivityStatus::Running;
        activity.approval = ToolApproval::Approved;
        activity.started_at = Some(Timestamp::now());
        self.repository
            .update_tool_activity(&activity)
            .map_err(ai_failure)?;
        if let Some(slot) = self.tool_batches.get_mut(&attempt_id).and_then(|batch| {
            batch
                .slots
                .iter_mut()
                .find(|slot| slot.activity.id == invocation_id)
        }) {
            slot.activity = activity.clone();
        }
        self.publish_tool_activity(activity.clone());
        let cancellation = self
            .cancellations
            .get(&attempt_id)
            .cloned()
            .ok_or_else(tool_not_resumable)?;
        match self.tool_pool.try_submit(ToolJob {
            context: AiToolExecutionContext {
                chat_id: activity.chat_id,
                attempt_id,
                invocation_id,
                workspace_ids: self
                    .repository
                    .chat_tool_policy(activity.chat_id)
                    .map_err(ai_failure)?
                    .ok_or_else(tool_not_resumable)?
                    .workspace_ids,
            },
            call: call.clone(),
            cancellation,
        }) {
            Ok(()) => self.work_admitted(attempt_id),
            Err(error) => {
                self.finish_tool_call(attempt_id, invocation_id, tool_error_result(&call, error))
                    .map_err(ai_failure)?;
            }
        }
        let activity = self
            .repository
            .tool_activity(invocation_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        Ok(ToolDecisionResponse { activity })
    }

    fn deny_tool_call(
        &mut self,
        context: RequestContext,
        invocation_id: ToolInvocationId,
        choice: Option<String>,
    ) -> ApiResult<ToolDecisionResponse> {
        authorize(&context, AiScope::Chat)?;
        self.admit_chat_work(None)?;
        let mut activity = self
            .repository
            .tool_activity(invocation_id)
            .map_err(ai_failure)?
            .ok_or_else(not_found)?;
        if AgentToolKind::is_agent_token(&activity.tool_name) {
            if activity.status != ToolActivityStatus::ApprovalRequired
                || activity.approval != ToolApproval::Pending
            {
                return Err(tool_decision_conflict());
            }
            return self.answer_agent_approval(&activity, choice, false);
        }
        if activity.status == ToolActivityStatus::Denied
            && activity.approval == ToolApproval::Denied
        {
            return Ok(ToolDecisionResponse { activity });
        }
        if activity.status != ToolActivityStatus::ApprovalRequired
            || activity.approval != ToolApproval::Pending
        {
            return Err(tool_decision_conflict());
        }
        let attempt_id = self
            .tool_attempts
            .get(&invocation_id)
            .copied()
            .ok_or_else(tool_not_resumable)?;
        {
            let batch = self
                .tool_batches
                .get_mut(&attempt_id)
                .ok_or_else(tool_not_resumable)?;
            let index = batch
                .slots
                .iter()
                .position(|slot| slot.activity.id == invocation_id)
                .ok_or_else(tool_not_resumable)?;
            let call = &batch.calls[index];
            let result = AiToolResult {
                call_id: call.call_id.clone(),
                name: call.name.clone(),
                content: json!({
                    "error": {
                        "code": "permission_denied",
                        "message": "The user denied this workspace change."
                    }
                }),
                is_error: true,
            };
            activity.status = ToolActivityStatus::Denied;
            activity.approval = ToolApproval::Denied;
            activity.completed_at = Some(Timestamp::now());
            activity.result_bytes = Some(tool_result_content_bytes(&result));
            activity.error_code = Some(ToolErrorCode::PermissionDenied);
            activity.error_message = Some("The user denied this workspace change.".into());
            batch.slots[index].activity = activity.clone();
            batch.slots[index].result = Some(result);
        }
        self.repository
            .update_tool_activity(&activity)
            .map_err(ai_failure)?;
        self.publish_tool_activity(activity.clone());
        self.continue_after_tools(attempt_id).map_err(ai_failure)?;
        Ok(ToolDecisionResponse { activity })
    }

    fn fail_event_persistence(&mut self, attempt_id: InferenceAttemptId) {
        tracing::error!(attempt = %attempt_id, "AI event persistence failed");
        if let Some(cancellation) = self.cancellations.remove(&attempt_id) {
            cancellation.cancel();
        }
        self.tool_loops.remove(&attempt_id);
        if let Some(batch) = self.tool_batches.remove(&attempt_id) {
            for slot in batch.slots {
                self.tool_attempts.remove(&slot.activity.id);
            }
        }
        self.streams.remove(&attempt_id);
        self.finish_agent_rows(attempt_id);
        self.work_terminal(attempt_id);
    }

    fn publish(&mut self, event: AiEvent) {
        let attempt_id = event.attempt_id();
        let terminal = event.is_terminal();
        let Ok(event) = PublicAiEvent::try_from(event) else {
            debug_assert!(false, "provider tool calls must remain worker-internal");
            return;
        };
        let mut remove = terminal;
        if let Some(stream) = self.streams.get_mut(&attempt_id) {
            let public = StreamEvent {
                version: AI_STREAM_VERSION,
                sequence: stream.next_sequence,
                event,
            };
            stream.next_sequence = stream.next_sequence.saturating_add(1);
            match stream.sender.try_send(public) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                    remove = true;
                    tracing::debug!(
                        attempt = %attempt_id,
                        "AI response stream dropped; authoritative state remains available"
                    );
                }
            }
        }
        if remove {
            self.streams.remove(&attempt_id);
        }
    }

    fn cancel_active_for_shutdown(&mut self) {
        if let Some((_, _, cancellation)) = self.ollama_pull.take() {
            cancellation.cancel();
        }
        let active: Vec<_> = self.cancellations.keys().copied().collect();
        for attempt_id in active {
            if let Some(cancellation) = self.cancellations.get(&attempt_id) {
                cancellation.cancel();
            }
            self.handle_provider_event(AiEvent::Cancelled {
                attempt_id,
                at: Timestamp::now(),
            });
        }
        self.cancellations.clear();
        self.streams.clear();
        self.active_work.clear();
    }
}

fn runtime_settings(runtime: &ProviderRuntime, mut settings: AiSettings) -> AiSettings {
    settings.openai.key_account = runtime
        .openai_credential_configured()
        .then(|| runtime.openai_account().to_owned());
    settings
}

fn settings_view(
    settings: &AiSettings,
    agents: &[koru_ai_wire::AgentProviderView],
) -> ProviderSettingsResponse {
    ProviderSettingsResponse {
        agents: agents.to_vec(),
        selected_provider: settings.selected_provider.clone(),
        openai: openai_settings_view(settings),
        ollama: ollama_settings_view(settings),
    }
}

fn openai_settings_view(settings: &AiSettings) -> OpenAiSettingsView {
    OpenAiSettingsView {
        model: settings.openai.model.clone(),
        reasoning: settings.openai.reasoning,
        credential_configured: settings.openai.key_account.is_some(),
    }
}

fn ollama_settings_view(settings: &AiSettings) -> OllamaSettingsView {
    OllamaSettingsView {
        endpoint: settings.ollama.endpoint.to_string(),
        model: settings.ollama.model.clone(),
        reasoning: settings.ollama.reasoning,
    }
}

fn invalid_provider_text(value: &str) -> bool {
    value.trim().is_empty() || value.len() > 500 || value.chars().any(char::is_control)
}

fn valid_ollama_endpoint(endpoint: &url::Url) -> bool {
    matches!(endpoint.scheme(), "http" | "https")
        && endpoint.host_str().is_some()
        && endpoint.username().is_empty()
        && endpoint.password().is_none()
        && endpoint.query().is_none()
        && endpoint.fragment().is_none()
}

enum ProviderAdminJob {
    Validate {
        provider: AiProviderId,
        reply: SyncSender<ApiResult<ProviderValidationResponse>>,
    },
    Pull {
        pull_id: Uuid,
        model: String,
        cancellation: Cancellation,
        events: SyncSender<ModelPullEvent>,
    },
}

struct AdminSubmitError {
    job: ProviderAdminJob,
    failure: Failure,
}

struct ProviderAdminPool {
    sender: Option<SyncSender<ProviderAdminJob>>,
    thread: Option<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl ProviderAdminPool {
    fn spawn(
        runtime: ProviderRuntime,
        agents: Option<AgentSessions>,
        output: SyncSender<Envelope>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let (sender, receiver) = sync_channel(PROVIDER_ADMIN_QUEUE_CAPACITY);
        let thread_shutdown = Arc::clone(&shutdown);
        let thread = std::thread::Builder::new()
            .name("koru-ai-provider-admin".into())
            .spawn(move || {
                while !thread_shutdown.load(Ordering::Acquire) {
                    let job = match receiver.recv_timeout(LOOP_INTERVAL) {
                        Ok(job) => job,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    match job {
                        ProviderAdminJob::Validate { provider, reply } => {
                            let started = Instant::now();
                            let diagnostic_provider = provider.clone();
                            let response = catch_unwind(AssertUnwindSafe(|| {
                                if is_agent_provider(&provider) {
                                    agent_validation(agents.as_ref(), provider)
                                } else {
                                    validate_provider_configuration(&runtime, provider)
                                }
                            }))
                            .unwrap_or_else(|_| {
                                Err(failure(
                                    "ai_provider",
                                    "The AI provider stopped unexpectedly.",
                                    true,
                                ))
                            });
                            let (status, model_count) = response
                                .as_ref()
                                .map(|response| {
                                    (
                                        provider_validation_status_name(response.status),
                                        response.models.len(),
                                    )
                                })
                                .unwrap_or(("failed", 0));
                            tracing::info!(
                                provider = %diagnostic_provider,
                                status,
                                "AI provider validation finished"
                            );
                            tracing::debug!(
                                target: "koru_ai::provider",
                                provider = %diagnostic_provider,
                                status,
                                model_count,
                                elapsed_ms = started.elapsed().as_secs_f64() * 1_000.0,
                                "AI provider validation timing"
                            );
                            let _ = reply.send(response);
                        }
                        ProviderAdminJob::Pull {
                            pull_id,
                            model,
                            cancellation,
                            events,
                        } => {
                            let outcome = catch_unwind(AssertUnwindSafe(|| {
                                run_model_pull(
                                    &runtime,
                                    &model,
                                    cancellation,
                                    &events,
                                    &thread_shutdown,
                                )
                            }));
                            if outcome.is_err() {
                                let _ = events.try_send(ModelPullEvent::Failed {
                                    failure: failure(
                                        "ai_provider",
                                        "The Ollama model pull stopped unexpectedly.",
                                        true,
                                    ),
                                });
                            }
                            send_admin_envelope(
                                &output,
                                Envelope::PullFinished(pull_id),
                                &thread_shutdown,
                            );
                        }
                    }
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
            shutdown,
        })
    }

    fn try_submit(&self, job: ProviderAdminJob) -> Result<(), AdminSubmitError> {
        let Some(sender) = &self.sender else {
            return Err(AdminSubmitError {
                job,
                failure: unavailable_failure(),
            });
        };
        sender.try_send(job).map_err(|error| match error {
            TrySendError::Full(job) => AdminSubmitError {
                job,
                failure: failure(
                    "ai_worker_saturated",
                    "The provider-management queue is busy. Try again.",
                    true,
                ),
            },
            TrySendError::Disconnected(job) => AdminSubmitError {
                job,
                failure: unavailable_failure(),
            },
        })
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn validate_provider_configuration(
    runtime: &ProviderRuntime,
    provider: AiProviderId,
) -> ApiResult<ProviderValidationResponse> {
    let Some(settings) = runtime.settings() else {
        return Ok(unconfigured_validation(provider));
    };
    match provider.as_str() {
        AiProviderId::OPENAI => {
            if settings.openai.key_account.is_none() {
                return Ok(unconfigured_validation(provider));
            }
            match runtime.openai_models() {
                Ok(models) => {
                    let Some(model) = settings.openai.model.as_deref() else {
                        return Ok(ProviderValidationResponse {
                            provider,
                            status: ProviderValidationStatus::Unconfigured,
                            models,
                            selected_model: None,
                            failure: None,
                        });
                    };
                    if !models.iter().any(|available| available == model) {
                        return Ok(validation_failure(
                            provider,
                            models,
                            AiError::new(
                                AiErrorClass::ModelUnavailable,
                                "The selected OpenAI model is not available to this project.",
                                false,
                            ),
                        ));
                    }
                    let descriptor = openai_model(model);
                    match validate_reasoning(
                        &descriptor.capabilities.reasoning,
                        settings.openai.reasoning,
                    ) {
                        Ok(_) => Ok(ProviderValidationResponse {
                            provider,
                            status: ProviderValidationStatus::Ready,
                            models,
                            selected_model: Some(descriptor),
                            failure: None,
                        }),
                        Err(error) => Ok(validation_failure(provider, models, error)),
                    }
                }
                Err(error) => Ok(validation_failure(provider, Vec::new(), error)),
            }
        }
        AiProviderId::OLLAMA => match runtime.ollama_models() {
            Ok(models) if models.is_empty() => Ok(ProviderValidationResponse {
                provider,
                status: ProviderValidationStatus::NoModels,
                models,
                selected_model: None,
                failure: None,
            }),
            Ok(models) => {
                let Some(model) = settings.ollama.model.as_deref() else {
                    return Ok(ProviderValidationResponse {
                        provider,
                        status: ProviderValidationStatus::Unconfigured,
                        models,
                        selected_model: None,
                        failure: None,
                    });
                };
                if !models.iter().any(|available| available == model) {
                    return Ok(validation_failure(
                        provider,
                        models,
                        AiError::new(
                            AiErrorClass::ModelUnavailable,
                            "The selected Ollama model is not installed.",
                            false,
                        ),
                    ));
                }
                match runtime.validate_ollama(model, settings.ollama.reasoning) {
                    Ok(descriptor) => Ok(ProviderValidationResponse {
                        provider,
                        status: ProviderValidationStatus::Ready,
                        models,
                        selected_model: Some(descriptor),
                        failure: None,
                    }),
                    Err(error) => Ok(validation_failure(provider, models, error)),
                }
            }
            Err(error) => Ok(validation_failure(provider, Vec::new(), error)),
        },
        AiProviderId::KORU => match runtime.validate_koru(ReasoningChoice::Default) {
            Ok(descriptor) => Ok(ProviderValidationResponse {
                provider,
                status: ProviderValidationStatus::Ready,
                models: vec![KORU_MODEL.to_owned()],
                selected_model: Some(descriptor),
                failure: None,
            }),
            Err(error) if error.class == AiErrorClass::Configuration => {
                Ok(unconfigured_validation(provider))
            }
            Err(error) => Ok(validation_failure(provider, Vec::new(), error)),
        },
        _ => Err(failure(
            "invalid_request",
            "The selected AI provider is not supported.",
            false,
        )),
    }
}

fn unconfigured_validation(provider: AiProviderId) -> ProviderValidationResponse {
    ProviderValidationResponse {
        provider,
        status: ProviderValidationStatus::Unconfigured,
        models: Vec::new(),
        selected_model: None,
        failure: None,
    }
}

fn validation_failure(
    provider: AiProviderId,
    models: Vec<String>,
    error: AiError,
) -> ProviderValidationResponse {
    let status = match error.class {
        AiErrorClass::Authentication | AiErrorClass::ModelUnavailable | AiErrorClass::Network => {
            ProviderValidationStatus::Unavailable
        }
        AiErrorClass::Configuration => ProviderValidationStatus::Unconfigured,
        _ => ProviderValidationStatus::Failed,
    };
    ProviderValidationResponse {
        provider,
        status,
        models,
        selected_model: None,
        failure: Some(ai_failure(error)),
    }
}

fn run_model_pull(
    runtime: &ProviderRuntime,
    model: &str,
    cancellation: Cancellation,
    events: &SyncSender<ModelPullEvent>,
    shutdown: &AtomicBool,
) {
    let mut diagnostic = ModelPullTrace::new(model);
    let mut stream = match runtime.pull_ollama(model, cancellation.clone()) {
        Ok(stream) => stream,
        Err(error) => {
            diagnostic.outcome = if error.class == AiErrorClass::Cancelled {
                "cancelled"
            } else {
                "failed"
            };
            let event = if error.class == AiErrorClass::Cancelled {
                ModelPullEvent::Cancelled {
                    model: model.to_owned(),
                }
            } else {
                ModelPullEvent::Failed {
                    failure: ai_failure(error),
                }
            };
            let _ = events.try_send(event);
            return;
        }
    };
    let mut terminal = false;
    for item in &mut stream {
        if shutdown.load(Ordering::Acquire) {
            cancellation.cancel();
        }
        let event = match item {
            Ok(progress) if progress.status == "success" => ModelPullEvent::Completed {
                model: progress.model,
            },
            Ok(progress) => ModelPullEvent::Progress {
                model: progress.model,
                status: progress.status,
                completed_bytes: progress.completed_bytes,
                total_bytes: progress.total_bytes,
            },
            Err(error) if error.class == AiErrorClass::Cancelled => ModelPullEvent::Cancelled {
                model: model.to_owned(),
            },
            Err(error) => ModelPullEvent::Failed {
                failure: ai_failure(error),
            },
        };
        let terminal_outcome = match &event {
            ModelPullEvent::Completed { .. } => Some("complete"),
            ModelPullEvent::Failed { .. } => Some("failed"),
            ModelPullEvent::Cancelled { .. } => Some("cancelled"),
            ModelPullEvent::Progress { .. } => None,
        };
        if events.try_send(event).is_err() {
            diagnostic.outcome = "subscriber_dropped";
            cancellation.cancel();
            return;
        }
        if let Some(outcome) = terminal_outcome {
            diagnostic.outcome = outcome;
            terminal = true;
            cancellation.cancel();
            break;
        }
    }
    if !terminal {
        let event = if shutdown.load(Ordering::Acquire) || cancellation.is_cancelled() {
            diagnostic.outcome = "cancelled";
            ModelPullEvent::Cancelled {
                model: model.to_owned(),
            }
        } else {
            diagnostic.outcome = "failed";
            ModelPullEvent::Failed {
                failure: ai_failure(AiError::new(
                    AiErrorClass::Network,
                    "Ollama model pull ended before reporting completion.",
                    true,
                )),
            }
        };
        let _ = events.try_send(event);
    }
}

struct ModelPullTrace<'a> {
    model: &'a str,
    started: Instant,
    outcome: &'static str,
}

impl<'a> ModelPullTrace<'a> {
    fn new(model: &'a str) -> Self {
        Self {
            model,
            started: Instant::now(),
            outcome: "abandoned",
        }
    }
}

impl Drop for ModelPullTrace<'_> {
    fn drop(&mut self) {
        tracing::info!(
            provider = AiProviderId::OLLAMA,
            model = self.model,
            outcome = self.outcome,
            elapsed_ms = self.started.elapsed().as_secs_f64() * 1_000.0,
            "AI model pull finished"
        );
    }
}

fn send_admin_envelope(output: &SyncSender<Envelope>, envelope: Envelope, shutdown: &AtomicBool) {
    let mut envelope = envelope;
    loop {
        match output.try_send(envelope) {
            Ok(()) => return,
            Err(TrySendError::Full(returned)) => {
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                envelope = returned;
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

enum PreparedContextRef {
    Document(koru_ai_core::DocumentId),
    MessageBlock(MessageBlockContext),
}

struct ContextJob {
    attempt_id: InferenceAttemptId,
    user_message_id: MessageId,
    chat_id: ChatId,
    request: AiRequest,
    refs: Vec<PreparedContextRef>,
    known_documents: HashMap<koru_ai_core::DocumentId, PinnedDocument>,
    replace_pinned_documents: bool,
    started_at: Timestamp,
    cancellation: Cancellation,
}

struct ContextResolution {
    commit: ContextCommit,
    result: Result<AiRequest, AiError>,
}

struct ContextPool {
    sender: Option<SyncSender<ContextJob>>,
    thread: Option<JoinHandle<()>>,
}

impl ContextPool {
    fn spawn(
        store: Option<Arc<dyn StoreContextPort>>,
        output: SyncSender<Envelope>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let (sender, receiver) = sync_channel::<ContextJob>(CONTEXT_QUEUE_CAPACITY);
        let thread = std::thread::Builder::new()
            .name("koru-ai-context".into())
            .spawn(move || {
                while !shutdown.load(Ordering::Acquire) {
                    let job = match receiver.recv_timeout(LOOP_INTERVAL) {
                        Ok(job) => job,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    let attempt_id = job.attempt_id;
                    let started_at = job.started_at;
                    let message_count = job.request.messages.len().try_into().unwrap_or(u32::MAX);
                    let resolution = catch_unwind(AssertUnwindSafe(|| {
                        resolve_context_job(store.as_deref(), job)
                    }))
                    .unwrap_or_else(|_| ContextResolution {
                        commit: ContextCommit {
                            attempt_id,
                            started_at,
                            completed_at: Timestamp::now(),
                            measurements: RequestMeasurements {
                                message_count,
                                ..RequestMeasurements::default()
                            },
                            pinned_documents: Vec::new(),
                            document_refs: Vec::new(),
                            message_block_refs: Vec::new(),
                            replace_pinned_documents: false,
                        },
                        result: Err(AiError::new(
                            AiErrorClass::Storage,
                            "AI context preparation stopped unexpectedly.",
                            true,
                        )),
                    });
                    send_context_envelope(&output, Envelope::Context(resolution), &shutdown);
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    fn try_submit(&self, job: ContextJob) -> Result<(), AiError> {
        self.sender
            .as_ref()
            .ok_or_else(context_pool_stopped)?
            .try_send(job)
            .map_err(|error| match error {
                TrySendError::Full(_) => AiError::new(
                    AiErrorClass::Storage,
                    "AI context preparation is busy. Try again.",
                    true,
                ),
                TrySendError::Disconnected(_) => context_pool_stopped(),
            })
    }

    fn stop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct ExportJob {
    export: DocumentExport,
    markdown: String,
}

struct ExportResolution {
    export_id: DocumentExportId,
    result: Result<koru_ai_core::DocumentId, AiError>,
    elapsed: Duration,
}

struct ExportPool {
    sender: Option<SyncSender<ExportJob>>,
    thread: Option<JoinHandle<()>>,
}

impl ExportPool {
    fn spawn(
        store: Option<Arc<dyn StoreContextPort>>,
        output: SyncSender<Envelope>,
        shutdown: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let (sender, receiver) = sync_channel::<ExportJob>(EXPORT_QUEUE_CAPACITY);
        let thread = std::thread::Builder::new()
            .name("koru-ai-export".into())
            .spawn(move || {
                while !shutdown.load(Ordering::Acquire) {
                    let job = match receiver.recv_timeout(LOOP_INTERVAL) {
                        Ok(job) => job,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    let export_id = job.export.id;
                    let started = Instant::now();
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        let store = store.as_deref().ok_or_else(|| {
                            AiError::new(
                                AiErrorClass::Storage,
                                "Creating a Koru document is unavailable.",
                                true,
                            )
                        })?;
                        store.create_document(
                            job.export.idempotency_key,
                            &job.export.destination,
                            &job.markdown,
                        )
                    }))
                    .unwrap_or_else(|_| {
                        Err(AiError::new(
                            AiErrorClass::Storage,
                            "Creating the Koru document stopped unexpectedly.",
                            true,
                        ))
                    });
                    send_context_envelope(
                        &output,
                        Envelope::Export(ExportResolution {
                            export_id,
                            result,
                            elapsed: started.elapsed(),
                        }),
                        &shutdown,
                    );
                }
            })
            .map_err(|error| error.to_string())?;
        Ok(Self {
            sender: Some(sender),
            thread: Some(thread),
        })
    }

    fn try_submit(&self, job: ExportJob) -> Result<(), AiError> {
        self.sender
            .as_ref()
            .ok_or_else(export_pool_stopped)?
            .try_send(job)
            .map_err(|error| match error {
                TrySendError::Full(_) => AiError::new(
                    AiErrorClass::Storage,
                    "Document export is busy. Try again.",
                    true,
                ),
                TrySendError::Disconnected(_) => export_pool_stopped(),
            })
    }

    fn stop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn export_pool_stopped() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "Document export is unavailable.",
        true,
    )
}

fn resolve_context_job(
    store: Option<&dyn StoreContextPort>,
    mut job: ContextJob,
) -> ContextResolution {
    let message_count = job.request.messages.len().try_into().unwrap_or(u32::MAX);
    let mut context = Vec::with_capacity(job.refs.len());
    let mut pinned_documents = Vec::new();
    let mut document_refs = Vec::new();
    let mut message_block_refs = Vec::new();
    let mut payload_bytes = 0_u64;
    let result = (|| {
        for reference in job.refs {
            if job.cancellation.is_cancelled() {
                return Err(AiError::new(
                    AiErrorClass::Cancelled,
                    "The AI request was cancelled.",
                    false,
                ));
            }
            match reference {
                PreparedContextRef::Document(document_id) => {
                    let document = store
                        .ok_or_else(context_port_unavailable)?
                        .resolve_document(document_id)?;
                    add_context_bytes(&mut payload_bytes, document.markdown.len())?;
                    let now = Timestamp::now();
                    let first_used_at = job
                        .known_documents
                        .remove(&document_id)
                        .map_or(job.started_at, |known| known.first_used_at);
                    pinned_documents.push(PinnedDocument {
                        chat_id: job.chat_id,
                        document_id,
                        display_title: document.display_title.clone(),
                        pinned: true,
                        first_used_at,
                        last_used_at: now,
                    });
                    document_refs.push(MessageDocumentRef {
                        message_id: job.user_message_id,
                        document_id,
                        relation: DocumentRelation::Context,
                        display_title: document.display_title.clone(),
                        source_revision: Some(document.source_revision.clone()),
                        used_at: now,
                    });
                    context.push(ContextItem::Document(document));
                }
                PreparedContextRef::MessageBlock(block) => {
                    add_context_bytes(&mut payload_bytes, block.markdown.len())?;
                    message_block_refs.push(MessageBlockRef {
                        message_id: job.user_message_id,
                        source_block_id: block.block_id,
                        used_at: Timestamp::now(),
                    });
                    context.push(ContextItem::MessageBlock(block));
                }
            }
        }
        job.request.context = context;
        Ok(job.request)
    })();
    let completed_at = Timestamp::now();
    let successful = result.is_ok();
    let measurements = RequestMeasurements {
        message_count,
        context_document_count: if successful {
            document_refs.len().try_into().unwrap_or(u32::MAX)
        } else {
            0
        },
        context_message_block_count: if successful {
            message_block_refs.len().try_into().unwrap_or(u32::MAX)
        } else {
            0
        },
        context_payload_bytes: payload_bytes,
    };
    if !successful {
        pinned_documents.clear();
        document_refs.clear();
        message_block_refs.clear();
    }
    ContextResolution {
        commit: ContextCommit {
            attempt_id: job.attempt_id,
            started_at: job.started_at,
            completed_at,
            measurements,
            pinned_documents,
            document_refs,
            message_block_refs,
            replace_pinned_documents: job.replace_pinned_documents,
        },
        result,
    }
}

fn add_context_bytes(total: &mut u64, bytes: usize) -> Result<(), AiError> {
    *total = total
        .checked_add(bytes.try_into().unwrap_or(u64::MAX))
        .ok_or_else(context_too_large)?;
    if *total > MAX_CONTEXT_PAYLOAD_BYTES {
        return Err(context_too_large());
    }
    Ok(())
}

fn send_context_envelope(output: &SyncSender<Envelope>, envelope: Envelope, shutdown: &AtomicBool) {
    let mut envelope = envelope;
    loop {
        match output.try_send(envelope) {
            Ok(()) => return,
            Err(TrySendError::Full(returned)) => {
                if shutdown.load(Ordering::Acquire) {
                    return;
                }
                envelope = returned;
                std::thread::yield_now();
            }
            Err(TrySendError::Disconnected(_)) => return,
        }
    }
}

fn context_port_unavailable() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "Document context is not available in this service.",
        true,
    )
}

fn context_pool_stopped() -> AiError {
    AiError::new(
        AiErrorClass::Storage,
        "AI context preparation is unavailable.",
        true,
    )
}

fn context_too_large() -> AiError {
    AiError::new(
        AiErrorClass::ContextTooLarge,
        "The selected context is too large for one AI request.",
        false,
    )
}

fn tool_loop_missing() -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "The AI tool conversation is no longer available.",
        true,
    )
}

fn tool_protocol_failure(message: &'static str) -> AiError {
    AiError::new(AiErrorClass::Provider, message, false)
}

fn tool_not_resumable() -> Failure {
    failure(
        "ai_tool_not_resumable",
        "This tool request is no longer active. Start a new turn to try again.",
        false,
    )
}

fn tool_decision_conflict() -> Failure {
    failure(
        "ai_tool_decision_conflict",
        "This tool request has already moved past the requested decision.",
        false,
    )
}

fn tool_result_content_bytes(result: &AiToolResult) -> u64 {
    serde_json::to_vec(&result.content)
        .map(|content| u64::try_from(content.len()).unwrap_or(u64::MAX))
        .unwrap_or(u64::MAX)
}

fn tool_activity_targets(call: &AiToolCall) -> (Option<WorkspaceId>, Option<DocumentId>) {
    let string_argument = |key| call.arguments.get(key).and_then(serde_json::Value::as_str);
    let workspace = string_argument("workspace_id").and_then(WorkspaceId::parse);
    let document = ["document_id", "source_document_id", "target_document_id"]
        .into_iter()
        .find_map(|key| string_argument(key).and_then(DocumentId::parse));
    (workspace, document)
}

fn tool_result_error_message(result: &AiToolResult) -> String {
    const MAX_CHARS: usize = 512;
    let message = result
        .content
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("The workspace tool could not complete its work.");
    let mut summary = message.chars().take(MAX_CHARS).collect::<String>();
    if message.chars().count() > MAX_CHARS {
        summary.push('…');
    }
    summary
}

fn tool_error_result(call: &AiToolCall, error: AiError) -> AiToolResult {
    AiToolResult {
        call_id: call.call_id.clone(),
        name: call.name.clone(),
        content: json!({
            "error": {
                "class": error_code(error.class),
                "message": error.public_message,
                "retryable": error.retryable,
            }
        }),
        is_error: true,
    }
}

fn accumulated_usage(current: &AiUsage, next: &AiUsage) -> AiUsage {
    fn add(left: Option<u64>, right: Option<u64>) -> Option<u64> {
        match (left, right) {
            (None, None) => None,
            (left, right) => Some(left.unwrap_or(0).saturating_add(right.unwrap_or(0))),
        }
    }

    let usage = AiUsage {
        prompt_tokens: add(current.prompt_tokens, next.prompt_tokens),
        cached_prompt_tokens: add(current.cached_prompt_tokens, next.cached_prompt_tokens),
        completion_tokens: add(current.completion_tokens, next.completion_tokens),
        reasoning_tokens: add(current.reasoning_tokens, next.reasoning_tokens),
        total_tokens: add(current.total_tokens, next.total_tokens),
        source: if current.source == UsageSource::ProviderReported
            || next.source == UsageSource::ProviderReported
        {
            UsageSource::ProviderReported
        } else {
            UsageSource::Unavailable
        },
    };
    usage
}

struct PendingToolBatch {
    calls: Vec<AiToolCall>,
    slots: Vec<PendingToolCall>,
}

struct PendingToolCall {
    activity: ToolActivity,
    result: Option<AiToolResult>,
}

struct ToolJob {
    context: AiToolExecutionContext,
    call: AiToolCall,
    cancellation: Cancellation,
}

struct ToolResolution {
    invocation_id: ToolInvocationId,
    attempt_id: InferenceAttemptId,
    call: AiToolCall,
    result: Result<AiToolResult, AiError>,
}

struct ToolPool {
    sender: Option<SyncSender<ToolJob>>,
    threads: Vec<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl ToolPool {
    fn spawn(
        executor: Option<Arc<dyn AiToolExecutor>>,
        output: SyncSender<Envelope>,
        shutdown: Arc<AtomicBool>,
        thread_count: usize,
    ) -> Result<Self, String> {
        let Some(executor) = executor else {
            return Ok(Self {
                sender: None,
                threads: Vec::new(),
                shutdown,
            });
        };
        let (sender, receiver) = sync_channel::<ToolJob>(TOOL_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut threads = Vec::with_capacity(thread_count);
        for index in 0..thread_count {
            let executor = Arc::clone(&executor);
            let receiver = Arc::clone(&receiver);
            let output = output.clone();
            let thread_shutdown = Arc::clone(&shutdown);
            let thread = std::thread::Builder::new()
                .name(format!("koru-ai-tool-{index}"))
                .spawn(move || {
                    while !thread_shutdown.load(Ordering::Acquire) {
                        let job = match receiver
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .recv_timeout(LOOP_INTERVAL)
                        {
                            Ok(job) => job,
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => break,
                        };
                        let resolution = run_tool_job(executor.as_ref(), job);
                        let mut envelope = Envelope::Tools(resolution);
                        loop {
                            match output.try_send(envelope) {
                                Ok(()) => break,
                                Err(TrySendError::Full(returned)) => {
                                    if thread_shutdown.load(Ordering::Acquire) {
                                        return;
                                    }
                                    envelope = returned;
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                                Err(TrySendError::Disconnected(_)) => return,
                            }
                        }
                    }
                })
                .map_err(|error| error.to_string())?;
            threads.push(thread);
        }
        Ok(Self {
            sender: Some(sender),
            threads,
            shutdown,
        })
    }

    fn try_submit(&self, job: ToolJob) -> Result<(), AiError> {
        self.sender
            .as_ref()
            .ok_or_else(|| {
                AiError::new(
                    AiErrorClass::Configuration,
                    "Workspace tools are not available in this service.",
                    false,
                )
            })?
            .try_send(job)
            .map_err(|error| match error {
                TrySendError::Full(_) => AiError::new(
                    AiErrorClass::Provider,
                    "The AI tool queue is busy. Try again.",
                    true,
                ),
                TrySendError::Disconnected(_) => AiError::new(
                    AiErrorClass::Provider,
                    "Workspace tool execution is unavailable.",
                    true,
                ),
            })
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sender.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn run_tool_job(executor: &dyn AiToolExecutor, job: ToolJob) -> ToolResolution {
    let ToolJob {
        context,
        call,
        cancellation,
    } = job;
    let invocation_id = context.invocation_id;
    let attempt_id = context.attempt_id;
    let result = if cancellation.is_cancelled() {
        Err(AiError::new(
            AiErrorClass::Cancelled,
            "The AI request was cancelled.",
            false,
        ))
    } else {
        executor
            .invoke(&context, &call, &cancellation)
            .or_else(|error| Ok(tool_error_result(&call, error)))
    };
    ToolResolution {
        invocation_id,
        attempt_id,
        call,
        result,
    }
}

struct ProviderJob {
    request: AiRequest,
    cancellation: Cancellation,
    queued_at: Instant,
}

struct ProviderTrace {
    attempt_id: InferenceAttemptId,
    provider: AiProviderId,
    model: String,
    reasoning: ReasoningChoice,
    queue_elapsed: Duration,
    started: Instant,
    raw_delta_count: u64,
    raw_delta_bytes: u64,
    persisted_delta_count: u64,
    outcome: &'static str,
}

impl ProviderTrace {
    fn new(job: &ProviderJob) -> Self {
        Self {
            attempt_id: job.request.attempt_id,
            provider: job.request.provider.clone(),
            model: job.request.model.clone(),
            reasoning: job.request.reasoning,
            queue_elapsed: job.queued_at.elapsed(),
            started: Instant::now(),
            raw_delta_count: 0,
            raw_delta_bytes: 0,
            persisted_delta_count: 0,
            outcome: "abandoned",
        }
    }
}

impl Drop for ProviderTrace {
    fn drop(&mut self) {
        tracing::debug!(
            target: "koru_ai::provider",
            attempt = %self.attempt_id,
            provider = %self.provider,
            model = self.model,
            reasoning = self.reasoning.as_token(),
            outcome = self.outcome,
            queue_ms = self.queue_elapsed.as_secs_f64() * 1_000.0,
            provider_ms = self.started.elapsed().as_secs_f64() * 1_000.0,
            raw_delta_count = self.raw_delta_count,
            raw_delta_bytes = self.raw_delta_bytes,
            persisted_delta_count = self.persisted_delta_count,
            "AI provider stream coalescing summary"
        );
    }
}

struct ProviderPool {
    sender: Option<SyncSender<ProviderJob>>,
    threads: Vec<JoinHandle<()>>,
    shutdown: Arc<AtomicBool>,
}

impl ProviderPool {
    fn spawn(
        providers: Arc<ProviderRegistry>,
        output: SyncSender<Envelope>,
        shutdown: Arc<AtomicBool>,
        thread_count: usize,
    ) -> Result<Self, String> {
        let (sender, receiver) = sync_channel::<ProviderJob>(PROVIDER_QUEUE_CAPACITY);
        let receiver = Arc::new(Mutex::new(receiver));
        let mut threads = Vec::with_capacity(thread_count);
        for index in 0..thread_count {
            let providers = Arc::clone(&providers);
            let receiver = Arc::clone(&receiver);
            let output = output.clone();
            let thread_shutdown = Arc::clone(&shutdown);
            let thread = std::thread::Builder::new()
                .name(format!("koru-ai-provider-{index}"))
                .spawn(move || {
                    while !thread_shutdown.load(Ordering::Acquire) {
                        let job = match receiver
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                            .recv_timeout(LOOP_INTERVAL)
                        {
                            Ok(job) => job,
                            Err(RecvTimeoutError::Timeout) => continue,
                            Err(RecvTimeoutError::Disconnected) => break,
                        };
                        let attempt_id = job.request.attempt_id;
                        let outcome = catch_unwind(AssertUnwindSafe(|| {
                            run_provider_job(&providers, job, &output, &thread_shutdown)
                        }));
                        if outcome.is_err() {
                            let _ = send_provider_event(
                                &output,
                                AiEvent::Failed {
                                    attempt_id,
                                    error: AiError::new(
                                        AiErrorClass::Provider,
                                        "The AI provider stopped unexpectedly.",
                                        true,
                                    ),
                                    at: Timestamp::now(),
                                },
                                &thread_shutdown,
                            );
                        }
                        // Returned, whatever happened above: the work
                        // registry counts the job back only from here.
                        send_context_envelope(
                            &output,
                            Envelope::ProviderReturned(attempt_id),
                            &thread_shutdown,
                        );
                    }
                })
                .map_err(|error| error.to_string())?;
            threads.push(thread);
        }
        Ok(Self {
            sender: Some(sender),
            threads,
            shutdown,
        })
    }

    fn try_submit(&self, job: ProviderJob) -> Result<(), AiError> {
        self.sender
            .as_ref()
            .ok_or_else(provider_stopped)?
            .try_send(job)
            .map_err(|error| match error {
                TrySendError::Full(job) => {
                    tracing::debug!(
                        target: "koru_ai::provider",
                        attempt = %job.request.attempt_id,
                        capacity = PROVIDER_QUEUE_CAPACITY,
                        "AI provider queue saturated"
                    );
                    AiError::new(
                        AiErrorClass::Provider,
                        "The AI provider queue is busy. Try again.",
                        true,
                    )
                }
                TrySendError::Disconnected(job) => {
                    tracing::debug!(
                        target: "koru_ai::provider",
                        attempt = %job.request.attempt_id,
                        "AI provider queue disconnected"
                    );
                    provider_stopped()
                }
            })
    }

    fn stop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        self.sender.take();
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
    }
}

fn run_provider_job(
    providers: &ProviderRegistry,
    job: ProviderJob,
    output: &SyncSender<Envelope>,
    shutdown: &AtomicBool,
) {
    let attempt_id = job.request.attempt_id;
    let mut diagnostic = ProviderTrace::new(&job);
    let payload_debug = koru_env::switch(koru_env::name::KORU_AI_PAYLOAD_DEBUG);
    let mut debug_response = payload_debug.then(String::new);
    if payload_debug {
        // Keep this on one physical log line. `run-all` follows only records
        // carrying this marker; pretty JSON would leave the continuation lines
        // indistinguishable from unrelated gateway output.
        match serde_json::to_string(&job.request) {
            Ok(payload) => tracing::warn!(
                target: "koru_ai::payload",
                attempt = %attempt_id,
                payload = %payload,
                "KORU_AI_PAYLOAD: provider-neutral JSON (debug opt-in; contains authored content)"
            ),
            Err(error) => tracing::warn!(
                target: "koru_ai::payload",
                attempt = %attempt_id,
                %error,
                "AI debug payload could not be serialized"
            ),
        }
    }
    let mut stream = match providers.start(job.request, job.cancellation) {
        Ok(stream) => stream,
        Err(error) => {
            diagnostic.outcome = error_code(error.class);
            let event = AiEvent::Failed {
                attempt_id,
                error,
                at: Timestamp::now(),
            };
            log_debug_response(&event, debug_response.as_deref().unwrap_or_default());
            let _ = send_provider_event(output, event, shutdown);
            return;
        }
    };
    let mut pending_text = String::new();
    let mut pending_at = None;
    let mut pending_reasoning = String::new();
    let mut pending_reasoning_at = None;
    let mut last_flush = Instant::now();
    let mut step_ended = false;
    for event in &mut stream {
        if event.attempt_id() != attempt_id {
            let _ = flush_reasoning(
                output,
                attempt_id,
                &mut pending_reasoning,
                &mut pending_reasoning_at,
                shutdown,
            );
            flush_text(
                output,
                attempt_id,
                &mut pending_text,
                &mut pending_at,
                &mut diagnostic,
                shutdown,
            );
            diagnostic.outcome = "invalid_event";
            let event = AiEvent::Failed {
                attempt_id,
                error: AiError::new(
                    AiErrorClass::Provider,
                    "The AI provider returned an invalid response.",
                    false,
                ),
                at: Timestamp::now(),
            };
            log_debug_response(&event, debug_response.as_deref().unwrap_or_default());
            let _ = send_provider_event(output, event, shutdown);
            return;
        }
        match event {
            AiEvent::TextDelta { text, at, .. } => {
                if !flush_reasoning(
                    output,
                    attempt_id,
                    &mut pending_reasoning,
                    &mut pending_reasoning_at,
                    shutdown,
                ) {
                    diagnostic.outcome = "worker_delivery_stopped";
                    return;
                }
                if let Some(response) = &mut debug_response {
                    response.push_str(&text);
                }
                diagnostic.raw_delta_count = diagnostic.raw_delta_count.saturating_add(1);
                diagnostic.raw_delta_bytes = diagnostic
                    .raw_delta_bytes
                    .saturating_add(text.len().try_into().unwrap_or(u64::MAX));
                if !pending_text.is_empty() && last_flush.elapsed() >= COALESCE_INTERVAL {
                    if !flush_text(
                        output,
                        attempt_id,
                        &mut pending_text,
                        &mut pending_at,
                        &mut diagnostic,
                        shutdown,
                    ) {
                        diagnostic.outcome = "worker_delivery_stopped";
                        return;
                    }
                    last_flush = Instant::now();
                }
                pending_at.get_or_insert(at);
                pending_text.push_str(&text);
                if pending_text.len() >= COALESCE_BYTES {
                    if !flush_text(
                        output,
                        attempt_id,
                        &mut pending_text,
                        &mut pending_at,
                        &mut diagnostic,
                        shutdown,
                    ) {
                        diagnostic.outcome = "worker_delivery_stopped";
                        return;
                    }
                    last_flush = Instant::now();
                }
            }
            AiEvent::ReasoningDelta { text, at, .. } => {
                if !flush_text(
                    output,
                    attempt_id,
                    &mut pending_text,
                    &mut pending_at,
                    &mut diagnostic,
                    shutdown,
                ) {
                    diagnostic.outcome = "worker_delivery_stopped";
                    return;
                }
                if !pending_reasoning.is_empty() && last_flush.elapsed() >= COALESCE_INTERVAL {
                    if !flush_reasoning(
                        output,
                        attempt_id,
                        &mut pending_reasoning,
                        &mut pending_reasoning_at,
                        shutdown,
                    ) {
                        diagnostic.outcome = "worker_delivery_stopped";
                        return;
                    }
                    last_flush = Instant::now();
                }
                pending_reasoning_at.get_or_insert(at);
                pending_reasoning.push_str(&text);
                if pending_reasoning.len() >= COALESCE_BYTES {
                    if !flush_reasoning(
                        output,
                        attempt_id,
                        &mut pending_reasoning,
                        &mut pending_reasoning_at,
                        shutdown,
                    ) {
                        diagnostic.outcome = "worker_delivery_stopped";
                        return;
                    }
                    last_flush = Instant::now();
                }
            }
            event => {
                if !flush_reasoning(
                    output,
                    attempt_id,
                    &mut pending_reasoning,
                    &mut pending_reasoning_at,
                    shutdown,
                ) {
                    diagnostic.outcome = "worker_delivery_stopped";
                    return;
                }
                if !flush_text(
                    output,
                    attempt_id,
                    &mut pending_text,
                    &mut pending_at,
                    &mut diagnostic,
                    shutdown,
                ) {
                    diagnostic.outcome = "worker_delivery_stopped";
                    return;
                }
                step_ended = event.ends_provider_step();
                if step_ended {
                    diagnostic.outcome = ai_event_name(&event);
                    if event.is_terminal() {
                        log_debug_response(&event, debug_response.as_deref().unwrap_or_default());
                    }
                }
                if !send_provider_event(output, event, shutdown) {
                    diagnostic.outcome = "worker_delivery_stopped";
                    return;
                }
                if step_ended {
                    break;
                }
            }
        }
    }
    if !step_ended && !shutdown.load(Ordering::Acquire) {
        let _ = flush_reasoning(
            output,
            attempt_id,
            &mut pending_reasoning,
            &mut pending_reasoning_at,
            shutdown,
        );
        let _ = flush_text(
            output,
            attempt_id,
            &mut pending_text,
            &mut pending_at,
            &mut diagnostic,
            shutdown,
        );
        diagnostic.outcome = "provider_stream_ended";
        let event = AiEvent::Failed {
            attempt_id,
            error: AiError::new(
                AiErrorClass::Provider,
                "The AI provider ended before completing the response.",
                true,
            ),
            at: Timestamp::now(),
        };
        log_debug_response(&event, debug_response.as_deref().unwrap_or_default());
        let _ = send_provider_event(output, event, shutdown);
    }
}

fn log_debug_response(event: &AiEvent, markdown: &str) {
    if !koru_env::switch(koru_env::name::KORU_AI_PAYLOAD_DEBUG) {
        return;
    }
    let response = serde_json::json!({
        "event": event,
        "markdown": markdown,
    });
    tracing::warn!(
        target: "koru_ai::payload",
        attempt = %event.attempt_id(),
        response = %response,
        "KORU_AI_RESPONSE: provider-neutral JSON (debug opt-in; contains provider output)"
    );
}

fn flush_reasoning(
    output: &SyncSender<Envelope>,
    attempt_id: InferenceAttemptId,
    text: &mut String,
    at: &mut Option<Timestamp>,
    shutdown: &AtomicBool,
) -> bool {
    if text.is_empty() {
        return true;
    }
    send_provider_event(
        output,
        AiEvent::ReasoningDelta {
            attempt_id,
            text: std::mem::take(text),
            at: at.take().unwrap_or_else(Timestamp::now),
        },
        shutdown,
    )
}

fn flush_text(
    output: &SyncSender<Envelope>,
    attempt_id: InferenceAttemptId,
    text: &mut String,
    at: &mut Option<Timestamp>,
    diagnostic: &mut ProviderTrace,
    shutdown: &AtomicBool,
) -> bool {
    if text.is_empty() {
        return true;
    }
    let event = AiEvent::TextDelta {
        attempt_id,
        text: std::mem::take(text),
        at: at.take().unwrap_or_else(Timestamp::now),
    };
    let sent = send_provider_event(output, event, shutdown);
    if sent {
        diagnostic.persisted_delta_count = diagnostic.persisted_delta_count.saturating_add(1);
    }
    sent
}

fn send_provider_event(
    output: &SyncSender<Envelope>,
    event: AiEvent,
    shutdown: &AtomicBool,
) -> bool {
    let mut envelope = Envelope::Provider(event);
    loop {
        match output.try_send(envelope) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => {
                if shutdown.load(Ordering::Acquire) {
                    return false;
                }
                envelope = returned;
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
}

fn replay_stream(
    repository: &Repository,
    attempt: InferenceAttempt,
) -> ApiResult<AiResponseStream> {
    let (sender, events) = sync_channel(1);
    drop(sender);
    let assistant_id = attempt.assistant_message_id.ok_or_else(not_found)?;
    let chat_id = attempt.chat_id.ok_or_else(not_found)?;
    let messages = repository.messages(chat_id).map_err(ai_failure)?;
    let assistant = messages
        .iter()
        .find(|message| message.id == assistant_id)
        .ok_or_else(not_found)?;
    let user_message_id = messages
        .iter()
        .find(|message| {
            message.role == ChatRole::User
                && message.ordinal.checked_add(1) == Some(assistant.ordinal)
        })
        .map(|message| message.id)
        .ok_or_else(not_found)?;
    Ok(AiResponseStream {
        accepted: SendTurnAccepted {
            attempt_id: attempt.id,
            user_message_id,
            assistant_message_id: assistant_id,
            replayed: true,
        },
        events,
    })
}

fn authorize(context: &RequestContext, required: AiScope) -> ApiResult<()> {
    if context
        .deadline_millis
        .is_some_and(|deadline| Timestamp::now().millis() > deadline)
    {
        return Err(failure(
            "deadline_exceeded",
            "The AI request deadline expired.",
            true,
        ));
    }
    if !context.principal.scopes.contains(&required) {
        return Err(failure(
            "forbidden",
            "The session does not permit AI chat.",
            false,
        ));
    }
    Ok(())
}

fn receive_api<T>(receiver: Receiver<ApiResult<T>>) -> ApiResult<T> {
    receiver
        .recv_timeout(RESPONSE_TIMEOUT)
        .map_err(|error| match error {
            RecvTimeoutError::Timeout => failure(
                "ai_worker_timeout",
                "The AI service did not answer in time.",
                true,
            ),
            RecvTimeoutError::Disconnected => unavailable_failure(),
        })?
}

fn queue_failure<T>(error: TrySendError<T>) -> Failure {
    match error {
        TrySendError::Full(_) => failure(
            "ai_worker_saturated",
            "The AI service is busy. Try again.",
            true,
        ),
        TrySendError::Disconnected(_) => unavailable_failure(),
    }
}

fn map_try_send<T>(error: TrySendError<T>) -> WorkerError {
    match error {
        TrySendError::Full(_) => WorkerError::Saturated,
        TrySendError::Disconnected(_) => WorkerError::Stopped,
    }
}

fn map_recv(error: RecvTimeoutError) -> WorkerError {
    match error {
        RecvTimeoutError::Timeout => WorkerError::TimedOut,
        RecvTimeoutError::Disconnected => WorkerError::Stopped,
    }
}

fn failure(code: &str, message: &str, retryable: bool) -> Failure {
    Failure {
        code: code.into(),
        message: message.into(),
        retryable,
        details: BTreeMap::new(),
    }
}

fn ai_failure(error: AiError) -> Failure {
    failure(
        error_code(error.class),
        &error.public_message,
        error.retryable,
    )
}

fn error_code(class: AiErrorClass) -> &'static str {
    match class {
        AiErrorClass::Configuration => "ai_configuration",
        AiErrorClass::Authentication => "ai_authentication",
        AiErrorClass::RateLimit => "ai_rate_limit",
        AiErrorClass::ContextTooLarge => "ai_context_too_large",
        AiErrorClass::ModelUnavailable => "ai_model_unavailable",
        AiErrorClass::Network => "ai_network",
        AiErrorClass::Provider => "ai_provider",
        AiErrorClass::Cancelled => "ai_cancelled",
        AiErrorClass::Storage => "ai_storage",
    }
}

fn not_found() -> Failure {
    failure("not_found", "The requested AI item was not found.", false)
}

fn active_history_conflict() -> Failure {
    failure(
        "ai_attempt_active",
        "Wait for the active AI response to finish or stop it first.",
        true,
    )
}

fn export_markdown(
    messages: &[Message],
    through_ordinal: u32,
    chat: ChatId,
    block: MessageBlockId,
) -> String {
    let mut markdown = String::new();
    for message in messages
        .iter()
        .filter(|message| message.ordinal <= through_ordinal)
    {
        markdown.push_str(match message.role {
            ChatRole::User => "## You\n\n",
            ChatRole::Assistant => "## Koru AI\n\n",
        });
        markdown.push_str(message.markdown.trim_end());
        markdown.push_str("\n\n");
    }
    markdown.push_str("[Open source chat](");
    markdown.push_str(&koru_link::chat_link(chat.as_uuid(), Some(block.as_uuid())));
    markdown.push_str(")\n");
    markdown
}

fn unavailable_failure() -> Failure {
    failure("ai_unavailable", "The AI service is not available.", true)
}

fn invariant_failure() -> Failure {
    failure(
        "ai_invariant",
        "The AI operation could not be represented.",
        false,
    )
}

fn provider_stopped() -> AiError {
    AiError::new(
        AiErrorClass::Provider,
        "The AI provider is not available.",
        true,
    )
}

fn attempt_status_name(status: AttemptStatus) -> &'static str {
    match status {
        AttemptStatus::Pending => "pending",
        AttemptStatus::Streaming => "streaming",
        AttemptStatus::Complete => "complete",
        AttemptStatus::Interrupted => "interrupted",
        AttemptStatus::Failed => "failed",
        AttemptStatus::Cancelled => "cancelled",
    }
}

fn validate_web_search(chat: &Chat, requested: bool) -> ApiResult<()> {
    if requested
        && (chat.current_model.provider.as_str() != AiProviderId::OPENAI
            || !openai_model(&chat.current_model.requested_model)
                .capabilities
                .web_search)
    {
        return Err(failure(
            "invalid_request",
            "Web search is not supported by the selected model.",
            false,
        ));
    }
    Ok(())
}

fn ai_event_name(event: &AiEvent) -> &'static str {
    match event {
        AiEvent::Started { .. } => "started",
        AiEvent::TextDelta { .. } => "text_delta",
        AiEvent::ReasoningDelta { .. } => "reasoning_delta",
        AiEvent::Usage { .. } => "usage",
        AiEvent::ToolCalls { .. } => "tool_calls",
        AiEvent::ToolActivity { .. } => "tool_activity",
        AiEvent::AgentActivity { .. } => "agent_activity",
        AiEvent::Notice { .. } => "notice",
        AiEvent::WebSearch { .. } => "web_search",
        AiEvent::Citation { .. } => "citation",
        AiEvent::Completed { .. } => "complete",
        AiEvent::Failed { .. } => "failed",
        AiEvent::Cancelled { .. } => "cancelled",
    }
}

fn provider_validation_status_name(status: ProviderValidationStatus) -> &'static str {
    match status {
        ProviderValidationStatus::Unconfigured => "unconfigured",
        ProviderValidationStatus::Unavailable => "unavailable",
        ProviderValidationStatus::NoModels => "no_models",
        ProviderValidationStatus::Ready => "ready",
        ProviderValidationStatus::Failed => "failed",
    }
}

fn timestamp_delta_ms(start: Timestamp, end: Timestamp) -> u64 {
    end.millis().saturating_sub(start.millis()).max(0) as u64
}

fn optional_timestamp_delta_ms(start: Option<Timestamp>, end: Option<Timestamp>) -> Option<u64> {
    Some(timestamp_delta_ms(start?, end?))
}

fn log_terminal_attempt(attempt: &InferenceAttempt) {
    tracing::debug!(
        target: "koru_ai::attempt",
        attempt = %attempt.id,
        surface = ?attempt.surface,
        provider = %attempt.provider,
        requested_model = attempt.requested_model,
        resolved_model = attempt.resolved_model.as_deref().unwrap_or("unavailable"),
        requested_reasoning = attempt.requested_reasoning.as_token(),
        effective_reasoning = attempt
            .effective_reasoning
            .map(ReasoningChoice::as_token)
            .unwrap_or("unavailable"),
        status = attempt_status_name(attempt.status),
        error_class = attempt.error_class.map(error_code).unwrap_or("none"),
        queue_ms = ?optional_timestamp_delta_ms(
            Some(attempt.queued_at),
            attempt.context_started_at,
        ),
        context_ms = ?optional_timestamp_delta_ms(
            attempt.context_started_at,
            attempt.context_completed_at,
        ),
        provider_start_ms = ?optional_timestamp_delta_ms(
            attempt.context_completed_at,
            attempt.provider_started_at,
        ),
        time_to_first_token_ms = ?optional_timestamp_delta_ms(
            attempt.provider_started_at,
            attempt.first_token_at,
        ),
        generation_ms = ?optional_timestamp_delta_ms(
            attempt.first_token_at,
            attempt.completed_at,
        ),
        total_ms = ?optional_timestamp_delta_ms(Some(attempt.queued_at), attempt.completed_at),
        message_count = attempt.message_count,
        document_count = attempt.context_document_count,
        message_block_count = attempt.context_message_block_count,
        context_bytes = attempt.context_payload_bytes,
        prompt_tokens = ?attempt.usage.prompt_tokens,
        cached_prompt_tokens = ?attempt.usage.cached_prompt_tokens,
        completion_tokens = ?attempt.usage.completion_tokens,
        reasoning_tokens = ?attempt.usage.reasoning_tokens,
        total_tokens = ?attempt.usage.total_tokens,
        "AI attempt timing summary"
    );
}

fn set_status(status: &Mutex<WorkerStatus>, value: WorkerStatus) {
    *lock_status(status) = value;
}

fn lock_status(status: &Mutex<WorkerStatus>) -> std::sync::MutexGuard<'_, WorkerStatus> {
    status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests;
