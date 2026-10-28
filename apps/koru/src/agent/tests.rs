// SPDX-License-Identifier: MPL-2.0

//! What a window does with what the agent says, and what it does without one.

use super::*;

#[test]
fn zero_profile_connection_requests_account_scope_without_workspace_presence() {
    let (link, first) = AgentLink::for_test();
    link.sync_connection_state();
    assert!(matches!(
        first.recv().unwrap(),
        FromShell::Request {
            body: Request::Account {
                expected_owner_scope: None,
                command: koru_agent_wire::AccountCommand::Status,
            },
            ..
        }
    ));
    assert!(
        first.try_recv().is_err(),
        "onboarding must not invent workspace Hello"
    );
    let (writer, replacement) = std::sync::mpsc::channel();
    link.publish_connection(writer, None);
    link.sync_connection_state();
    assert!(matches!(
        replacement.recv().unwrap(),
        FromShell::Request {
            body: Request::Account {
                command: koru_agent_wire::AccountCommand::Status,
                ..
            },
            ..
        }
    ));
    assert!(replacement.try_recv().is_err());
}

#[test]
fn account_admission_without_writer_or_with_failed_send_returns_exact_local_failure() {
    use koru_workspace_host::account_ui::{CommandFailure, CommandSubmissionId};
    let absent = AgentLink::absent();
    let submission = CommandSubmissionId::next();
    absent.account_tracked(
        submission,
        koru_agent_wire::AccountCommand::ConfirmAccountCreation,
    );
    assert!(absent.take_account_dirty());
    assert_eq!(
        absent.take_account_command_failure(),
        Some(CommandFailure {
            submission,
            uncertain: false
        })
    );
    assert!(absent.take_account_result().is_none());
    let (link, receiver) = AgentLink::for_test();
    drop(receiver);
    let submission = CommandSubmissionId::next();
    link.account_tracked(
        submission,
        koru_agent_wire::AccountCommand::CancelAccountCreation,
    );
    assert_eq!(
        link.take_account_command_failure(),
        Some(CommandFailure {
            submission,
            uncertain: false
        })
    );
    assert!(link.account_command_deadline().is_none());
}

#[test]
fn retirement_between_writer_capture_and_enqueue_keeps_the_old_epoch() {
    use koru_workspace_host::account_ui::{CommandFailure, CommandSubmissionId};
    let (link, receiver) = AgentLink::for_test();
    link.connection_generation.store(7, Ordering::Release);
    let submission = CommandSubmissionId::next();
    let other = link.clone();
    let (attempt, attempted) = std::sync::mpsc::channel();
    let mut retire = None;
    link.account_tracked_with(
        submission,
        koru_agent_wire::AccountCommand::ConfirmAccountCreation,
        || {
            retire = Some(std::thread::spawn(move || {
                // The exact retirement interleave cannot advance the epoch while
                // the UI submission holds its captured writer.
                assert!(other.account_admissions.try_lock().is_err());
                attempt.send(()).unwrap();
                other.connection_ended(7);
            }));
            attempted.recv().unwrap();
            assert_eq!(link.connection_generation.load(Ordering::Acquire), 7);
        },
    );
    retire.unwrap().join().unwrap();
    assert!(matches!(
        receiver.recv().unwrap(),
        FromShell::Request {
            body: Request::Account { .. },
            ..
        }
    ));
    assert_eq!(
        link.take_account_command_failure(),
        Some(CommandFailure {
            submission,
            uncertain: true
        })
    );
    assert!(link.account_command_deadline().is_none());
    assert_eq!(link.connection_generation.load(Ordering::Acquire), 8);
}

#[test]
fn replacement_publishes_writer_pid_and_admission_epoch_together() {
    use koru_workspace_host::account_ui::CommandSubmissionId;
    let link = AgentLink::absent();
    let (old_writer, old_receiver) = std::sync::mpsc::channel();
    let old_epoch = link.publish_connection(old_writer, Some(10));
    let old = CommandSubmissionId::next();
    link.account_tracked(old, koru_agent_wire::AccountCommand::ConfirmAccountCreation);
    let FromShell::Request {
        id: old_request, ..
    } = old_receiver.recv().unwrap();
    let (new_writer, new_receiver) = std::sync::mpsc::channel();
    let epoch = link.publish_connection(new_writer, Some(20));
    assert_eq!(link.take_account_command_failure().unwrap().submission, old);
    link.connection_ended(old_epoch);
    assert_eq!(*link.peer_pid.lock().unwrap(), Some(20));
    assert!(link.connected());
    let new = CommandSubmissionId::next();
    link.account_tracked(new, koru_agent_wire::AccountCommand::Retry);
    let FromShell::Request { id, .. } = new_receiver.recv().unwrap();
    let mut admissions = link.account_admissions.lock().unwrap();
    assert!(!admissions.response(
        old_request,
        old_epoch,
        &Response::AccountQueued {
            generation: 1,
            owner_scope: 0
        }
    ));
    assert!(!admissions.response(
        id,
        old_epoch,
        &Response::AccountQueued {
            generation: 1,
            owner_scope: 0
        }
    ));
    assert!(admissions.response(
        id,
        epoch,
        &Response::AccountQueued {
            generation: 2,
            owner_scope: 0
        }
    ));
    assert!(admissions.take_failure().is_none());
}

fn target(profile: &str, workspace: &str, document: Option<&str>) -> koru_agent_wire::Target {
    koru_agent_wire::Target {
        profile: profile.into(),
        workspace: workspace.into(),
        document: document.map(str::to_string),
        node: None,
        trashed: false,
        match_text: None,
        new_window: false,
    }
}

