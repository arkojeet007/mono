// SPDX-License-Identifier: MPL-2.0

//! **The sessions role's contract, over real IPC to a real child.**
//!
//! Every case here starts `koru-agent.exe --worker sessions --root <root>` and
//! drives it through `koru-session-client` exactly as a window would. The role
//! calls straight into `koru_session_host::run_supervised`, which is the same
//! composition the retired `koru-session-host.exe` ran, so nothing about the
//! protocol, the bootstrap or the sweep changed when the host did — only which
//! image is on the other end of the pipe.
//!
//! This file lives in `apps/koru-agent` because `CARGO_BIN_EXE_*` is set only
//! for binaries the *testing package itself* declares, and `koru-agent` is the
//! only package that declares an image hosting this role.
//!
//! Every child runs on its own temporary `--root`, and endpoint names carry a
//! digest of the root, so these hosts never meet each other or the sessions
//! host a developer has open.

use std::collections::BTreeMap;
use std::io::{Cursor, Write as _};
// `Path` is the npm-shim fixture's, which only Windows builds.
#[cfg_attr(not(windows), allow(unused_imports))]
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::{Duration, Instant};

use koru_session_client::{ClientError, SessionClient, Subscription};
use koru_session_protocol::{
    AttachmentDeliveryPolicy, AttachmentId, AttachmentSource, AttachmentStatus, BuiltInProfileKey,
    CapabilityName, CapabilitySet, DeliveryMode, DeliveryState, ErrorCode, GridSize,
    IdempotencyKey, PendingInput, ProfileKind, ProtocolOffer, ProtocolRange, RawCommand,
    RenderUpdate, RowIndex, RowRange, SessionCommand, SessionId, SessionMode, SessionProfileKey,
    ShellQuoting, ViewportGeneration,
};
use koru_session_wire::{
    CreateSession, EnvironmentSpec, LaunchBootstrap, OutputUpdate, SubscriptionUpdate,
};

mod sessions_harness;
use sessions_harness::{Host, fixture_codex, wait_until};

#[cfg(windows)]
fn fixture_npm_shim(root: &Path, program: &str, marker: &str) -> PathBuf {
    let destination = root.join(format!("{program}-fixture-bin"));
    std::fs::create_dir_all(&destination).unwrap();
    // npm installs an extensionless Unix shim beside the Windows command
    // shim. The resolver must choose PATHEXT and the host must execute the
    // resulting batch file through cmd.exe.
    // Keep it alive until the test explicitly terminates it: a fixed sleep
    // makes cleanup race the registry's ended-session sweep under load.
    std::fs::write(destination.join(program), "not a Windows executable").unwrap();
    // PowerShell by absolute path, not PATH: under a full `cargo test
    // --workspace` the harness prepends every native build-script directory
    // (the voice engines alone add ~13,000 characters), cmd.exe stops
    // searching past its 8191-character expansion limit, and a bare
    // `powershell.exe` is no longer found — the shim then exits after its
    // echo, the host reaps the session, and Terminate finds nothing. Same
    // fix and reason as the managed-codex fixture.
    let powershell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    std::fs::write(
        destination.join(format!("{program}.cmd")),
        format!(
            "@echo off\r\necho {marker}\r\n\"{powershell}\" -NoLogo -NoProfile -NonInteractive -Command \"Wait-Event\"\r\n"
        ),
    )
    .unwrap();
    destination
}

fn create(root: &std::path::Path, marker: &str) -> CreateSession {
    #[cfg(windows)]
    let argv = vec![
        "powershell.exe".into(),
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        format!("Write-Output '{marker}'; Start-Sleep -Seconds 30"),
    ];
    #[cfg(unix)]
    let argv = vec![
        "/bin/sh".into(),
        "-c".into(),
        format!("printf '{marker}\\n'; sleep 30"),
    ];
    CreateSession {
        profile: SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("platform-shell").unwrap()),
        parent_session_id: None,
        profile_kind: ProfileKind::Shell,
        display_name: marker.into(),
        working_directory: root.to_path_buf(),
        mode: SessionMode::RawPty,
        attachment_delivery: koru_session_protocol::AttachmentDeliveryPolicy::Unsupported,
        capabilities: CapabilitySet::default(),
        project_id: None,
        argv,
        environment: EnvironmentSpec::Inherit,
        size: GridSize::new(80, 24).unwrap(),
    }
}

fn wait_for_marker(subscription: &Subscription, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frames = 0;
    let mut last = None;
    while Instant::now() < deadline {
        match subscription.recv_timeout(Duration::from_millis(250)) {
            Ok(SubscriptionUpdate::Render { update }) => {
                let text = render_text(&update);
                if text.contains(marker) {
                    return;
                }
                frames += 1;
                last = Some(text);
            }
            Ok(other) => last = Some(format!("{other:?}")),
            Err(_) => {}
        }
    }
    panic!("render stream never contained {marker} ({frames} frames; last: {last:?})");
}

fn wait_for_output(subscription: &Subscription, expected: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut bytes = Vec::new();
    while Instant::now() < deadline {
        if let Ok(SubscriptionUpdate::Output {
            update: OutputUpdate::Chunk { chunk },
        }) = subscription.recv_timeout(Duration::from_millis(250))
        {
            bytes.extend_from_slice(&chunk.bytes);
            if bytes
                .windows(expected.len())
                .any(|window| window == expected)
            {
                return;
            }
        }
    }
    panic!("raw output never contained the expected bytes");
}

fn render_text(update: &RenderUpdate) -> String {
    let rows = match update {
        RenderUpdate::Snapshot { snapshot } => &snapshot.rows,
        RenderUpdate::Delta { delta } => &delta.rows,
        RenderUpdate::Gap { .. } => return String::new(),
    };
    rows.iter()
        .flat_map(|row| row.cells.iter())
        .map(|cell| cell.text.as_str())
        .collect()
}

fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbaImage::from_pixel(width, height, image::Rgba([20, 60, 100, 255]));
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

#[test]
fn rooted_clients_share_sessions_reconnect_and_cleanup_without_a_gateway() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let ui = host.client("koru-workspace-test");
    let gateway = host.client("koru-api-gateway-test");

    let first = ui.create(create(root.path(), "KORU:ONE")).unwrap();
    let second = gateway.create(create(root.path(), "KORU:TWO")).unwrap();
    assert_eq!(ui.list().unwrap().len(), 2);
    assert_eq!(gateway.list().unwrap().len(), 2);

    let (_first_lease, first_render) = ui
        .attach_live(first.id, ViewportGeneration::new(1).unwrap(), 80, 2)
        .unwrap();
    wait_for_marker(&first_render, "KORU:ONE");
    let conflict = match gateway.attach_live(first.id, ViewportGeneration::new(1).unwrap(), 80, 2) {
        Ok(_) => panic!("a second controller was accepted"),
        Err(error) => error,
    };
    assert!(matches!(
        conflict,
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ControllerConflict,
            ..
        })
    ));

    drop(first_render);
    drop(ui);
    let replacement = host.client("koru-workspace-test");
    let (replacement_lease, replacement_render) = loop {
        // A replacement window may be wider than the detached PTY. Attach is
        // still required to succeed; its first snapshot is clamped to the
        // current grid and the new controller resizes through the normal path.
        match replacement.attach_live(first.id, ViewportGeneration::new(2).unwrap(), 180, 2) {
            Ok(attached) => break attached,
            Err(ClientError::Session(koru_session_protocol::SessionFailure {
                code: ErrorCode::ControllerConflict,
                ..
            })) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => panic!("reattach failed: {error}"),
        }
    };
    wait_for_marker(&replacement_render, "KORU:ONE");
    replacement
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: first.id,
            controller_lease: replacement_lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();

    let (second_lease, _) = gateway
        .attach_live(second.id, ViewportGeneration::new(1).unwrap(), 80, 0)
        .unwrap();
    gateway
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: second.id,
            controller_lease: second_lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    wait_until(|| replacement.list().unwrap().is_empty());
    host.stop();
}

