// SPDX-License-Identifier: MPL-2.0

//! What the overlay must not receive, and what must not be quarantined.

use super::*;
mod account_switching;
mod enrollment_sets;
use koru_draw::text::TextEngine;
use std::sync::{Arc, Mutex};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};

fn agent() -> Agent {
    Agent::new(
        Paths::for_root(std::path::PathBuf::from("C:/nowhere")),
        "device".into(),
        "instance".into(),
    )
}

#[test]
fn system_theme_changes_only_move_an_appearance_that_follows_the_system() {
    let system = koru_window::Appearance::default();
    assert_eq!(system_theme_result(system, false, true), Some(true));
    assert_eq!(system_theme_result(system, true, true), None);

    for theme in [
        koru_settings::ThemeChoice::Light,
        koru_settings::ThemeChoice::Dark,
    ] {
        let fixed = koru_window::Appearance::from_section(&koru_settings::GlobalSettings {
            theme,
            ..Default::default()
        });
        assert_eq!(system_theme_result(fixed, false, true), None);
    }
}

#[test]
fn an_unidentified_open_client_is_not_an_existing_workspace_window() {
    let mut agent = agent();
    let connection = ConnectionId(41);
    let (conn, _queue) = Connection::for_test(connection);
    agent.connections.push(conn);

    assert!(agent.registry.shells().is_empty());
    assert_eq!(
        agent.connections.len(),
        1,
        "the CLI socket exists but has not identified as a Koru shell"
    );
    let mut launched = false;
    let opened = agent
        .open_workspace_with(Some("Research"), true, |root, profile, inherit_stdio| {
            launched = true;
            assert_eq!(root, Some(Path::new("C:/nowhere")));
            assert_eq!(profile, Some("Research"));
            assert!(inherit_stdio);
            Ok(4242)
        })
        .unwrap();
    assert!(
        launched,
        "an unidentified CLI must not suppress a shell launch"
    );
    assert_eq!(opened, Some(4242));
}

#[test]
fn opening_an_existing_workspace_restores_without_launching() {
    let mut agent = agent();
    let connection = ConnectionId(42);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);
    agent
        .registry
        .hello(connection, None, ProfileId::new(), WorkspaceId::new());

    assert_eq!(
        agent
            .open_workspace_with(None, false, |_, _, _| panic!(
                "an existing shell must restore"
            ))
            .unwrap(),
        None
    );
    assert_eq!(
        queue.try_recv(),
        Ok(FromAgent::Push {
            body: Push::Restore,
        })
    );
}

/// A build with no Koru Cloud has no account capability, so a sign-in is
/// answered here, at once, with *not configured* — never queued, never
/// retried, and with no network attempt, because nothing exists to make one.
#[test]
fn a_build_without_koru_cloud_answers_every_account_request_not_configured() {
    let mut agent = agent();
    agent.set_cloud_configured(false);
    let connection = ConnectionId(43);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);
    assert!(agent.account.is_none());

    for (id, command) in [
        (1, koru_agent_wire::AccountCommand::Status),
        (
            2,
            koru_agent_wire::AccountCommand::StartGoogle {
                locale: "en".into(),
            },
        ),
        (3, koru_agent_wire::AccountCommand::Retry),
    ] {
        agent.said(
            connection,
            id,
            Request::Account {
                command,
                expected_owner_scope: Some(0),
            },
            Duration::ZERO,
        );
        match queue.try_recv() {
            Ok(FromAgent::Response {
                id: answered,
                body: Response::Account { state },
            }) => {
                assert_eq!(answered, id);
                assert_eq!(state, koru_agent_wire::AccountState::not_configured());
            }
            other => panic!("request {id}: expected a not-configured state, got {other:?}"),
        }
    }
    assert!(agent.account.is_none(), "nothing started a capability");

    // A build that has a Cloud but no capability still says unavailable.
    agent.set_cloud_configured(true);
    agent.said(
        connection,
        4,
        Request::Account {
            command: koru_agent_wire::AccountCommand::Status,
            expected_owner_scope: None,
        },
        Duration::ZERO,
    );
    assert!(matches!(
        queue.try_recv(),
        Ok(FromAgent::Response {
            body: Response::Account { state },
            ..
        }) if state.stage == koru_agent_wire::AccountStage::Unavailable
    ));
}

#[test]
fn unit_tests_refuse_the_real_shell_launch_boundary() {
    for inherit_stdio in [false, true] {
        let error = spawn_koru_with_stdio(&[], inherit_stdio).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
    }
}

#[test]
fn notification_test_copy_is_bounded_before_the_worker() {
    let cases = [
        ("empty title", String::new(), None, false),
        ("title boundary", "界".repeat(256), None, true),
        ("title over boundary", "界".repeat(257), None, false),
        (
            "body boundary",
            "localized title".into(),
            Some("界".repeat(1_024)),
            true,
        ),
        (
            "body over boundary",
            "localized title".into(),
            Some("界".repeat(1_025)),
            false,
        ),
    ];
    for (label, title, body, valid) in cases {
        let operation = koru_agent_wire::NotificationOperation::SendTest {
            tone: koru_agent_wire::NotificationTone::Soft,
            title,
            body,
        };
        assert_eq!(
            notification_control_request(operation).is_ok(),
            valid,
            "{label}"
        );
    }
}

#[test]
fn first_enable_only_adds_startup_when_the_host_can_own_it() {
    let ordinary = voice_enable_patch(true, true);
    assert_eq!(ordinary.global_dictation_enabled, Some(true));
    assert_eq!(ordinary.start_on_login, Some(true));

    let rooted = voice_enable_patch(true, false);
    assert_eq!(rooted.global_dictation_enabled, Some(true));
    assert_eq!(rooted.start_on_login, None);

    let reenabling = voice_enable_patch(false, true);
    assert_eq!(reenabling.start_on_login, None);
}

#[test]
fn a_full_activation_fifo_is_not_acknowledged_and_the_same_wire_payload_can_retry() {
    let ingress = crate::reminders::ReminderActivationIngress::for_test();
    for suffix in 0..64 {
        let activation = koru_native_notifications::Activation::new(
            koru_native_notifications::RegistrationId::new(format!("{suffix:064x}")).unwrap(),
            koru_native_notifications::Action::Open,
        );
        ingress.accept_opaque(&activation.encode_opaque()).unwrap();
    }
    let retry = koru_native_notifications::Activation::new(
        koru_native_notifications::RegistrationId::new(format!("{:064x}", 64)).unwrap(),
        koru_native_notifications::Action::Open,
    );
    let envelope = retry.encode_opaque();

    let mut agent = agent();
    agent.set_reminder_activation_ingress(ingress.clone());
    let connection = ConnectionId(44);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);
    agent.said(
        connection,
        7,
        Request::ReminderActivation {
            envelope: envelope.clone(),
        },
        Duration::ZERO,
    );
    assert!(matches!(
        queue.recv(),
        Ok(FromAgent::Response {
            id: 7,
            body: Response::Failed { code, .. }
        }) if code == "notification_activation_busy"
    ));

    assert!(ingress.take_one_for_test().is_some());
    agent.said(
        connection,
        8,
        Request::ReminderActivation { envelope },
        Duration::ZERO,
    );
    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Response {
            id: 8,
            body: Response::Ok,
        })
    );
}

fn snooze_target() -> crate::reminders::ActivationTarget {
    let profile = koru_store_core::ProfileId::new();
    let reminder = koru_store_core::ReminderId::new();
    crate::reminders::ActivationTarget {
        profile,
        reminder,
        occurrence: koru_store_core::OccurrenceKey::parse(&format!("occ1-{}", "01".repeat(32)))
            .unwrap(),
        entity: koru_store_core::EntityRef::new(
            "task",
            koru_store_core::WorkspaceId::new(),
            koru_store_core::DocumentId::new(),
            uuid::Uuid::new_v4(),
        )
        .unwrap(),
    }
}

#[test]
fn snooze_activation_routes_open_then_a_typed_acknowledged_one_shot() {
    let target = snooze_target();
    let connection = ConnectionId(46);
    let (live, queue) = Connection::for_test(connection);
    let mut agent = agent();
    agent.connections.push(live);
    agent
        .registry
        .hello(connection, None, target.profile, target.entity.workspace());

    agent.wake(AgentEvent::ReminderSnooze {
        target: target.clone(),
    });
    assert!(matches!(
        queue.recv(),
        Ok(FromAgent::Push {
            body: Push::Open { .. }
        })
    ));
    let Ok(FromAgent::Push {
        body: Push::ReminderSnooze { intent },
    }) = queue.recv()
    else {
        panic!("typed snooze push did not follow navigation")
    };
    assert_eq!(intent.profile, target.profile.to_storage());
    assert_eq!(intent.reminder, target.reminder.to_storage());
    assert_eq!(intent.occurrence, target.occurrence.to_storage());
    assert_eq!(intent.entity, target.entity.to_storage());
    assert_eq!(agent.reminder_snoozes.len(), 1);

    let wrong_connection = ConnectionId(49);
    let (wrong_live, wrong_queue) = Connection::for_test(wrong_connection);
    agent.connections.push(wrong_live);
    agent.said(
        wrong_connection,
        90,
        Request::AcceptReminderSnooze {
            delivery: intent.delivery.clone(),
        },
        Duration::ZERO,
    );
    assert!(matches!(
        wrong_queue.recv(),
        Ok(FromAgent::Response {
            id: 90,
            body: Response::Failed { code, .. },
        }) if code == "unknown_reminder_snooze"
    ));
    assert_eq!(agent.reminder_snoozes.len(), 1);

    agent.said(
        connection,
        91,
        Request::AcceptReminderSnooze {
            delivery: intent.delivery,
        },
        Duration::ZERO,
    );
    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Response {
            id: 91,
            body: Response::Ok,
        })
    );
    assert!(agent.reminder_snoozes.is_empty());
}

#[test]
fn unacknowledged_snooze_survives_disconnect_and_the_next_cold_hello() {
    let target = snooze_target();
    let intent = koru_agent_wire::ReminderSnoozeIntent {
        delivery: uuid::Uuid::new_v4().to_string(),
        profile: target.profile.to_storage(),
        reminder: target.reminder.to_storage(),
        occurrence: target.occurrence.to_storage(),
        entity: target.entity.to_storage(),
    };
    let first = ConnectionId(47);
    let mut agent = agent();
    agent.reminder_snoozes.push_back(PendingReminderSnooze {
        profile: target.profile,
        workspace: target.entity.workspace(),
        intent: intent.clone(),
        assigned: Some(first),
    });
    agent.drop_connection(first);
    assert_eq!(agent.reminder_snoozes[0].assigned, None);

    let wire = koru_agent_wire::Target {
        profile: target.profile.to_storage(),
        workspace: target.entity.workspace().to_storage(),
        document: Some(target.entity.document().to_storage()),
        node: Some(target.entity.node().to_string()),
        trashed: false,
        match_text: None,
        new_window: false,
    };
    assert!(matches!(
        agent.router.open(
            &agent.registry,
            target.profile,
            target.entity.workspace(),
            wire.clone(),
            Instant::now(),
        ),
        Routed::Launch { .. }
    ));

    let second = ConnectionId(48);
    let (live, queue) = Connection::for_test(second);
    agent.connections.push(live);
    agent.said(
        second,
        92,
        Request::Hello {
            profile: target.profile.to_storage(),
            workspace: target.entity.workspace().to_storage(),
            profile_name: String::new(),
            workspace_name: String::new(),
            pinned: Vec::new(),
            reopen_claim: None,
        },
        Duration::ZERO,
    );
    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Response {
            id: 92,
            body: Response::Ok,
        })
    );
    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Push {
            body: Push::Open { target: wire },
        })
    );
    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Push {
            body: Push::ReminderSnooze {
                intent: intent.clone(),
            },
        })
    );
    assert_eq!(agent.reminder_snoozes[0].assigned, Some(second));
}

#[test]
fn only_an_installed_voice_setup_warms_the_worker_for_its_input_picker() {
    let mut voice = read_voice_state(None, koru_voice::InputChoice::SystemDefault);
    for row in [
        crate::tray::VoiceRow::NotInstalled { megabytes: None },
        crate::tray::VoiceRow::Downloading { percent: 42 },
        crate::tray::VoiceRow::Unavailable(koru_voice::Off::Switch),
    ] {
        voice.row = row;
        assert!(!warms_voice_input_picker(&voice));
    }
    voice.row = crate::tray::VoiceRow::Installed;
    assert!(warms_voice_input_picker(&voice));
}

#[test]
fn dictation_foreground_routing_fails_closed_and_distinguishes_local_from_external() {
    let own = 10;
    let mut registry = Registry::new();
    assert_eq!(
        dictation_destination(
            crate::event::ForegroundPid::Unavailable,
            own,
            true,
            &registry
        ),
        DictationDestination::Refuse
    );
    assert_eq!(
        dictation_destination(
            crate::event::ForegroundPid::Known(own),
            own,
            false,
            &registry
        ),
        DictationDestination::Refuse
    );
    assert_eq!(
        dictation_destination(
            crate::event::ForegroundPid::Known(own),
            own,
            true,
            &registry
        ),
        DictationDestination::Local(crate::event::LocalDictationOwner::Agent)
    );
    assert_eq!(
        dictation_destination(crate::event::ForegroundPid::Known(99), own, true, &registry),
        DictationDestination::Global
    );

    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    registry.hello(ConnectionId(1), Some(42), profile, workspace);
    assert_eq!(
        dictation_destination(crate::event::ForegroundPid::Known(42), own, true, &registry),
        DictationDestination::Local(crate::event::LocalDictationOwner::Workspace(ConnectionId(
            1
        )))
    );
    registry.hello(ConnectionId(2), Some(42), profile, WorkspaceId::new());
    assert_eq!(
        dictation_destination(crate::event::ForegroundPid::Known(42), own, true, &registry),
        DictationDestination::Refuse,
        "an ambiguous authenticated Workspace PID cannot fall through to global insertion"
    );
}

