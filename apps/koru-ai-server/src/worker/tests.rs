// SPDX-License-Identifier: MPL-2.0

use std::sync::Arc;
use std::sync::mpsc::sync_channel;
use std::time::{Duration, Instant};

use koru_ai_api::{AiPrincipal, AiScope, AiServerPort, RequestContext};
use koru_ai_core::{
    AiError, AiErrorClass, AiProvider, AiProviderId, AiRequest, AiSurface, AiToolCall,
    AiToolDefinition, AiToolExecutionContext, AiToolExecutor, AiToolResult, AiToolSafety,
    AttemptStatus, Cancellation, ChatId, ChatRole, ContextItem, ContextRef, DocumentContext,
    DocumentDestination, DocumentId, FeedbackEventId, FeedbackValue, InferenceAttemptId,
    MessageState, ModelSelection, ProviderStream, ReasoningChoice, RecoveryCounts,
    StoreContextPort, StoreRevision, ToolActivityStatus, ToolApproval, ToolMode, WorkspaceId,
};
use koru_ai_provider::{
    CredentialStore, DeterministicMockProvider, ProviderRegistry, SecretString,
    SessionCredentialStore,
};
use koru_ai_wire::{
    BranchChatRequest, CreateChatRequest, FeedbackRequest, PublicAiEvent, RenameChatRequest,
    RetryTurnRequest, SelectProviderRequest, SendTurnRequest, UpdateChatPrivacyRequest,
    UpdateChatToolPolicyRequest, UpdateOllamaSettingsRequest, UpdateOpenAiSettingsRequest,
};
use serde_json::json;
use uuid::Uuid;

use super::*;

fn context() -> RequestContext {
    RequestContext {
        principal: AiPrincipal {
            label: "worker-test".into(),
            scopes: vec![AiScope::Chat],
        },
        trace_id: "trace-worker-test".into(),
        deadline_millis: None,
    }
}

fn provider_context() -> RequestContext {
    RequestContext {
        principal: AiPrincipal {
            label: "worker-provider-test".into(),
            scopes: vec![AiScope::ManageProviders],
        },
        trace_id: "trace-worker-provider-test".into(),
        deadline_millis: None,
    }
}

fn usage_context() -> RequestContext {
    RequestContext {
        principal: AiPrincipal {
            label: "worker-usage-test".into(),
            scopes: vec![AiScope::ReadUsage],
        },
        trace_id: "trace-worker-usage-test".into(),
        deadline_millis: None,
    }
}

fn model() -> koru_ai_core::ModelSelection {
    koru_ai_core::ModelSelection {
        provider: AiProviderId::parse("mock").unwrap(),
        requested_model: "deterministic-v1".into(),
        default_reasoning: ReasoningChoice::Default,
    }
}

fn create_chat(client: &AiWorkerClient) -> ChatId {
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: "Worker test".into(),
                model: model(),
            },
        )
        .unwrap();
    chat_id
}

fn send(client: &AiWorkerClient, chat_id: ChatId, operation_id: Uuid) -> AiResponseStream {
    client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id,
                chat_id,
                surface: AiSurface::Standalone,
                markdown: "Question".into(),
                explicit_refs: Vec::new(),
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap()
}

#[derive(Clone)]
struct CapturingProvider {
    requests: std::sync::mpsc::SyncSender<AiRequest>,
}

#[derive(Clone)]
struct WebSearchProvider {
    requests: std::sync::mpsc::SyncSender<AiRequest>,
}

impl AiProvider for WebSearchProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        let attempt_id = request.attempt_id;
        self.requests.send(request).unwrap();
        let source = koru_ai_core::SourceLink {
            url: "https://example.com/source".into(),
            title: "Example source".into(),
        };
        Ok(Box::new(
            vec![
                AiEvent::Started {
                    attempt_id,
                    provider_response_id: Some("response-search".into()),
                    request_payload_bytes: Some(321),
                    resolved_model: Some("gpt-5.4".into()),
                    effective_reasoning: None,
                    at: Timestamp::now(),
                },
                AiEvent::WebSearch {
                    attempt_id,
                    call_id: "search-1".into(),
                    phase: koru_ai_core::WebSearchPhase::Started,
                    at: Timestamp::now(),
                },
                AiEvent::TextDelta {
                    attempt_id,
                    text: "Current answer".into(),
                    at: Timestamp::now(),
                },
                AiEvent::Citation {
                    attempt_id,
                    source: source.clone(),
                    at: Timestamp::now(),
                },
                AiEvent::Citation {
                    attempt_id,
                    source,
                    at: Timestamp::now(),
                },
                AiEvent::WebSearch {
                    attempt_id,
                    call_id: "search-1".into(),
                    phase: koru_ai_core::WebSearchPhase::Completed,
                    at: Timestamp::now(),
                },
                AiEvent::Usage {
                    attempt_id,
                    usage: koru_ai_core::AiUsage {
                        prompt_tokens: Some(10),
                        cached_prompt_tokens: None,
                        completion_tokens: Some(2),
                        reasoning_tokens: None,
                        total_tokens: Some(12),
                        source: koru_ai_core::UsageSource::ProviderReported,
                    },
                },
                AiEvent::Completed {
                    attempt_id,
                    stop_reason: Some("stop".into()),
                    at: Timestamp::now(),
                },
            ]
            .into_iter(),
        ))
    }
}

impl AiProvider for CapturingProvider {
    fn start(
        &self,
        request: AiRequest,
        _cancellation: Cancellation,
    ) -> Result<ProviderStream, AiError> {
        let attempt_id = request.attempt_id;
        self.requests.send(request).unwrap();
        Ok(Box::new(
            vec![
                AiEvent::Started {
                    attempt_id,
                    provider_response_id: None,
                    request_payload_bytes: None,
                    resolved_model: Some("capture-v1".into()),
                    effective_reasoning: None,
                    at: Timestamp::now(),
                },
                AiEvent::TextDelta {
                    attempt_id,
                    text: "Reusable answer.".into(),
                    at: Timestamp::now(),
                },
                AiEvent::Completed {
                    attempt_id,
                    stop_reason: Some("stop".into()),
                    at: Timestamp::now(),
                },
            ]
            .into_iter(),
        ))
    }
}

struct FixedStoreContext {
    document: DocumentId,
    failure: bool,
}

#[derive(Default)]
struct FixedToolExecutor;