#[test]
fn profile_creation_replays_once_and_observers_need_no_controller_lease() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-api-gateway-test");
    let profile =
        SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("platform-shell").unwrap());
    let key = IdempotencyKey::mint();
    let first = client
        .create_profile(
            key,
            profile.clone(),
            None,
            root.path().to_path_buf(),
            GridSize::new(80, 24).unwrap(),
        )
        .unwrap();
    let replayed = client
        .create_profile(
            key,
            profile.clone(),
            None,
            root.path().to_path_buf(),
            GridSize::new(80, 24).unwrap(),
        )
        .unwrap();
    assert_eq!(replayed.id, first.id);

    let conflict = client
        .create_profile(
            key,
            profile,
            None,
            root.path().to_path_buf(),
            GridSize::new(81, 24).unwrap(),
        )
        .unwrap_err();
    assert!(matches!(
        conflict,
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::IdempotencyConflict,
            ..
        })
    ));

    let snapshot = client
        .snapshot(
            first.id,
            ViewportGeneration::new(1).unwrap(),
            RowRange {
                first: RowIndex(0),
                count: 1,
            },
            80,
        )
        .unwrap();
    assert_eq!(snapshot.session_id, first.id);

    let (lease, _) = client
        .attach_live(first.id, ViewportGeneration::new(2).unwrap(), 80, 0)
        .unwrap();
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: first.id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    host.stop();
}

#[test]
fn attachment_ingest_preview_remove_and_termination_share_one_rooted_lifecycle() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-workspace-test");
    let session = client
        .create(create(root.path(), "KORU:ATTACHMENTS"))
        .unwrap();
    let (lease, _render) = client
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();

    let attachment_id = AttachmentId::mint();
    let attachment = client
        .ingest_attachment(
            session.id,
            lease.token,
            attachment_id,
            AttachmentSource::Clipboard,
            &png(64, 32),
        )
        .unwrap();
    assert_eq!(attachment.status, AttachmentStatus::Pending);
    assert_eq!(client.attachments(session.id).unwrap(), [attachment]);
    assert_eq!(
        client
            .list()
            .unwrap()
            .into_iter()
            .find(|summary| summary.id == session.id)
            .unwrap()
            .pending_attachment_count,
        1
    );
    let preview = client
        .attachment_preview(session.id, attachment_id, 24)
        .unwrap();
    assert!(preview.dimensions.width <= 24 && preview.dimensions.height <= 24);
    assert_eq!(preview.media_type, "image/png");
    assert_eq!(
        image::guess_format(&preview.bytes).unwrap(),
        image::ImageFormat::Png
    );

    let corrupt = client
        .ingest_attachment(
            session.id,
            lease.token,
            AttachmentId::mint(),
            AttachmentSource::Drop,
            b"corrupt",
        )
        .unwrap_err();
    assert!(matches!(
        corrupt,
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::InvalidImage,
            ..
        })
    ));
    assert_eq!(client.attachments(session.id).unwrap().len(), 1);

    let removed = client
        .remove_attachment(session.id, lease.token, attachment_id)
        .unwrap();
    assert_eq!(removed.status, AttachmentStatus::Removed);
    assert!(client.attachments(session.id).unwrap().is_empty());

    let final_id = AttachmentId::mint();
    client
        .ingest_attachment(
            session.id,
            lease.token,
            final_id,
            AttachmentSource::FilePicker,
            &png(8, 8),
        )
        .unwrap();
    let session_directory = root
        .path()
        .join("agent/sessions/attachments")
        .join(session.id.to_string());
    assert!(session_directory.is_dir());
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    wait_until(|| !session_directory.exists());
    host.stop();
}

#[test]
fn pending_input_uses_the_host_rooted_path_once() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-workspace");
    let mut request = create(root.path(), "KORU:DELIVERY");
    #[cfg(windows)]
    {
        request.argv = vec!["cmd.exe".into(), "/Q".into(), "/D".into(), "/K".into()];
        request.attachment_delivery = AttachmentDeliveryPolicy::QuotedPath {
            shell: ShellQuoting::PowerShell,
        };
    }
    #[cfg(unix)]
    {
        request.argv = vec!["/bin/bash".into(), "--noprofile".into(), "--norc".into()];
        request.attachment_delivery = AttachmentDeliveryPolicy::QuotedPath {
            shell: ShellQuoting::Posix,
        };
    }
    request.capabilities = koru_session_host::protocol_offer().capabilities;
    let session = client.create(request).unwrap();
    let (lease, _render) = client
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 2)
        .unwrap();
    let output = client.subscribe_output(session.id).unwrap();
    let attachment_id = AttachmentId::mint();
    let attachment = client
        .ingest_attachment(
            session.id,
            lease.token,
            attachment_id,
            AttachmentSource::Drop,
            &png(3, 2),
        )
        .unwrap();
    assert_eq!(attachment.delivery_mode, DeliveryMode::QuotedPath);
    let input = PendingInput {
        idempotency_key: IdempotencyKey::mint(),
        #[cfg(windows)]
        text: "echo".into(),
        #[cfg(unix)]
        text: "printf '%s\\n'".into(),
        attachment_ids: vec![attachment_id],
    };
    let sent = client
        .send_pending_input(session.id, lease.token, input.clone())
        .unwrap();
    assert_eq!(sent.attachments.len(), 1);
    assert!(matches!(
        sent.attachments[0].state,
        DeliveryState::Delivered { .. }
    ));
    wait_for_output(&output, attachment_id.to_string().as_bytes());

    let retried = client
        .send_pending_input(session.id, lease.token, input)
        .unwrap();
    assert_eq!(retried.turn_id, sent.turn_id);
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    host.stop();
}

#[cfg(windows)]
#[test]
fn raw_profile_launch_uses_the_same_windows_shim_as_presence_probe() {
    let root = tempfile::tempdir().unwrap();
    let provider_path = fixture_npm_shim(root.path(), "opencode", "KORU:OPENCODE");
    let host = Host::start_with_provider_path(root.path(), Some(&provider_path));
    let client = host.client("koru-workspace-profile-test");
    let session = client
        .create_profile_mode(
            IdempotencyKey::mint(),
            SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("opencode").unwrap()),
            None,
            SessionMode::RawPty,
            root.path().to_path_buf(),
            GridSize::new(80, 24).unwrap(),
        )
        .unwrap();
    let (lease, render) = client
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    wait_for_marker(&render, "KORU:OPENCODE");
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    host.stop();
}