#[test]
fn search_routed_press_adopts_only_the_same_physical_native_press() {
    let d = PhysicalKey::Code(KeyCode::KeyD);
    let m = PhysicalKey::Code(KeyCode::KeyM);
    for mode in [
        koru_dictation::Mode::PushToTalk,
        koru_dictation::Mode::Toggle,
    ] {
        assert_eq!(
            search_routed_press(None, 7, koru_dictation::Stage::Live, Some(d), d),
            koru_dictation::RoutedPress::Adopt,
            "same-key native duplicate in {mode:?}"
        );
        assert_eq!(
            search_routed_press(None, 7, koru_dictation::Stage::Live, Some(d), m),
            koru_dictation::RoutedPress::Refuse,
            "different-key session in {mode:?}"
        );
        assert_eq!(
            search_routed_press(None, 7, koru_dictation::Stage::Live, None, d),
            koru_dictation::RoutedPress::Refuse,
            "command-started session in {mode:?}"
        );
    }
    assert_eq!(
        search_routed_press(Some(7), 7, koru_dictation::Stage::Live, Some(d), d),
        koru_dictation::RoutedPress::Continue
    );
    assert_eq!(
        search_routed_press(Some(8), 7, koru_dictation::Stage::Live, Some(d), d),
        koru_dictation::RoutedPress::Ignore
    );
}

#[test]
fn workspace_dictation_state_requires_the_exact_authenticated_connection_and_route() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let mut registry = Registry::new();
    registry.hello(ConnectionId(1), Some(42), profile, workspace);
    registry.hello(ConnectionId(2), Some(84), profile, workspace);

    let active = Some((ConnectionId(1), 7));
    assert!(workspace_state_authenticated(
        &registry,
        active,
        ConnectionId(1),
        7
    ));
    assert!(!workspace_state_authenticated(
        &registry,
        active,
        ConnectionId(2),
        7
    ));
    assert!(!workspace_state_authenticated(
        &registry,
        active,
        ConnectionId(1),
        8
    ));
    registry.gone(ConnectionId(1));
    assert!(!workspace_state_authenticated(
        &registry,
        active,
        ConnectionId(1),
        7
    ));
}

/// Once update quiescence begins, a late shell must not restart the gateway or
/// standalone chat behind the handoff's back. The updater has its own defensive
/// sweep, but the Agent's promise is to close this race before handing over.
#[test]
fn quiescing_rejects_new_process_launches() {
    for request in [
        Request::EnsureService {
            service: Service::ApiGateway,
        },
        Request::OpenAiChat,
        Request::OpenWorkspace {
            profile: None,
            inherit_stdio: false,
        },
    ] {
        let mut agent = agent();
        agent.handoff = crate::update::Handoff::StoppingChat;
        let connection = ConnectionId(44);
        let (conn, queue) = Connection::for_test(connection);
        agent.connections.push(conn);

        agent.said(connection, 7, request, Duration::ZERO);
        assert_eq!(
            queue.recv(),
            Ok(FromAgent::Response {
                id: 7,
                body: Response::Failed {
                    code: "update_in_progress".into(),
                    message: "Koru is closing services to install an update".into(),
                },
            })
        );
    }
}

#[test]
fn post_update_restart_gate_rejects_child_launch_requests() {
    for request in [
        Request::EnsureService {
            service: Service::ApiGateway,
        },
        Request::OpenAiChat,
        Request::OpenWorkspace {
            profile: None,
            inherit_stdio: false,
        },
    ] {
        let mut agent = agent();
        agent.update_health_token = Some("0123456789abcdef0123456789abcdef".into());
        let connection = ConnectionId(45);
        let (conn, queue) = Connection::for_test(connection);
        agent.connections.push(conn);

        agent.said(connection, 8, request, Duration::ZERO);
        assert_eq!(
            queue.recv(),
            Ok(FromAgent::Response {
                id: 8,
                body: Response::Failed {
                    code: "update_in_progress".into(),
                    message: "Koru is closing services to install an update.".into(),
                },
            })
        );
    }
}

#[test]
fn local_profile_account_binding_is_classified_as_restart_gated_work() {
    let request = Request::BindLocalProfileAccount {
        profile_id: "00000000-0000-4000-8000-000000000001".into(),
        expected_identity_id: None,
        new_identity_id: "00000000-0000-4000-8000-000000000002".into(),
    };
    assert!(starts_process_during_restart(&request));
    assert!(starts_work_while_quiescing(&request));
}
#[test]
fn local_profile_removal_is_classified_as_restart_gated_work() {
    let request = Request::RemoveLocalProfile {
        profile_id: "00000000-0000-4000-8000-000000000001".into(),
    };
    assert!(starts_process_during_restart(&request));
    assert!(starts_work_while_quiescing(&request));
}

fn code(c: KeyCode) -> PhysicalKey {
    PhysicalKey::Code(c)
}

#[test]
fn the_agent_owned_search_window_hosts_the_physical_dictation_hold() {
    let mut agent = agent();
    agent.open_overlay();
    agent
        .dictation
        .set_consent(Some(koru_voice_consent::Granted::recorded(
            koru_voice_consent::Terms {
                model: "whisper-base".into(),
                version: "q5_1".into(),
                licence: "MIT".into(),
                attribution: "OpenAI".into(),
                download_bytes: 1,
            },
            1,
        )));
    let physical = code(KeyCode::KeyD);
    let key = Key::Character("d".into());
    let chord = koru_keys::registry::get(koru_keys::ShortcutId::Dictate).chord;
    let mut engine = TextEngine::new();

    assert!(agent.physical_key_pressed(&mut engine, physical, &key, Some("d"), chord.mods, false,));
    assert!(agent.dictation.is_live());
    assert_eq!(agent.dictation_key, Some(physical));

    agent.physical_key_released(physical);
    assert_eq!(agent.dictation.stage(), koru_dictation::Stage::Finalising);
    assert_eq!(agent.dictation_key, None);
}

/// The bug as it was reported: "it captures the k and shows it in the search
/// input", and "shift or something is stuck too so moving the mouse selects".
#[test]
fn the_chord_that_opened_the_overlay_does_not_reach_it() {
    let mut agent = agent();
    agent.wake(AgentEvent::Hotkey(crate::event::HotkeyPress::Search {
        trigger: KeyCode::KeyK,
    }));

    let held = agent.take_key_quarantine();
    assert!(
        held.contains(&code(KeyCode::KeyK)),
        "the letter typed itself"
    );
    // **Both sides of each modifier.** `WM_HOTKEY` reports that a chord was
    // pressed and never which Shift, and it is the *release* of the one that
    // was down that goes missing — which is what leaves it reading as held.
    assert!(held.contains(&code(KeyCode::ShiftLeft)));
    assert!(held.contains(&code(KeyCode::ShiftRight)));
    assert!(held.contains(&code(KeyCode::ControlLeft)));
    assert!(held.contains(&code(KeyCode::ControlRight)));
}

/// **Drained, so the next press is not swallowed too.** The loop applies these
/// once; a list that kept its contents would suppress the chord's keys for
/// every window this agent ever opens.
#[test]
fn taking_the_quarantine_empties_it() {
    let mut agent = agent();
    agent.wake(AgentEvent::Hotkey(crate::event::HotkeyPress::Search {
        trigger: KeyCode::KeyK,
    }));
    assert!(!agent.take_key_quarantine().is_empty());
    assert!(agent.take_key_quarantine().is_empty(), "not drained");
}

/// The second press of the chord *closes* the overlay, and there is then no
/// window for the keys to leak into. Quarantining anyway would swallow the
/// person's next Ctrl or Shift in whatever window the focus returns to.
#[test]
fn closing_the_overlay_quarantines_nothing() {
    let mut agent = agent();
    agent.wake(AgentEvent::Hotkey(crate::event::HotkeyPress::Search {
        trigger: KeyCode::KeyK,
    }));
    let _ = agent.take_key_quarantine();

    agent.wake(AgentEvent::Hotkey(crate::event::HotkeyPress::Search {
        trigger: KeyCode::KeyK,
    }));
    assert!(agent.overlay.is_none(), "the second press closes it");
    assert!(agent.take_key_quarantine().is_empty());
}

/// The tray's *Search* opens the same overlay, and no chord was pressed to do
/// it — so there is nothing to suppress and a person's held Shift stays theirs.
#[test]
fn the_tray_opens_the_overlay_and_quarantines_nothing() {
    let mut agent = agent();
    agent.wake(AgentEvent::Tray(TrayAsk::Search));
    assert!(agent.overlay.is_some());
    assert!(agent.take_key_quarantine().is_empty());
}

/// **The icon's left button brings back the workspace window in use.** The
/// most-recently-active shell is the answer, the same one every other
/// "the window the person means" routing keeps.
#[test]
fn the_tray_icon_restores_the_workspace_in_use() {
    let mut agent = agent();
    let connection = ConnectionId(70);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);
    workspace_hello(&mut agent, connection, &queue);

    agent.wake(AgentEvent::Tray(TrayAsk::IconClick));
    assert_eq!(
        queue.try_recv(),
        Ok(FromAgent::Push {
            body: Push::Restore,
        }),
        "the open workspace's window is asked to come back"
    );
    assert!(
        agent.overlay.is_none(),
        "no Search overlay over a workspace that exists"
    );
}

/// With no window open the same click is the Search overlay it always was.
#[test]
fn the_tray_icon_opens_search_when_no_workspace_is_open() {
    let mut agent = agent();
    agent.wake(AgentEvent::Tray(TrayAsk::IconClick));
    assert!(agent.overlay.is_some());
}

#[test]
fn only_the_drag_handle_pill_is_a_window_caption() {
    let mut agent = agent();
    agent.wake(AgentEvent::Tray(TrayAsk::Search));

    let pill_x = SEARCH_WIDTH * 0.5;
    // Below the outer edge band the pill moves; on the edge itself it does not.
    assert!(agent.is_caption(pill_x, DRAG_RAIL_H * 0.75));
    assert!(
        !agent.is_caption(pill_x, 1.0),
        "the outer top edge resizes, even over the pill"
    );
    assert!(
        !agent.is_caption(40.0, DRAG_RAIL_H * 0.75),
        "the rest of the top rail is for resize, not move"
    );
    assert!(!agent.is_caption(pill_x, DRAG_RAIL_H));
    assert!(!agent.is_caption(-1.0, DRAG_RAIL_H * 0.75));
    assert_eq!(
        agent.cursor_icon(pill_x, DRAG_RAIL_H * 0.75),
        winit::window::CursorIcon::Move
    );
    assert_eq!(
        agent.cursor_icon(pill_x, DRAG_RAIL_H),
        winit::window::CursorIcon::Default
    );
}

#[test]
fn search_resizes_only_from_its_top_and_bottom_edges() {
    let mut agent = agent();
    agent.wake(AgentEvent::Tray(TrayAsk::Search));

    assert_eq!(
        agent.resize_direction(SEARCH_WIDTH, 600.0, 40.0, DRAG_RAIL_H * 0.5, false),
        Some(winit::window::ResizeDirection::North),
        "the top rail (off the pill) is a North target"
    );
    assert_eq!(
        agent.resize_direction(SEARCH_WIDTH, 600.0, SEARCH_WIDTH * 0.5, 1.0, false),
        Some(winit::window::ResizeDirection::North),
        "the outer top edge resizes full-width, including over the pill"
    );
    assert_eq!(
        agent.resize_direction(
            SEARCH_WIDTH,
            600.0,
            SEARCH_WIDTH * 0.5,
            DRAG_RAIL_H * 0.75,
            false
        ),
        None,
        "the pill body (below the edge) moves the window"
    );
    assert_eq!(
        agent.resize_direction(SEARCH_WIDTH, 600.0, 1.0, 599.0, false),
        Some(winit::window::ResizeDirection::South),
        "a corner is still a generous vertical target"
    );
    assert_eq!(
        agent.resize_direction(SEARCH_WIDTH, 600.0, 1.0, 300.0, false),
        None,
        "the fixed-width side is not draggable"
    );
    assert_eq!(
        agent.resize_direction(SEARCH_WIDTH, 600.0, 300.0, 1.0, true),
        None,
        "a maximised window cannot be resized"
    );
}

#[test]
fn opening_search_seeds_a_height_for_windows_conf() {
    let mut agent = agent();
    assert_eq!(agent.search_geometry.height(), None);
    agent.wake(AgentEvent::Tray(TrayAsk::Search));
    assert_eq!(
        agent.search_geometry.height(),
        Some(SEARCH_DEFAULT_CONTENT_H + DRAG_RAIL_H)
    );
}