const P: &str = "11111111-1111-4111-8111-111111111111";
const W: &str = "22222222-2222-4222-8222-222222222222";
const D: &str = "33333333-3333-4333-8333-333333333333";
const B: &str = "44444444-4444-4444-8444-444444444444";

/// **Every method is safe with no agent**, which is what "the window runs on
/// without one" has to mean in code rather than in a comment. A panic here
/// would turn a helper process that would not start into a crash on launch.
#[test]
fn a_window_with_no_agent_still_works() {
    let link = AgentLink::absent();
    assert!(!link.connected());
    assert!(link.credential_context().is_none());
    link.ensure_store(None);
    link.ensure_voice();
    link.hello(
        koru_core::ProfileId::from_uuid(uuid::Uuid::nil()),
        koru_core::WorkspaceId::from_uuid(uuid::Uuid::nil()),
        "Profile",
        "Workspace",
        &[],
    );
    link.active();
    assert!(link.drain().is_empty());
    // The update conversation is the same promise: no agent means no report and
    // nowhere to send a choice, and pressing the button is a no-op rather than a
    // panic. The Welcome screen draws those buttons disabled for this reason.
    let feed = link.update_feed();
    assert!(feed.report().is_none());
    assert!(
        !feed.choose(UpdateChoice::CheckNow),
        "a feed with no agent says the choice went nowhere"
    );
    assert!(!feed.take_dirty(), "nothing arrived, so nothing to redraw");
}

#[test]
fn only_an_agent_acknowledged_device_id_enables_durable_oauth_scope() {
    let root = std::path::Path::new("C:/dev/koru-data");
    assert!(credential_context(Some(root), "not-a-device-id").is_none());
    let context = credential_context(Some(root), "a6258102-2534-4fab-bd4d-40717b066f29")
        .expect("the welcomed Device ID scopes credentials");
    let entry = context.entry("https://accounts.google.com").unwrap();
    assert!(
        entry
            .account()
            .contains("a6258102-2534-4fab-bd4d-40717b066f29")
    );
    assert!(!entry.account().contains("koru-data"));
}

#[test]
fn replacement_connection_replays_the_latest_window_presence() {
    let (link, first) = AgentLink::for_test();
    link.hello(
        koru_core::ProfileId::parse(P).unwrap(),
        koru_core::WorkspaceId::parse(W).unwrap(),
        "Profile",
        "Workspace",
        &[],
    );
    let FromShell::Request {
        body: expected @ Request::Hello { .. },
        ..
    } = first.recv().unwrap()
    else {
        panic!("the first connection did not receive Hello")
    };

    let (writer, replacement) = std::sync::mpsc::channel();
    *link.writer.lock().unwrap() = Some(writer);
    link.replay_presence();

    assert_eq!(
        replacement.recv().unwrap(),
        FromShell::Request {
            id: 2,
            body: expected,
        },
        "a replacement Agent must learn which open window owns the workspace"
    );
}

#[test]
fn only_the_current_connection_generation_can_retire_the_shared_writer() {
    let (link, _receive) = AgentLink::for_test();
    link.connection_generation.store(7, Ordering::Release);
    link.live.store(true, Ordering::Release);

    link.connection_ended(6);
    assert!(link.current_writer().is_some());
    assert!(link.connected());

    link.connection_ended(7);
    assert!(link.current_writer().is_none());
    assert!(!link.connected());
}

#[test]
fn reconnect_retirement_completes_account_operations_once() {
    let (link, receive) = AgentLink::for_test();
    link.connection_generation.store(7, Ordering::Release);
    link.live.store(true, Ordering::Release);
    let profile = ProfileId::parse(P).unwrap();
    link.remove_local_profile(profile);
    link.bind_local_profile_account(profile, None, "account-a".into());
    assert!(matches!(
        receive.recv().unwrap(),
        FromShell::Request {
            body: Request::RemoveLocalProfile { .. },
            ..
        }
    ));
    assert!(matches!(
        receive.recv().unwrap(),
        FromShell::Request {
            body: Request::BindLocalProfileAccount { .. },
            ..
        }
    ));
    assert!(matches!(
        begin_browser(&link.browser_completions, 3, 5),
        BeginBrowser::Accepted
    ));

    link.connection_ended(6);
    assert!(link.take_local_profile_removal().is_none());
    assert!(link.take_local_profile_binding().is_none());
    link.connection_ended(7);
    assert_eq!(
        link.take_local_profile_removal(),
        Some((P.into(), Err("agent_disconnected".into())))
    );
    assert_eq!(
        link.take_local_profile_binding(),
        Some((P.into(), Err("agent_disconnected".into())))
    );
    assert!(link.browser_completions.lock().unwrap().attempts.is_empty());
    link.connection_ended(7);
    assert!(link.take_local_profile_removal().is_none());
    assert!(link.take_local_profile_binding().is_none());
}

#[test]
fn old_browser_completion_cannot_cancel_a_reused_account_attempt() {
    let (link, receive) = AgentLink::for_test();
    link.connection_generation.store(7, Ordering::Release);
    let completion = connection_browser_completion(
        Arc::clone(&link.connection_generation),
        7,
        browser_completion_handler(
            link.current_writer(),
            Arc::clone(&link.next_id),
            Arc::clone(&link.account_results),
            Arc::clone(&link.browser_completions),
            Arc::clone(&link.account_dirty),
            Arc::clone(&link.wake),
        ),
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 3, 5),
        BeginBrowser::Accepted
    );
    link.connection_ended(7);
    link.connection_generation.store(9, Ordering::Release);
    assert_eq!(
        begin_browser(&link.browser_completions, 3, 5),
        BeginBrowser::Accepted
    );

    completion(3, 5, false);

    assert!(
        receive.try_recv().is_err(),
        "old launch cancelled the new attempt"
    );
    assert_eq!(
        link.browser_completions.lock().unwrap().attempts[0].browser,
        BrowserStatus::Pending
    );
    assert!(link.take_account_result().is_none());
}