#[cfg(windows)]
#[test]
fn codex_cli_is_a_raw_pty_over_the_windows_npm_command_shim() {
    let root = tempfile::tempdir().unwrap();
    let provider_path = fixture_npm_shim(root.path(), "codex", "KORU:CODEX-CLI");
    let host = Host::start_with_provider_path(root.path(), Some(&provider_path));
    let client = host.client("koru-workspace-codex-cli-test");
    let session = client
        .create_profile_mode(
            IdempotencyKey::mint(),
            SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("codex-cli").unwrap()),
            None,
            SessionMode::RawPty,
            root.path().to_path_buf(),
            GridSize::new(80, 24).unwrap(),
        )
        .unwrap();
    assert_eq!(session.mode, SessionMode::RawPty);
    let (lease, render) = client
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    wait_for_marker(&render, "KORU:CODEX-CLI");
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    host.stop();
}

#[cfg(windows)]
#[test]
fn built_in_powershell_accepts_input_through_conpty() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-workspace-powershell-profile-test");
    let session = client
        .create_profile_mode(
            IdempotencyKey::mint(),
            SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("powershell").unwrap()),
            None,
            SessionMode::RawPty,
            root.path().to_path_buf(),
            GridSize::new(80, 24).unwrap(),
        )
        .unwrap();
    let (lease, render) = client
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    let output = client.subscribe_output(session.id).unwrap();
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::WriteText {
                text: "Write-Output KORU:POWERSHELL\r".into(),
            },
        })
        .unwrap();
    wait_for_output(&output, b"KORU:POWERSHELL");
    wait_for_marker(&render, "KORU:POWERSHELL");
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    host.stop();
}

#[test]
fn managed_codex_crosses_real_ipc_for_image_events_approval_cancel_and_reattach() {
    use koru_session_protocol::{
        AgentEvent, ApprovalDecision, DeliveryState, ProviderKind, capability,
    };

    let root = tempfile::tempdir().unwrap();
    let provider_path = fixture_codex(root.path());
    let host = Host::start_with_provider_path(root.path(), Some(&provider_path));
    let client = host.client("koru-workspace-managed-test");
    let provider = ProviderKind::from_str("codex").unwrap();
    let session = client
        .create_profile_mode(
            IdempotencyKey::mint(),
            SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("codex").unwrap()),
            None,
            SessionMode::Managed {
                provider: provider.clone(),
            },
            root.path().to_path_buf(),
            GridSize::new(80, 24).unwrap(),
        )
        .unwrap();
    assert_eq!(session.mode, SessionMode::Managed { provider });
    assert!(session.capabilities.supports(capability::MANAGED_EVENTS));
    assert!(
        session
            .capabilities
            .supports(capability::NATIVE_ATTACHMENT_DELIVERY)
    );

    let (lease, render) = client
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    let events = client
        .subscribe_agent_events_from(session.id, None)
        .unwrap();
    let attachment_id = AttachmentId::mint();
    let attachment = client
        .ingest_attachment(
            session.id,
            lease.token,
            attachment_id,
            AttachmentSource::Clipboard,
            &png(3, 2),
        )
        .unwrap();
    assert_eq!(attachment.delivery_mode, DeliveryMode::NativeStructured);
    let submitted = client
        .send_pending_input(
            session.id,
            lease.token,
            PendingInput {
                idempotency_key: IdempotencyKey::mint(),
                text: "inspect the fixture image".into(),
                attachment_ids: vec![attachment_id],
            },
        )
        .unwrap();
    assert_eq!(submitted.attachments.len(), 1);
    assert!(matches!(
        submitted.attachments[0].state,
        DeliveryState::Delivering { .. } | DeliveryState::Delivered { .. }
    ));

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_text = false;
    let mut saw_image = false;
    let mut saw_approval = false;
    let mut saw_resolution = false;
    while Instant::now() < deadline && !(saw_text && saw_image && saw_resolution) {
        let Ok(SubscriptionUpdate::AgentEvent { event }) =
            events.recv_timeout(Duration::from_millis(250))
        else {
            continue;
        };
        match event.event {
            AgentEvent::MessageDelta { text, .. } => saw_text |= text == "FIXTURE_TEXT",
            AgentEvent::AttachmentAccepted {
                attachment_id: accepted,
                ..
            } => saw_image |= accepted == attachment_id,
            AgentEvent::ApprovalRequired { approval_id, .. } => {
                saw_approval = true;
                wait_until(|| {
                    client.list().is_ok_and(|sessions| {
                        sessions.iter().any(|current| {
                            current.id == session.id
                                && current.attention.requires_user
                                && current.attention.unread_count > 0
                        })
                    })
                });
                client
                    .acknowledge_attention(session.id, lease.token, event.sequence)
                    .unwrap();
                wait_until(|| {
                    client.list().is_ok_and(|sessions| {
                        sessions.iter().any(|current| {
                            current.id == session.id && !current.attention.requires_user
                        })
                    })
                });
                client
                    .answer_approval(
                        session.id,
                        lease.token,
                        IdempotencyKey::mint(),
                        approval_id,
                        ApprovalDecision::Approve,
                        None,
                    )
                    .unwrap();
            }
            AgentEvent::ApprovalResolved { .. } => saw_resolution = true,
            _ => {}
        }
    }
    assert!(saw_text && saw_image && saw_approval && saw_resolution);
    wait_until(|| {
        client.attachments(session.id).is_ok_and(|attachments| {
            attachments.iter().any(|attachment| {
                attachment.id == attachment_id
                    && matches!(attachment.delivery, DeliveryState::Delivered { .. })
            })
        })
    });

    drop(render);
    drop(events);
    client.detach(session.id, lease.token).unwrap();
    let (lease, render) = client
        .attach_live(session.id, ViewportGeneration::new(2).unwrap(), 80, 1)
        .unwrap();
    let observation = client.observe_agent_events(session.id).unwrap();
    assert!(!observation.events.is_empty());
    assert_eq!(
        observation.cursor,
        client.observe_agent_events(session.id).unwrap().cursor
    );
    client
        .cancel_turn(
            session.id,
            lease.token,
            IdempotencyKey::mint(),
            submitted.turn_id,
        )
        .unwrap();

    let replay = client
        .subscribe_agent_events_from(session.id, Some(observation.cursor))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_cancelled = false;
    while Instant::now() < deadline {
        match replay.recv_timeout(Duration::from_millis(250)) {
            Ok(SubscriptionUpdate::AgentEvent { event })
                if matches!(event.event, AgentEvent::TurnCancelled { .. }) =>
            {
                saw_cancelled = true;
                break;
            }
            Ok(SubscriptionUpdate::AgentEventGap { expected, actual }) => {
                panic!("short managed replay gapped: expected {expected:?}, actual {actual:?}")
            }
            _ => {}
        }
    }
    assert!(
        saw_cancelled,
        "reattached client did not replay cancellation"
    );

    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    drop(render);
    drop(replay);
    let session_directory = root
        .path()
        .join("agent/sessions/attachments")
        .join(session.id.to_string());
    wait_until(|| !session_directory.exists());
    host.stop();
}