/// The overlay paints before it is shown. Pinned because the delegation in
/// `run.rs` is hand-written over a trait full of defaults, so losing this
/// forward would not fail to compile — it would silently fall through to
/// `false` and the panel would flash empty on every hotkey.
#[test]
fn agent_search_paints_before_it_is_shown() {
    assert!(agent().paint_before_showing());
}

#[test]
fn voice_and_search_recreate_the_single_window_and_last_intent_wins() {
    let mut agent = agent();
    agent.open_overlay();
    assert!(agent.overlay.is_some());
    agent.open_voice_surface();
    assert!(agent.overlay.is_none());
    assert!(
        !agent.wants_window(),
        "old Search HWND gets one settle turn"
    );
    agent.maintain(Instant::now());
    assert!(!agent.wants_window());
    agent.maintain(Instant::now());
    assert!(agent.voice_window.has_surface());

    agent.open_overlay();
    agent.open_voice_surface();
    agent.open_overlay();
    agent.maintain(Instant::now());
    agent.maintain(Instant::now());
    assert!(agent.overlay.is_some());
    assert!(!agent.voice_window.has_surface());
}

#[test]
fn closing_during_voice_transition_cancels_only_transient_window_state() {
    let mut agent = agent();
    agent.open_overlay();
    agent.open_voice_surface();
    assert!(agent.window_transition.is_some());
    agent.voice_write = Some(VoiceWrite {
        intent: VoiceIntent::Startup(false),
        receipt: agent
            .settings
            .save_confirmed(koru_settings::GlobalPatch::default()),
    });
    agent.voice_shortcut_prepare = Some((7, "Ctrl+Alt+Space".into()));
    agent.voice_start = Some(VoiceStartStage::Closing);
    agent.close_requested();
    assert!(agent.window_transition.is_none());
    assert!(!agent.voice_window.has_surface());
    assert!(agent.voice_start.is_none());
    assert!(
        agent.voice_write.is_some(),
        "confirmed settings receipt survives close"
    );
    assert_eq!(
        agent.voice_shortcut_prepare,
        Some((7, "Ctrl+Alt+Space".into())),
        "prepared hotkey transaction survives close"
    );

    agent
        .voice_window
        .open_surface(koru_voice_ui::VoiceSurface::new(
            koru_i18n::Lang::En,
            agent.voice_model(),
        ));
    agent.voice_start = Some(VoiceStartStage::Starting);
    agent.window_transition = Some(WindowTransition::ReadyVoice);
    agent.handle_voice_action(koru_voice_ui::Action::Close);
    assert!(!agent.voice_window.has_surface());
    assert!(agent.voice_start.is_none());
    assert!(agent.window_transition.is_none());
    assert!(agent.voice_write.is_some());
    assert_eq!(
        agent.voice_shortcut_prepare.as_ref().map(|value| value.0),
        Some(7)
    );
}

#[test]
fn voice_close_discards_only_the_hint_and_cancel_remains_live_during_a_write() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Call {
        Discard,
        Cancel(crate::global_input::CancelReason),
    }
    struct Sink(Arc<Mutex<Vec<Call>>>);
    impl GlobalDictationSink for Sink {
        fn configure(&self, _policy: crate::dictation_coordinator::Policy) {}
        fn prepare_target(&self) {}
        fn start_prepared(&self) {}
        fn finish(&self) {}
        fn discard_prepared(&self) {
            self.0.lock().unwrap().push(Call::Discard);
        }
        fn cancel(&self, reason: crate::global_input::CancelReason) {
            self.0.lock().unwrap().push(Call::Cancel(reason));
        }
        fn press(
            &self,
            _local: Option<crate::event::LocalDictationOwner>,
            _key: crate::global_input::VirtualKey,
        ) {
        }
        fn local_state(
            &self,
            _owner: crate::event::LocalDictationOwner,
            _route: u64,
            _state: crate::dictation_coordinator::LocalState,
        ) {
        }
        fn shell_gone(&self, _connection: ConnectionId) {}
    }

    let mut agent = agent();
    let calls = Arc::new(Mutex::new(Vec::new()));
    agent.global_dictation = Some(Box::new(Sink(Arc::clone(&calls))));
    agent.voice_write = Some(VoiceWrite {
        intent: VoiceIntent::Startup(false),
        receipt: agent
            .settings
            .save_confirmed(koru_settings::GlobalPatch::default()),
    });
    agent.voice_shortcut_prepare = Some((17, "Ctrl+Alt+Space".into()));
    agent
        .voice_window
        .open_surface(koru_voice_ui::VoiceSurface::new(
            koru_i18n::Lang::En,
            agent.voice_model(),
        ));
    agent.handle_voice_action(koru_voice_ui::Action::Close);
    assert_eq!(&*calls.lock().unwrap(), &[Call::Discard]);
    assert!(agent.voice_write.is_some());
    assert_eq!(
        agent.voice_shortcut_prepare.as_ref().map(|value| value.0),
        Some(17)
    );

    agent
        .voice_window
        .open_surface(koru_voice_ui::VoiceSurface::new(
            koru_i18n::Lang::En,
            agent.voice_model(),
        ));
    agent.handle_voice_action(koru_voice_ui::Action::Cancel);
    assert_eq!(
        &*calls.lock().unwrap(),
        &[
            Call::Discard,
            Call::Cancel(crate::global_input::CancelReason::Explicit)
        ]
    );
    assert!(
        agent.voice_write.is_some(),
        "Cancel does not cancel settings"
    );

    agent.close_requested();
    assert_eq!(
        calls.lock().unwrap().last(),
        Some(&Call::Discard),
        "native close uses the same hint-discard contract"
    );
    assert!(agent.voice_write.is_some());
    assert_eq!(
        agent.voice_shortcut_prepare.as_ref().map(|value| value.0),
        Some(17)
    );
}

#[test]
fn opening_voice_while_active_skips_a_new_target_preparation() {
    let mut agent = agent();
    agent.global_dictation_snapshot.phase = crate::dictation_coordinator::Phase::Listening;
    agent.open_voice_surface();
    assert!(agent.voice_window.has_surface());
    assert!(!agent.voice_open_waiting);
    assert_eq!(agent.voice_model().phase, koru_voice_ui::Phase::Listening);
}

#[test]
fn voice_failure_presentation_follows_the_attempt_origin() {
    let refusal = crate::dictation_coordinator::Snapshot {
        refusal: Some(crate::dictation_coordinator::Refusal::Voice(
            koru_dictation::ErrorCode::WorkerUnavailable,
        )),
        display: Some(crate::global_input::TargetDisplay {
            work_area: (0, 0, 1920, 1080),
            dpi: 96,
        }),
        ..Default::default()
    };

    let mut hotkey = agent();
    hotkey.voice_attempt = Some(VoiceAttemptOrigin::Hotkey);
    assert!(hotkey.wake(AgentEvent::GlobalDictationChanged(refusal)));
    assert!(hotkey.voice_window.has_hud());
    assert!(!hotkey.voice_window.has_surface());
    assert!(hotkey.voice_window.maintenance_deadline().is_some());

    let mut surface = agent();
    surface.voice_attempt = Some(VoiceAttemptOrigin::Surface);
    surface.voice_start = Some(VoiceStartStage::Starting);
    assert!(surface.wake(AgentEvent::GlobalDictationChanged(refusal)));
    assert!(surface.voice_window.has_surface());
    assert!(!surface.voice_window.has_hud());
    assert_eq!(surface.voice_window.maintenance_deadline(), None);
}

#[test]
fn hud_consumes_every_click_and_only_the_exact_first_stop_click_finishes() {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Call {
        Finish,
        Cancel,
    }
    struct Sink(Arc<Mutex<Vec<Call>>>);
    impl GlobalDictationSink for Sink {
        fn configure(&self, _policy: crate::dictation_coordinator::Policy) {}
        fn prepare_target(&self) {}
        fn start_prepared(&self) {}
        fn discard_prepared(&self) {}
        fn cancel(&self, _reason: crate::global_input::CancelReason) {
            self.0.lock().unwrap().push(Call::Cancel);
        }
        fn finish(&self) {
            self.0.lock().unwrap().push(Call::Finish);
        }
        fn press(
            &self,
            _local: Option<crate::event::LocalDictationOwner>,
            _key: crate::global_input::VirtualKey,
        ) {
        }
        fn local_state(
            &self,
            _owner: crate::event::LocalDictationOwner,
            _route: u64,
            _state: crate::dictation_coordinator::LocalState,
        ) {
        }
        fn shell_gone(&self, _connection: ConnectionId) {}
    }

    let mut agent = agent();
    let calls = Arc::new(Mutex::new(Vec::new()));
    agent.global_dictation = Some(Box::new(Sink(Arc::clone(&calls))));
    let snapshot = crate::dictation_coordinator::Snapshot {
        phase: crate::dictation_coordinator::Phase::Listening,
        display: Some(crate::global_input::TargetDisplay {
            work_area: (0, 0, 1920, 1080),
            dpi: 96,
        }),
        ..Default::default()
    };
    assert!(agent.voice_window.sync_hud(snapshot, false, Instant::now()));
    let mut engine = TextEngine::new();
    assert!(agent.click(&mut engine, 10.0, 10.0, false));
    assert!(agent.double_click(&mut engine, 10.0, 10.0));
    assert!(calls.lock().unwrap().is_empty());

    assert!(agent.click(
        &mut engine,
        koru_voice_ui::HUD_WIDTH - 16.0,
        koru_voice_ui::HUD_HEIGHT * 0.5,
        false,
    ));
    assert!(agent.double_click(
        &mut engine,
        koru_voice_ui::HUD_WIDTH - 16.0,
        koru_voice_ui::HUD_HEIGHT * 0.5,
    ));
    assert_eq!(&*calls.lock().unwrap(), &[Call::Finish]);
}

#[test]
fn live_voice_hud_wins_over_a_search_request_without_interrupting_capture() {
    let mut agent = agent();
    let snapshot = crate::dictation_coordinator::Snapshot {
        phase: crate::dictation_coordinator::Phase::Listening,
        display: Some(crate::global_input::TargetDisplay {
            work_area: (0, 0, 1920, 1080),
            dpi: 96,
        }),
        ..Default::default()
    };
    agent.global_dictation_snapshot = snapshot;
    assert!(agent.voice_window.sync_hud(snapshot, false, Instant::now()));
    agent.open_overlay();
    assert!(agent.voice_window.has_hud());
    assert!(agent.overlay.is_none());
    assert!(agent.window_transition.is_none());
}