#[test]
fn reconnect_requires_the_original_credential_scope() {
    let mut link = AgentLink::absent();
    link.reconnect_root = Some(std::path::PathBuf::from("C:/dev/koru-data"));
    link.credentials = credential_context(link.reconnect_root.as_deref(), P);
    assert!(link.reconnect_scope_matches(P));
    assert!(!link.reconnect_scope_matches(W));
    assert!(!link.reconnect_scope_matches("not-a-device-id"));
}

#[test]
fn notification_control_is_typed_and_correlated_on_the_existing_stream() {
    let (link, receive) = AgentLink::for_test();
    let id = link
        .notification_control(koru_agent_wire::NotificationOperation::Inspect)
        .unwrap();
    assert_eq!(
        receive.recv().unwrap(),
        FromShell::Request {
            id,
            body: Request::NotificationControl {
                operation: koru_agent_wire::NotificationOperation::Inspect,
            },
        }
    );
}

#[test]
fn disconnect_completes_every_pending_notification_control_as_unavailable() {
    let link = AgentLink::absent();
    link.pending_notification_controls
        .lock()
        .unwrap()
        .extend([7, 9]);
    assert_eq!(
        fail_pending_notification_controls(
            &link.pending_notification_controls,
            &link.notification_controls,
        ),
        2
    );
    assert!(
        link.pending_notification_controls
            .lock()
            .unwrap()
            .is_empty()
    );
    let mut answers = [
        link.take_notification_control().unwrap(),
        link.take_notification_control().unwrap(),
    ];
    answers.sort_by_key(|(id, _)| *id);
    assert_eq!(answers[0].0, 7);
    assert_eq!(answers[1].0, 9);
    assert!(answers.iter().all(|(_, result)| matches!(
        result,
        koru_agent_wire::NotificationControlResult::Failed {
            code: koru_agent_wire::NotificationFailure::Unavailable
        }
    )));
}

#[test]
fn correlated_notification_take_does_not_consume_another_windows_answer() {
    let link = AgentLink::absent();
    link.notification_controls.lock().unwrap().extend([
        (11, koru_agent_wire::NotificationControlResult::Done),
        (22, koru_agent_wire::NotificationControlResult::Done),
    ]);
    assert_eq!(
        link.take_notification_control_for(22),
        Some(koru_agent_wire::NotificationControlResult::Done)
    );
    assert_eq!(
        link.take_notification_control(),
        Some((11, koru_agent_wire::NotificationControlResult::Done))
    );
}

/// Voice startup is supervision, so the window asks Agent rather than trying
/// to spawn `koru-voice-worker.exe` itself.
#[test]
fn asking_for_voice_queues_the_voice_service_request() {
    let (link, rx) = AgentLink::for_test();

    link.ensure_voice();

    let FromShell::Request { body, .. } = rx.recv().expect("the request was not queued");
    assert_eq!(
        body,
        Request::EnsureService {
            service: koru_agent_wire::Service::Voice
        }
    );
}

#[test]
fn a_chosen_result_arrives_as_something_this_window_can_open() {
    let Some(FromTheAgent::Open(target)) = translate(Push::Open {
        target: target(P, W, Some(D)),
    }) else {
        panic!("not an open");
    };
    let koru_search_ui::Target::Store {
        profile,
        workspace,
        document,
        ..
    } = *target
    else {
        panic!("not a Store target")
    };
    assert_eq!(profile.as_uuid().to_string(), P);
    assert_eq!(workspace.as_uuid().to_string(), W);
    assert_eq!(document.map(|d| d.as_uuid().to_string()), Some(D.into()));
}

fn snooze_push() -> Push {
    Push::ReminderSnooze {
        intent: koru_agent_wire::ReminderSnoozeIntent {
            delivery: "77777777-7777-4777-8777-777777777777".into(),
            profile: P.into(),
            reminder: "88888888-8888-4888-8888-888888888888".into(),
            occurrence: format!("occ1-{}", "ab".repeat(32)),
            entity: format!("task:{W}:{D}:99999999-9999-4999-8999-999999999999"),
        },
    }
}

#[test]
fn snooze_push_converts_every_identifier_before_entering_the_shell() {
    let Some(FromTheAgent::ReminderSnooze(intent)) = translate(snooze_push()) else {
        panic!("valid snooze was refused")
    };
    assert_eq!(intent.profile.to_storage(), P);
    assert_eq!(
        intent.reminder.to_storage(),
        "88888888-8888-4888-8888-888888888888"
    );
    assert_eq!(intent.entity.workspace().to_storage(), W);
    assert_eq!(intent.entity.document().to_storage(), D);
    assert_eq!(intent.delivery, "77777777-7777-4777-8777-777777777777");
}

#[test]
fn snooze_ack_is_an_explicit_event_loop_owned_request() {
    let (link, receive) = AgentLink::for_test();
    link.accept_reminder_snooze("77777777-7777-4777-8777-777777777777".into());
    assert_eq!(
        receive.recv().unwrap(),
        FromShell::Request {
            id: 1,
            body: Request::AcceptReminderSnooze {
                delivery: "77777777-7777-4777-8777-777777777777".into(),
            },
        }
    );
}

#[test]
fn malformed_snooze_identifiers_are_refused_as_one_indivisible_ask() {
    for field in ["profile", "reminder", "occurrence", "entity"] {
        let Push::ReminderSnooze { mut intent } = snooze_push() else {
            unreachable!()
        };
        match field {
            "profile" => intent.profile = "bad".into(),
            "reminder" => intent.reminder = "bad".into(),
            "occurrence" => intent.occurrence = "bad".into(),
            "entity" => intent.entity = "bad".into(),
            _ => unreachable!(),
        }
        assert!(
            translate(Push::ReminderSnooze { intent }).is_none(),
            "{field} escaped validation"
        );
    }
}