#[test]
fn attachment_commands_require_protocol_four_and_the_negotiated_capability() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let legacy = SessionClient::connect(
        &host.endpoint,
        "legacy-terminal",
        ProtocolOffer {
            versions: ProtocolRange { min: 1, max: 3 },
            capabilities: CapabilitySet::default(),
        },
    )
    .unwrap();
    let incompatible = legacy.attachments(SessionId::mint()).unwrap_err();
    assert!(matches!(
        incompatible,
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProtocolIncompatible,
            ..
        })
    ));

    let no_capability = SessionClient::connect(
        &host.endpoint,
        "v4-without-attachments",
        ProtocolOffer {
            versions: ProtocolRange { min: 4, max: 4 },
            capabilities: CapabilitySet::new(["controller-leases"
                .parse::<CapabilityName>()
                .unwrap()]),
        },
    )
    .unwrap();
    let refused = no_capability.attachments(SessionId::mint()).unwrap_err();
    assert!(matches!(
        refused,
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        })
    ));

    let v4 = SessionClient::connect(
        &host.endpoint,
        "v4-terminal",
        ProtocolOffer {
            versions: ProtocolRange { min: 4, max: 4 },
            capabilities: CapabilitySet::new(
                ["controller-leases", "attachments"]
                    .into_iter()
                    .map(|name| name.parse::<CapabilityName>().unwrap()),
            ),
        },
    )
    .unwrap();
    let session = v4.create(create(root.path(), "KORU:V4")).unwrap();
    let (lease, _render) = v4
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    let error = v4
        .send_pending_input(
            session.id,
            lease.token,
            PendingInput {
                idempotency_key: IdempotencyKey::mint(),
                text: "echo legacy".into(),
                attachment_ids: Vec::new(),
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProtocolIncompatible,
            ..
        })
    ));
    v4.command(SessionCommand {
        idempotency_key: IdempotencyKey::mint(),
        session_id: session.id,
        controller_lease: lease.token,
        action: RawCommand::Terminate,
    })
    .unwrap();
    host.stop();
}

#[test]
fn shutdown_consent_compares_the_catalog_and_freezes_new_sessions() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-agent-supervisor");
    let observed = client.catalog().unwrap();
    assert!(observed.sessions.is_empty());

    client.create(create(root.path(), "KORU:ACTIVE")).unwrap();
    let stale = client
        .prepare_shutdown(host.authority.clone(), observed.generation)
        .unwrap_err();
    assert!(matches!(
        stale,
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::StaleGeneration,
            ..
        })
    ));

    let current = client.catalog().unwrap();
    assert_eq!(current.sessions.len(), 1);
    let prepared = client
        .prepare_shutdown(host.authority.clone(), current.generation)
        .unwrap();
    assert_eq!(prepared.generation, current.generation);
    assert_eq!(prepared.sessions.len(), 1);
    assert!(matches!(
        client.create(create(root.path(), "KORU:REFUSED")),
        Err(ClientError::Session(
            koru_session_protocol::SessionFailure {
                code: ErrorCode::HostUnavailable,
                ..
            }
        ))
    ));

    client.cancel_shutdown(host.authority.clone()).unwrap();
    client.create(create(root.path(), "KORU:REOPENED")).unwrap();
    host.stop();
}

#[test]
fn incompatible_protocols_fail_with_the_stable_code() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let error = match SessionClient::connect(
        &host.endpoint,
        "future-client",
        ProtocolOffer {
            versions: ProtocolRange {
                min: koru_session_protocol::MAX_PROTOCOL_VERSION + 1,
                max: koru_session_protocol::MAX_PROTOCOL_VERSION + 1,
            },
            capabilities: CapabilitySet::default(),
        },
    ) {
        Ok(_) => panic!("an incompatible protocol was accepted"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        ClientError::Protocol(koru_session_protocol::ProtocolRefusal {
            error: koru_session_protocol::SessionFailure {
                code: ErrorCode::ProtocolIncompatible,
                ..
            },
            ..
        })
    ));
    host.stop();
}

#[test]
fn version_one_connections_cannot_send_version_two_input_commands() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = SessionClient::connect(
        &host.endpoint,
        "legacy-terminal",
        ProtocolOffer {
            versions: ProtocolRange { min: 1, max: 1 },
            capabilities: CapabilitySet::default(),
        },
    )
    .unwrap();
    assert_eq!(client.host().agreement.version, 1);
    assert!(
        client.list().unwrap().is_empty(),
        "the v1 ListSessions response shape remains decodable"
    );
    assert!(matches!(
        client.catalog().unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProtocolIncompatible,
            ..
        })
    ));
    let session = client.create(create(root.path(), "KORU:LEGACY")).unwrap();
    let (lease, _render) = client
        .attach_live(session.id, ViewportGeneration::new(1).unwrap(), 80, 1)
        .unwrap();
    let error = client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::SetFocus { focused: true },
        })
        .unwrap_err();
    assert!(matches!(
        error,
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProtocolIncompatible,
            ..
        })
    ));
    client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: session.id,
            controller_lease: lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    host.stop();
}

#[test]
fn an_out_of_root_bootstrap_exits_without_touching_the_foreign_directory() {
    let root = tempfile::tempdir().unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let sentinel = foreign.path().join("sentinel.txt");
    std::fs::write(&sentinel, b"owned elsewhere").unwrap();
    let bootstrap = LaunchBootstrap::new(
        uuid::Uuid::new_v4().to_string(),
        koru_channel::segment().into(),
        root.path().to_path_buf(),
        foreign.path().join("sessions"),
        root.path().join("store"),
        true,
        uuid::Uuid::new_v4().to_string(),
    );
    let mut child = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_koru-agent")))
        .args(["--worker", "sessions", "--root"])
        .arg(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut bootstrap_pipe = child.stdin.take().unwrap();
    bootstrap_pipe
        .write_all(&serde_json::to_vec(&bootstrap).unwrap())
        .unwrap();
    bootstrap_pipe.write_all(b"\n").unwrap();
    bootstrap_pipe.flush().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success());
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"owned elsewhere");
    assert!(!foreign.path().join("sessions").exists());
    let endpoint = koru_ipc::Endpoint::for_service_channel_root(
        koru_ipc::SESSION_HOST,
        koru_channel::segment(),
        root.path(),
    )
    .unwrap();
    assert!(!koru_ipc::probe(&endpoint));
}

#[test]
fn startup_sweeps_only_after_the_root_and_endpoint_are_owned() {
    let root = tempfile::tempdir().unwrap();
    let abandoned = root.path().join("agent/sessions/attachments/abandoned");
    std::fs::create_dir_all(&abandoned).unwrap();
    std::fs::write(abandoned.join("stale.part"), b"incomplete").unwrap();
    let sentinel = root.path().join("sentinel");
    std::fs::write(&sentinel, b"outside attachment storage").unwrap();

    let host = Host::start(root.path());
    assert!(!abandoned.exists());
    assert_eq!(
        std::fs::read(&sentinel).unwrap(),
        b"outside attachment storage"
    );
    host.stop();
}

#[test]
fn an_invalid_bootstrap_reports_a_stable_diagnostic_and_skips_the_sweep() {
    let root = tempfile::tempdir().unwrap();
    let abandoned = root.path().join("agent/sessions/attachments/abandoned");
    std::fs::create_dir_all(&abandoned).unwrap();
    let sentinel = abandoned.join("must-survive");
    std::fs::write(&sentinel, b"not owned").unwrap();
    std::fs::create_dir_all(root.path().join("store")).unwrap();
    let bootstrap = LaunchBootstrap::new(
        "invalid-launch-id".into(),
        koru_channel::segment().into(),
        root.path().to_path_buf(),
        root.path().join("agent/sessions"),
        root.path().join("store"),
        true,
        uuid::Uuid::new_v4().to_string(),
    );
    let mut child = Command::new(PathBuf::from(env!("CARGO_BIN_EXE_koru-agent")))
        .args(["--worker", "sessions", "--root"])
        .arg(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input
        .write_all(&serde_json::to_vec(&bootstrap).unwrap())
        .unwrap();
    input.write_all(b"\n").unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("koru-agent --worker sessions: invalid Agent bootstrap")
    );
    assert_eq!(std::fs::read(sentinel).unwrap(), b"not owned");
}