#[test]
fn voice_window_prepares_before_open_and_starts_only_after_two_close_turns() {
    #[derive(Clone)]
    struct Sink(Arc<Mutex<Vec<&'static str>>>);
    impl GlobalDictationSink for Sink {
        fn configure(&self, _policy: crate::dictation_coordinator::Policy) {}
        fn prepare_target(&self) {
            self.0.lock().unwrap().push("prepare");
        }
        fn start_prepared(&self) {
            self.0.lock().unwrap().push("start");
        }
        fn finish(&self) {}
        fn discard_prepared(&self) {
            self.0.lock().unwrap().push("discard");
        }
        fn cancel(&self, _reason: crate::global_input::CancelReason) {}
        fn press(
            &self,
            _local: Option<crate::event::LocalDictationOwner>,
            _key: crate::global_input::VirtualKey,
        ) {
        }
        fn local_state(
            &self,
            _owner: crate::event::LocalDictationOwner,
            _route: u64,
            _state: crate::dictation_coordinator::LocalState,
        ) {
        }
        fn shell_gone(&self, _connection: ConnectionId) {}
    }

    let mut agent = agent();
    let seen = Arc::new(Mutex::new(Vec::new()));
    agent.global_dictation = Some(Box::new(Sink(Arc::clone(&seen))));
    agent.open_voice_surface_fresh();
    assert_eq!(&*seen.lock().unwrap(), &["discard", "prepare"]);
    assert!(!agent.voice_window.has_surface());
    assert!(agent.voice_open_waiting);

    agent.wake(AgentEvent::GlobalDictationChanged(
        crate::dictation_coordinator::Snapshot {
            prepared: true,
            ..Default::default()
        },
    ));
    assert!(agent.voice_window.has_surface());
    agent.handle_voice_action(koru_voice_ui::Action::Start);
    assert!(!agent.voice_window.has_surface());
    agent.maintain(Instant::now());
    assert_eq!(&*seen.lock().unwrap(), &["discard", "prepare"]);
    agent.maintain(Instant::now());
    assert_eq!(&*seen.lock().unwrap(), &["discard", "prepare", "start"]);

    agent.wake(AgentEvent::GlobalDictationChanged(
        crate::dictation_coordinator::Snapshot {
            refusal: Some(crate::dictation_coordinator::Refusal::Voice(
                koru_dictation::ErrorCode::WorkerUnavailable,
            )),
            ..Default::default()
        },
    ));
    assert_eq!(
        agent.voice_problem,
        Some(koru_voice_ui::Problem::WorkerUnavailable)
    );
    assert!(
        agent.voice_open_waiting,
        "failure reacquires a fresh hint before reopening"
    );
    agent.wake(AgentEvent::GlobalDictationChanged(
        crate::dictation_coordinator::Snapshot {
            prepared: true,
            ..Default::default()
        },
    ));
    assert_eq!(
        agent.voice_window.surface().unwrap().model().problem,
        Some(koru_voice_ui::Problem::WorkerUnavailable),
        "the close/reprepare barrier preserves the actionable failure"
    );
}

#[test]
fn first_enable_can_remain_pending_without_creating_a_window() {
    let mut agent = agent();
    assert!(!agent.voice_window.has_surface());
    agent.start_voice_write(VoiceIntent::Enable);
    assert!(!agent.voice_window.has_surface());
    assert!(agent.voice_write.is_some());
    assert_eq!(
        agent.voice_model().pending,
        Some(koru_voice_ui::Pending::Enable)
    );
    assert_eq!(
        agent.settings.authoritative().global_dictation_enabled,
        None,
        "pending is not reported as durable before its confirmed receipt"
    );
}

#[test]
fn requested_shortcut_survives_the_os_prepare_until_receipt_settles() {
    let mut agent = agent();
    agent.dictation_shortcut = crate::tray::Shortcut::Chord("Ctrl+Shift+D".into());
    agent.voice_write = Some(VoiceWrite {
        intent: VoiceIntent::Shortcut("Ctrl+Alt+Space".into(), 9),
        receipt: agent
            .settings
            .save_confirmed(koru_settings::GlobalPatch::default()),
    });
    let model = agent.voice_model();
    assert_eq!(model.shortcut.as_deref(), Some("Ctrl+Shift+D"));
    assert_eq!(model.requested_shortcut.as_deref(), Some("Ctrl+Alt+Space"));
    assert_eq!(model.pending, Some(koru_voice_ui::Pending::Shortcut));
}

#[test]
fn shortcut_settlement_keeps_effective_and_pending_truth_until_the_pump_answers() {
    let mut agent = agent();
    agent.dictation_shortcut = crate::tray::Shortcut::Chord("Ctrl+Alt+Space".into());
    agent.hotkey_settling = Some((9, "Ctrl+Alt+Space".into()));
    let model = agent.voice_model();
    assert_eq!(model.shortcut.as_deref(), Some("Ctrl+Alt+Space"));
    assert_eq!(model.requested_shortcut.as_deref(), Some("Ctrl+Alt+Space"));
    assert_eq!(model.pending, Some(koru_voice_ui::Pending::Shortcut));
}

#[test]
fn stale_hotkey_events_cannot_replace_newer_registration_truth() {
    let mut agent = agent();
    agent.dictation_shortcut = crate::tray::Shortcut::Chord("Ctrl+Shift+D".into());
    agent.hotkey_preparing = Some((11, "Ctrl+Shift+C".into()));
    assert!(!agent.wake(AgentEvent::DictationHotkeyPrepared {
        generation: 10,
        requested: "Ctrl+Alt+B".into(),
        active: Some("Ctrl+Alt+B".into()),
        prepared: true,
    }));
    assert_eq!(
        agent.dictation_shortcut,
        crate::tray::Shortcut::Chord("Ctrl+Shift+D".into())
    );

    agent.hotkey_settling = Some((11, "Ctrl+Shift+C".into()));
    assert!(!agent.wake(AgentEvent::DictationHotkeySettled {
        generation: 10,
        active: Some("Ctrl+Alt+B".into()),
    }));
    assert_eq!(
        agent.dictation_shortcut,
        crate::tray::Shortcut::Chord("Ctrl+Shift+D".into())
    );
    assert!(!agent.wake(AgentEvent::DictationHotkeySettled {
        generation: 11,
        active: Some("Ctrl+Shift+C".into()),
    }));
    assert_eq!(
        agent.dictation_shortcut,
        crate::tray::Shortcut::Chord("Ctrl+Shift+C".into())
    );
}

#[test]
fn voice_keyboard_navigation_invalidates_without_dispatching_an_action() {
    let mut agent = agent();
    let mut model = agent.voice_model();
    model.saved = koru_voice_ui::Saved::Enabled;
    model.effective_enabled = true;
    model.start_on_login = true;
    agent
        .voice_window
        .open_surface(koru_voice_ui::VoiceSurface::new(koru_i18n::Lang::En, model));
    let mut engine = TextEngine::new();
    assert!(agent.key(
        &mut engine,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::empty(),
    ));
    assert!(
        agent.voice_write.is_none(),
        "Tab only changes presentation focus"
    );
}

#[test]
fn stale_consent_refresh_completion_cannot_reopen_the_gate() {
    let mut agent = agent();
    agent.consent_refresh_generation = 2;
    agent.global_consent = None;
    let stale = koru_voice_consent::Granted::recorded(
        koru_voice_consent::Terms {
            model: "model".into(),
            version: "1".into(),
            licence: "Apache-2.0".into(),
            attribution: "test".into(),
            download_bytes: 1,
        },
        1,
    );
    assert!(!agent.wake(AgentEvent::GlobalConsentRefreshed {
        generation: 1,
        consent: Some(stale),
    }));
    assert!(agent.global_consent.is_none());
}

#[test]
fn a_peer_signed_revocation_is_durable_and_removes_local_authorization() {
    use koru_device::TrustRepository as _;

    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(root.path().to_owned());
    let local = crate::identity::IdentityManager::open(&paths, 10).unwrap();
    let peer_key = koru_device::DeviceSigningKey::generate();
    let peer_endpoint = koru_device::ReplicationEndpointSecret::generate();
    let peer = koru_device::DeviceDescriptorV1::new(
        koru_device::DeviceId::new(),
        "Windows".into(),
        &peer_key,
        peer_endpoint.endpoint_id(),
        11,
    );
    let invitation = koru_pairing::PairingInvitationV1::new(peer.clone(), None);
    let transcript = koru_pairing::PairingTranscriptV1::new(&invitation, local.descriptor.clone());
    let prepared = koru_pairing::PreparedPairingV1::sign_for_test(
        transcript,
        &peer_key,
        &local.signing_key,
        12,
    )
    .unwrap();
    let mut repository =
        crate::identity::SqliteTrustRepository::open(&paths.identity_db()).unwrap();
    repository
        .commit_pairing(&prepared, local.descriptor.device_id)
        .unwrap();

    let local_id = local.descriptor.device_id;
    let revocation = koru_device::RevocationV1::sign(
        prepared.transcript.relationship_id,
        local_id,
        peer.device_id,
        koru_device::RecordId::new(),
        13,
        &peer_key,
    );
    let mut agent = Agent::new(paths.clone(), local_id.to_storage(), "instance".into());
    agent.set_sync_identity(local, 1, 1);
    agent
        .apply_remote_trust_record(
            peer.device_id,
            koru_device::TrustRecord::Revocation(revocation),
        )
        .unwrap();

    let view = repository.trust_view().unwrap();
    assert_eq!(view.revocations.len(), 1);
    assert!(linked_devices(&view, None).is_empty());
    assert_eq!(agent.sync_status.trusted_device_count, 0);
    // Three revisions: the pairing, this device describing its own build on
    // becoming sync-ready, and the revocation.
    assert_eq!(agent.sync_status.authorization_generation, 3);
}

#[test]
fn a_peer_profile_deletion_is_durable_before_duplicate_delivery_is_accepted() {
    use koru_device::TrustRepository as _;

    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(root.path().to_owned());
    let local = crate::identity::IdentityManager::open(&paths, 10).unwrap();
    let peer_key = koru_device::DeviceSigningKey::generate();
    let peer = koru_device::DeviceDescriptorV1::new(
        koru_device::DeviceId::new(),
        "Phone".into(),
        &peer_key,
        koru_device::ReplicationEndpointSecret::generate().endpoint_id(),
        11,
    );
    let invitation = koru_pairing::PairingInvitationV1::new(peer.clone(), None);
    let transcript = koru_pairing::PairingTranscriptV1::new(&invitation, local.descriptor.clone());
    let prepared = koru_pairing::PreparedPairingV1::sign_for_test(
        transcript,
        &peer_key,
        &local.signing_key,
        12,
    )
    .unwrap();
    crate::identity::SqliteTrustRepository::open(&paths.identity_db())
        .unwrap()
        .commit_pairing(&prepared, local.descriptor.device_id)
        .unwrap();

    let profile = koru_store_core::ProfileId::new();
    let operation = uuid::Uuid::new_v4();
    let lifecycle = koru_device::ProfileLifecycleV1::sign(
        koru_device::RecordId::new(),
        peer.device_id,
        profile,
        koru_device::ProfileLifecycleStateV1::Deleted,
        1,
        operation,
        13,
        &peer_key,
    );
    let local_id = local.descriptor.device_id;
    let mut agent = Agent::new(paths.clone(), local_id.to_storage(), "instance".into());
    agent.set_sync_identity(local, 1, 1);
    for _ in 0..2 {
        agent
            .apply_remote_trust_record(
                peer.device_id,
                koru_device::TrustRecord::ProfileLifecycle(lifecycle.clone()),
            )
            .expect("both first delivery and exact replay are acknowledged");
    }

    let view = crate::identity::SqliteTrustRepository::open(&paths.identity_db())
        .unwrap()
        .trust_view()
        .unwrap();
    assert!(view.profile_is_deleted(profile));
    let cleanup =
        crate::account::lifecycle::LifecycleRepository::open(&paths.profile_lifecycle_db())
            .unwrap()
            .get(profile)
            .unwrap()
            .expect("receipt is durable before the transport can be acknowledged");
    assert_eq!(cleanup.operation.as_uuid(), operation);
    assert_eq!(
        cleanup.stage,
        koru_agent_wire::ProfileCleanupStage::HostedCommitted
    );
}

/// A pairing between two devices, signed by both, ready to commit on either.
fn paired(
    inviter_name: &str,
    joiner_name: &str,
) -> (
    koru_device::DeviceSigningKey,
    koru_device::DeviceDescriptorV1,
    koru_device::DeviceSigningKey,
    koru_device::DeviceDescriptorV1,
    Vec<koru_device::TrustRecord>,
) {
    let inviter_key = koru_device::DeviceSigningKey::generate();
    let joiner_key = koru_device::DeviceSigningKey::generate();
    let inviter = koru_device::DeviceDescriptorV1::new(
        koru_device::DeviceId::new(),
        inviter_name.into(),
        &inviter_key,
        koru_device::ReplicationEndpointSecret::generate().endpoint_id(),
        11,
    );
    let joiner = koru_device::DeviceDescriptorV1::new(
        koru_device::DeviceId::new(),
        joiner_name.into(),
        &joiner_key,
        koru_device::ReplicationEndpointSecret::generate().endpoint_id(),
        11,
    );
    let relationship_id = koru_device::RelationshipId::new();
    let relationship = koru_device::TrustRelationshipV1::sign(
        relationship_id,
        inviter.clone(),
        joiner.clone(),
        [9; 32],
        12,
        &inviter_key,
        &joiner_key,
    );
    let records = vec![
        koru_device::TrustRecord::Relationship(relationship),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            relationship_id,
            joiner.device_id,
            inviter.device_id,
            koru_device::RecordId::new(),
            13,
            &inviter_key,
        )),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            relationship_id,
            inviter.device_id,
            joiner.device_id,
            koru_device::RecordId::new(),
            13,
            &joiner_key,
        )),
    ];
    (inviter_key, inviter, joiner_key, joiner, records)
}

#[test]
fn a_gossiped_device_list_authorizes_the_transitive_peer() {
    use koru_device::TrustRepository as _;

    // A pairs with B. Separately, B pairs with C. B's trust records then
    // arrive on A as a gossip batch.
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(root.path().to_owned());
    let local = crate::identity::IdentityManager::open(&paths, 10).unwrap();
    let (b_key, b, c_key, c, mut bc_records) = paired("Windows", "Android");
    let profile = koru_store_core::ProfileId::new();
    bc_records.push(koru_device::TrustRecord::ProfileLifecycle(
        koru_device::ProfileLifecycleV1::sign(
            koru_device::RecordId::new(),
            c.device_id,
            profile,
            koru_device::ProfileLifecycleStateV1::Deleted,
            1,
            uuid::Uuid::new_v4(),
            14,
            &c_key,
        ),
    ));
    let mut repository =
        crate::identity::SqliteTrustRepository::open(&paths.identity_db()).unwrap();

    // The local A⇔B pairing, applied the way pairing commits it.
    let ab_relationship_id = koru_device::RelationshipId::new();
    let ab = koru_device::TrustRelationshipV1::sign(
        ab_relationship_id,
        local.descriptor.clone(),
        b.clone(),
        [7; 32],
        12,
        &local.signing_key,
        &b_key,
    );
    for record in [
        koru_device::TrustRecord::Relationship(ab),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            ab_relationship_id,
            b.device_id,
            local.descriptor.device_id,
            koru_device::RecordId::new(),
            13,
            &local.signing_key,
        )),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            ab_relationship_id,
            local.descriptor.device_id,
            b.device_id,
            koru_device::RecordId::new(),
            13,
            &b_key,
        )),
    ] {
        let view = repository.trust_view().unwrap();
        let verified = koru_device::verify_gossip_record(&view, record).unwrap();
        repository.apply(verified).unwrap();
    }

    let mut agent = Agent::new(
        paths.clone(),
        local.descriptor.device_id.to_storage(),
        "i".into(),
    );
    agent.set_sync_identity(local, 1, 1);
    agent
        .apply_remote_trust_records(b.device_id, bc_records)
        .unwrap();

    let view = repository.trust_view().unwrap();
    assert!(
        view.profile_is_deleted(profile),
        "the transitive peer's authorized lifecycle record follows its relationship and grants"
    );
    let peers = koru_device::Authorization::devices(&view);
    let transitive = peers
        .iter()
        .find(|peer| peer.device_id == c.device_id)
        .expect("C is authorized through B");
    assert_eq!(transitive.via, Some(b.device_id));

    let listed = linked_devices(&view, None);
    assert_eq!(listed.len(), 2);
    let introduced = listed
        .iter()
        .find(|device| device.device_id == c.device_id.to_storage())
        .unwrap();
    assert_eq!(introduced.display_name, "Android");
    assert_eq!(introduced.introduced_by.as_deref(), Some("Windows"));
    let direct = listed
        .iter()
        .find(|device| device.device_id == b.device_id.to_storage())
        .unwrap();
    assert_eq!(direct.introduced_by, None);
    assert_eq!(agent.sync_status.trusted_device_count, 2);
}