/// A hit on a workspace's own name has no document, and that is not a failure —
/// it opens the workspace. `None` already means this, which is exactly why an
/// *unreadable* id cannot also be `None`; see the test below.
#[test]
fn a_workspace_level_hit_carries_no_document() {
    let Some(FromTheAgent::Open(target)) = translate(Push::Open {
        target: target(P, W, None),
    }) else {
        panic!("not an open");
    };
    assert!(matches!(
        *target,
        koru_search_ui::Target::Store { document: None, .. }
    ));
}

/// **Refused, not guessed at.** An id this build cannot parse is a navigation
/// that cannot be performed, and performing it somewhere arbitrary is worse
/// than declining — a person watching a window jump to the wrong document has
/// no way to tell that is what happened.
#[test]
fn a_target_with_an_id_that_is_not_a_uuid_is_refused() {
    assert!(
        translate(Push::Open {
            target: target("not-a-uuid", W, None)
        })
        .is_none()
    );
    assert!(
        translate(Push::Open {
            target: target(P, "not-a-uuid", None)
        })
        .is_none()
    );
    // And a document that is present but unreadable, which must not collapse
    // into the workspace-level case above.
    assert!(
        translate(Push::Open {
            target: target(P, W, Some("not-a-uuid"))
        })
        .is_none()
    );
}

#[test]
fn a_restore_is_carried_through() {
    assert!(matches!(
        translate(Push::Restore),
        Some(FromTheAgent::Restore)
    ));
}

/// **The tray's Agents rows** (Q-E8 P9): a named session becomes the typed
/// show-this-session ask; the `None` spelling is New; and an id that will not
/// parse is refused rather than guessed at — the `Open` rule, applied here.
#[test]
fn a_show_session_push_is_parsed_at_the_boundary() {
    let session = koru_session_protocol::SessionId::mint();
    let Some(FromTheAgent::ShowSession(parsed)) = translate(Push::ShowSession {
        session: Some(session.to_string()),
    }) else {
        panic!("not a show-session");
    };
    assert_eq!(parsed, session);

    assert!(matches!(
        translate(Push::ShowSession { session: None }),
        Some(FromTheAgent::NewAgentSession)
    ));

    assert!(
        translate(Push::ShowSession {
            session: Some("not-a-session-id".into()),
        })
        .is_none()
    );
}

/// The chord is state the taskbar draws from, never an instruction — it is
/// stored by the reader and must not be queued into `FromTheAgent`.
#[test]
fn the_shortcut_is_not_an_ask() {
    assert!(
        translate(Push::Shortcut {
            shortcut: Some("Ctrl+Shift+K".into()),
        })
        .is_none()
    );
    assert!(translate(Push::Shortcut { shortcut: None }).is_none());
}

/// `take_shortcut` reports a change exactly once, and `None` both as "no chord"
/// and as "nothing new" — the outer `None` and the inner `None` are different
/// facts, which is why the return is `Option<Option<String>>`.
#[test]
fn take_shortcut_is_consumed_and_distinguishes_none_from_nothing() {
    let link = AgentLink::absent();
    assert_eq!(link.take_shortcut(), None, "nothing yet");

    // Simulate what the reader does on `Push::Shortcut { shortcut: None }`.
    *link.shortcut.lock().unwrap() = None;
    link.shortcut_dirty
        .store(true, std::sync::atomic::Ordering::Release);
    assert_eq!(
        link.take_shortcut(),
        Some(None),
        "no chord is a real answer"
    );
    assert_eq!(link.take_shortcut(), None, "and it is said once");
}

#[test]
fn a_correlated_ai_chat_failure_becomes_one_actionable_notice() {
    let link = AgentLink::absent();
    link.pending_ai_chat.lock().unwrap().insert(73);
    assert_eq!(
        handle_ai_chat_response(
            &link.pending_ai_chat,
            &link.notices,
            73,
            &Response::Failed {
                code: "launch_failed".into(),
                message: "C:/Koru/koru-ai-chat.exe is not beside this executable".into(),
            },
        ),
        (true, true)
    );

    let notice = link.take_notice().expect("the requester sees the failure");
    assert_eq!(notice.title, "Koru AI Chat couldn't open");
    assert!(
        notice
            .body
            .contains("standalone chat window was not opened")
    );
    assert!(
        notice
            .body
            .contains("workspace and your conversations were not changed")
    );
    assert!(notice.body.contains("Reinstall this Koru build"));
    assert_eq!(
        notice.details.as_deref(),
        Some(
            "Koru AI Chat was not found beside Koru Agent. Reinstall this Koru build or restore the missing application file, then try again."
        )
    );
    assert!(link.take_notice().is_none(), "a report is consumed once");
}

#[test]
fn only_the_matching_ai_chat_response_is_claimed() {
    let link = AgentLink::absent();
    link.pending_ai_chat.lock().unwrap().insert(73);
    assert_eq!(
        handle_ai_chat_response(
            &link.pending_ai_chat,
            &link.notices,
            72,
            &Response::Failed {
                code: "unrelated".into(),
                message: "another request failed".into(),
            },
        ),
        (false, false),
        "an unrelated correlated response stays available to its own handler"
    );
    assert!(link.take_notice().is_none());
    assert_eq!(
        handle_ai_chat_response(&link.pending_ai_chat, &link.notices, 73, &Response::Ok),
        (true, false),
        "success completes the tracked ask without manufacturing an error"
    );
    assert!(link.take_notice().is_none());
}