#[test]
fn losing_the_agent_bootstrap_pipe_stops_the_host_without_polling() {
    let root = tempfile::tempdir().unwrap();
    let mut host = Host::start(root.path());
    host.bootstrap_pipe.take();
    wait_until(|| host.child.try_wait().unwrap().is_some());
    assert!(!koru_ipc::probe(&host.endpoint));
}

/// Block until the catalog subscription says something changed. Nothing here
/// polls the registry: the point of the stream is that a second window learns
/// of a mutation without asking.
fn wait_for_invalidation(subscription: &Subscription) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match subscription.recv_timeout(Duration::from_millis(250)) {
            Ok(SubscriptionUpdate::Catalog {
                event: koru_session_protocol::CatalogEvent::ProjectsChanged,
            }) => return,
            Ok(SubscriptionUpdate::Closed { error }) => {
                panic!("the catalog subscription closed: {error:?}")
            }
            Ok(other) => panic!("the catalog stream carried {other:?}"),
            Err(ClientError::TimedOut) => {}
            Err(error) => panic!("catalog subscription failed: {error}"),
        }
    }
    panic!("no project invalidation arrived");
}

fn scope() -> koru_session_protocol::WorkspaceScope {
    koru_session_protocol::WorkspaceScope {
        profile: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap(),
        workspace: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".parse().unwrap(),
    }
}

/// A second workspace under the same profile, so the join is tested against a
/// near miss rather than only against nothing.
fn other_scope() -> koru_session_protocol::WorkspaceScope {
    koru_session_protocol::WorkspaceScope {
        profile: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".parse().unwrap(),
        workspace: "cccccccc-cccc-4ccc-8ccc-cccccccccccc".parse().unwrap(),
    }
}

/// **The supervised lifecycle verb, end to end**: the join it resolves, the
/// sessions it leaves alone, and that a retry closes nothing.
///
/// The join is the one nobody had written down — a session carries a project,
/// a project carries an optional workspace scope, and nothing carries the
/// workspace — so this exercises all four populations at once: a session in a
/// project scoped to this workspace, one in a project scoped to a *different*
/// workspace under the same profile, one in a project with no scope, and one
/// in no project at all. Only the first is closed.
#[test]
fn removing_a_workspace_closes_only_the_sessions_its_projects_hold() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-workspace-test");
    let gateway = host.client("koru-api-gateway");

    let mine = koru_session_protocol::ProjectId::mint();
    let neighbour = koru_session_protocol::ProjectId::mint();
    let unscoped = koru_session_protocol::ProjectId::mint();
    client
        .create_project(mine, "Mine", None, Some(scope()))
        .unwrap();
    client
        .create_project(neighbour, "Neighbour", None, Some(other_scope()))
        .unwrap();
    client
        .create_project(unscoped, "Everywhere", None, None)
        .unwrap();

    let mut placed = create(root.path(), "KORU:DOOMED");
    placed.project_id = Some(mine);
    let doomed = client.create(placed).unwrap();
    let mut next_door = create(root.path(), "KORU:NEIGHBOUR");
    next_door.project_id = Some(neighbour);
    let spared_neighbour = client.create(next_door).unwrap();
    let mut nowhere = create(root.path(), "KORU:UNSCOPED");
    nowhere.project_id = Some(unscoped);
    let spared_unscoped = client.create(nowhere).unwrap();
    let spared_unplaced = client.create(create(root.path(), "KORU:UNPLACED")).unwrap();

    let terminated = gateway
        .terminate_workspace_sessions(host.lifecycle_authority.clone(), scope())
        .unwrap();
    assert_eq!(
        terminated,
        vec![doomed.id],
        "the operation closed something other than the workspace's own session"
    );
    wait_until(|| {
        client.list().unwrap().iter().all(|session| {
            session.id != doomed.id
                || session.process != koru_session_protocol::ProcessState::Running
        })
    });
    let live = client.list().unwrap();
    for spared in [&spared_neighbour, &spared_unscoped, &spared_unplaced] {
        let seen = live
            .iter()
            .find(|session| session.id == spared.id)
            .expect("a session outside the workspace was reaped");
        assert_eq!(
            seen.process,
            koru_session_protocol::ProcessState::Running,
            "{} was closed by another workspace's removal",
            seen.display_name
        );
    }

    // Idempotent: a second call finds nothing left and says so, which is what
    // makes the whole management operation safe to retry after the Store
    // mutation fails.
    assert!(
        gateway
            .terminate_workspace_sessions(host.lifecycle_authority.clone(), scope())
            .unwrap()
            .is_empty()
    );

    for spared in [&spared_neighbour, &spared_unscoped, &spared_unplaced] {
        terminate(&client, spared.id);
    }
    host.stop();
}

/// **The authority decides, not the capability.** An ordinary client
/// negotiates `workspace-lifecycle` like any other — anybody may ask for a
/// capability — and still cannot close a workspace's terminals, because the
/// launch-scoped secret reaches only a process the Agent started down its
/// bootstrap pipe. The host's own `StopHost` authority is refused too: the two
/// secrets are deliberately different values, so a Gateway that learns one
/// cannot use the other.
#[test]
fn a_workspace_terminate_without_the_lifecycle_authority_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let ordinary = host.client("koru-workspace-test");
    assert!(
        ordinary
            .host()
            .agreement
            .capabilities
            .supports("workspace-lifecycle"),
        "the refusal below has to be about the authority, not the capability"
    );

    let project = koru_session_protocol::ProjectId::mint();
    ordinary
        .create_project(project, "Mine", None, Some(scope()))
        .unwrap();
    let mut placed = create(root.path(), "KORU:PROTECTED");
    placed.project_id = Some(project);
    let session = ordinary.create(placed).unwrap();

    for presented in [
        String::new(),
        "not-the-authority".to_owned(),
        host.authority.clone(),
    ] {
        assert!(
            matches!(
                ordinary
                    .terminate_workspace_sessions(presented, scope())
                    .unwrap_err(),
                ClientError::Session(koru_session_protocol::SessionFailure {
                    code: ErrorCode::Forbidden,
                    ..
                })
            ),
            "a client without the lifecycle authority closed a workspace's sessions"
        );
    }
    assert_eq!(
        ordinary
            .list()
            .unwrap()
            .iter()
            .find(|listed| listed.id == session.id)
            .map(|listed| listed.process.clone()),
        Some(koru_session_protocol::ProcessState::Running)
    );

    terminate(&ordinary, session.id);
    host.stop();
}