#[test]
fn a_rename_signs_a_gossipable_record_and_renames_the_next_pairing() {
    use koru_device::TrustRepository as _;

    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(root.path().to_owned());
    let local = crate::identity::IdentityManager::open(&paths, 10).unwrap();
    let mut agent = Agent::new(
        paths.clone(),
        local.descriptor.device_id.to_storage(),
        "i".into(),
    );
    agent.set_sync_identity(local, 1, 1);

    let response = agent.rename_device("Studio PC");
    let Response::LinkedDevices {
        this_device: Some(this_device),
        ..
    } = response
    else {
        panic!("a rename answers with the refreshed list: {response:?}")
    };
    assert_eq!(this_device.display_name, "Studio PC");

    // The rename is a gossipable trust record, and the local descriptor
    // carries the new name into the next pairing offer.
    let repository = crate::identity::SqliteTrustRepository::open(&paths.identity_db()).unwrap();
    let view = repository.trust_view().unwrap();
    assert_eq!(view.names.len(), 1);
    assert_eq!(
        view.display_name_of(view.local_device.device_id).as_deref(),
        Some("Studio PC")
    );
    assert_eq!(view.local_device.display_name, "Studio PC");

    // Validation happens before anything is signed.
    assert!(matches!(
        agent.rename_device("   "),
        Response::Failed { .. }
    ));
    assert_eq!(repository.trust_view().unwrap().names.len(), 1);
}

#[test]
fn a_gossiped_rename_travels_with_the_device_list() {
    use koru_device::TrustRepository as _;

    // A pairs with B; B renames itself; the rename record reaches A in a
    // gossip batch and A's list shows the new name.
    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(root.path().to_owned());
    let local = crate::identity::IdentityManager::open(&paths, 10).unwrap();
    let b_key = koru_device::DeviceSigningKey::generate();
    let b = koru_device::DeviceDescriptorV1::new(
        koru_device::DeviceId::new(),
        "Windows".into(),
        &b_key,
        koru_device::ReplicationEndpointSecret::generate().endpoint_id(),
        11,
    );
    let repository = crate::identity::SqliteTrustRepository::open(&paths.identity_db()).unwrap();

    let ab_relationship_id = koru_device::RelationshipId::new();
    let batch = vec![
        koru_device::TrustRecord::Relationship(koru_device::TrustRelationshipV1::sign(
            ab_relationship_id,
            local.descriptor.clone(),
            b.clone(),
            [7; 32],
            12,
            &local.signing_key,
            &b_key,
        )),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            ab_relationship_id,
            b.device_id,
            local.descriptor.device_id,
            koru_device::RecordId::new(),
            13,
            &local.signing_key,
        )),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            ab_relationship_id,
            local.descriptor.device_id,
            b.device_id,
            koru_device::RecordId::new(),
            13,
            &b_key,
        )),
        koru_device::TrustRecord::DeviceName(koru_device::DeviceNameV1::sign(
            koru_device::RecordId::new(),
            b.device_id,
            "Workshop".into(),
            20,
            &b_key,
        )),
    ];
    let mut agent = Agent::new(
        paths.clone(),
        local.descriptor.device_id.to_storage(),
        "i".into(),
    );
    agent.set_sync_identity(local, 1, 1);
    agent
        .apply_remote_trust_records(b.device_id, batch)
        .unwrap();

    let view = repository.trust_view().unwrap();
    let listed = linked_devices(&view, None);
    assert_eq!(
        listed
            .iter()
            .find(|device| device.device_id == b.device_id.to_storage())
            .unwrap()
            .display_name,
        "Workshop"
    );
}

#[test]
fn a_device_advertises_its_build_once_and_one_description_per_device_survives() {
    use koru_device::TrustRepository as _;

    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(root.path().to_owned());
    let local = crate::identity::IdentityManager::open(&paths, 10).unwrap();
    let local_id = local.descriptor.device_id;
    let mut agent = Agent::new(paths.clone(), local_id.to_storage(), "i".into());
    // Becoming sync-ready is what advertises this build.
    agent.set_sync_identity(local, 1, 1);

    let repository = crate::identity::SqliteTrustRepository::open(&paths.identity_db()).unwrap();
    let view = repository.trust_view().unwrap();
    assert_eq!(view.builds.len(), 1);
    let own = view
        .build_of(local_id)
        .expect("this device described itself");
    assert_eq!(own, crate::build_stamp::payload());
    let this = this_device(&view).unwrap();
    assert_eq!(this.build_label.as_deref(), Some(own.label().as_str()));
    assert_eq!(this.build_severity, None);
    assert_eq!(
        own.peer_protocol,
        Some(koru_sync_control::PEER_PROTOCOL_VERSION),
        "the stamp carries the protocol number this agent speaks"
    );

    // The same build again writes nothing: a launch is not a change.
    let revision = view.effective_revision;
    agent.advertise_build(&own).unwrap();
    let view = repository.trust_view().unwrap();
    assert_eq!(view.builds.len(), 1);
    assert_eq!(view.effective_revision, revision);

    // A changed build supersedes the row rather than adding one.
    let mut next = own.clone();
    next.app_version = format!("{}+next", own.app_version);
    agent.advertise_build(&next).unwrap();
    let view = repository.trust_view().unwrap();
    assert_eq!(view.builds.len(), 1, "superseded descriptions are pruned");
    assert_eq!(view.build_of(local_id).as_ref(), Some(&next));
    assert_eq!(view.effective_revision, revision + 1);
}

#[test]
fn a_peers_build_travels_with_the_device_list_and_a_different_one_is_marked() {
    use koru_device::TrustRepository as _;

    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(root.path().to_owned());
    let local = crate::identity::IdentityManager::open(&paths, 10).unwrap();
    let b_key = koru_device::DeviceSigningKey::generate();
    let b = koru_device::DeviceDescriptorV1::new(
        koru_device::DeviceId::new(),
        "Pixel".into(),
        &b_key,
        koru_device::ReplicationEndpointSecret::generate().endpoint_id(),
        11,
    );
    let repository = crate::identity::SqliteTrustRepository::open(&paths.identity_db()).unwrap();
    let ab_relationship_id = koru_device::RelationshipId::new();
    let phone = koru_device::BuildPayload {
        // Gradle's form of the workspace version — the phone and this
        // agent are built from one tree, which is what "same" is about.
        app_version: format!("{} (1)", env!("CARGO_PKG_VERSION")),
        channel: "debug".into(),
        branch: None,
        commit: None,
        os: Some("Android 14".into()),
        os_version: Some("14 (API 34)".into()),
        peer_protocol: Some(koru_sync_control::PEER_PROTOCOL_VERSION),
    };
    let batch = vec![
        koru_device::TrustRecord::Relationship(koru_device::TrustRelationshipV1::sign(
            ab_relationship_id,
            local.descriptor.clone(),
            b.clone(),
            [7; 32],
            12,
            &local.signing_key,
            &b_key,
        )),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            ab_relationship_id,
            b.device_id,
            local.descriptor.device_id,
            koru_device::RecordId::new(),
            13,
            &local.signing_key,
        )),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            ab_relationship_id,
            local.descriptor.device_id,
            b.device_id,
            koru_device::RecordId::new(),
            13,
            &b_key,
        )),
        koru_device::TrustRecord::DeviceBuild(koru_device::DeviceBuildV1::sign(
            koru_device::RecordId::new(),
            b.device_id,
            &phone,
            20,
            &b_key,
        )),
        // What a newer device would send: a kind this build does not know.
        // It is skipped on its own and the rest of the batch still lands.
        serde_json::from_str(r#"{"kind":"device_hologram","shape":"cube"}"#).unwrap(),
    ];
    let mut agent = Agent::new(
        paths.clone(),
        local.descriptor.device_id.to_storage(),
        "i".into(),
    );
    agent.set_sync_identity(local, 1, 1);
    agent
        .apply_remote_trust_records(b.device_id, batch)
        .unwrap();

    let view = repository.trust_view().unwrap();
    let listed = linked_devices(&view, None);
    let pixel = listed
        .iter()
        .find(|device| device.device_id == b.device_id.to_storage())
        .expect("the batch's relationship linked the phone");
    assert_eq!(
        pixel.build_label.as_deref(),
        Some(format!("{} (1) · debug · Android 14", env!("CARGO_PKG_VERSION")).as_str())
    );
    // This agent's own stamp is cargo's workspace version and the phone's is
    // Gradle's "<same> (1)": the same three numbers, so nothing is marked — a phone
    // beside a desktop built from one tree was the false positive this
    // comparison replaced.
    assert_eq!(pixel.build_drift.as_deref(), Some("same"));
    assert_eq!(pixel.build_severity, None);
    assert_eq!(pixel.update_needed_on, None);

    let status = koru_sync_control::ControlStatus {
        peers: vec![koru_sync_control::PeerStatus {
            device_id: b.device_id,
            state: koru_sync_control::PeerState::ConnectedDirect,
            last_seen_millis: Some(40),
            last_state_change_millis: Some(42),
        }],
        ..Default::default()
    };
    let listed = linked_devices(&view, Some(&status));
    let pixel = listed
        .iter()
        .find(|device| device.device_id == b.device_id.to_storage())
        .unwrap();
    assert_eq!(pixel.last_sync_millis, Some(42));
    assert!(
        pixel.history.is_empty(),
        "device lists do not carry history pages"
    );

    // The phone updates to a build on the next protocol number: it stays in
    // the list, and its row says the update is on this side.
    let mut newer = phone.clone();
    newer.app_version = "0.1.1 (2)".into();
    newer.peer_protocol = Some(koru_sync_control::PEER_PROTOCOL_VERSION + 1);
    agent
        .apply_remote_trust_records(
            b.device_id,
            vec![koru_device::TrustRecord::DeviceBuild(
                koru_device::DeviceBuildV1::sign(
                    koru_device::RecordId::new(),
                    b.device_id,
                    &newer,
                    30,
                    &b_key,
                ),
            )],
        )
        .unwrap();
    let view = repository.trust_view().unwrap();
    let listed = linked_devices(&view, None);
    let pixel = listed
        .iter()
        .find(|device| device.device_id == b.device_id.to_storage())
        .expect("an incompatible device is still a linked device");
    assert_eq!(pixel.build_drift.as_deref(), Some("incompatible"));
    assert_eq!(pixel.build_severity.as_deref(), Some("danger"));
    assert_eq!(pixel.update_needed_on.as_deref(), Some("this_device"));
}

#[test]
fn a_forged_record_in_a_gossip_batch_is_skipped_not_trusted() {
    use koru_device::TrustRepository as _;

    let root = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(root.path().to_owned());
    let local = crate::identity::IdentityManager::open(&paths, 10).unwrap();
    let (b_key, b, _, c, mut bc_records) = paired("Windows", "Android");
    let mut repository =
        crate::identity::SqliteTrustRepository::open(&paths.identity_db()).unwrap();

    let ab_relationship_id = koru_device::RelationshipId::new();
    let ab = koru_device::TrustRelationshipV1::sign(
        ab_relationship_id,
        local.descriptor.clone(),
        b.clone(),
        [7; 32],
        12,
        &local.signing_key,
        &b_key,
    );
    for record in [
        koru_device::TrustRecord::Relationship(ab),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            ab_relationship_id,
            b.device_id,
            local.descriptor.device_id,
            koru_device::RecordId::new(),
            13,
            &local.signing_key,
        )),
        koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
            ab_relationship_id,
            local.descriptor.device_id,
            b.device_id,
            koru_device::RecordId::new(),
            13,
            &b_key,
        )),
    ] {
        let view = repository.trust_view().unwrap();
        let verified = koru_device::verify_gossip_record(&view, record).unwrap();
        repository.apply(verified).unwrap();
    }

    // Forge C's grant: the batch carries a record claiming it, signed by an
    // unrelated key, *instead of* the genuine one.
    let bc_relationship_id = match &bc_records[0] {
        koru_device::TrustRecord::Relationship(relationship) => relationship.relationship_id,
        _ => unreachable!(),
    };
    bc_records.remove(2);
    let forged = koru_device::TrustRecord::AllDataGrant(koru_device::AllDataGrantV1::sign(
        bc_relationship_id,
        b.device_id,
        c.device_id,
        koru_device::RecordId::new(),
        14,
        &koru_device::DeviceSigningKey::generate(),
    ));
    bc_records.push(forged);

    let mut agent = Agent::new(
        paths.clone(),
        local.descriptor.device_id.to_storage(),
        "i".into(),
    );
    agent.set_sync_identity(local, 1, 1);
    assert!(
        agent
            .apply_remote_trust_records(b.device_id, bc_records)
            .is_err()
    );

    // The genuine records landed; the forged one did not. Without C's grant
    // the B⇔C edge is half-signed, so C stays out of the mesh.
    let view = repository.trust_view().unwrap();
    assert_eq!(view.relationships.len(), 2);
    let peers = koru_device::Authorization::devices(&view);
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].device_id, b.device_id);
}