#[test]
fn asking_for_ai_chat_without_an_agent_is_visible_and_safe() {
    let link = AgentLink::absent();
    link.open_ai_chat();
    let notice = link.take_notice().expect("the click must not appear inert");
    assert_eq!(notice.title, "Koru AI Chat couldn't open");
    assert_eq!(
        notice.details.as_deref(),
        Some("Koru Agent is not connected")
    );
}

#[test]
fn a_disconnect_answers_every_ai_chat_request_that_was_in_flight() {
    let link = AgentLink::absent();
    link.pending_ai_chat.lock().unwrap().extend([7, 9]);
    assert_eq!(
        fail_pending_ai_chat(
            &link.pending_ai_chat,
            &link.notices,
            "the test connection ended",
        ),
        2
    );
    assert!(link.pending_ai_chat.lock().unwrap().is_empty());
    for _ in 0..2 {
        assert_eq!(
            link.take_notice().and_then(|notice| notice.details),
            Some("the test connection ended".into())
        );
    }
    assert!(link.take_notice().is_none());
}

#[test]
fn a_failed_send_reports_only_when_it_wins_the_disconnect_race() {
    let link = AgentLink::absent();
    link.pending_ai_chat.lock().unwrap().insert(7);
    assert!(fail_ai_chat_send(
        &link.pending_ai_chat,
        &link.notices,
        7,
        "the writer ended",
    ));
    assert_eq!(
        link.take_notice().and_then(|notice| notice.details),
        Some("the writer ended".into())
    );

    // The reader won the real race: it already drained this id and queued the
    // connection-ended notice. The failed `send` must not manufacture a
    // second report for the same request.
    assert!(!fail_ai_chat_send(
        &link.pending_ai_chat,
        &link.notices,
        7,
        "duplicate",
    ));
    assert!(link.take_notice().is_none());
}

#[test]
fn launchservices_links_use_the_existing_agent_routes() {
    assert!(matches!(
        request_for_external_url(&format!("koru://doc/{D}#{B}")),
        Ok(Request::OpenLink {
            document,
            node: Some(node),
        }) if document == D && node == B
    ));
    assert!(matches!(
        request_for_external_url(&format!("koru://chat/{D}")),
        Ok(Request::OpenChatLink {
            chat,
            block: None,
        }) if chat == D
    ));
    assert!(matches!(
        request_for_external_url(&format!("koru://chat/{D}#{B}")),
        Ok(Request::OpenChatLink {
            chat,
            block: Some(block),
        }) if chat == D && block == B
    ));
}

#[test]
fn launchservices_refuses_invalid_and_context_free_links() {
    assert!(request_for_external_url("https://example.com").is_err());
    assert!(request_for_external_url(&format!("koru://{D}")).is_err());
}

/// Ctrl+Enter in the overlay opens a second window, and that choice belongs to
/// the person rather than to whichever window happens to receive the push.
#[test]
fn a_new_window_target_keeps_asking_for_a_new_window() {
    let mut wire = target(P, W, Some(D));
    wire.new_window = true;
    let Some(FromTheAgent::Open(target)) = translate(Push::Open { target: wire }) else {
        panic!("not an open");
    };
    assert!(matches!(
        *target,
        koru_search_ui::Target::Store {
            disposition: koru_search_ui::Disposition::NewWindow,
            ..
        }
    ));
}

/// **The endpoint must be the agent's, and it must follow `--root`.** Three
/// processes derive a name from the same path; a shell that ignored the flag
/// would dial the machine's shared agent from a window meant to be isolated,
/// and the two would look connected while pointing at different stores.
#[test]
fn the_endpoint_follows_the_root() {
    let plain = endpoint(None).expect("a name for this channel");
    let rooted = endpoint(Some(std::path::Path::new("C:/dev/koru-data"))).expect("a rooted name");
    assert_ne!(
        format!("{plain:?}"),
        format!("{rooted:?}"),
        "a rooted window would dial the shared agent"
    );
}

#[test]
fn browser_worker_is_injectable_and_surfaces_open_failure() {
    let (link, requests) = AgentLink::for_test();
    let completed = browser_completion_handler(
        link.current_writer(),
        Arc::clone(&link.next_id),
        Arc::clone(&link.account_results),
        Arc::clone(&link.browser_completions),
        Arc::clone(&link.account_dirty),
        Arc::clone(&link.wake),
    );
    let sender = start_browser_worker(
        |_| Err(std::io::Error::other("seeded browser refusal")),
        completed,
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 37, 1),
        BeginBrowser::Accepted
    );
    sender
        .send((37, 1, "https://example.invalid/auth".into()))
        .unwrap();
    let request = requests
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        request,
        FromShell::Request {
            body: Request::Account {
                command: koru_agent_wire::AccountCommand::Cancel { generation: 37 },
                expected_owner_scope: Some(0)
            },
            ..
        }
    ));
    assert!(
        accept_agent_account_result(
            &link.browser_completions,
            &link.account_results,
            37,
            koru_agent_wire::AccountOperationResult::Cancelled,
        ),
        "Agent cancellation resolves the deferred browser failure"
    );
    assert_eq!(
        link.take_account_result(),
        Some((
            37,
            koru_agent_wire::AccountOperationResult::Failed {
                code: "browser_unavailable".into()
            }
        ))
    );
}