/// A peer that did not negotiate the capability is refused before the
/// authority is looked at — the same shape as the project gate, and the reason
/// this took a protocol version rather than a defaulted field.
#[test]
fn a_peer_without_the_workspace_lifecycle_capability_cannot_ask() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let legacy = SessionClient::connect(
        &host.endpoint,
        "koru-workspace-v10",
        ProtocolOffer {
            versions: ProtocolRange { min: 10, max: 10 },
            capabilities: CapabilitySet::new(
                ["projects"]
                    .into_iter()
                    .map(|name| name.parse::<CapabilityName>().unwrap()),
            ),
        },
    )
    .unwrap();
    assert_eq!(legacy.host().agreement.version, 10);
    assert!(matches!(
        legacy
            .terminate_workspace_sessions(host.lifecycle_authority.clone(), scope())
            .unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        })
    ));
    host.stop();
}

/// **The projects follow the workspace, and nothing else moves.**
///
/// The repair half of SP-15a, against a real host: a project scoped to the
/// workspace gets the new profile, one in another workspace under the same old
/// profile does not, and an unscoped project is untouched. The repair is a
/// project mutation of one field — the workspace half stays what it was,
/// because it is the same workspace.
#[test]
fn moving_a_workspace_rescopes_only_its_own_projects() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-workspace-test");
    let gateway = host.client("koru-api-gateway");
    let elsewhere: koru_session_protocol::CatalogProfileId =
        "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee".parse().unwrap();

    let mine = koru_session_protocol::ProjectId::mint();
    let neighbour = koru_session_protocol::ProjectId::mint();
    let unscoped = koru_session_protocol::ProjectId::mint();
    client
        .create_project(mine, "Mine", None, Some(scope()))
        .unwrap();
    client
        .create_project(neighbour, "Neighbour", None, Some(other_scope()))
        .unwrap();
    client
        .create_project(unscoped, "Everywhere", None, None)
        .unwrap();

    let rescoped = gateway
        .rescope_workspace_projects(
            host.lifecycle_authority.clone(),
            scope().workspace,
            elsewhere,
        )
        .unwrap();
    assert_eq!(rescoped, vec![mine]);

    let scoped = |id| {
        client
            .list_projects()
            .unwrap()
            .into_iter()
            .find(|project| project.id == id)
            .expect("a project vanished")
            .workspace_scope
    };
    assert_eq!(
        scoped(mine),
        Some(koru_session_protocol::WorkspaceScope {
            profile: elsewhere,
            workspace: scope().workspace,
        })
    );
    assert_eq!(scoped(neighbour), Some(other_scope()));
    assert_eq!(scoped(unscoped), None);

    // **Idempotent, which is what makes the whole migration retryable.** The
    // request names the state to be in, so a second call — after a crash, or
    // after the operation was retried because the host was unreachable the
    // first time — finds everything already right and says so.
    assert!(
        gateway
            .rescope_workspace_projects(
                host.lifecycle_authority.clone(),
                scope().workspace,
                elsewhere,
            )
            .unwrap()
            .is_empty()
    );

    // And it survives a restart, because the repair was committed to the file
    // rather than held in the running registry.
    host.stop();
    let host = Host::start(root.path());
    let reopened = host.client("koru-workspace-test");
    assert_eq!(
        reopened
            .list_projects()
            .unwrap()
            .into_iter()
            .find(|project| project.id == mine)
            .unwrap()
            .workspace_scope
            .unwrap()
            .profile,
        elsewhere
    );
    host.stop();
}

/// The authority decides here too, and it is the same secret closing a
/// workspace's terminals takes — moving somebody's projects between personas is
/// no smaller a thing than ending their sessions.
#[test]
fn a_project_rescope_without_the_lifecycle_authority_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let ordinary = host.client("koru-workspace-test");
    let elsewhere: koru_session_protocol::CatalogProfileId =
        "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee".parse().unwrap();

    let project = koru_session_protocol::ProjectId::mint();
    ordinary
        .create_project(project, "Mine", None, Some(scope()))
        .unwrap();

    for presented in [
        String::new(),
        "not-the-authority".to_owned(),
        host.authority.clone(),
    ] {
        assert!(
            matches!(
                ordinary
                    .rescope_workspace_projects(presented, scope().workspace, elsewhere)
                    .unwrap_err(),
                ClientError::Session(koru_session_protocol::SessionFailure {
                    code: ErrorCode::Forbidden,
                    ..
                })
            ),
            "a client without the lifecycle authority moved a project between profiles"
        );
    }
    assert_eq!(
        ordinary
            .list_projects()
            .unwrap()
            .into_iter()
            .find(|listed| listed.id == project)
            .unwrap()
            .workspace_scope,
        Some(scope()),
        "a refused rescope changed something"
    );
    host.stop();
}

/// **A v11 peer holds the capability and still cannot be sent a rescope.**
///
/// The version is the half that moved. `workspace-lifecycle` names an authority
/// family rather than a frame, so a peer that stopped at v11 negotiates it
/// exactly as before — and is refused, because it would answer that the frame
/// is unreadable, and "unreadable" is not an answer a caller can act on.
#[test]
fn a_v11_peer_with_the_lifecycle_capability_cannot_be_sent_a_rescope() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let legacy = SessionClient::connect(
        &host.endpoint,
        "koru-api-gateway-v11",
        ProtocolOffer {
            versions: ProtocolRange { min: 11, max: 11 },
            capabilities: CapabilitySet::new(
                ["projects", "workspace-lifecycle"]
                    .into_iter()
                    .map(|name| name.parse::<CapabilityName>().unwrap()),
            ),
        },
    )
    .unwrap();
    assert_eq!(legacy.host().agreement.version, 11);
    assert!(
        legacy
            .host()
            .agreement
            .capabilities
            .supports("workspace-lifecycle"),
        "the refusal below has to be about the version, not the capability"
    );
    // The v11 verb still works for it, which is the point of a version rather
    // than a second capability.
    assert!(
        legacy
            .terminate_workspace_sessions(host.lifecycle_authority.clone(), scope())
            .unwrap()
            .is_empty()
    );
    assert!(matches!(
        legacy
            .rescope_workspace_projects(
                host.lifecycle_authority.clone(),
                scope().workspace,
                "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee".parse().unwrap(),
            )
            .unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        })
    ));
    host.stop();
}