#[test]
fn a_store_failure_becomes_visible_in_the_open_search_window() {
    let mut agent = agent();
    agent.wake(AgentEvent::Tray(TrayAsk::Search));

    assert!(agent.wake(AgentEvent::StoreAvailability {
        warning: Some("Koru Store was not found.".into()),
        running: false,
    }));
    assert_eq!(
        agent
            .overlay
            .as_ref()
            .and_then(|surface| surface.availability_warning()),
        Some("Koru Store was not found.")
    );
    assert_eq!(agent.store_running(), Some(false));

    assert!(agent.wake(AgentEvent::StoreAvailability {
        warning: None,
        running: true,
    }));
    assert_eq!(
        agent
            .overlay
            .as_ref()
            .and_then(|surface| surface.availability_warning()),
        None
    );
    assert_eq!(agent.store_running(), Some(true));
}

#[test]
fn a_missing_sibling_app_has_specific_actionable_copy() {
    let warning = shell_warning(&std::io::Error::other(
        "C:/koru.exe is not beside this executable",
    ));
    assert!(warning.contains("Koru app was not found beside Koru Agent"));
    assert!(warning.contains("Build or install"));
}

#[test]
fn a_missing_ai_chat_sibling_reopens_the_overlay_with_actionable_copy() {
    let mut agent = agent();
    let changed = agent.wake(AgentEvent::AiChatOpened {
        error: Some("C:/Koru/koru-ai-chat.exe is not beside this executable".into()),
    });

    assert!(changed);
    assert!(agent.overlay.is_some(), "Agent owns the fallback surface");
    let warning = agent
        .overlay
        .as_ref()
        .and_then(|surface| surface.host_warning())
        .expect("the failed gesture has visible copy");
    assert!(warning.contains("Koru AI Chat was not found beside Koru Agent"));
    assert!(warning.contains("Reinstall this Koru build"));
    assert!(warning.contains("try again"));
}

#[test]
fn a_successful_ai_chat_open_does_not_create_an_agent_window() {
    let mut agent = agent();
    assert!(!agent.wake(AgentEvent::AiChatOpened { error: None }));
    assert!(agent.overlay.is_none());
}

/// **The guard that keeps this file off a real store.** Opening the overlay
/// ensures one, which spawns a `--worker gateway` child of this same image
/// against whatever root the `Paths` name. Nothing here
/// asserts that a connection *fails*, which would pass on CI and fail on the
/// machine of anyone with Koru running; it asserts that the directory the fake
/// root names was never created, which is the observable consequence.
#[test]
fn opening_the_overlay_under_test_starts_no_store() {
    let root = std::path::PathBuf::from("C:/koru-agent-test-must-not-exist");
    let mut agent = Agent::new(
        Paths::for_root(root.clone()),
        "device".into(),
        "instance".into(),
    );
    agent.wake(AgentEvent::Tray(TrayAsk::Search));
    assert!(agent.overlay.is_some(), "the overlay still opens");
    assert!(!root.exists(), "a store was started on {}", root.display());
}

#[test]
fn escape_on_an_empty_search_hides_the_overlay() {
    let mut agent = agent();
    agent.wake(AgentEvent::Tray(TrayAsk::Search));
    let mut engine = TextEngine::new();

    assert!(agent.key(
        &mut engine,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    ));
    assert_eq!(agent.take_window_request(), Some(WindowRequest::Close));
    assert!(agent.overlay.is_none());
}

#[test]
fn a_shells_transport_pid_survives_until_hello() {
    let mut agent = agent();
    let connection = ConnectionId(17);
    let profile = koru_store_core::ids::ProfileId::new();
    let workspace = koru_store_core::ids::WorkspaceId::new();
    // Exercise the less convenient event order too: Hello made a registry row
    // before the accept notice carrying the authenticated pid reached the loop.
    agent.registry.hello(connection, None, profile, workspace);
    agent.wake(AgentEvent::ShellArrived {
        connection,
        pid: Some(4242),
    });
    assert_eq!(agent.peer_pids.get(&connection), Some(&Some(4242)));
    assert_eq!(
        agent.registry.route(profile, workspace).unwrap().pid,
        Some(4242)
    );

    agent.drop_connection(connection);
    assert!(!agent.peer_pids.contains_key(&connection));
}

#[test]
fn a_workspace_launch_names_the_exact_destination_and_no_search_content() {
    let profile = koru_store_core::ids::ProfileId::new();
    let workspace = koru_store_core::ids::WorkspaceId::new();

    assert_eq!(
        shell_launch_arguments(
            profile,
            workspace,
            Some(std::path::Path::new("C:/root")),
            None,
        ),
        vec![
            "--store-profile".to_string(),
            profile.to_storage(),
            "--workspace".to_string(),
            workspace.to_storage(),
            "--root".to_string(),
            "C:/root".to_string(),
        ]
    );
}

#[test]
fn tray_and_missed_summary_share_the_all_reminders_destination() {
    let mut agent = agent();
    let connection = ConnectionId(35);
    let (shell, queue) = Connection::for_test(connection);
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    agent.connections.push(shell);
    agent.said(
        connection,
        1,
        Request::Hello {
            profile: profile.to_storage(),
            workspace: workspace.to_storage(),
            profile_name: String::new(),
            workspace_name: String::new(),
            pinned: Vec::new(),
            reopen_claim: None,
        },
        Duration::ZERO,
    );
    assert!(matches!(
        queue.recv(),
        Ok(FromAgent::Response {
            id: 1,
            body: Response::Ok
        })
    ));

    for event in [
        AgentEvent::Tray(TrayAsk::Reminders),
        AgentEvent::ReminderProfileOpen { profile },
    ] {
        agent.wake(event);
        assert!(
            (0..8).any(|_| matches!(
                queue.recv(),
                Ok(FromAgent::Push {
                    body: Push::OpenReminders
                })
            )),
            "the event did not reach All Reminders"
        );
    }
}

#[test]
fn a_cold_missed_summary_survives_hello_inside_the_spawn_call() {
    let mut agent = agent();
    let connection = ConnectionId(36);
    let (shell, queue) = Connection::for_test(connection);
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    agent.connections.push(shell);

    agent.open_reminder_profile(profile, |agent, launched, _| {
        assert_eq!(launched, profile);
        agent.said(
            connection,
            1,
            Request::Hello {
                profile: profile.to_storage(),
                workspace: workspace.to_storage(),
                profile_name: String::new(),
                workspace_name: String::new(),
                pinned: Vec::new(),
                reopen_claim: None,
            },
            Duration::ZERO,
        );
        Ok(4242)
    });

    let first: Vec<_> = queue.try_iter().collect();
    assert!(first.iter().any(|message| matches!(
        message,
        FromAgent::Push {
            body: Push::OpenReminders
        }
    )));
    assert!(!agent.reminder_opens_owed.contains(&profile));

    let later = ConnectionId(37);
    let (later_shell, later_queue) = Connection::for_test(later);
    agent.connections.push(later_shell);
    agent.said(
        later,
        2,
        Request::Hello {
            profile: profile.to_storage(),
            workspace: WorkspaceId::new().to_storage(),
            profile_name: String::new(),
            workspace_name: String::new(),
            pinned: Vec::new(),
            reopen_claim: None,
        },
        Duration::ZERO,
    );
    assert!(!later_queue.try_iter().any(|message| matches!(
        message,
        FromAgent::Push {
            body: Push::OpenReminders
        }
    )));
}

#[test]
fn a_failed_cold_reminder_launch_leaves_no_open_latch() {
    let mut agent = agent();
    let profile = ProfileId::new();
    agent.open_reminder_profile(profile, |_, _, _| {
        Err(std::io::Error::other("test launch failed"))
    });
    assert!(!agent.reminder_opens_owed.contains(&profile));
}

#[test]
fn an_update_reopen_claim_reaches_only_that_shell_launch() {
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let token = "5ee8ffdf-00c8-47ba-b294-054820454a0d";
    let arguments = shell_launch_arguments(profile, workspace, None, Some(token));
    assert!(
        arguments
            .windows(2)
            .any(|pair| pair == ["--reopen-claim", token])
    );
}

#[test]
fn a_shell_hello_acknowledges_reopen_intent_off_the_event_loop() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(directory.path().to_path_buf());
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let pair = (profile.to_storage(), workspace.to_storage());
    let mut record = crate::update::Reopen::default();
    record.windows.push(pair.clone());
    let claim = record.claim_window(&pair);
    record.save(&paths).unwrap();
    let agent = Agent::new(paths.clone(), "device".into(), "instance".into());
    // This existing filesystem integration explicitly owns the saved record;
    // ordinary Agent fixtures suppress constructor filesystem discovery.
    agent
        .reopen_pending
        .store(true, std::sync::atomic::Ordering::Release);

    agent.acknowledge_reopened_window(profile, workspace, Some(&claim));

    let started = Instant::now();
    while crate::update::Reopen::has_record(&paths) && started.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !crate::update::Reopen::has_record(&paths),
        "the durable entry survived its shell acknowledgement"
    );
}

/// Say a Workspace hello for `connection`, and consume its initial reply and reminder status.
fn workspace_hello(
    agent: &mut Agent,
    connection: ConnectionId,
    queue: &std::sync::mpsc::Receiver<FromAgent>,
) {
    agent.said(
        connection,
        1,
        Request::Hello {
            profile: ProfileId::new().to_storage(),
            workspace: WorkspaceId::new().to_storage(),
            profile_name: "Personal".into(),
            workspace_name: "Notes".into(),
            pinned: Vec::new(),
            reopen_claim: None,
        },
        std::time::Duration::ZERO,
    );
    assert!(matches!(
        queue.recv(),
        Ok(FromAgent::Response {
            id: 1,
            body: Response::Ok,
        })
    ));
    assert!(matches!(
        queue.recv(),
        Ok(FromAgent::Push {
            body: Push::ReminderDiagnostics { .. },
        })
    ));
}

/// **An Agents row routes the show to Koru Workspace** (Q-E8 P9): the most
/// recently active Workspace window is the one asked, carrying the session id
/// — the same most-recently-active fallback every routing decision uses,
/// applied to the process that owns session tabs. Which pane shows the tab is
/// koru.exe's own knowledge; the Agent routes to the process and the window
/// sorts its panes.
#[test]
fn an_agents_row_routes_the_session_to_the_most_recent_workspace_window() {
    let mut agent = agent();
    let first = ConnectionId(60);
    let second = ConnectionId(61);
    let (first_connection, first_queue) = Connection::for_test(first);
    let (second_connection, second_queue) = Connection::for_test(second);
    agent.connections.push(first_connection);
    agent.connections.push(second_connection);
    workspace_hello(&mut agent, first, &first_queue);
    workspace_hello(&mut agent, second, &second_queue);

    // The second window is the one the person was just in.
    agent.said(second, 2, Request::Active, std::time::Duration::ZERO);
    let _ = second_queue.recv();

    let session = koru_session_protocol::SessionId::mint();
    agent.wake(AgentEvent::Tray(TrayAsk::ShowAgentSession(session)));
    assert_eq!(
        second_queue.recv(),
        Ok(FromAgent::Push {
            body: Push::ShowSession {
                session: Some(session.to_string()),
            },
        }),
        "the most recently active Workspace window is the one asked"
    );
    assert!(
        first_queue.try_recv().is_err(),
        "the other window is not asked too"
    );
}

/// **The Agents menu's New asks Workspace to offer a new agent** — the same
/// push with no session named, which the window answers by opening its `+`
/// chooser.
#[test]
fn tray_agents_new_routes_to_the_workspace_window() {
    let mut agent = agent();
    let workspace = ConnectionId(62);
    let (connection, queue) = Connection::for_test(workspace);
    agent.connections.push(connection);
    workspace_hello(&mut agent, workspace, &queue);

    agent.wake(AgentEvent::Tray(TrayAsk::NewAgent));
    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Push {
            body: Push::ShowSession { session: None },
        })
    );
}

/// **An ask made with no Workspace window is latched, not lost.** The click
/// launches koru.exe; the window it produces says Hello, and the Hello spends
/// the latch — the `feedback_owed` shape. Spent exactly once: a second window
/// must not be greeted by a session tab somebody asked for last week.
#[test]
fn a_latched_agents_ask_is_spent_by_the_next_workspace_hello() {
    let mut agent = agent();
    let session = koru_session_protocol::SessionId::mint();
    agent.pending_workspace_session = Some(Some(session));

    let workspace = ConnectionId(63);
    let (connection, queue) = Connection::for_test(workspace);
    agent.connections.push(connection);
    workspace_hello(&mut agent, workspace, &queue);
    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Push {
            body: Push::ShowSession {
                session: Some(session.to_string()),
            },
        }),
        "the launched window is handed the ask"
    );

    let later = ConnectionId(64);
    let (later_connection, later_queue) = Connection::for_test(later);
    agent.connections.push(later_connection);
    workspace_hello(&mut agent, later, &later_queue);
    assert!(
        later_queue.try_recv().is_err(),
        "the latch is spent by the first Hello, not re-read"
    );
}