impl AiToolExecutor for FixedToolExecutor {
    fn definitions(&self) -> Vec<AiToolDefinition> {
        vec![AiToolDefinition {
            name: "read_document".into(),
            title: "Read document".into(),
            description: "Read one authorized Koru document".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "document_id": { "type": "string" } },
                "required": ["document_id"],
                "additionalProperties": false
            }),
            output_schema: json!({ "type": "object" }),
            safety: AiToolSafety {
                read_only: true,
                destructive: false,
                idempotent: true,
                open_world: false,
            },
        }]
    }

    fn invoke(
        &self,
        _context: &AiToolExecutionContext,
        call: &AiToolCall,
        _cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError> {
        Ok(AiToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: json!({ "title": "Roadmap", "markdown": "# Roadmap" }),
            is_error: false,
        })
    }
}

#[derive(Default)]
struct MutationToolExecutor;

impl AiToolExecutor for MutationToolExecutor {
    fn definitions(&self) -> Vec<AiToolDefinition> {
        vec![AiToolDefinition {
            name: "update_document".into(),
            title: "Update document".into(),
            description: "Apply one semantic document update".into(),
            input_schema: json!({ "type": "object" }),
            output_schema: json!({ "type": "object" }),
            safety: AiToolSafety {
                read_only: false,
                destructive: false,
                idempotent: false,
                open_world: false,
            },
        }]
    }

    fn invoke(
        &self,
        _context: &AiToolExecutionContext,
        call: &AiToolCall,
        _cancellation: &Cancellation,
    ) -> Result<AiToolResult, AiError> {
        Ok(AiToolResult {
            call_id: call.call_id.clone(),
            name: call.name.clone(),
            content: json!({ "updated": true }),
            is_error: false,
        })
    }
}

impl StoreContextPort for FixedStoreContext {
    fn resolve_document(&self, document: DocumentId) -> Result<DocumentContext, AiError> {
        if self.failure || document != self.document {
            return Err(AiError::new(
                AiErrorClass::Storage,
                "The selected document is unavailable.",
                false,
            ));
        }
        Ok(DocumentContext {
            document_id: document,
            display_title: "Roadmap".into(),
            source_revision: StoreRevision(vec![7, 8, 9]),
            markdown: "# Roadmap\n\nShip context.\n".into(),
        })
    }

    fn create_document(
        &self,
        _idempotency_key: Uuid,
        _destination: &DocumentDestination,
        _markdown: &str,
    ) -> Result<DocumentId, AiError> {
        unreachable!("not exercised by chat context tests")
    }
}

struct MultiStoreContext {
    documents: Vec<(DocumentId, &'static str, &'static str)>,
}

impl StoreContextPort for MultiStoreContext {
    fn resolve_document(&self, document: DocumentId) -> Result<DocumentContext, AiError> {
        let (_, title, markdown) = self
            .documents
            .iter()
            .find(|(id, _, _)| *id == document)
            .ok_or_else(|| {
                AiError::new(
                    AiErrorClass::Storage,
                    "The selected document is unavailable.",
                    false,
                )
            })?;
        Ok(DocumentContext {
            document_id: document,
            display_title: (*title).into(),
            source_revision: StoreRevision(vec![1, 2, 3]),
            markdown: (*markdown).into(),
        })
    }

    fn create_document(
        &self,
        _idempotency_key: Uuid,
        _destination: &DocumentDestination,
        _markdown: &str,
    ) -> Result<DocumentId, AiError> {
        unreachable!("not exercised by chat context tests")
    }
}