/// **The orphan area's second resolution, across real IPC** (protocol v13).
///
/// One window moves one project to a workspace: its identity, its name, its
/// folder and its sessions' placement all survive, which is the whole reason
/// ADR-264 refused to fake this with a delete-and-recreate. The second window
/// learns about it from the catalog subscription rather than by asking.
#[test]
fn moving_one_project_to_a_workspace_keeps_its_identity_and_its_sessions() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let author = host.client("koru-workspace-test");
    let observer = host.client("koru-workspace-second-window");
    let watch = observer.subscribe_projects().unwrap();

    let id = koru_session_protocol::ProjectId::mint();
    let untouched = koru_session_protocol::ProjectId::mint();
    let folder = root.path().to_string_lossy().into_owned();
    // One wait per mutation, each before the next: the host queues a single
    // pending invalidation per subscriber, so two creates back to back can
    // reach this window as one event, and a second wait then never ends.
    let created = author
        .create_project(id, "Orphan", Some(folder.clone()), Some(scope()))
        .unwrap();
    wait_for_invalidation(&watch);
    author
        .create_project(untouched, "Neighbour", None, Some(scope()))
        .unwrap();
    wait_for_invalidation(&watch);

    let session = author.create(create(root.path(), "KORU:SCOPE")).unwrap();
    author.assign_project(session.id, Some(id)).unwrap();
    wait_for_invalidation(&watch);

    let moved = author.set_project_scope(id, Some(other_scope())).unwrap();
    wait_for_invalidation(&watch);
    assert_eq!(moved.workspace_scope, Some(other_scope()));
    // **The same project, not a replacement.** This is the sentence the verb
    // exists for: nothing about the row changed except the field that was
    // wrong, and nobody had to mint a new identity.
    assert_eq!(moved.id, created.id);
    assert_eq!(moved.name, created.name);
    assert_eq!(moved.root_directory, created.root_directory);
    assert_eq!(moved.created_at_ms, created.created_at_ms);

    let seen = |client: &SessionClient, project| {
        client
            .list_projects()
            .unwrap()
            .into_iter()
            .find(|row| row.id == project)
            .expect("a project vanished")
            .workspace_scope
    };
    assert_eq!(seen(&observer, id), Some(other_scope()));
    assert_eq!(
        seen(&observer, untouched),
        Some(scope()),
        "a project that merely shared the old scope was moved with it"
    );

    // The session went nowhere: it is still in the project, which is what a
    // delete-and-recreate would have thrown away.
    assert_eq!(
        observer
            .list()
            .unwrap()
            .into_iter()
            .find(|row| row.id == session.id)
            .expect("the session is visible to both windows")
            .project_id,
        Some(id)
    );

    // Idempotent, and a repeat is not a second event: naming the destination
    // means the second call has nothing to commit and nothing to announce.
    assert_eq!(
        author
            .set_project_scope(id, Some(other_scope()))
            .unwrap()
            .workspace_scope,
        Some(other_scope())
    );
    let idle = watch.recv_timeout(Duration::from_millis(300));
    assert!(
        matches!(idle, Err(ClientError::TimedOut)),
        "idempotent rescope subscription returned {idle:?}"
    );

    // A project nobody has is refused rather than invented.
    assert!(matches!(
        author
            .set_project_scope(koru_session_protocol::ProjectId::mint(), Some(scope()))
            .unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProjectNotFound,
            ..
        })
    ));

    terminate(&author, session.id);
    host.stop();
}

/// **Two windows racing to move the same project.**
///
/// ADR-264: concurrent mutations serialize at the session host as typed
/// operations over the latest registry state — no client replaces a registry it
/// read a moment ago. So both calls succeed, the registry ends on exactly one
/// of the two destinations, both windows read that same one, and each committed
/// move is announced once. What cannot happen is a merge, a torn row, or a
/// window whose stale read wins by being slower.
#[test]
fn two_windows_moving_one_project_serialize_on_the_last_commit() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let first = host.client("koru-workspace-window-one");
    let second = host.client("koru-workspace-window-two");
    let observer = host.client("koru-workspace-observer");
    let watch = observer.subscribe_projects().unwrap();

    let id = koru_session_protocol::ProjectId::mint();
    first.create_project(id, "Contested", None, None).unwrap();
    wait_for_invalidation(&watch);

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let racer = {
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            second.set_project_scope(id, Some(other_scope())).unwrap()
        })
    };
    barrier.wait();
    let mine = first.set_project_scope(id, Some(scope())).unwrap();
    let theirs = racer.join().unwrap();

    // Each call answered the state it committed, so each is a whole scope and
    // never a half of one.
    for answer in [&mine, &theirs] {
        assert!(
            answer.workspace_scope == Some(scope())
                || answer.workspace_scope == Some(other_scope()),
            "a serialized move answered a scope nobody asked for: {answer:?}"
        );
        assert_eq!(answer.id, id);
        assert_eq!(answer.name, "Contested");
    }

    // **One registry, one answer.** Whichever landed last is what every reader
    // now sees, and they see the same thing.
    let settled = observer.list_projects().unwrap();
    assert_eq!(settled.len(), 1);
    let landed = settled[0].workspace_scope;
    assert!(landed == Some(scope()) || landed == Some(other_scope()));
    assert_eq!(first.list_projects().unwrap()[0].workspace_scope, landed);

    // The fan-out is per committed change and the queue holds one, so a reader
    // that drains sees at least the invalidation for the last commit — never
    // silence after a mutation landed.
    wait_for_invalidation(&watch);

    // And re-stating the settled destination now changes nothing at all, which
    // is what makes either window's retry safe after a race it lost.
    assert_eq!(
        first.set_project_scope(id, landed).unwrap().workspace_scope,
        landed
    );
    host.stop();
}

/// **A v12 peer holds `projects` and still cannot be sent a scope rewrite.**
///
/// The same shape as `a_v11_peer_with_the_lifecycle_capability_cannot_be_sent_a_rescope`,
/// one family over: the capability says a peer speaks the project family, and
/// the version says which of its frames it can read. The v10 verbs still work
/// for it, which is the point of a version rather than a second capability.
#[test]
fn a_v12_peer_with_the_projects_capability_cannot_be_sent_a_scope_rewrite() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let current = host.client("koru-workspace-test");
    let id = koru_session_protocol::ProjectId::mint();
    current
        .create_project(id, "Orphan", None, Some(scope()))
        .unwrap();

    let legacy = SessionClient::connect(
        &host.endpoint,
        "koru-workspace-v12",
        ProtocolOffer {
            versions: ProtocolRange { min: 12, max: 12 },
            capabilities: CapabilitySet::new(
                ["projects"]
                    .into_iter()
                    .map(|name| name.parse::<CapabilityName>().unwrap()),
            ),
        },
    )
    .unwrap();
    assert_eq!(legacy.host().agreement.version, 12);
    assert!(
        legacy.host().agreement.capabilities.supports("projects"),
        "the refusal below has to be about the version, not the capability"
    );
    // Every v10 project verb still works for it.
    assert_eq!(legacy.list_projects().unwrap().len(), 1);
    assert_eq!(
        legacy.rename_project(id, "Renamed").unwrap().name,
        "Renamed"
    );

    assert!(matches!(
        legacy
            .set_project_scope(id, Some(other_scope()))
            .unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        })
    ));

    // And the other half of the gate, unchanged by the new version: v13
    // without the capability is refused too. A version is not a licence.
    let uncapable = SessionClient::connect(
        &host.endpoint,
        "v13-without-projects",
        ProtocolOffer {
            versions: ProtocolRange { min: 13, max: 13 },
            capabilities: CapabilitySet::new(["controller-leases"
                .parse::<CapabilityName>()
                .unwrap()]),
        },
    )
    .unwrap();
    assert_eq!(uncapable.host().agreement.version, 13);
    assert!(matches!(
        uncapable
            .set_project_scope(id, Some(other_scope()))
            .unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        })
    ));

    // Neither refusal touched the registry.
    assert_eq!(
        current.list_projects().unwrap()[0].workspace_scope,
        Some(scope())
    );
    host.stop();
}

fn terminate(client: &SessionClient, session_id: SessionId) {
    let Ok((lease, _render)) =
        client.attach_live(session_id, ViewportGeneration::new(1).unwrap(), 80, 1)
    else {
        return;
    };
    let _ = client.command(SessionCommand {
        idempotency_key: IdempotencyKey::mint(),
        session_id,
        controller_lease: lease.token,
        action: RawCommand::Terminate,
    });
}