#[test]
fn replay_discards_a_window_that_already_reconnected_after_an_agent_crash() {
    let directory = tempfile::tempdir().unwrap();
    let paths = Paths::for_root(directory.path().to_path_buf());
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let pair = (profile.to_storage(), workspace.to_storage());
    let mut record = crate::update::Reopen::default();
    record.windows.push(pair.clone());
    let claim = record.claim_window(&pair);
    record.save(&paths).unwrap();
    let pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

    replay_reopen(
        paths.clone(),
        std::sync::Arc::new(std::sync::Mutex::new(())),
        pending.clone(),
        running.clone(),
        vec![(profile.to_storage(), workspace.to_storage(), claim)],
    );

    assert!(!crate::update::Reopen::has_record(&paths));
    assert!(!pending.load(std::sync::atomic::Ordering::Acquire));
    assert!(!running.load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn a_pending_launch_wakes_maintenance_and_expires_without_an_overlay() {
    let mut agent = agent();
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let now = Instant::now();
    let target = koru_agent_wire::Target {
        profile: profile.to_storage(),
        workspace: workspace.to_storage(),
        document: None,
        node: None,
        trashed: false,
        match_text: None,
        new_window: false,
    };
    assert!(matches!(
        agent
            .router
            .open(&agent.registry, profile, workspace, target, now),
        Routed::Launch { .. }
    ));

    let deadline = now + crate::route::LAUNCH_DEADLINE;
    assert_eq!(agent.maintenance_deadline(), Some(deadline));
    assert!(!agent.maintain(deadline));
    assert_eq!(agent.router.pending_count(), 0);
    assert_eq!(agent.maintenance_deadline(), None);
}

#[test]
fn a_post_health_gate_retry_never_reopens_after_store_readiness_falls_away() {
    let mut agent = agent();
    let now = Instant::now();
    agent.store_running = Some(false);
    agent.startup_gate_retry_at = Some(now);

    assert!(!agent.maintain(now));
    assert!(!agent.startup_transition_complete);
    assert_eq!(agent.startup_gate_retry_at, None);

    agent.wake(AgentEvent::StoreAvailability {
        warning: None,
        running: true,
    });
    assert!(agent.startup_transition_complete);
}

/// Each platform's default origin is one that holds its feed: Windows and
/// macOS keep `updates.koru.do`; Linux reads the nightly release on GitHub,
/// in the flat layout that origin uses.
#[test]
fn the_default_update_origin_is_where_this_platforms_feed_lives() {
    let base = url::Url::parse(DEFAULT_UPDATE_BASE_URL).unwrap();
    let target = koru_update::UpdateTarget {
        channel: koru_update::ReleaseChannel::Nightly,
        platform: koru_update::build_info().platform.into(),
        architecture: "x86_64".into(),
    };
    let manifest = target.manifest_url(&base).unwrap();
    if cfg!(target_os = "linux") {
        assert_eq!(
            manifest.as_str(),
            "https://github.com/koruworks/mono/releases/download/nightly/nightly-linux-x86_64.json"
        );
    } else {
        assert_eq!(base.host_str(), Some("updates.koru.do"));
        assert_eq!(
            koru_update::FeedLayout::of(&base),
            koru_update::FeedLayout::Channels
        );
    }
}

#[test]
fn rehearsal_health_failure_cannot_affect_an_installed_or_ordinary_launch() {
    assert!(should_fail_rehearsal_health(true, true, true));
    assert!(!should_fail_rehearsal_health(false, true, true));
    assert!(!should_fail_rehearsal_health(true, false, true));
    assert!(!should_fail_rehearsal_health(true, true, false));
}

#[test]
fn a_selected_target_waits_one_settle_for_the_overlay_window_to_drop() {
    let mut agent = agent();
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    let connection = ConnectionId(23);
    agent.registry.hello(connection, None, profile, workspace);
    let now = Instant::now();
    agent.deferred_open = Some(DeferredOpen {
        target: koru_search_ui::Target::Store {
            profile,
            workspace,
            document: None,
            node: None,
            trashed: false,
            match_text: None,
            disposition: koru_search_ui::Disposition::CurrentWindow,
        },
        stage: DeferredStage::ClosingOverlay,
        deadline: now,
    });

    assert!(!agent.maintain(now));
    assert_eq!(agent.router.pending_count(), 0, "routed before window drop");
    assert!(
        agent
            .deferred_open
            .as_ref()
            .is_some_and(|open| { open.stage == DeferredStage::Ready && open.deadline == now })
    );

    // The next settle represents the loop turn after `live = None` dropped the
    // overlay HWND. Routing is now allowed; the fake connection is then
    // discarded because this unit test deliberately owns no transport.
    assert!(!agent.maintain(now));
    assert!(agent.deferred_open.is_none());
    assert!(agent.registry.shell(connection).is_none());
}

/// The chord the hotkey thread actually registered is pushed to every shell, so
/// a surface the agent does not draw — the taskbar Jump List — can say it. A
/// window that says *Hello* afterwards learns it too: the tray is not the only
/// place the truth has to reach.
#[test]
fn the_registered_shortcut_reaches_every_shell_and_a_late_hello() {
    let mut agent = agent();
    let first = ConnectionId(7);
    let (first_conn, first_queue) = Connection::for_test(first);
    agent.connections.push(first_conn);

    agent.wake(AgentEvent::HotkeyRegistered {
        action: crate::hotkey::Action::Search,
        shortcut: Some("Ctrl+Shift+J".into()),
    });
    assert_eq!(
        first_queue.recv(),
        Ok(FromAgent::Push {
            body: Push::Shortcut {
                shortcut: Some("Ctrl+Shift+J".into()),
            },
        }),
        "registration is broadcast"
    );

    // A shell that connects afterwards says Hello and is told too — the Hello
    // answer's `Ok` arrives first, then the chord.
    let late = ConnectionId(8);
    let (late_conn, late_queue) = Connection::for_test(late);
    agent.connections.push(late_conn);
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();
    agent.wake(AgentEvent::ShellSaid {
        connection: late,
        id: 1,
        body: Box::new(Request::Hello {
            profile: profile.to_storage(),
            workspace: workspace.to_storage(),
            profile_name: "Personal".into(),
            workspace_name: "Notes".into(),
            pinned: Vec::new(),
            reopen_claim: None,
        }),
        read_at: std::time::Instant::now(),
    });
    assert_eq!(
        late_queue.recv(),
        Ok(FromAgent::Response {
            id: 1,
            body: Response::Ok,
        }),
        "Hello is answered"
    );
    assert_eq!(
        late_queue.recv(),
        Ok(FromAgent::Push {
            body: Push::Shortcut {
                shortcut: Some("Ctrl+Shift+J".into()),
            },
        }),
        "a late Hello is told the chord"
    );
}

#[test]
fn a_late_shell_is_told_every_durable_profile_cleanup_marker() {
    let mut agent = agent();
    let mut deleted = vec![ProfileId::new().to_storage(), ProfileId::new().to_storage()];
    deleted.sort();
    agent.set_pending_profile_cleanup(deleted.clone());
    let connection = ConnectionId(81);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);
    let profile = ProfileId::new();
    let workspace = WorkspaceId::new();

    agent.said(
        connection,
        1,
        Request::Hello {
            profile: profile.to_storage(),
            workspace: workspace.to_storage(),
            profile_name: "Personal".into(),
            workspace_name: "Notes".into(),
            pinned: Vec::new(),
            reopen_claim: None,
        },
        Duration::ZERO,
    );

    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Response {
            id: 1,
            body: Response::Ok,
        })
    );
    // Hello synchronously queues the initial state for several independent
    // domains. Cleanup replay must be complete even with reminder diagnostics
    // in that stream; an empty queue must fail instead of waiting forever.
    let replayed: Vec<_> = queue
        .try_iter()
        .filter_map(|message| match message {
            FromAgent::Push {
                body: Push::ProfileLifecycleDeleted { profile_id },
            } => Some(profile_id),
            FromAgent::Push {
                body: Push::ReminderDiagnostics { .. },
            } => None,
            other => panic!("unexpected Hello initialization message: {other:?}"),
        })
        .collect();
    assert_eq!(
        replayed, deleted,
        "restart cleanup is replayed without reading SQLite on Hello"
    );
}

/// **`UpdateChanged` is a wake, not a change.**
///
/// The update worker raises it whenever it looks at anything, so a banner
/// sitting at one stage with nothing moving was re-sent about fifty-eight times
/// a second for as long as it was on screen — 14,315 pushes in one session's
/// log, an IPC frame and a log line each, burying the install failure that was
/// happening underneath them.
#[test]
fn an_update_report_that_has_not_changed_is_not_sent_again() {
    let dir = tempfile::tempdir().unwrap();
    let paths = koru_update::UpdatePaths {
        config_dir: dir.path().join("config"),
        data_dir: dir.path().join("data"),
        cache_dir: dir.path().join("cache"),
        logs_dir: dir.path().join("logs"),
    };
    let mut agent = agent();
    agent.updates = Some(koru_update::UpdateController::new("http://127.0.0.1:0", paths).unwrap());
    let connection = ConnectionId(21);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);

    agent.wake(AgentEvent::UpdateChanged);
    assert!(
        matches!(
            queue.recv(),
            Ok(FromAgent::Push {
                body: Push::Update { .. }
            })
        ),
        "the first report is news and goes out"
    );

    // Nothing moved in between. The worker waking again is not a second answer.
    agent.wake(AgentEvent::UpdateChanged);
    agent.wake(AgentEvent::UpdateChanged);
    // `try_recv`, not `recv`: a push is put on the queue inside `wake`, so
    // anything owed is already there — and `recv` on a live empty channel
    // blocks for ever rather than failing the test.
    assert_eq!(
        queue.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty),
        "an unchanged report was pushed again"
    );
}

/// `None` is a real answer — every candidate was taken, or the agent has an
/// explicit `--root` — and it is pushed as such, not left silent.
#[test]
fn an_absent_shortcut_is_pushed_as_none() {
    let mut agent = agent();
    let connection = ConnectionId(9);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);

    agent.wake(AgentEvent::HotkeyRegistered {
        action: crate::hotkey::Action::Search,
        shortcut: None,
    });
    assert_eq!(
        queue.recv(),
        Ok(FromAgent::Push {
            body: Push::Shortcut { shortcut: None },
        }),
        "no chord is reported, not skipped"
    );
}

#[test]
fn every_dictation_registration_updates_only_the_dictation_model() {
    let mut agent = agent();
    agent.shortcut = crate::tray::Shortcut::Chord("Ctrl+Shift+K".into());
    for label in crate::hotkey::DICTATION_CANDIDATES {
        agent.wake(AgentEvent::HotkeyRegistered {
            action: crate::hotkey::Action::Dictation,
            shortcut: Some((*label).to_owned()),
        });
        assert_eq!(
            agent.dictation_shortcut(),
            &crate::tray::Shortcut::Chord((*label).to_owned())
        );
        assert_eq!(
            agent.shortcut(),
            &crate::tray::Shortcut::Chord("Ctrl+Shift+K".into()),
            "Dictation registration must not change Search"
        );
    }
    agent.wake(AgentEvent::HotkeyRegistered {
        action: crate::hotkey::Action::Dictation,
        shortcut: None,
    });
    assert_eq!(agent.dictation_shortcut(), &crate::tray::Shortcut::Absent);
    assert_eq!(
        agent.shortcut(),
        &crate::tray::Shortcut::Chord("Ctrl+Shift+K".into())
    );
}

#[test]
fn a_dictation_hotkey_press_routes_only_to_the_coordinator_sink() {
    struct Sink(Arc<Mutex<Vec<crate::global_input::VirtualKey>>>);
    impl GlobalDictationSink for Sink {
        fn configure(&self, _policy: crate::dictation_coordinator::Policy) {}
        fn prepare_target(&self) {}
        fn start_prepared(&self) {}
        fn finish(&self) {}
        fn discard_prepared(&self) {}
        fn cancel(&self, _reason: crate::global_input::CancelReason) {}
        fn press(
            &self,
            _local: Option<crate::event::LocalDictationOwner>,
            key: crate::global_input::VirtualKey,
        ) {
            self.0.lock().unwrap().push(key);
        }

        fn local_state(
            &self,
            _owner: crate::event::LocalDictationOwner,
            _route: u64,
            _state: crate::dictation_coordinator::LocalState,
        ) {
        }

        fn shell_gone(&self, _connection: crate::registry::ConnectionId) {}
    }

    let mut agent = agent();
    let seen = Arc::new(Mutex::new(Vec::new()));
    agent.global_dictation = Some(Box::new(Sink(Arc::clone(&seen))));
    let key = crate::global_input::VirtualKey::new(b'D' as u16);
    assert!(
        !agent.wake(AgentEvent::Hotkey(crate::event::HotkeyPress::Dictation {
            key,
            foreground: crate::event::ForegroundPid::Known(42),
        }))
    );
    assert_eq!(*seen.lock().unwrap(), [key]);
    assert!(agent.overlay.is_none(), "Dictation does not open Search");
}

