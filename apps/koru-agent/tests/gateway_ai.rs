// SPDX-License-Identifier: MPL-2.0

//! **Koru AI through a real Gateway process**, and what survives a host that
//! is killed mid-stream.
//!
//! The host is `koru-agent.exe --worker gateway`, started by
//! [`gateway_process`]. The deterministic mock provider is reached exactly as
//! it was on the retired standalone host — `--deterministic-ai-test`, which the
//! gateway *role* now accepts under `cfg!(debug_assertions)` and refuses in any
//! other build. Everything below the fixture is the suite as it stood.

mod gateway_process;

use std::path::Path;
use std::time::{Duration, Instant};

use koru_ai_core::{
    AiProviderId, AiSurface, AttemptStatus, ChatId, MessageState, ModelSelection, ReasoningChoice,
};
use koru_ai_wire::{
    CreateChatRequest, PublicAiEvent, RenameChatRequest, SendTurnRequest,
    UpdateOllamaSettingsRequest,
};
use koru_api_client::Client;
use uuid::Uuid;

const TEST_SCOPES: &[&str] = &[
    "ai.chat",
    "ai.manage_providers",
    "ai.read_usage",
    "store.read_documents",
    "store.edit_documents",
    // Named rather than spelled, because R9 splits this one and a copy that
    // spells it would go on asking for whichever half kept the old string.
    // `koru-ai-ui`'s `CLIENT_SCOPES` still spells it; it does not link this
    // crate, and moving it is that surface's packet.
    koru_api_gateway_core::MANAGE_WORKSPACES_SCOPE,
];

use gateway_process::Gateway;

fn start(root: &Path) -> (Gateway, Client) {
    start_with_ai(root, true)
}

fn start_with_ai(root: &Path, deterministic: bool) -> (Gateway, Client) {
    let extra: &[&str] = if deterministic {
        &["--deterministic-ai-test"]
    } else {
        &[]
    };
    let mut gateway = Gateway::start_with(root, extra);
    gateway.published(&gateway_process::hello("gateway-ai-integration"));
    let client = Client::open_at_with_scopes(koru_channel::segment(), root, TEST_SCOPES)
        .expect("build renewable gateway client");
    (gateway, client)
}

#[test]
fn public_client_persists_non_secret_provider_settings_through_production_gateway() {
    let root = tempfile::tempdir().unwrap();
    let (_gateway, _ordinary) = start_with_ai(root.path(), false);
    let client = Client::open_at_with_scopes(
        koru_channel::segment(),
        root.path(),
        &["ai.manage_providers"],
    )
    .unwrap();
    let saved = client
        .ai()
        .update_ollama_settings(&UpdateOllamaSettingsRequest {
            endpoint: "http://127.0.0.1:11434".into(),
            model: Some("gpt-oss:20b".into()),
            reasoning: ReasoningChoice::Level(koru_ai_core::ReasoningLevel::High),
        })
        .unwrap();
    assert_eq!(saved.model.as_deref(), Some("gpt-oss:20b"));

    let settings = client.ai().provider_settings().unwrap();
    assert_eq!(
        settings
            .selected_provider
            .as_ref()
            .map(AiProviderId::as_str),
        Some("ollama")
    );
    let serialized = serde_json::to_string(&settings).unwrap();
    assert!(!serialized.contains("api_key"));
    assert!(!serialized.contains("key_account"));
}

fn create_chat(client: &Client) -> ChatId {
    let chat_id = ChatId::new();
    let response = client
        .ai()
        .create_chat(&CreateChatRequest {
            chat_id,
            title: "Public vertical slice".into(),
            model: ModelSelection {
                provider: AiProviderId::parse("mock").unwrap(),
                requested_model: "deterministic-v1".into(),
                default_reasoning: ReasoningChoice::Default,
            },
        })
        .unwrap();
    assert_eq!(response.chat.id, chat_id);
    chat_id
}