#[test]
fn duplicate_browser_push_and_completion_publish_one_failure() {
    let (link, requests) = AgentLink::for_test();
    let completed = browser_completion_handler(
        link.current_writer(),
        Arc::clone(&link.next_id),
        Arc::clone(&link.account_results),
        Arc::clone(&link.browser_completions),
        Arc::clone(&link.account_dirty),
        Arc::clone(&link.wake),
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 39, 7),
        BeginBrowser::Accepted
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 39, 7),
        BeginBrowser::Duplicate
    );

    completed(39, 7, false);
    assert_eq!(
        begin_browser(&link.browser_completions, 39, 7),
        BeginBrowser::Duplicate
    );
    completed(39, 7, false);
    completed(39, 7, true);

    assert!(matches!(
        requests
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap(),
        FromShell::Request {
            body: Request::Account {
                command: koru_agent_wire::AccountCommand::Cancel { generation: 39 },
                expected_owner_scope: Some(0)
            },
            ..
        }
    ));
    assert!(
        requests
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "duplicate completion sent a second Cancel"
    );
    assert_eq!(link.take_account_result(), None);
    assert!(accept_agent_account_result(
        &link.browser_completions,
        &link.account_results,
        39,
        koru_agent_wire::AccountOperationResult::Cancelled,
    ));
    assert_eq!(
        link.take_account_result(),
        Some((
            39,
            koru_agent_wire::AccountOperationResult::Failed {
                code: "browser_unavailable".into()
            }
        ))
    );
    assert_eq!(link.take_account_result(), None);
}

#[test]
fn browser_failure_before_agent_success_preserves_committed_outcome() {
    let (link, requests) = AgentLink::for_test();
    let completed = browser_completion_handler(
        link.current_writer(),
        Arc::clone(&link.next_id),
        Arc::clone(&link.account_results),
        Arc::clone(&link.browser_completions),
        Arc::clone(&link.account_dirty),
        Arc::clone(&link.wake),
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 45, 4),
        BeginBrowser::Accepted
    );
    completed(45, 4, false);
    assert!(matches!(
        requests.recv_timeout(std::time::Duration::from_secs(1)),
        Ok(FromShell::Request {
            body: Request::Account {
                command: koru_agent_wire::AccountCommand::Cancel { generation: 45 },
                expected_owner_scope: Some(0)
            },
            ..
        })
    ));
    assert_eq!(
        link.take_account_result(),
        None,
        "browser failure is not terminal before Agent resolves commit vs cancel"
    );

    assert!(accept_agent_account_result(
        &link.browser_completions,
        &link.account_results,
        45,
        koru_agent_wire::AccountOperationResult::Profile {
            profile: koru_agent_wire::AccountProfileRow {
                koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                profile_id: P.into(),
                name: None,
                locale: None,
                auth_identities: Vec::new(),
            }
        },
    ));
    assert!(matches!(
        link.take_account_result(),
        Some((45, koru_agent_wire::AccountOperationResult::Profile { .. }))
    ));
    assert_eq!(link.take_account_result(), None);
}

#[test]
fn fallback_then_late_agent_success_stays_one_terminal() {
    let (link, requests) = AgentLink::for_test();
    drop(requests);
    let completed = browser_completion_handler(
        link.current_writer(),
        Arc::clone(&link.next_id),
        Arc::clone(&link.account_results),
        Arc::clone(&link.browser_completions),
        Arc::clone(&link.account_dirty),
        Arc::clone(&link.wake),
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 46, 5),
        BeginBrowser::Accepted
    );
    completed(46, 5, false);
    assert_eq!(
        fail_browser_attempts_on_disconnect(&link.browser_completions, &link.account_results),
        0,
        "the failed Cancel writer already ran the fallback"
    );
    assert_eq!(
        link.take_account_result(),
        Some((
            46,
            koru_agent_wire::AccountOperationResult::Failed {
                code: "browser_unavailable".into()
            }
        ))
    );
    assert_eq!(link.take_account_result(), None);
    assert!(
        !accept_agent_account_result(
            &link.browser_completions,
            &link.account_results,
            46,
            koru_agent_wire::AccountOperationResult::Profile {
                profile: koru_agent_wire::AccountProfileRow {
                    koru_identity_id: "00000000-0000-4000-8000-000000000099".into(),
                    profile_id: P.into(),
                    name: None,
                    locale: None,
                    auth_identities: Vec::new(),
                }
            },
        ),
        "the buffered success arrived after the fallback terminal"
    );
    assert_eq!(link.take_account_result(), None);
}

#[test]
fn fallback_then_late_agent_failure_stays_one_terminal() {
    let (link, requests) = AgentLink::for_test();
    drop(requests);
    let completed = browser_completion_handler(
        link.current_writer(),
        Arc::clone(&link.next_id),
        Arc::clone(&link.account_results),
        Arc::clone(&link.browser_completions),
        Arc::clone(&link.account_dirty),
        Arc::clone(&link.wake),
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 47, 6),
        BeginBrowser::Accepted
    );
    completed(47, 6, false);
    assert_eq!(
        fail_browser_attempts_on_disconnect(&link.browser_completions, &link.account_results),
        0,
        "the failed Cancel writer already ran the fallback"
    );
    assert_eq!(
        link.take_account_result(),
        Some((
            47,
            koru_agent_wire::AccountOperationResult::Failed {
                code: "browser_unavailable".into()
            }
        ))
    );
    assert!(
        !accept_agent_account_result(
            &link.browser_completions,
            &link.account_results,
            47,
            koru_agent_wire::AccountOperationResult::Failed {
                code: "seeded_late_failure".into()
            },
        ),
        "the buffered Agent failure arrived after the fallback terminal"
    );
    assert_eq!(link.take_account_result(), None);
}