#[test]
fn one_clients_project_mutations_reach_a_second_without_it_polling() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let author = host.client("koru-workspace-test");
    let observer = host.client("koru-workspace-second-window");
    let watch = observer.subscribe_projects().unwrap();

    let id = koru_session_protocol::ProjectId::mint();
    let created = author
        .create_project(id, "Koru", None, Some(scope()))
        .unwrap();
    assert_eq!(created.id, id);
    wait_for_invalidation(&watch);
    assert_eq!(observer.list_projects().unwrap(), vec![created.clone()]);

    let renamed = author.rename_project(id, "Koru Terminal").unwrap();
    wait_for_invalidation(&watch);
    assert_eq!(observer.list_projects().unwrap(), vec![renamed.clone()]);
    assert_eq!(renamed.created_at_ms, created.created_at_ms);

    let rooted = author
        .set_project_root(id, Some(root.path().to_string_lossy().into_owned()))
        .unwrap();
    wait_for_invalidation(&watch);
    assert_eq!(observer.list_projects().unwrap(), vec![rooted]);

    // Assigning persists nothing, but the tree that groups sessions by
    // project is now wrong in the other window, so it is invalidated too.
    let session = author.create(create(root.path(), "KORU:PROJECT")).unwrap();
    assert_eq!(session.project_id, None);
    author.assign_project(session.id, Some(id)).unwrap();
    wait_for_invalidation(&watch);
    let seen = observer
        .list()
        .unwrap()
        .into_iter()
        .find(|listed| listed.id == session.id)
        .expect("the session is visible to both clients");
    assert_eq!(
        seen.project_id,
        Some(id),
        "the read-only placement did not reach the second client"
    );

    author.assign_project(session.id, None).unwrap();
    wait_for_invalidation(&watch);
    author.delete_project(id).unwrap();
    wait_for_invalidation(&watch);
    assert!(observer.list_projects().unwrap().is_empty());

    terminate(&author, session.id);
    host.stop();
}

#[test]
fn a_project_is_refused_deletion_while_a_session_runs_and_keeps_its_subtree() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());
    let client = host.client("koru-workspace-test");

    let home = koru_session_protocol::ProjectId::mint();
    let elsewhere = koru_session_protocol::ProjectId::mint();
    client.create_project(home, "Home", None, None).unwrap();
    client
        .create_project(elsewhere, "Elsewhere", None, None)
        .unwrap();

    // Placement is atomic at create: the summary comes back already placed,
    // so no window ever sees this session outside its project.
    let mut placed = create(root.path(), "KORU:PARENT");
    placed.project_id = Some(home);
    let parent = client.create(placed).unwrap();
    assert_eq!(parent.project_id, Some(home));

    // A child inherits, and cannot be created into another project.
    let mut child_request = create(root.path(), "KORU:CHILD");
    child_request.parent_session_id = Some(parent.id);
    let child = client.create(child_request.clone()).unwrap();
    assert_eq!(child.project_id, Some(home));

    child_request.project_id = Some(elsewhere);
    assert!(matches!(
        client.create(child_request).unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProjectMismatch,
            ..
        })
    ));

    // Deleting is bookkeeping, never a process operation in disguise.
    assert!(matches!(
        client.delete_project(home).unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProjectHasSessions,
            ..
        })
    ));

    // Nor can a child be moved out from under its parent...
    assert!(matches!(
        client
            .assign_project(child.id, Some(elsewhere))
            .unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProjectMismatch,
            ..
        })
    ));
    // ...but moving the parent takes the subtree with it.
    client.assign_project(parent.id, Some(elsewhere)).unwrap();
    let placement: BTreeMap<_, _> = client
        .list()
        .unwrap()
        .into_iter()
        .map(|session| (session.id, session.project_id))
        .collect();
    assert_eq!(placement[&parent.id], Some(elsewhere));
    assert_eq!(placement[&child.id], Some(elsewhere));

    // Home is empty now, so it deletes; Elsewhere still refuses.
    client.delete_project(home).unwrap();
    assert!(matches!(
        client.delete_project(elsewhere).unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProjectHasSessions,
            ..
        })
    ));
    assert!(matches!(
        client.rename_project(home, "ghost").unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::ProjectNotFound,
            ..
        })
    ));

    terminate(&client, child.id);
    terminate(&client, parent.id);
    host.stop();
}

#[test]
fn project_verbs_require_protocol_ten_and_the_negotiated_capability() {
    let root = tempfile::tempdir().unwrap();
    let host = Host::start(root.path());

    // A pre-project peer: it negotiates v9, keeps every session verb it had,
    // and cannot reach a project one. `#[serde(default)]` makes an added
    // *field* readable by an older peer and does nothing for an unknown
    // *variant*, which is why the verbs arrived under a version at all.
    let legacy = SessionClient::connect(
        &host.endpoint,
        "pre-project-terminal",
        ProtocolOffer {
            versions: ProtocolRange { min: 1, max: 9 },
            capabilities: CapabilitySet::new(
                ["controller-leases", "render-deltas"]
                    .into_iter()
                    .map(|name| name.parse::<CapabilityName>().unwrap()),
            ),
        },
    )
    .unwrap();
    assert_eq!(legacy.host().agreement.version, 9);
    assert!(!legacy.host().agreement.capabilities.supports("projects"));
    assert!(matches!(
        legacy.list_projects().unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        })
    ));
    let legacy_session = legacy.create(create(root.path(), "KORU:LEGACY")).unwrap();
    assert_eq!(
        legacy_session.project_id, None,
        "a peer with no projects must not receive a placement"
    );

    // v10 without the capability is the other half of the gate: a version is
    // not a licence, and asking for a placement is refused the same way.
    let uncapable = SessionClient::connect(
        &host.endpoint,
        "v10-without-projects",
        ProtocolOffer {
            versions: ProtocolRange { min: 10, max: 10 },
            capabilities: CapabilitySet::new(["controller-leases"
                .parse::<CapabilityName>()
                .unwrap()]),
        },
    )
    .unwrap();
    assert_eq!(uncapable.host().agreement.version, 10);
    assert!(matches!(
        uncapable.list_projects().unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        })
    ));
    let mut placing = create(root.path(), "KORU:UNPLACEABLE");
    placing.project_id = Some(koru_session_protocol::ProjectId::mint());
    assert!(matches!(
        uncapable.create(placing).unwrap_err(),
        ClientError::Session(koru_session_protocol::SessionFailure {
            code: ErrorCode::CapabilityNotNegotiated,
            ..
        })
    ));

    // A capability neither peer knows is retained by its holder and enabled
    // by nobody; it turns the project verbs neither on nor off.
    let stranger = SessionClient::connect(
        &host.endpoint,
        "future-terminal",
        ProtocolOffer {
            versions: ProtocolRange { min: 10, max: 10 },
            capabilities: CapabilitySet::new(
                ["projects", "a-capability-from-the-future"]
                    .into_iter()
                    .map(|name| name.parse::<CapabilityName>().unwrap()),
            ),
        },
    )
    .unwrap();
    let agreed = &stranger.host().agreement.capabilities;
    assert!(agreed.supports("projects"));
    assert!(
        !agreed.supports("a-capability-from-the-future"),
        "an unknown capability was enabled"
    );
    assert!(stranger.list_projects().unwrap().is_empty());

    terminate(&legacy, legacy_session.id);
    host.stop();
}