/// **The menu handler does no work; the loop does.**
///
/// The closure `Tray::show` installs carries an id and a `Waker` and nothing
/// else — it cannot read the model cache or the kill switch, because the tray
/// runs on the winit thread under `ControlFlow::Wait` and a file walk in a menu
/// callback is exactly what this process may not do. So the ask arrives as a
/// bare `TrayAsk::Voice` and *this* is where the state is decided: the wake
/// re-reads it rather than trusting what the row said when it was built.
#[test]
fn the_voice_ask_is_decided_when_the_loop_wakes_not_in_the_menu() {
    let mut agent = agent();
    // A press on a row that has nothing behind it changes nothing on the
    // agent's own surface — no overlay, and no window of its own.
    assert!(!agent.wake(AgentEvent::Tray(TrayAsk::Voice)));
    assert!(agent.overlay.is_none());
    // Re-read, and answered from what this installation actually has: a test
    // root has no model cache, so there is nothing to start (`read_voice_state`
    // never touches a disk under test).
    assert_eq!(
        agent.voice().row,
        crate::tray::VoiceRow::NotInstalled { megabytes: None }
    );
    assert!(
        agent.voice().worker.is_none(),
        "and nothing was dialled to find out, because a menu drawn when no \
         worker is running has to be answerable when no worker is running"
    );
}

#[test]
fn tray_start_and_open_voice_settings_remain_distinct_when_enabled_and_idle() {
    struct Sink(Arc<Mutex<usize>>);
    impl GlobalDictationSink for Sink {
        fn configure(&self, _policy: crate::dictation_coordinator::Policy) {}
        fn prepare_target(&self) {
            *self.0.lock().unwrap() += 1;
        }
        fn start_prepared(&self) {}
        fn finish(&self) {}
        fn discard_prepared(&self) {}
        fn cancel(&self, _reason: crate::global_input::CancelReason) {}
        fn press(
            &self,
            _local: Option<crate::event::LocalDictationOwner>,
            _key: crate::global_input::VirtualKey,
        ) {
        }
        fn local_state(
            &self,
            _owner: crate::event::LocalDictationOwner,
            _route: u64,
            _state: crate::dictation_coordinator::LocalState,
        ) {
        }
        fn shell_gone(&self, _connection: ConnectionId) {}
    }

    let mut start_agent = agent();
    let settings = koru_settings::GlobalSettings {
        global_dictation_enabled: Some(true),
        ..Default::default()
    };
    start_agent.settings.install_confirmed(settings);
    start_agent.dictation_shortcut = crate::tray::Shortcut::Absent;
    start_agent.voice.row = crate::tray::VoiceRow::Installed;
    let prepared = Arc::new(Mutex::new(0));
    start_agent.global_dictation = Some(Box::new(Sink(Arc::clone(&prepared))));
    assert_ne!(TrayAsk::Voice, TrayAsk::VoiceSettings);
    assert_eq!(start_agent.handle_installed_voice_tray(), Some(false));
    assert_eq!(*prepared.lock().unwrap(), 1);
    assert!(
        !start_agent.voice_window.has_surface(),
        "Start does not open settings"
    );
    assert!(start_agent.tray_voice_start);
    assert!(start_agent.voice_model().shortcut.is_none());

    let mut settings_agent = agent();
    settings_agent
        .settings
        .install_confirmed(koru_settings::GlobalSettings {
            global_dictation_enabled: Some(true),
            ..Default::default()
        });
    settings_agent.dictation_shortcut = crate::tray::Shortcut::Absent;
    assert!(settings_agent.wake(AgentEvent::Tray(TrayAsk::VoiceSettings)));
    assert!(
        settings_agent.voice_window.has_surface(),
        "settings is always reopenable"
    );
    assert!(!settings_agent.tray_voice_start);
}

/// **Two rows, two asks, and the loop can tell them apart.**
///
/// *Release model from memory* frees the weights and leaves voice working — the
/// next dictation pays the model load again — while *Stop voice worker* ends the
/// process. Collapsing them into one ask would put the choice back on the loop,
/// which cannot know which of the two somebody pressed.
///
/// Neither does any work here: both hand a control verb to a worker thread, and
/// under test that thread is never started — the reason
/// `ensure_voice_off_thread` gives at length, applied to a verb that would reach
/// a developer's own running worker.
#[test]
fn releasing_the_model_and_stopping_the_worker_are_two_asks_the_loop_tells_apart() {
    assert_ne!(TrayAsk::VoiceRelease, TrayAsk::VoiceStop);
    assert_ne!(TrayAsk::VoiceRelease, TrayAsk::Voice);

    let mut agent = agent();
    assert!(!agent.wake(AgentEvent::Tray(TrayAsk::VoiceRelease)));
    assert!(!agent.wake(AgentEvent::Tray(TrayAsk::VoiceStop)));
    // Neither opened a window, an overlay, or anything else on the agent's own
    // surface: what they do is a socket conversation somewhere else.
    assert!(agent.overlay.is_none());
    assert!(agent.voice().worker.is_none());
}

/// **A reading is what the submenu draws, and the two halves are read apart.**
///
/// The disk moves when a model is installed or pruned; the worker moves when a
/// dictation starts or ends. They arrive on different events, so applying one
/// must not blank the other — a menu that refreshed half of itself would be
/// confidently wrong about the half it did not.
#[test]
fn a_worker_reading_survives_the_next_disk_reading() {
    let mut agent = agent();
    let worker = crate::tray::VoiceWorker {
        state: koru_voice::State::Listening,
        model_loaded: true,
        last_error: None,
    };
    assert!(!agent.wake(AgentEvent::VoiceWorkerRead {
        worker: Some(worker.clone()),
        devices: Vec::new(),
    }));
    assert_eq!(agent.voice().worker, Some(worker.clone()));

    // A row pressed re-reads the disk. What the worker said is still true.
    assert!(!agent.wake(AgentEvent::Tray(TrayAsk::Voice)));
    assert_eq!(agent.voice().worker, Some(worker));

    // And a worker that has gone is reported as gone rather than remembered.
    assert!(!agent.wake(AgentEvent::VoiceWorkerRead {
        worker: None,
        devices: Vec::new(),
    }));
    assert!(agent.voice().worker.is_none());
}

// ------------------------------------------------------------ voice setup

/// **A press with no window open is not lost.** `restore_windows` spawns one,
/// and a push sent into an empty connection list would be a menu row that
/// opened a window and then did nothing — the most confusing possible answer to
/// a press. The ask is latched and spent by the next `Hello`.
#[test]
fn a_voice_offer_with_no_window_waits_for_one() {
    let mut agent = agent();
    assert!(agent.connections.is_empty());

    agent.offer_voice_setup();

    assert!(agent.voice_setup_owed, "the ask was dropped");

    let connection = ConnectionId(91);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);
    agent.push_voice_setup_to(connection);

    assert_eq!(queue.recv(), Ok(FromAgent::Push { body: Push::Voice }));
    assert!(
        !agent.voice_setup_owed,
        "taken rather than read: a second window must not be asked again"
    );
}

/// With a window already there the offer goes straight out and nothing is
/// latched — otherwise the *next* window to connect would open a second modal
/// for a question somebody had already answered.
#[test]
fn a_voice_offer_with_a_window_is_sent_rather_than_latched() {
    let mut agent = agent();
    let connection = ConnectionId(92);
    let (conn, queue) = Connection::for_test(connection);
    agent.connections.push(conn);

    agent.offer_voice_setup();

    assert_eq!(queue.recv(), Ok(FromAgent::Push { body: Push::Voice }));
    assert!(!agent.voice_setup_owed);
}

#[test]
fn rooted_updates_require_a_copied_bundle_inside_the_same_root() {
    let root = tempfile::tempdir().unwrap();
    let copied = root.path().join("copied-bundle");
    std::fs::create_dir(&copied).unwrap();
    let paths = crate::Paths::for_root(root.path().to_path_buf());
    let resolved = super::update_paths_for(&paths, &copied).expect("owned copy is eligible");
    assert_eq!(resolved, koru_update::UpdatePaths::under(root.path()));

    let checkout = tempfile::tempdir().unwrap();
    assert!(super::update_paths_for(&paths, checkout.path()).is_none());
    assert!(
        !root.path().join("agent/updates").exists(),
        "availability checks write nothing"
    );
}

#[test]
fn reminder_diagnostics_preserve_counts_and_report_only_transitions() {
    let current = crate::reminders::SchedulerReport {
        pending_count: 3,
        registered_count: 4,
        cursor: Some(9),
        next_deadline_ms: Some(20),
        last_successful_sync_ms: Some(18),
        ..Default::default()
    };
    let failure = crate::reminders::SchedulerReport {
        failure: Some(crate::reminders::FailureCategory::Ledger),
        ..Default::default()
    };
    let (failed, changed) = super::merge_reminder_report(&current, failure.clone());
    assert!(changed);
    assert_eq!(
        (
            failed.pending_count,
            failed.registered_count,
            failed.failed_count
        ),
        (3, 4, 1)
    );
    assert_eq!(
        (
            failed.cursor,
            failed.next_deadline_ms,
            failed.last_successful_sync_ms
        ),
        (Some(9), Some(20), Some(18))
    );

    let (_, repeated) = super::merge_reminder_report(&failed, failure);
    assert!(!repeated, "an unchanged failure category is not news");
    let isolated = crate::reminders::SchedulerReport {
        pending_count: 2,
        registered_count: 3,
        failed_count: 2,
        cursor: Some(10),
        next_deadline_ms: Some(30),
        failure: Some(crate::reminders::FailureCategory::NativeAsset),
        ..Default::default()
    };
    let (isolated, _) = super::merge_reminder_report(&current, isolated);
    assert_eq!(
        (
            isolated.pending_count,
            isolated.registered_count,
            isolated.failed_count,
            isolated.cursor,
            isolated.next_deadline_ms,
        ),
        (2, 3, 2, Some(10), Some(30)),
        "an isolated failure report is already a complete scheduler snapshot"
    );
    let changed_category = crate::reminders::SchedulerReport {
        failure: Some(crate::reminders::FailureCategory::NativePlatform),
        ..Default::default()
    };
    let (_, changed_category) = super::merge_reminder_report(&failed, changed_category);
    assert!(changed_category, "a new failure category is news");
    let (_, recovered) = super::merge_reminder_report(&failed, Default::default());
    assert!(recovered, "recovery is one new transition");
}

#[test]
fn sparse_event_failure_preserves_the_disabled_device_choice() {
    let current = crate::reminders::SchedulerReport {
        delivery_enabled: false,
        last_successful_sync_ms: Some(18),
        ..Default::default()
    };
    let event_failure = crate::reminders::SchedulerReport {
        failure: Some(crate::reminders::FailureCategory::Events),
        ..Default::default()
    };

    let (merged, changed) = super::merge_reminder_report(&current, event_failure);
    assert!(changed);
    assert!(!merged.delivery_enabled);
    assert_eq!(merged.last_successful_sync_ms, Some(18));
}

#[test]
fn reminder_health_projection_is_content_free_and_keeps_local_choice() {
    let report = crate::reminders::SchedulerReport {
        delivery_enabled: false,
        pending_count: 2,
        registered_count: 1,
        failed_count: 1,
        cursor: Some(7),
        next_deadline_ms: Some(30),
        last_successful_sync_ms: Some(20),
        failure: Some(crate::reminders::FailureCategory::NativePermission),
        ..Default::default()
    };
    let diagnostics = super::reminder_diagnostics(&report);
    assert!(!diagnostics.enabled);
    assert_eq!(diagnostics.next_deadline_ms, Some(30));
    assert_eq!(diagnostics.last_successful_sync_ms, Some(20));
    assert_eq!(
        diagnostics.failure_category.as_deref(),
        Some("native_permission")
    );
    let json = serde_json::to_string(&diagnostics).unwrap();
    for forbidden in ["title", "body", "target", "registration_id", "profile_id"] {
        assert!(
            !json.contains(forbidden),
            "health leaked {forbidden}: {json}"
        );
    }
}

/// **A build that hides Koru AI opens nothing**, whichever door asks: a
/// `koru://chat/…` from another app reaches the Agent as `OpenChatLink`, and
/// the title row's twin as `OpenAiChat`. Both answer `Ok` — silent in the
/// shell — and launch no chat process (koru-features, M39).
#[test]
fn a_build_that_hides_koru_ai_opens_no_chat_from_any_door() {
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        for request in [
            Request::OpenAiChat,
            Request::OpenChatLink {
                chat: "00000000-0000-4000-8000-000000000001".into(),
                block: None,
            },
        ] {
            let mut agent = agent();
            let connection = ConnectionId(46);
            let (conn, queue) = Connection::for_test(connection);
            agent.connections.push(conn);

            agent.said(connection, 9, request, Duration::ZERO);
            assert_eq!(
                queue.recv(),
                Ok(FromAgent::Response {
                    id: 9,
                    body: Response::Ok,
                })
            );
        }
    });
}