#[test]
fn browser_fallback_generation_is_reusable_after_reader_eof() {
    let (link, requests) = AgentLink::for_test();
    drop(requests);
    let completed = browser_completion_handler(
        link.current_writer(),
        Arc::clone(&link.next_id),
        Arc::clone(&link.account_results),
        Arc::clone(&link.browser_completions),
        Arc::clone(&link.account_dirty),
        Arc::clone(&link.wake),
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 48, 7),
        BeginBrowser::Accepted
    );
    completed(48, 7, false);
    assert_eq!(
        link.take_account_result(),
        Some((
            48,
            koru_agent_wire::AccountOperationResult::Failed {
                code: "browser_unavailable".into()
            }
        ))
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 48, 1),
        BeginBrowser::Duplicate,
        "the old reader can still deliver its buffered terminal before EOF"
    );
    assert_eq!(
        finish_browser_disconnect(&link.browser_completions, &link.account_results),
        0,
        "the failed Cancel writer already published the fallback"
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 48, 1),
        BeginBrowser::Accepted,
        "a restarted Agent may reuse its process-local generation"
    );
    assert!(accept_agent_account_result(
        &link.browser_completions,
        &link.account_results,
        48,
        koru_agent_wire::AccountOperationResult::SignedIn,
    ));
    assert_eq!(
        link.take_account_result(),
        Some((48, koru_agent_wire::AccountOperationResult::SignedIn))
    );
}

#[test]
fn collision_attempts_both_open_and_each_success_deduplicates() {
    let link = AgentLink::absent();
    let completed = browser_completion_handler(
        None,
        Arc::clone(&link.next_id),
        Arc::clone(&link.account_results),
        Arc::clone(&link.browser_completions),
        Arc::clone(&link.account_dirty),
        Arc::clone(&link.wake),
    );
    let (completion_sent, completion_seen) = std::sync::mpsc::channel();
    let completed = Arc::new(move |generation, attempt, succeeded| {
        completed(generation, attempt, succeeded);
        completion_sent.send(attempt).unwrap();
    });
    let (opened, opened_urls) = std::sync::mpsc::channel();
    let sender = start_browser_worker(
        move |url| {
            opened.send(url.to_owned()).unwrap();
            Ok(())
        },
        completed,
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 40, 1),
        BeginBrowser::Accepted
    );
    sender
        .send((40, 1, "https://phase-one.test".into()))
        .unwrap();
    assert_eq!(
        opened_urls
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap(),
        "https://phase-one.test"
    );
    assert_eq!(completion_seen.recv().unwrap(), 1);
    assert_eq!(
        begin_browser(&link.browser_completions, 40, 1),
        BeginBrowser::Duplicate
    );
    assert!(
        opened_urls
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err(),
        "the duplicate phase-one attempt reopened"
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 40, 2),
        BeginBrowser::Accepted,
        "collision phase two has a distinct attempt within one generation"
    );
    sender
        .send((40, 2, "https://phase-two.test".into()))
        .unwrap();
    assert_eq!(
        opened_urls
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap(),
        "https://phase-two.test"
    );
    assert_eq!(completion_seen.recv().unwrap(), 2);
    assert_eq!(
        begin_browser(&link.browser_completions, 40, 2),
        BeginBrowser::Duplicate
    );
    assert!(accept_agent_account_result(
        &link.browser_completions,
        &link.account_results,
        40,
        koru_agent_wire::AccountOperationResult::SignedIn,
    ));
    assert!(link.browser_completions.lock().unwrap().attempts.is_empty());
}

#[test]
fn delayed_browser_failure_is_suppressed_after_agent_completion() {
    let (link, requests) = AgentLink::for_test();
    let completed = browser_completion_handler(
        link.current_writer(),
        Arc::clone(&link.next_id),
        Arc::clone(&link.account_results),
        Arc::clone(&link.browser_completions),
        Arc::clone(&link.account_dirty),
        Arc::clone(&link.wake),
    );
    let (completion_sent, completion_seen) = std::sync::mpsc::channel();
    let completed = Arc::new(move |generation, attempt, succeeded| {
        completed(generation, attempt, succeeded);
        completion_sent.send(()).unwrap();
    });
    let (opened, wait_for_failure) = std::sync::mpsc::channel();
    let (release_failure, release) = std::sync::mpsc::channel();
    let sender = start_browser_worker(
        move |_| {
            opened.send(()).unwrap();
            release.recv().unwrap();
            Err(std::io::Error::other("seeded delayed browser refusal"))
        },
        completed,
    );
    assert_eq!(
        begin_browser(&link.browser_completions, 41, 9),
        BeginBrowser::Accepted
    );
    sender
        .send((41, 9, "https://example.invalid/auth".into()))
        .unwrap();
    wait_for_failure
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();

    assert!(accept_agent_account_result(
        &link.browser_completions,
        &link.account_results,
        41,
        koru_agent_wire::AccountOperationResult::SignedIn,
    ));
    for generation in 100..100 + ACCOUNT_RESULT_CAPACITY as u64 + 8 {
        assert!(accept_agent_account_result(
            &link.browser_completions,
            &link.account_results,
            generation,
            koru_agent_wire::AccountOperationResult::SignedOut,
        ));
    }
    release_failure.send(()).unwrap();
    completion_seen
        .recv_timeout(std::time::Duration::from_secs(1))
        .unwrap();

    assert!(
        requests
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err(),
        "a late opener failure must not send Cancel after Agent completion"
    );
    let browser_completions = link.browser_completions.lock().unwrap();
    assert!(browser_completions.attempts.is_empty());
    assert!(browser_completions.refused.is_empty());
    drop(browser_completions);
    while let Some((generation, result)) = link.take_account_result() {
        assert_ne!(
            generation, 41,
            "the old terminal was correctly queue-evicted"
        );
        assert_ne!(
            result,
            koru_agent_wire::AccountOperationResult::Failed {
                code: "browser_unavailable".into()
            }
        );
    }
}