#[test]
fn public_client_streams_and_recovers_ai_through_one_gateway_session() {
    let root = tempfile::tempdir().unwrap();
    let (_gateway, client) = start(root.path());
    assert!(client.ai().available());
    assert!(client.capabilities().iter().any(|value| value == "store"));

    let chat_id = create_chat(&client);
    let renamed = client
        .ai()
        .rename_chat(
            chat_id,
            &RenameChatRequest {
                title: "Renamed over the gateway".into(),
            },
        )
        .unwrap();
    assert_eq!(renamed.chat.title, "Renamed over the gateway");
    let operation_id = Uuid::new_v4();
    let mut stream = client
        .ai()
        .send_turn(&SendTurnRequest {
            operation_id,
            chat_id,
            surface: AiSurface::Standalone,
            markdown: "Question".into(),
            explicit_refs: Vec::new(),
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
        })
        .unwrap();
    let accepted = stream.accepted.clone();
    let mut markdown = String::new();
    let mut terminal = false;
    for event in &mut stream.events {
        let event = event.unwrap();
        match event.event {
            PublicAiEvent::TextDelta { text, .. } => markdown.push_str(&text),
            event if event.is_terminal() => {
                terminal = true;
                break;
            }
            _ => {}
        }
    }
    assert!(terminal);
    assert_eq!(markdown, "Deterministic mock response.");
    assert_eq!(
        client
            .ai()
            .read_attempt(accepted.attempt_id)
            .unwrap()
            .attempt
            .status,
        AttemptStatus::Complete
    );
    let transcript = client.ai().read_chat(chat_id).unwrap();
    assert_eq!(transcript.messages[1].markdown, markdown);
    assert_eq!(transcript.blocks[0].plain_text, markdown);
    assert!(root.path().join("ai").join("ai.db").is_file());

    let replay = client
        .ai()
        .send_turn(&SendTurnRequest {
            operation_id,
            chat_id,
            surface: AiSurface::Standalone,
            markdown: "Question".into(),
            explicit_refs: Vec::new(),
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
        })
        .unwrap();
    assert!(replay.accepted.replayed);
    assert_eq!(replay.accepted.attempt_id, accepted.attempt_id);

    let mut dropped = client
        .ai()
        .send_turn(&SendTurnRequest {
            operation_id: Uuid::new_v4(),
            chat_id,
            surface: AiSurface::Standalone,
            markdown: "Second question".into(),
            explicit_refs: Vec::new(),
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
        })
        .unwrap();
    let dropped_attempt = dropped.accepted.attempt_id;
    let dropped_assistant = dropped.accepted.assistant_message_id;
    assert!(
        dropped
            .events
            .any(|event| matches!(event.unwrap().event, PublicAiEvent::TextDelta { .. })),
        "the public stream must be active before it is interrupted"
    );
    drop(dropped);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = client
            .ai()
            .read_attempt(dropped_attempt)
            .unwrap()
            .attempt
            .status;
        if status.is_terminal() {
            assert_eq!(status, AttemptStatus::Complete);
            break;
        }
        assert!(Instant::now() < deadline, "dropped stream did not recover");
        std::thread::sleep(Duration::from_millis(5));
    }

    let transcript = client.ai().read_chat(chat_id).unwrap();
    assert_eq!(
        transcript
            .messages
            .iter()
            .filter(|message| message.id == dropped_assistant)
            .count(),
        1,
        "authoritative recovery must not duplicate the assistant message"
    );
}

#[test]
fn public_stream_disconnect_then_cancel_is_durable_and_not_duplicated() {
    let root = tempfile::tempdir().unwrap();
    let (_gateway, client) = start(root.path());
    let chat_id = create_chat(&client);
    let mut stream = client
        .ai()
        .send_turn(&SendTurnRequest {
            operation_id: Uuid::new_v4(),
            chat_id,
            surface: AiSurface::Standalone,
            markdown: "Cancel after disconnect".into(),
            explicit_refs: Vec::new(),
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
        })
        .unwrap();
    let accepted = stream.accepted.clone();
    assert!(
        stream
            .events
            .any(|event| matches!(event.unwrap().event, PublicAiEvent::TextDelta { .. }))
    );
    drop(stream);

    client.ai().cancel_attempt(accepted.attempt_id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = client
            .ai()
            .read_attempt(accepted.attempt_id)
            .unwrap()
            .attempt
            .status;
        if status.is_terminal() {
            assert_eq!(status, AttemptStatus::Cancelled);
            break;
        }
        assert!(Instant::now() < deadline, "cancel did not become durable");
        std::thread::sleep(Duration::from_millis(5));
    }

    let transcript = client.ai().read_chat(chat_id).unwrap();
    let assistants = transcript
        .messages
        .iter()
        .filter(|message| message.id == accepted.assistant_message_id)
        .collect::<Vec<_>>();
    assert_eq!(assistants.len(), 1);
    assert_eq!(assistants[0].state, MessageState::Interrupted);
}