fn wait_for_terminal(client: &AiWorkerClient, attempt_id: InferenceAttemptId) -> AttemptStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = client
            .read_attempt(context(), attempt_id)
            .unwrap()
            .attempt
            .status;
        if status.is_terminal() {
            return status;
        }
        assert!(Instant::now() < deadline, "attempt never became terminal");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn worker_reports_readiness_only_after_schema_and_recovery_are_ready() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();

    let info = worker.info().unwrap();
    assert_eq!(info.api_version, AI_WIRE_VERSION);
    assert_eq!(info.schema_version, koru_ai_sqlite::SCHEMA_VERSION);
    assert_eq!(info.providers, vec!["mock"]);
    assert!(info.capabilities.contains(&"ai.chat".into()));
    assert_eq!(info.recovery, RecoveryCounts::default());
    assert_eq!(worker.status(), WorkerStatus::Ready(info));
    assert!(directory.path().join("ai.db").exists());
    worker.ping().unwrap();

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn ready_worker_without_a_provider_advertises_no_ai_capability() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        ProviderRegistry::default(),
    ))
    .spawn()
    .unwrap();

    let info = worker.info().unwrap();
    assert!(info.providers.is_empty());
    assert!(info.capabilities.is_empty());

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn client_minted_chat_identity_is_retry_safe_but_not_reusable() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let request = CreateChatRequest {
        chat_id: ChatId::new(),
        title: "Retry-safe".into(),
        model: model(),
    };
    let first = client.create_chat(context(), request.clone()).unwrap();
    assert_eq!(client.create_chat(context(), request).unwrap(), first);

    let conflict = client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id: first.chat.id,
                title: "Different".into(),
                model: model(),
            },
        )
        .unwrap_err();
    assert_eq!(conflict.code, "idempotency_conflict");

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn chat_titles_are_renamed_durably_through_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);

    let renamed = client
        .rename_chat(
            context(),
            chat,
            RenameChatRequest {
                title: "Renamed chat".into(),
            },
        )
        .unwrap();
    assert_eq!(renamed.chat.title, "Renamed chat");
    assert_eq!(
        client.read_chat(context(), chat).unwrap().chat,
        renamed.chat
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn chat_models_are_changed_durably_through_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    let selected = ModelSelection {
        provider: AiProviderId::parse("mock").unwrap(),
        requested_model: "deterministic-mock".into(),
        default_reasoning: ReasoningChoice::Default,
    };

    let updated = client
        .update_chat_model(
            context(),
            chat,
            UpdateChatModelRequest {
                model: selected.clone(),
            },
        )
        .unwrap();
    assert_eq!(updated.chat.current_model, selected);
    assert_eq!(
        client.read_chat(context(), chat).unwrap().chat,
        updated.chat
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn mock_provider_completes_through_bounded_worker_and_real_database() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let operation_id = Uuid::new_v4();
    let stream = send(&client, chat_id, operation_id);
    let accepted = stream.accepted.clone();
    let mut text = String::new();
    let mut text_events = 0;
    let mut last_sequence = 0;
    loop {
        let public = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(public.version, AI_STREAM_VERSION);
        assert!(public.sequence > last_sequence);
        last_sequence = public.sequence;
        match public.event {
            PublicAiEvent::TextDelta { text: delta, .. } => {
                text_events += 1;
                text.push_str(&delta);
            }
            event if event.is_terminal() => break,
            _ => {}
        }
    }
    assert_eq!(text, "Deterministic mock response.");
    assert!(
        text_events < text.chars().count(),
        "provider character chunks were not coalesced"
    );
    assert_eq!(
        client
            .read_attempt(context(), accepted.attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.messages[1].markdown, text);
    assert_eq!(transcript.messages[1].state, MessageState::Complete);
    assert_eq!(transcript.blocks.len(), 1);
    assert_eq!(transcript.blocks[0].plain_text, text);
    assert_eq!(transcript.usage.len(), 1);
    assert_eq!(
        transcript.usage[0].message_id,
        accepted.assistant_message_id
    );
    let persisted_attempt = client
        .read_attempt(context(), accepted.attempt_id)
        .unwrap()
        .attempt;
    assert_eq!(transcript.usage[0].usage, persisted_attempt.usage);

    let mut unavailable_duplicate = persisted_attempt.clone();
    unavailable_duplicate.usage = AiUsage::default();
    let mut latest_reported_duplicate = persisted_attempt.clone();
    latest_reported_duplicate.usage.completion_tokens = Some(999);
    let resolved = transcript_usage(
        &transcript.messages,
        vec![
            persisted_attempt,
            unavailable_duplicate,
            latest_reported_duplicate.clone(),
        ],
    );
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].usage, latest_reported_duplicate.usage);

    let replay = send(&client, chat_id, operation_id);
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, accepted.attempt_id);
    assert!(replay.events.recv().is_err());

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn armed_web_search_is_audited_and_citations_reach_transcript_and_usage() {
    let directory = tempfile::tempdir().unwrap();
    let (captured, requests) = sync_channel(2);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
        Arc::new(WebSearchProvider { requests: captured }),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: "Search test".into(),
                model: ModelSelection {
                    provider: AiProviderId::parse(AiProviderId::OPENAI).unwrap(),
                    requested_model: "gpt-5.4".into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    let stream = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: "What changed?".into(),
                explicit_refs: Vec::new(),
                web_search: true,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    assert!(
        requests
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .options
            .web_search
    );
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let attempt = client
        .read_attempt(context(), stream.accepted.attempt_id)
        .unwrap()
        .attempt;
    assert!(attempt.web_search_requested);
    assert_eq!(attempt.web_search_calls, 1);
    assert_eq!(attempt.request_payload_bytes, Some(321));
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.sources.len(), 1);
    assert_eq!(
        transcript.sources[0].sources.len(),
        1,
        "duplicate URLs are ignored"
    );
    assert_eq!(transcript.sources[0].sources[0].title, "Example source");
    let branch = client
        .branch_chat(
            context(),
            chat_id,
            BranchChatRequest {
                branch_chat_id: ChatId::new(),
                through_message_id: stream.accepted.assistant_message_id,
                title: "Search branch".into(),
            },
        )
        .unwrap()
        .transcript;
    assert_eq!(branch.sources.len(), 1);
    assert_eq!(
        branch.sources[0].sources[0].url,
        "https://example.com/source"
    );
    let usage = client.usage(usage_context(), Some(chat_id)).unwrap().report;
    assert_eq!(usage.current_chat.unwrap().web_search_calls, 1);

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn unsupported_web_search_is_rejected_before_an_attempt_or_provider_call() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let error = match client.send_turn(
        context(),
        SendTurnRequest {
            operation_id: Uuid::new_v4(),
            chat_id,
            surface: AiSurface::Standalone,
            markdown: "Search".into(),
            explicit_refs: Vec::new(),
            web_search: true,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
        },
    ) {
        Ok(_) => panic!("unsupported web search unexpectedly started"),
        Err(error) => error,
    };
    assert_eq!(error.code, "invalid_request");
    assert_eq!(
        error.message,
        "Web search is not supported by the selected model."
    );
    assert!(
        client
            .read_chat(context(), chat_id)
            .unwrap()
            .messages
            .is_empty()
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn document_and_message_block_context_reach_provider_with_durable_provenance() {
    let directory = tempfile::tempdir().unwrap();
    let document_id = DocumentId::new();
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_store_context(Arc::new(FixedStoreContext {
                document: document_id,
                failure: false,
            })),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);

    let first = send(&client, chat_id, Uuid::new_v4());
    let first_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(first_request.context.is_empty());
    assert_eq!(
        wait_for_terminal(&client, first.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let first_transcript = client.read_chat(context(), chat_id).unwrap();
    assert!(first_transcript.usage.is_empty());
    let source_block = first_transcript.blocks[0].id;

    let second = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::ContextPane,
                markdown: "Use both sources".into(),
                explicit_refs: vec![
                    ContextRef::Document { document_id },
                    ContextRef::MessageBlock {
                        block_id: source_block,
                    },
                ],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    let second_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        second_request.context[0],
        ContextItem::Document(_)
    ));
    assert!(matches!(
        second_request.context[1],
        ContextItem::MessageBlock(_)
    ));
    assert_eq!(
        wait_for_terminal(&client, second.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let attempt = client
        .read_attempt(context(), second.accepted.attempt_id)
        .unwrap()
        .attempt;
    assert!(attempt.context_started_at.is_some());
    assert!(attempt.context_completed_at.is_some());
    assert_eq!(attempt.context_document_count, 1);
    assert_eq!(attempt.context_message_block_count, 1);
    assert!(attempt.context_payload_bytes > 0);
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.pinned_documents.len(), 1);
    assert_eq!(transcript.pinned_documents[0].document_id, document_id);
    assert_eq!(transcript.pinned_documents[0].display_title, "Roadmap");

    let third = send(&client, chat_id, Uuid::new_v4());
    let third_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        third_request.context.as_slice(),
        [ContextItem::Document(_)]
    ));
    assert_eq!(
        wait_for_terminal(&client, third.accepted.attempt_id),
        AttemptStatus::Complete
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn replace_pinned_documents_cleans_prior_context_for_context_pane() {
    let directory = tempfile::tempdir().unwrap();
    let first_document = DocumentId::new();
    let second_document = DocumentId::new();
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_store_context(Arc::new(MultiStoreContext {
                documents: vec![
                    (
                        first_document,
                        "First document",
                        "# First\n\nFirst content.\n",
                    ),
                    (
                        second_document,
                        "Second document",
                        "# Second\n\nSecond content.\n",
                    ),
                ],
            })),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);

    let first = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::ContextPane,
                markdown: "Question about first".into(),
                explicit_refs: vec![ContextRef::Document {
                    document_id: first_document,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    let first_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(first_request.context.len(), 1);
    assert_eq!(
        wait_for_terminal(&client, first.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let second = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::ContextPane,
                markdown: "Question about second".into(),
                explicit_refs: vec![ContextRef::Document {
                    document_id: second_document,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: true,
            },
        )
        .unwrap();
    let second_request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(second_request.context.len(), 1);
    assert!(matches!(
        second_request.context[0],
        ContextItem::Document(DocumentContext {
            document_id,
            ..
        }) if document_id == second_document
    ));
    assert_eq!(
        wait_for_terminal(&client, second.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.pinned_documents.len(), 1);
    assert_eq!(transcript.pinned_documents[0].document_id, second_document);

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn private_message_blocks_are_available_only_to_their_own_chat() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let source = create_chat(&client);
    let first = send(&client, source, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, first.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let source_block = client.read_chat(context(), source).unwrap().blocks[0].id;

    let target = create_chat(&client);
    let public_reference = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id: target,
                surface: AiSurface::Standalone,
                markdown: "Use the public answer".into(),
                explicit_refs: vec![ContextRef::MessageBlock {
                    block_id: source_block,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, public_reference.accepted.attempt_id),
        AttemptStatus::Complete
    );

    client
        .update_chat_privacy(
            context(),
            source,
            UpdateChatPrivacyRequest { private: true },
        )
        .unwrap();
    let own_reference = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id: source,
                surface: AiSurface::Standalone,
                markdown: "Use my private answer".into(),
                explicit_refs: vec![ContextRef::MessageBlock {
                    block_id: source_block,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, own_reference.accepted.attempt_id),
        AttemptStatus::Complete
    );

    let foreign_reference = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id: target,
                surface: AiSurface::Standalone,
                markdown: "Try the now-private answer".into(),
                explicit_refs: vec![ContextRef::MessageBlock {
                    block_id: source_block,
                }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, foreign_reference.accepted.attempt_id),
        AttemptStatus::Failed
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn context_failure_is_durable_and_never_calls_the_provider() {
    let directory = tempfile::tempdir().unwrap();
    let document_id = DocumentId::new();
    let (captured, requests) = sync_channel(1);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_store_context(Arc::new(FixedStoreContext {
                document: document_id,
                failure: true,
            })),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let response = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: "Use unavailable context".into(),
                explicit_refs: vec![ContextRef::Document { document_id }],
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();

    assert_eq!(
        wait_for_terminal(&client, response.accepted.attempt_id),
        AttemptStatus::Failed
    );
    assert!(requests.recv_timeout(Duration::from_millis(100)).is_err());
    let attempt = client
        .read_attempt(context(), response.accepted.attempt_id)
        .unwrap()
        .attempt;
    assert!(attempt.context_started_at.is_some());
    assert!(attempt.context_completed_at.is_some());
    assert!(
        client
            .read_chat(context(), chat_id)
            .unwrap()
            .pinned_documents
            .is_empty()
    );
    let retry = client
        .retry_turn(
            context(),
            chat_id,
            RetryTurnRequest {
                operation_id: Uuid::new_v4(),
                message_id: response.accepted.assistant_message_id,
                surface: AiSurface::Standalone,
                explicit_refs: vec![ContextRef::Document { document_id }],
                web_search: false,
                reasoning_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    assert_ne!(
        retry.accepted.assistant_message_id,
        response.accepted.assistant_message_id
    );
    assert_eq!(
        wait_for_terminal(&client, retry.accepted.attempt_id),
        AttemptStatus::Failed
    );
    let history = client.read_chat(context(), chat_id).unwrap().messages;
    assert_eq!(history.len(), 3);
    assert_eq!(history[1].state, MessageState::Failed);
    assert_eq!(history[2].origin_message_id, Some(history[1].id));

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn dropped_stream_does_not_stop_persistence_or_authoritative_recovery() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    drop(stream);

    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(
        transcript.messages[1].markdown,
        "Deterministic mock response."
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn provider_tool_calls_continue_within_one_durable_attempt() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("Answer from workspace.")
                .with_tool_call("read_document", json!({ "document_id": "document-a" })),
        ),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_tool_executor(Arc::new(FixedToolExecutor)),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;

    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.messages.len(), 2);
    assert_eq!(transcript.messages[1].markdown, "Answer from workspace.");
    assert_eq!(
        client
            .read_attempt(context(), attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Complete
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn provider_can_read_its_own_private_chat_without_reentering_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let chat_id = ChatId::new();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("Read the private chat.").with_tool_call(
                "read_chat",
                json!({ "chat_id": chat_id.to_storage(), "page_size": 20 }),
            ),
        ),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_tool_executor(Arc::new(crate::ChatToolAdapter::new(
                directory.path().to_owned(),
            ))),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: "Private self-read".into(),
                model: model(),
            },
        )
        .unwrap();
    client
        .update_chat_privacy(
            context(),
            chat_id,
            UpdateChatPrivacyRequest { private: true },
        )
        .unwrap();

    let attempt_id = send(&client, chat_id, Uuid::new_v4()).accepted.attempt_id;
    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat_id).unwrap();
    assert_eq!(transcript.messages[1].markdown, "Read the private chat.");
    assert_eq!(transcript.tool_activities.len(), 1);
    assert_eq!(
        transcript.tool_activities[0].status,
        ToolActivityStatus::Complete
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn manual_mode_pauses_mutation_until_idempotent_approval() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("Mutation complete.")
                .with_tool_call("update_document", json!({ "change": "safe fixture" })),
        ),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_tool_executor(Arc::new(MutationToolExecutor)),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    let invocation = loop {
        let event = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
        if let PublicAiEvent::ToolActivity { activity, .. } = event.event
            && activity.status == ToolActivityStatus::ApprovalRequired
        {
            break activity.id;
        }
    };
    assert_eq!(
        client
            .read_attempt(context(), attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Streaming
    );

    let approved = client
        .approve_tool_call(context(), invocation, None)
        .unwrap();
    assert_eq!(approved.activity.approval, ToolApproval::Approved);
    assert_eq!(
        client
            .approve_tool_call(context(), invocation, None)
            .unwrap()
            .activity
            .approval,
        ToolApproval::Approved
    );
    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    let activity = &client
        .read_chat(context(), chat_id)
        .unwrap()
        .tool_activities[0];
    assert_eq!(activity.status, ToolActivityStatus::Complete);
    assert_eq!(activity.approval, ToolApproval::Approved);

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn denial_returns_permission_result_and_auto_mode_does_not_pause() {
    for (mode, deny) in [(ToolMode::Manual, true), (ToolMode::Auto, false)] {
        let directory = tempfile::tempdir().unwrap();
        let mut providers = ProviderRegistry::default();
        providers.insert(
            AiProviderId::parse("mock").unwrap(),
            Arc::new(
                DeterministicMockProvider::new("Provider continued.")
                    .with_tool_call("update_document", json!({ "change": "fixture" })),
            ),
        );
        let worker = AiWorker::new(
            WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
                .with_tool_executor(Arc::new(MutationToolExecutor)),
        )
        .spawn()
        .unwrap();
        let client = worker.client();
        let chat_id = create_chat(&client);
        client
            .update_chat_tool_policy(
                context(),
                chat_id,
                UpdateChatToolPolicyRequest {
                    mode,
                    workspace_ids: vec![WorkspaceId::new()],
                },
            )
            .unwrap();
        let stream = send(&client, chat_id, Uuid::new_v4());
        let attempt_id = stream.accepted.attempt_id;
        if deny {
            let invocation = loop {
                let event = stream.events.recv_timeout(Duration::from_secs(5)).unwrap();
                if let PublicAiEvent::ToolActivity { activity, .. } = event.event
                    && activity.status == ToolActivityStatus::ApprovalRequired
                {
                    break activity.id;
                }
            };
            let denied = client.deny_tool_call(context(), invocation, None).unwrap();
            assert_eq!(denied.activity.status, ToolActivityStatus::Denied);
            assert_eq!(
                client
                    .deny_tool_call(context(), invocation, None)
                    .unwrap()
                    .activity
                    .status,
                ToolActivityStatus::Denied
            );
        }
        assert_eq!(
            wait_for_terminal(&client, attempt_id),
            AttemptStatus::Complete
        );
        let transcript = client.read_chat(context(), chat_id).unwrap();
        assert_eq!(transcript.messages[1].markdown, "Provider continued.");
        assert_eq!(
            transcript.tool_activities[0].status,
            if deny {
                ToolActivityStatus::Denied
            } else {
                ToolActivityStatus::Complete
            }
        );

        worker.shutdown();
        assert_eq!(worker.join(), WorkerExit::Clean);
    }
}

#[test]
fn full_stream_buffer_detaches_while_authoritative_completion_continues() {
    let directory = tempfile::tempdir().unwrap();
    let response = "x".repeat(COALESCE_BYTES * (STREAM_CAPACITY + 8));
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new(response.clone())),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    // Keep the receiver alive without draining it. The bounded buffer fills,
    // then the worker detaches this subscriber and continues durable work.
    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Complete
    );
    assert_eq!(
        client.read_chat(context(), chat_id).unwrap().messages[1].markdown,
        response
    );
    worker.ping().unwrap();
    drop(stream);

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn cancellation_race_is_idempotent_and_terminal() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        // 32 characters at 200 ms: the cancel lands while the answer is still
        // streaming, and a cancelled stream stops at the next character, so
        // the length costs nothing. At 5 ms the whole answer took 160 ms and a
        // loaded runner finished it first (Complete, not Cancelled); 50 ms
        // still lost one run in 45 on four loaded cores.
        Arc::new(
            DeterministicMockProvider::new("a response long enough to cancel")
                .with_chunk_delay(Duration::from_millis(200)),
        ),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    while !matches!(
        stream
            .events
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .event,
        PublicAiEvent::Started { .. }
    ) {}

    let first = client.cancel_attempt(context(), attempt_id).unwrap();
    let second = client.cancel_attempt(context(), attempt_id).unwrap();
    assert_eq!(first.status, AttemptStatus::Cancelled);
    assert_eq!(second, first);
    assert_eq!(
        client
            .read_attempt(context(), attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Cancelled
    );
    assert_eq!(
        client.read_chat(context(), chat_id).unwrap().messages[1].state,
        MessageState::Interrupted
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn command_queue_is_bounded_and_shutdown_cannot_be_starved_by_it() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    worker
        .sender
        .try_send(Envelope::Command(WorkerCommand::Hold {
            entered: entered_tx,
            release: release_rx,
        }))
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();

    for _ in 0..WORKER_COMMAND_CAPACITY {
        let (reply, _response) = sync_channel(1);
        worker
            .sender
            .try_send(Envelope::Command(WorkerCommand::Ping(reply)))
            .unwrap();
    }
    assert_eq!(worker.ping().unwrap_err(), WorkerError::Saturated);
    worker.shutdown();
    release_tx.send(()).unwrap();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn provider_queue_saturation_fails_excess_work_without_blocking_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        // The one provider thread is held for about seven seconds, so the
        // queue behind it stays full for as long as the test needs: with
        // "slow" it was free again after two, and on a loaded runner the
        // excess attempt could arrive after that and simply be queued.
        Arc::new(
            DeterministicMockProvider::new("a slow response")
                .with_chunk_delay(Duration::from_millis(500)),
        ),
    );
    let worker = AiWorker::new(
        WorkerConfig::new(directory.path().to_owned(), "internal-debug", providers)
            .with_provider_threads(1),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let mut attempts = Vec::new();
    for _ in 0..PROVIDER_QUEUE_CAPACITY + 2 {
        let chat_id = create_chat(&client);
        attempts.push(send(&client, chat_id, Uuid::new_v4()).accepted.attempt_id);
    }

    // Each attempt reaches the provider queue only after its context is
    // resolved on another thread, so the refusal is waited for, not read
    // the instant the last send returns.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !attempts.iter().any(|attempt_id| {
        client
            .read_attempt(context(), *attempt_id)
            .unwrap()
            .attempt
            .status
            == AttemptStatus::Failed
    }) {
        assert!(
            Instant::now() < deadline,
            "no excess attempt was refused by the full provider queue"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    worker.ping().unwrap();

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn one_command_panic_is_contained_without_stopping_the_worker() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let (entered_tx, entered_rx) = sync_channel(1);
    worker
        .sender
        .try_send(Envelope::Command(WorkerCommand::Panic(entered_tx)))
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    worker.ping().unwrap();

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

struct PanickingProvider;

impl AiProvider for PanickingProvider {
    fn start(&self, _: AiRequest, _: Cancellation) -> Result<ProviderStream, AiError> {
        panic!("requested provider panic")
    }
}

#[test]
fn provider_panic_becomes_a_safe_failed_attempt_and_worker_survives() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(PanickingProvider),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    assert_eq!(
        wait_for_terminal(&client, attempt_id),
        AttemptStatus::Failed
    );
    let failed = client.read_attempt(context(), attempt_id).unwrap().attempt;
    assert_eq!(failed.error_class, Some(AiErrorClass::Provider));
    worker.ping().unwrap();

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn unauthorized_or_expired_internal_context_is_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let request = CreateChatRequest {
        chat_id: ChatId::new(),
        title: "Denied".into(),
        model: model(),
    };
    let mut denied = context();
    denied.principal.scopes.clear();
    assert_eq!(
        client
            .create_chat(denied, request.clone())
            .unwrap_err()
            .code,
        "forbidden"
    );
    let mut expired = context();
    expired.deadline_millis = Some(0);
    assert_eq!(
        client.create_chat(expired, request).unwrap_err().code,
        "deadline_exceeded"
    );

    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn startup_failure_never_reports_ready() {
    let directory = tempfile::tempdir().unwrap();
    let root_file = directory.path().join("not-a-directory");
    std::fs::write(&root_file, b"occupied").unwrap();
    let error = AiWorker::new(WorkerConfig::deterministic_mock(
        root_file,
        "internal-debug",
    ))
    .spawn()
    .unwrap_err();
    assert!(matches!(error, WorkerError::Startup(_)));
}

#[test]
fn startup_panic_is_observed_as_failure_not_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let mut config =
        WorkerConfig::deterministic_mock(directory.path().to_owned(), "internal-debug");
    config.panic_after_open = true;
    assert_eq!(
        AiWorker::new(config).spawn().unwrap_err(),
        WorkerError::Startup("worker panicked".into())
    );
}

#[test]
fn shutdown_cancels_active_attempt_before_database_close() {
    let directory = tempfile::tempdir().unwrap();
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(
            DeterministicMockProvider::new("slow response")
                .with_chunk_delay(Duration::from_millis(20)),
        ),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = send(&client, chat_id, Uuid::new_v4());
    let attempt_id = stream.accepted.attempt_id;
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    let repository = Repository::open(&directory.path().join("ai.db")).unwrap();
    let attempt = repository.attempt(attempt_id).unwrap().unwrap();
    assert_eq!(attempt.status, AttemptStatus::Cancelled, "{attempt:?}");
}

#[test]
fn provider_settings_are_scoped_persisted_and_rehydrated() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_owned();
    let credentials = Arc::new(SessionCredentialStore::default());
    let config = WorkerConfig::production_with_credentials(
        root.clone(),
        "internal-debug",
        credentials,
        "koru-internal-debug",
    )
    .unwrap();
    let worker = AiWorker::new(config).spawn().unwrap();
    let client = worker.client();

    let forbidden = client.provider_settings(context()).unwrap_err();
    assert_eq!(forbidden.code, "forbidden");

    let defaults = client.provider_settings(provider_context()).unwrap();
    assert_eq!(defaults.selected_provider, None);
    assert_eq!(defaults.ollama.endpoint, "http://localhost:11434/");
    assert!(!defaults.openai.credential_configured);

    let ollama = client
        .update_ollama_settings(
            provider_context(),
            UpdateOllamaSettingsRequest {
                endpoint: "http://127.0.0.1:11434".into(),
                model: Some("gpt-oss:20b".into()),
                reasoning: ReasoningChoice::Level(koru_ai_core::ReasoningLevel::High),
            },
        )
        .unwrap();
    assert_eq!(ollama.endpoint, "http://127.0.0.1:11434/");
    assert_eq!(ollama.model.as_deref(), Some("gpt-oss:20b"));

    let openai = client
        .update_openai_settings(
            provider_context(),
            UpdateOpenAiSettingsRequest {
                model: Some("gpt-5.6".into()),
                reasoning: ReasoningChoice::Level(koru_ai_core::ReasoningLevel::Medium),
            },
        )
        .unwrap();
    assert_eq!(openai.model.as_deref(), Some("gpt-5.6"));
    assert!(!openai.credential_configured);

    let selected = client
        .select_provider(
            provider_context(),
            SelectProviderRequest {
                provider: AiProviderId::parse("ollama").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        selected
            .selected_provider
            .as_ref()
            .map(AiProviderId::as_str),
        Some("ollama")
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    let credentials = Arc::new(SessionCredentialStore::default());
    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            root,
            "internal-debug",
            credentials,
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();
    let saved = worker
        .client()
        .provider_settings(provider_context())
        .unwrap();
    assert_eq!(
        saved.selected_provider.as_ref().map(AiProviderId::as_str),
        Some("ollama")
    );
    assert_eq!(saved.openai.model.as_deref(), Some("gpt-5.6"));
    assert_eq!(saved.ollama.model.as_deref(), Some("gpt-oss:20b"));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn select_provider_accepts_koru_and_rejects_unknown_tokens() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_path_buf();
    let credentials = Arc::new(SessionCredentialStore::default());
    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            root,
            "internal-debug",
            credentials,
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();
    let client = worker.client();
    let selected = client
        .select_provider(
            provider_context(),
            SelectProviderRequest {
                provider: AiProviderId::parse("koru").unwrap(),
            },
        )
        .unwrap();
    assert_eq!(
        selected
            .selected_provider
            .as_ref()
            .map(AiProviderId::as_str),
        Some("koru")
    );
    let rejected = client
        .select_provider(
            provider_context(),
            SelectProviderRequest {
                provider: AiProviderId::parse("anthropic").unwrap(),
            },
        )
        .unwrap_err();
    assert_eq!(rejected.code, "invalid_request");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn openai_settings_record_presence_only_after_the_host_saved_the_vault_secret() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_owned();
    let credentials = Arc::new(SessionCredentialStore::default());
    credentials
        .save_openai(
            "koru-internal-debug",
            SecretString::new("sk-test-never-persist").unwrap(),
        )
        .unwrap();
    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            root.clone(),
            "internal-debug",
            credentials.clone(),
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();

    let settings = worker
        .client()
        .update_openai_settings(
            provider_context(),
            UpdateOpenAiSettingsRequest {
                model: Some("gpt-5.6".into()),
                reasoning: ReasoningChoice::Default,
            },
        )
        .unwrap();
    assert!(settings.credential_configured);
    let persisted = worker
        .client()
        .provider_settings(provider_context())
        .unwrap();
    assert!(persisted.openai.credential_configured);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);

    let repository = Repository::open(&root.join("ai.db")).unwrap();
    assert_eq!(
        repository.settings().unwrap().unwrap().openai.key_account,
        None
    );
    drop(repository);

    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            root,
            "internal-debug",
            credentials,
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();
    let rehydrated = worker
        .client()
        .provider_settings(provider_context())
        .unwrap();
    assert!(rehydrated.openai.credential_configured);
    assert_eq!(rehydrated.openai.model.as_deref(), Some("gpt-5.6"));
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn chat_listing_is_scoped_and_returns_durable_rows() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    assert_eq!(client.list_chats(context()).unwrap().chats[0].id, chat);
    assert_eq!(
        client.list_chats(provider_context()).unwrap_err().code,
        "forbidden"
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn completed_responses_support_toggle_feedback_and_identity_preserving_branches() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    let stream = send(&client, chat, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat).unwrap();
    let response = transcript
        .messages
        .iter()
        .find(|message| message.role == ChatRole::Assistant)
        .unwrap();
    assert!(!transcript.blocks.is_empty());
    client
        .update_chat_privacy(context(), chat, UpdateChatPrivacyRequest { private: true })
        .unwrap();

    client
        .feedback(
            context(),
            response.id,
            FeedbackRequest {
                event_id: FeedbackEventId::new(),
                value: FeedbackValue::Good,
                surface: AiSurface::Standalone,
            },
        )
        .unwrap();
    assert_eq!(
        client.read_chat(context(), chat).unwrap().feedback[0].value,
        FeedbackValue::Good
    );

    let branch = ChatId::new();
    let branched = client
        .branch_chat(
            context(),
            chat,
            BranchChatRequest {
                branch_chat_id: branch,
                through_message_id: response.id,
                title: "Worker test — branch".into(),
            },
        )
        .unwrap()
        .transcript;
    assert_eq!(branched.chat.id, branch);
    assert!(branched.chat.private);
    assert_eq!(branched.messages.len(), 2);
    assert!(
        branched
            .messages
            .iter()
            .all(|message| message.origin_message_id.is_some())
    );
    assert_ne!(branched.messages[1].id, response.id);
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn usage_is_separately_scoped_and_history_deletion_anonymizes_before_clear() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat = create_chat(&client);
    let stream = send(&client, chat, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let transcript = client.read_chat(context(), chat).unwrap();
    let response = transcript
        .messages
        .iter()
        .find(|message| message.role == ChatRole::Assistant)
        .unwrap();
    client
        .feedback(
            context(),
            response.id,
            FeedbackRequest {
                event_id: FeedbackEventId::new(),
                value: FeedbackValue::Good,
                surface: AiSurface::Standalone,
            },
        )
        .unwrap();

    assert_eq!(
        client.usage(context(), Some(chat)).unwrap_err().code,
        "forbidden"
    );
    let before = client.usage(usage_context(), Some(chat)).unwrap().report;
    assert_eq!(before.current_chat.unwrap().attempts, 1);
    assert_eq!(before.groups[0].totals.good_feedback, 1);

    assert!(client.delete_chat(context(), chat).unwrap().deleted);
    assert!(client.list_chats(context()).unwrap().chats.is_empty());
    let retained = client.usage(usage_context(), None).unwrap().report;
    assert_eq!(retained.groups.len(), 1);
    assert_eq!(retained.groups[0].totals.good_feedback, 1);

    let cleared = client.clear_usage(usage_context()).unwrap();
    assert_eq!(cleared.attempts_deleted, 1);
    assert_eq!(cleared.feedback_events_deleted, 1);
    assert!(
        client
            .usage(usage_context(), None)
            .unwrap()
            .report
            .groups
            .is_empty()
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn clear_history_keeps_usage_and_is_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let first = create_chat(&client);
    let stream = send(&client, first, Uuid::new_v4());
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    create_chat(&client);
    assert_eq!(client.clear_history(context()).unwrap().chats_deleted, 2);
    assert_eq!(client.clear_history(context()).unwrap().chats_deleted, 0);
    assert_eq!(
        client
            .usage(usage_context(), None)
            .unwrap()
            .report
            .groups
            .len(),
        1
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn debug_lifecycle_logs_exclude_chat_content_titles_and_paths() {
    const TITLE: &str = "PRIVATE-TITLE-SENTINEL";
    const CONTENT: &str = "PRIVATE-PROMPT-RESPONSE-SECRET-SENTINEL";
    const PATH_PART: &str = "PRIVATE-PATH-SENTINEL";

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join(PATH_PART);
    std::fs::create_dir_all(&root).unwrap();
    let logging = koru_log::init_file(
        &root,
        "koru-ai-safety.log",
        "koru_ai=debug,koru_ai_server=debug,koru_ai_sqlite=debug",
    );
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        root.clone(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id,
                title: TITLE.into(),
                model: model(),
            },
        )
        .unwrap();
    let stream = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: CONTENT.into(),
                explicit_refs: Vec::new(),
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(logging);

    let logs = std::fs::read_dir(root.join(koru_log::LOG_DIR))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(!logs.contains(TITLE), "chat title reached lifecycle logs");
    assert!(
        !logs.contains(CONTENT),
        "chat content reached lifecycle logs"
    );
    assert!(
        !logs.contains(PATH_PART),
        "filesystem path reached lifecycle logs"
    );
    assert!(logs.contains("AI attempt timing summary"));
    assert!(logs.contains("AI provider stream coalescing summary"));
}

#[test]
fn payload_debug_logs_one_marked_request_and_terminal_response() {
    const CHILD: &str = koru_env::name::KORU_AI_PAYLOAD_TEST_CHILD;
    const CONTENT: &str = "PRIVATE-PAYLOAD-DEBUG-SENTINEL";
    const RESPONSE: &str = "Deterministic mock response.";

    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("worker::tests::payload_debug_logs_one_marked_request_and_terminal_response")
            .arg("--exact")
            .arg("--nocapture")
            .env(CHILD, "1")
            .env(koru_env::name::KORU_AI_PAYLOAD_DEBUG, "1")
            .output()
            .expect("run isolated payload-debug test");
        assert!(
            output.status.success(),
            "isolated payload-debug test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    assert!(koru_env::switch(koru_env::name::KORU_AI_PAYLOAD_DEBUG));
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("payload-debug");
    std::fs::create_dir_all(&root).unwrap();
    let logging = koru_log::init_file(
        &root,
        "koru-ai-payload-test.log",
        "koru_ai=debug,koru_ai_server=debug,koru_ai_sqlite=debug",
    );
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        root.clone(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let client = worker.client();
    let chat_id = create_chat(&client);
    let stream = client
        .send_turn(
            context(),
            SendTurnRequest {
                operation_id: Uuid::new_v4(),
                chat_id,
                surface: AiSurface::Standalone,
                markdown: CONTENT.into(),
                explicit_refs: Vec::new(),
                web_search: false,
                reasoning_override: None,
                system_prompt_override: None,
                replace_pinned_documents: false,
            },
        )
        .unwrap();
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
    drop(logging);

    let logs = std::fs::read_dir(root.join(koru_log::LOG_DIR))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    let requests = logs
        .lines()
        .filter(|line| line.contains("KORU_AI_PAYLOAD:"))
        .collect::<Vec<_>>();
    let responses = logs
        .lines()
        .filter(|line| line.contains("KORU_AI_RESPONSE:"))
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 1, "request payload records: {requests:?}");
    assert_eq!(
        responses.len(),
        1,
        "response payload records: {responses:?}"
    );
    assert!(requests[0].contains(CONTENT));
    assert!(responses[0].contains(RESPONSE));
    assert!(
        logs.lines()
            .filter(|line| line.contains(CONTENT))
            .all(|line| { line.contains("KORU_AI_PAYLOAD:") })
    );
    assert!(
        logs.lines()
            .filter(|line| line.contains(RESPONSE))
            .all(|line| { line.contains("KORU_AI_RESPONSE:") })
    );
}

#[test]
fn provider_management_is_unavailable_for_test_only_registries() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(WorkerConfig::deterministic_mock(
        directory.path().to_owned(),
        "internal-debug",
    ))
    .spawn()
    .unwrap();
    let error = worker
        .client()
        .provider_settings(provider_context())
        .unwrap_err();
    assert_eq!(error.code, "ai_provider_management_unavailable");
    let error = worker
        .client()
        .update_ollama_settings(
            provider_context(),
            UpdateOllamaSettingsRequest {
                endpoint: "http://localhost:11434".into(),
                model: None,
                reasoning: ReasoningChoice::Default,
            },
        )
        .unwrap_err();
    assert_eq!(error.code, "ai_provider_management_unavailable");
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn ollama_endpoint_validation_is_a_public_request_error() {
    let directory = tempfile::tempdir().unwrap();
    let worker = AiWorker::new(
        WorkerConfig::production_with_credentials(
            directory.path().to_owned(),
            "internal-debug",
            Arc::new(SessionCredentialStore::default()),
            "koru-internal-debug",
        )
        .unwrap(),
    )
    .spawn()
    .unwrap();
    for endpoint in [
        "ftp://localhost:11434",
        "http://user@localhost:11434",
        "http://localhost:11434?secret=no",
        "http://localhost:11434/#fragment",
    ] {
        let error = worker
            .client()
            .update_ollama_settings(
                provider_context(),
                UpdateOllamaSettingsRequest {
                    endpoint: endpoint.into(),
                    model: None,
                    reasoning: ReasoningChoice::Default,
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid_request", "{endpoint}");
    }
    worker.shutdown();
    assert_eq!(worker.join(), WorkerExit::Clean);
}

#[test]
fn ollama_pull_that_ends_without_success_gets_a_terminal_failure() {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = socket.read(&mut request).unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let mut settings = AiSettings::unconfigured(Timestamp::now());
    settings.ollama.endpoint = url::Url::parse(&format!("http://{address}")).unwrap();
    let runtime = ProviderRuntime::new(
        Some(settings),
        Arc::new(SessionCredentialStore::default()),
        "koru-pull-eof-test",
    )
    .unwrap();
    let (events, receiver) = sync_channel(2);
    run_model_pull(
        &runtime,
        "test-model",
        Cancellation::default(),
        &events,
        &AtomicBool::new(false),
    );
    let event = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(
        matches!(event, ModelPullEvent::Failed { ref failure } if failure.code == "ai_network")
    );
    assert!(receiver.try_recv().is_err());
    server.join().unwrap();
}

#[test]
fn tool_activity_extracts_only_stable_target_ids() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let call = AiToolCall {
        call_id: "call".into(),
        name: "read_document".into(),
        arguments: json!({
            "workspace_id": workspace,
            "document_id": document,
            "query": "must not be retained"
        }),
        thought_signatures: Vec::new(),
    };

    assert_eq!(
        tool_activity_targets(&call),
        (Some(workspace), Some(document))
    );
}

#[test]
fn tool_failure_summary_is_bounded_and_public() {
    let result = AiToolResult {
        call_id: "call".into(),
        name: "edit_document".into(),
        content: json!({ "error": { "message": "x".repeat(600) } }),
        is_error: true,
    };

    let summary = tool_result_error_message(&result);
    assert_eq!(summary.chars().count(), 513);
    assert!(summary.ends_with('…'));
}

#[test]
fn a_working_folder_is_only_for_an_empty_agent_chat_and_rides_the_request() {
    use koru_ai_wire::UpdateChatFolderRequest;
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path().to_string_lossy().into_owned();
    let (captured, requests) = sync_channel(8);
    let mut providers = ProviderRegistry::default();
    providers.insert(
        AiProviderId::parse("mock").unwrap(),
        Arc::new(DeterministicMockProvider::new("Answer.")),
    );
    providers.insert(
        AiProviderId::parse("agent-codex").unwrap(),
        Arc::new(CapturingProvider { requests: captured }),
    );
    let worker = AiWorker::new(WorkerConfig::new(
        directory.path().to_owned(),
        "internal-debug",
        providers,
    ))
    .spawn()
    .unwrap();
    let client = worker.client();

    // A chat without a local agent has no folder to choose.
    let plain = create_chat(&client);
    let refused = client
        .update_chat_folder(
            context(),
            plain,
            UpdateChatFolderRequest {
                folder: Some(folder.clone()),
            },
        )
        .unwrap_err();
    assert_eq!(refused.code, "invalid_request");

    let agent = ChatId::new();
    client
        .create_chat(
            context(),
            CreateChatRequest {
                chat_id: agent,
                title: "Agent chat".into(),
                model: koru_ai_core::ModelSelection {
                    provider: AiProviderId::parse("agent-codex").unwrap(),
                    requested_model: koru_ai_core::AGENT_DEFAULT_MODEL.into(),
                    default_reasoning: ReasoningChoice::Default,
                },
            },
        )
        .unwrap();
    // A folder that is not there is refused; an existing one is kept.
    let missing = client
        .update_chat_folder(
            context(),
            agent,
            UpdateChatFolderRequest {
                folder: Some(
                    directory
                        .path()
                        .join("nowhere")
                        .to_string_lossy()
                        .into_owned(),
                ),
            },
        )
        .unwrap_err();
    assert_eq!(missing.code, "invalid_request");
    let chosen = client
        .update_chat_folder(
            context(),
            agent,
            UpdateChatFolderRequest {
                folder: Some(folder.clone()),
            },
        )
        .unwrap();
    assert_eq!(
        chosen.chat.agent_working_folder.as_deref(),
        Some(folder.as_str())
    );
    assert_eq!(
        client
            .read_chat(context(), agent)
            .unwrap()
            .chat
            .agent_working_folder
            .as_deref(),
        Some(folder.as_str())
    );

    // The first turn carries the folder to the provider, and locks it.
    let stream = send(&client, agent, Uuid::new_v4());
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        request.options.working_folder.as_deref(),
        Some(folder.as_str())
    );
    assert_eq!(
        wait_for_terminal(&client, stream.accepted.attempt_id),
        AttemptStatus::Complete
    );
    let locked = client
        .update_chat_folder(context(), agent, UpdateChatFolderRequest { folder: None })
        .unwrap_err();
    assert_eq!(locked.code, "folder_locked");

    // A branch keeps the folder its conversation happened in.
    let branch = ChatId::new();
    client
        .branch_chat(
            context(),
            agent,
            BranchChatRequest {
                branch_chat_id: branch,
                through_message_id: stream.accepted.assistant_message_id,
                title: "Branch".into(),
            },
        )
        .unwrap();
    assert_eq!(
        client
            .read_chat(context(), branch)
            .unwrap()
            .chat
            .agent_working_folder
            .as_deref(),
        Some(folder.as_str())
    );
}