#[test]
fn account_result_queue_is_bounded_and_keeps_the_newest_completions() {
    let results = Mutex::new(VecDeque::new());
    for generation in 0..(ACCOUNT_RESULT_CAPACITY as u64 + 8) {
        push_account_result(
            &results,
            (
                0,
                generation,
                koru_agent_wire::AccountOperationResult::SignedOut,
            ),
        );
    }
    let results = results.lock().unwrap();
    assert_eq!(results.len(), ACCOUNT_RESULT_CAPACITY);
    assert_eq!(
        results.front().map(|(_, generation, _)| *generation),
        Some(8)
    );
}

#[test]
fn local_profile_removal_request_is_exact_and_correlated() {
    let (link, requests) = AgentLink::for_test();
    let profile = koru_core::ProfileId::new();
    link.remove_local_profile(profile);
    assert_eq!(
        requests
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap(),
        FromShell::Request {
            id: 1,
            body: Request::RemoveLocalProfile {
                profile_id: profile.to_storage(),
            },
        }
    );
}

#[test]
fn local_profile_binding_request_is_exact_and_correlated() {
    let (link, requests) = AgentLink::for_test();
    let profile = koru_core::ProfileId::new();
    link.bind_local_profile_account(
        profile,
        Some("00000000-0000-4000-8000-000000000001".into()),
        "00000000-0000-4000-8000-000000000002".into(),
    );
    assert_eq!(
        requests
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap(),
        FromShell::Request {
            id: 1,
            body: Request::BindLocalProfileAccount {
                profile_id: profile.to_storage(),
                expected_identity_id: Some("00000000-0000-4000-8000-000000000001".into(),),
                new_identity_id: "00000000-0000-4000-8000-000000000002".into(),
            },
        }
    );
}

/// *Start over* is one request, and the Agent stopping for it becomes a
/// relaunch — taken once — rather than a reconnect onto a changed identity.
#[test]
fn a_reset_in_flight_turns_the_agent_stopping_into_a_relaunch() {
    let (link, receiver) = AgentLink::for_test();
    link.reset_identity();
    let FromShell::Request { id, body } = receiver.recv().unwrap();
    assert_eq!(body, Request::ResetIdentity);
    assert_eq!(
        *link.identity_reset.lock().unwrap(),
        IdentityReset::Requested(id)
    );
    assert_eq!(link.take_identity_reset(), None, "nothing to act on yet");

    assert!(link.relaunch_for_identity_reset());
    assert_eq!(link.take_identity_reset(), Some(IdentityReset::Relaunch));
    assert_eq!(link.take_identity_reset(), None, "taken once");
    assert!(
        !link.relaunch_for_identity_reset(),
        "an ordinary disconnect reconnects"
    );
}

/// A chat link handed over by LaunchServices sends nothing to the Agent in a
/// build that hides Koru AI — the in-document link's answer, from outside.
#[test]
fn a_chat_link_from_another_app_sends_nothing_when_koru_ai_is_hidden() {
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let (link, receiver) = AgentLink::for_test();
        link.live.store(true, Ordering::Release);
        assert_eq!(
            link.open_external_url(&format!("koru://chat/{D}#{B}")),
            Ok(())
        );
        assert!(receiver.try_recv().is_err(), "no request reached the Agent");
    });
}

/// **A quit is not a crash.** After `Push::Quitting` the end of the link must
/// not start a reconnect — the reconnect is what dials, finds nothing and
/// starts a new Agent, and it made the tray's *Quit* bring the Agent back.
/// The window still learns the link ended, which is what closes a hidden one.
#[test]
fn a_quit_the_agent_announced_is_not_undone_by_starting_another() {
    let (mut link, _receive) = AgentLink::for_test();
    link.reconnect_enabled = true;
    link.connection_generation.store(7, Ordering::Release);
    link.live.store(true, Ordering::Release);
    assert!(link.recovers_from_link_end(), "an ordinary end is a crash");

    link.quit_announced.store(true, Ordering::Release);
    link.connection_ended(7);

    assert!(!link.connected());
    assert!(
        !link.reconnecting.load(Ordering::Acquire),
        "no reconnect, so no new Agent"
    );
    assert!(
        link.drain()
            .iter()
            .any(|ask| matches!(ask, FromTheAgent::AgentDisconnected)),
        "the window is still told, so a hidden one closes"
    );
}

/// The notice belongs to the Agent that sent it. A link that connects again —
/// to an Agent somebody started afterwards — recovers from a crash as usual.
#[test]
fn a_new_connection_forgets_the_last_agents_quit() {
    let (link, _receive) = AgentLink::for_test();
    link.quit_announced.store(true, Ordering::Release);
    let (writer, _replacement) = std::sync::mpsc::channel();
    link.publish_connection(writer, None);
    assert!(link.recovers_from_link_end());
}

#[test]
fn a_quit_notice_is_not_an_ask_for_the_window() {
    assert!(translate(Push::Quitting).is_none());
}

/// The icon is the Agent's to report, and it goes with the link: a window
/// whose Agent has gone must not hide behind the icon that Agent had.
#[test]
fn the_notification_area_needs_a_live_agent_that_said_so() {
    let (link, _receive) = AgentLink::for_test();
    link.connection_generation.store(7, Ordering::Release);
    link.live.store(true, Ordering::Release);
    assert!(!link.has_notification_area(), "none until the Agent says");

    link.notification_area.store(true, Ordering::Release);
    assert!(link.has_notification_area());

    link.connection_ended(7);
    assert!(!link.has_notification_area());
    assert!(
        !link.notification_area.load(Ordering::Acquire),
        "forgotten, so a reconnect starts from none"
    );
}

#[test]
fn a_notification_area_report_is_not_an_ask_for_the_window() {
    assert!(translate(Push::NotificationArea { present: true }).is_none());
}