#[test]
fn an_existing_client_recovers_interrupted_history_after_gateway_restart() {
    let root = tempfile::tempdir().unwrap();
    let (gateway, client) = start(root.path());
    let chat_id = create_chat(&client);
    let mut stream = client
        .ai()
        .send_turn(&SendTurnRequest {
            operation_id: Uuid::new_v4(),
            chat_id,
            surface: AiSurface::Standalone,
            markdown: "Restart the gateway".into(),
            explicit_refs: Vec::new(),
            web_search: false,
            reasoning_override: None,
            system_prompt_override: None,
            replace_pinned_documents: false,
        })
        .unwrap();
    let accepted = stream.accepted.clone();
    assert!(
        stream
            .events
            .any(|event| matches!(event.unwrap().event, PublicAiEvent::TextDelta { .. }))
    );
    gateway.terminate();
    drop(stream);

    let (_restarted, _fresh_client) = start(root.path());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match client.ai().read_attempt(accepted.attempt_id) {
            Ok(response) if response.attempt.status.is_terminal() => {
                assert_eq!(response.attempt.status, AttemptStatus::Interrupted);
                break;
            }
            Ok(_) | Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(response) => panic!(
                "restarted gateway left attempt non-terminal: {:?}",
                response.attempt.status
            ),
            Err(error) => panic!("existing client did not rediscover gateway: {error:?}"),
        }
    }

    let transcript = client.ai().read_chat(chat_id).unwrap();
    let assistants = transcript
        .messages
        .iter()
        .filter(|message| message.id == accepted.assistant_message_id)
        .collect::<Vec<_>>();
    assert_eq!(assistants.len(), 1);
    assert_eq!(assistants[0].state, MessageState::Interrupted);
}

#[test]
fn gateway_rejects_ai_before_the_worker_when_scope_is_absent() {
    let root = tempfile::tempdir().unwrap();
    let (_gateway, _ordinary) = start(root.path());
    let client = Client::open_at_with_scopes(
        koru_channel::segment(),
        root.path(),
        &["store.read_documents"],
    )
    .unwrap();
    let error = client
        .ai()
        .create_chat(&CreateChatRequest {
            chat_id: ChatId::new(),
            title: "Denied".into(),
            model: ModelSelection {
                provider: AiProviderId::parse("mock").unwrap(),
                requested_model: "deterministic-v1".into(),
                default_reasoning: ReasoningChoice::Default,
            },
        })
        .unwrap_err();
    assert_eq!(error.code, "forbidden");
}

/// **Every in-flight stream reaches exactly one terminal outcome when the host
/// dies**, not just the one the test happened to be watching.
///
/// `an_existing_client_recovers_interrupted_history_after_gateway_restart`
/// proves the single-stream case. This one is the property a rehost could
/// break: recovery is per attempt, so a host whose replacement finalised the
/// attempt it found first — or finalised one twice — would still pass that
/// test. Two chats stream at once, the host is killed mid-flight, and each
/// attempt is required to end terminal, once, with one assistant message.
#[test]
fn every_in_flight_stream_reaches_one_terminal_outcome_when_the_host_dies() {
    let root = tempfile::tempdir().unwrap();
    let (gateway, client) = start(root.path());
    let chats = [create_chat(&client), create_chat(&client)];
    // Both turns are sent before either is waited on. The deterministic
    // provider's whole answer is 28 characters at 20 ms, so a stream is
    // in flight for about half a second; sending the second turn only after
    // the first one's text arrived spent that half second on a loaded CI
    // runner, and the first attempt was Complete before the kill.
    let mut streams = chats
        .iter()
        .map(|&chat_id| {
            client
                .ai()
                .send_turn(&SendTurnRequest {
                    operation_id: Uuid::new_v4(),
                    chat_id,
                    surface: AiSurface::Standalone,
                    markdown: "Two streams, one kill".into(),
                    explicit_refs: Vec::new(),
                    web_search: false,
                    reasoning_override: None,
                    system_prompt_override: None,
                    replace_pinned_documents: false,
                })
                .unwrap()
        })
        .collect::<Vec<_>>();
    let mut accepted = Vec::new();
    for (chat_id, stream) in chats.iter().zip(&mut streams) {
        assert!(
            stream
                .events
                .any(|event| matches!(event.unwrap().event, PublicAiEvent::TextDelta { .. })),
            "a stream must be live before the host is killed"
        );
        accepted.push((*chat_id, stream.accepted.clone()));
    }

    gateway.terminate();
    drop(streams);

    let (_restarted, _fresh) = start(root.path());
    for (chat_id, accepted) in &accepted {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match client.ai().read_attempt(accepted.attempt_id) {
                Ok(response) if response.attempt.status.is_terminal() => {
                    assert_eq!(response.attempt.status, AttemptStatus::Interrupted);
                    break;
                }
                Ok(_) | Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(response) => panic!(
                    "the replacement left an attempt non-terminal: {:?}",
                    response.attempt.status
                ),
                Err(error) => panic!("the existing client did not rediscover: {error:?}"),
            }
        }
        let transcript = client.ai().read_chat(*chat_id).unwrap();
        let assistants = transcript
            .messages
            .iter()
            .filter(|message| message.id == accepted.assistant_message_id)
            .collect::<Vec<_>>();
        assert_eq!(
            assistants.len(),
            1,
            "recovery duplicated an assistant message"
        );
        assert_eq!(assistants[0].state, MessageState::Interrupted);
    }
}
