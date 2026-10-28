// SPDX-License-Identifier: MPL-2.0

//! These shapes are on the wire between two separately installed binaries, so
//! a change here is a change both of them have to agree about. What is pinned
//! is the *spelling* — the tags and the field names — because that is the part
//! a refactor moves without noticing.

use super::*;

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap()
}

#[test]
fn open_workspace_request_round_trips() {
    let request = Request::OpenWorkspace {
        profile: Some("Research".into()),
        inherit_stdio: true,
    };
    assert_eq!(
        serde_json::from_str::<Request>(&json(&request)).unwrap(),
        request
    );
}

/// A report with an offer in it — every optional field populated, so a spelling
/// that moved shows up in the round trip rather than surviving as a `None`.
fn offered() -> UpdateReport {
    UpdateReport {
        stage: UpdateStage::Available,
        version: "0.3.1".into(),
        build_number: 431,
        title: "Faster opening".into(),
        summary: vec!["Documents open in a second".into(), "Fewer crashes".into()],
        notes_url: Some("https://updates.koru.do/releases/0.3.1/notes.html".into()),
        received: 0,
        total: 0,
        can_install: true,
        download_url: Some("https://updates.koru.do/releases/0.3.1/koru.zip".into()),
        message: None,
        detail: None,
        channel: "stable".into(),
        automatic_checks: true,
        active_session_count: 2,
        session_generation: 7,
    }
}

#[test]
fn the_tag_names_are_stable_snake_case() {
    assert!(
        json(&FromShell::Request {
            id: 1,
            body: Request::Active,
        })
        .contains(r#""kind":"request""#)
    );
    assert!(json(&Request::Active).contains(r#""kind":"active""#));
    assert!(
        json(&Request::EnsureService {
            service: Service::ApiGateway
        })
        .contains(r#""service":"api_gateway""#)
    );
    // The spelling is what an older Agent fails the body on, so it is pinned
    // here rather than left to `rename_all` -- and a rename would silently
    // stop every dictation from ever starting a worker.
    assert!(
        json(&Request::EnsureService {
            service: Service::Voice
        })
        .contains(r#""service":"voice""#)
    );
    assert_eq!(
        serde_json::from_str::<Service>(r#""voice""#).unwrap(),
        Service::Voice
    );
    assert!(
        json(&FromAgent::Push {
            body: Push::Restore
        })
        .contains(r#""kind":"push""#)
    );
    assert!(json(&Response::Ok).contains(r#""kind":"ok""#));
    assert!(
        json(&Push::ShowSession {
            session: Some("11111111-1111-4111-8111-111111111111".into()),
        })
        .contains(r#""kind":"show_session""#)
    );
    assert!(
        json(&Push::Update {
            report: Box::new(offered()),
        })
        .contains(r#""kind":"update""#)
    );
    assert!(
        json(&Push::Shortcut {
            shortcut: Some("Ctrl+Shift+K".into()),
        })
        .contains(r#""kind":"shortcut""#)
    );
    assert!(json(&Push::Quitting).contains(r#""kind":"quitting""#));
    assert_eq!(
        json(&Push::NotificationArea { present: true }),
        r#"{"kind":"notification_area","present":true}"#
    );
    assert!(
        json(&Request::UpdateChoice {
            choice: UpdateChoice::Download,
        })
        .contains(r#""kind":"update_choice""#)
    );
    assert!(json(&UpdateChoice::CheckNow).contains(r#""kind":"check_now""#));
    assert!(json(&UpdateChoice::DismissNewer).contains(r#""kind":"dismiss_newer""#));
    let automatic = json(&UpdateChoice::SetAutomatic { on: true });
    assert!(
        automatic.contains(r#""kind":"set_automatic""#),
        "{automatic}"
    );
    assert!(automatic.contains(r#""on":true"#), "{automatic}");
    assert!(json(&UpdateStage::InstalledNewer).contains(r#""installed_newer""#));
    assert!(json(&UpdateStage::UpToDate).contains(r#""up_to_date""#));
}

#[test]
fn a_report_with_no_offer_round_trips() {
    // The case that is actually on the wire most of the time: nothing found,
    // so the version is empty and every count is zero. Worth its own test
    // because an empty string and a zero are what a hand-rolled `Default` or a
    // `skip_serializing_if` would quietly turn into an absent field, and the
    // other end would then fail to parse a report that says nothing is
    // happening.
    let quiet = UpdateReport {
        stage: UpdateStage::Idle,
        version: String::new(),
        build_number: 0,
        title: String::new(),
        summary: Vec::new(),
        notes_url: None,
        received: 0,
        total: 0,
        can_install: false,
        download_url: None,
        message: None,
        detail: None,
        channel: "nightly".into(),
        automatic_checks: false,
        active_session_count: 0,
        session_generation: 1,
    };
    let text = json(&quiet);
    assert_eq!(serde_json::from_str::<UpdateReport>(&text).unwrap(), quiet);
    assert!(text.contains(r#""version":"""#), "{text}");
    assert!(text.contains(r#""notes_url":null"#), "{text}");
}

#[test]
fn a_report_carries_no_package_signature_or_hash() {
    // **Not tidiness.** The manifest's signature, hash and size decide what
    // gets executed on this machine, and this channel crosses a process
    // boundary. Forwarding the manifest wholesale is the obvious shortcut and
    // this is what says no to it.
    let text = json(&offered());
    for forbidden in ["signature", "sha256", "package"] {
        assert!(!text.contains(forbidden), "{forbidden} in {text}");
    }
}

#[test]
fn a_hello_carries_no_pid() {
    // **The rule, asserted rather than remembered.** A pid a peer reports is a
    // claim; the agent reads the real one off the transport. If somebody adds
    // the field back for convenience, this is what says no.
    let hello = json(&Request::Hello {
        profile: "p".into(),
        workspace: "w".into(),
        profile_name: "Personal".into(),
        workspace_name: "Notes".into(),
        pinned: Vec::new(),
        reopen_claim: None,
    });
    assert!(!hello.contains("pid"), "{hello}");
}

#[test]
fn an_update_hello_carries_only_its_opaque_reopen_claim() {
    let claim = "01234567-89ab-4def-8123-456789abcdef";
    let hello = json(&Request::Hello {
        profile: "p".into(),
        workspace: "w".into(),
        profile_name: "Personal".into(),
        workspace_name: "Notes".into(),
        pinned: Vec::new(),
        reopen_claim: Some(claim.into()),
    });
    assert!(hello.contains(&format!(r#""reopen_claim":"{claim}""#)));
    assert!(!hello.contains("pid"), "{hello}");
}

#[test]
fn active_carries_no_clock() {
    // The agent owns the ordering. A timestamp here would be several machines'
    // clocks, and a shell paused in a debugger reports one from before it was
    // paused.
    let active = json(&Request::Active);
    assert!(!active.contains("time"), "{active}");
    assert!(!active.contains("_at"), "{active}");
    assert!(!active.contains("seq"), "{active}");
}

#[test]
fn a_refusal_names_both_numbers() {
    let refused = AgentGreeting::Refused {
        what: "protocol".into(),
        ours: PROTOCOL_VERSION,
        theirs: 99,
        message: "this agent is older".into(),
    };
    let back: AgentGreeting = serde_json::from_str(&json(&refused)).unwrap();
    assert_eq!(back, refused);
    let text = json(&refused);
    assert!(
        text.contains(r#""ours""#) && text.contains(r#""theirs""#),
        "{text}"
    );
}

#[test]
fn every_message_round_trips() {
    let welcome = AgentGreeting::Welcome(Box::new(AgentWelcome {
        agent_version: "0.1.0".into(),
        protocol_version: PROTOCOL_VERSION,
        device_id: "0197a2f0-0000-4000-8000-000000000001".into(),
        instance_id: "0197a2f0-0000-4000-8000-00000000000a".into(),
        sync: SyncStatus::default(),
    }));
    assert_eq!(
        serde_json::from_str::<AgentGreeting>(&json(&welcome)).unwrap(),
        welcome
    );

    let open = FromAgent::Push {
        body: Push::Open {
            target: Target {
                profile: "p".into(),
                workspace: "w".into(),
                document: Some("d".into()),
                node: Some("n".into()),
                trashed: true,
                match_text: Some("a phrase".into()),
                new_window: false,
            },
        },
    };
    assert_eq!(
        serde_json::from_str::<FromAgent>(&json(&open)).unwrap(),
        open
    );
    let history = FromAgent::Response {
        id: 8,
        body: Response::DeviceHistory {
            device_id: "00000000-0000-4000-8000-000000000001".into(),
            events: vec![DeviceStateEventView {
                event_id: Some(91),
                at_millis: 42,
                kind: "document_updated".into(),
                profile_id: None,
                workspace_id: Some("workspace".into()),
                document_id: Some("document".into()),
                document_name: Some("Roadmap".into()),
                blob_hash: None,
                detail: None,
                items: Some(3),
            }],
            next_before: Some(42),
        },
    };
    assert_eq!(
        serde_json::from_str::<FromAgent>(&json(&history)).unwrap(),
        history
    );

    // Every stage, on a report that also carries a failure's two texts — the
    // stage is what the banner branches on, so a spelling that drifted there is
    // a banner that draws the wrong thing rather than an error.
    for stage in [
        UpdateStage::Idle,
        UpdateStage::Checking,
        UpdateStage::UpToDate,
        UpdateStage::InstalledNewer,
        UpdateStage::Available,
        UpdateStage::Downloading,
        UpdateStage::Verifying,
        UpdateStage::Ready,
        UpdateStage::Installing,
        UpdateStage::Deferred,
        UpdateStage::Failed,
    ] {
        let pushed = FromAgent::Push {
            body: Push::Update {
                report: Box::new(UpdateReport {
                    stage,
                    received: 1_024,
                    total: 4_096,
                    message: Some("The download did not finish".into()),
                    detail: Some("connection reset by peer".into()),
                    ..offered()
                }),
            },
        };
        assert_eq!(
            serde_json::from_str::<FromAgent>(&json(&pushed)).unwrap(),
            pushed
        );
    }

    // The one-shot after an install. Outside the loop above because it is not a
    // report and carries no stage — see `Push::Updated`.
    let updated = FromAgent::Push {
        body: Push::Updated {
            version: "0.0.4-nightly.20260816+3b71911".into(),
        },
    };
    assert_eq!(
        serde_json::from_str::<FromAgent>(&json(&updated)).unwrap(),
        updated
    );

    // Both spellings of the Workspace show: a named running session, and the
    // `None` that means the tray's New — open the chooser rather than guess an
    // agent.
    for session in [
        Some("22222222-2222-4222-8222-222222222222".to_owned()),
        None,
    ] {
        let pushed = FromAgent::Push {
            body: Push::ShowSession { session },
        };
        assert_eq!(
            serde_json::from_str::<FromAgent>(&json(&pushed)).unwrap(),
            pushed
        );
    }

    // The chord is `Some` on a normal registration and `None` under a rooted
    // agent or when every candidate was taken — both spellings are on the wire.
    for shortcut in [Some("Ctrl+Shift+J".to_owned()), None] {
        let pushed = FromAgent::Push {
            body: Push::Shortcut { shortcut },
        };
        assert_eq!(
            serde_json::from_str::<FromAgent>(&json(&pushed)).unwrap(),
            pushed
        );
    }

    for body in [
        Request::EnsureService {
            service: Service::Store,
        },
        Request::Hello {
            profile: "p".into(),
            workspace: "w".into(),
            profile_name: "Personal".into(),
            workspace_name: "Notes".into(),
            pinned: Vec::new(),
            reopen_claim: None,
        },
        Request::WorkspaceChanged {
            profile: "p".into(),
            workspace: "w".into(),
            profile_name: "Personal".into(),
            workspace_name: "Notes".into(),
            pinned: Vec::new(),
        },
        Request::Active,
        Request::ShowOverlay,
        Request::UpdateChoice {
            choice: UpdateChoice::Download,
        },
        Request::UpdateChoice {
            choice: UpdateChoice::Install {
                session_generation: 7,
            },
        },
        Request::UpdateChoice {
            choice: UpdateChoice::Later,
        },
        Request::UpdateChoice {
            choice: UpdateChoice::Cancel,
        },
        Request::UpdateChoice {
            choice: UpdateChoice::Retry,
        },
        Request::UpdateChoice {
            choice: UpdateChoice::CheckNow,
        },
        Request::UpdateChoice {
            choice: UpdateChoice::DismissNewer,
        },
        Request::UpdateChoice {
            choice: UpdateChoice::SetAutomatic { on: true },
        },
        Request::UpdateChoice {
            choice: UpdateChoice::SetAutomatic { on: false },
        },
        Request::StartPairing,
        Request::AcceptLinkCode {
            code: LinkCode("koru-link-v1:secret".into()),
        },
        Request::PairingStatus,
        Request::CancelPairing,
        Request::LinkedDevices,
        Request::DeviceHistory {
            device_id: "00000000-0000-4000-8000-000000000001".into(),
            before: Some(42),
        },
        Request::UnlinkDevice {
            device_id: "00000000-0000-4000-8000-000000000001".into(),
        },
        Request::SetRelay { enabled: false },
        Request::ResetIdentity,
        Request::ReminderStatus,
        Request::NotificationControl {
            operation: NotificationOperation::Inspect,
        },
        Request::NotificationControl {
            operation: NotificationOperation::RequestAuthorization,
        },
        Request::NotificationControl {
            operation: NotificationOperation::SetDeliveryEnabled { enabled: false },
        },
        Request::NotificationControl {
            operation: NotificationOperation::Repair,
        },
        Request::OpenAiChat,
    ] {
        let framed = FromShell::Request { id: 7, body };
        assert_eq!(
            serde_json::from_str::<FromShell>(&json(&framed)).unwrap(),
            framed
        );
    }
}

#[test]
fn local_profile_account_binding_has_an_exact_typed_wire_shape() {
    let profile_id = "00000000-0000-4000-8000-000000000001".to_owned();
    let old = "00000000-0000-4000-8000-000000000002".to_owned();
    let new = "00000000-0000-4000-8000-000000000003".to_owned();
    let request = FromShell::Request {
        id: 42,
        body: Request::BindLocalProfileAccount {
            profile_id: profile_id.clone(),
            expected_identity_id: Some(old.clone()),
            new_identity_id: new.clone(),
        },
    };
    let encoded = json(&request);
    assert!(encoded.contains(r#""kind":"bind_local_profile_account""#));
    assert!(encoded.contains(&format!(r#""expected_identity_id":"{old}""#)));
    assert!(encoded.contains(&format!(r#""new_identity_id":"{new}""#)));
    assert!(!encoded.contains("email"));
    assert!(!encoded.contains("name"));
    assert_eq!(
        serde_json::from_str::<FromShell>(&encoded).unwrap(),
        request
    );

    let response = FromAgent::Response {
        id: 42,
        body: Response::LocalProfileAccountBound {
            profile_id,
            outcome: LocalProfileAccountBindingOutcome::AlreadyBound,
        },
    };
    let encoded = json(&response);
    assert!(encoded.contains(r#""outcome":"already_bound""#));
    assert_eq!(
        serde_json::from_str::<FromAgent>(&encoded).unwrap(),
        response
    );
}
#[test]
fn local_profile_removal_has_a_correlated_typed_wire_shape() {
    let profile_id = "00000000-0000-4000-8000-000000000001".to_owned();
    let request = FromShell::Request {
        id: 41,
        body: Request::RemoveLocalProfile {
            profile_id: profile_id.clone(),
        },
    };
    let encoded = json(&request);
    assert!(encoded.contains(r#""kind":"remove_local_profile""#));
    assert_eq!(
        serde_json::from_str::<FromShell>(&encoded).unwrap(),
        request
    );

    let response = FromAgent::Response {
        id: 41,
        body: Response::LocalProfileRemoved {
            profile_id,
            outcome: LocalProfileRemovalOutcome::AlreadyRemoved,
        },
    };
    let encoded = json(&response);
    assert!(encoded.contains(r#""outcome":"already_removed""#));
    assert_eq!(
        serde_json::from_str::<FromAgent>(&encoded).unwrap(),
        response
    );
}

#[test]
fn status_carries_only_content_free_reminder_diagnostics() {
    let status = Response::Status {
        agent_version: "0.1.0".into(),
        device_id: "device".into(),
        instance_id: "instance".into(),
        shells: 1,
        pending_launches: 0,
        owns_the_machine: true,
        sync: SyncStatus::default(),
        reminders: ReminderDiagnostics {
            enabled: true,
            pending: 2,
            registered: 3,
            failed: 1,
            cursor: Some(9),
            next_deadline_ms: Some(20),
            last_successful_sync_ms: Some(19),
            attention: vec![ReminderAttention {
                profile: "profile".into(),
                reminder: "reminder".into(),
                occurrence: "occurrence".into(),
                fired_at_ms: 1_700_000_000_000,
            }],
            failure_category: Some("ledger".into()),
        },
    };
    let encoded = json(&status);
    assert_eq!(serde_json::from_str::<Response>(&encoded).unwrap(), status);
    for forbidden in ["private title", "profile_id", "registration_id", "target"] {
        assert!(
            !encoded.contains(forbidden),
            "status leaked {forbidden}: {encoded}"
        );
    }
}

#[test]
fn reminder_snooze_and_its_ack_have_pinned_content_free_shapes() {
    let push = FromAgent::Push {
        body: Push::ReminderSnooze {
            intent: ReminderSnoozeIntent {
                delivery: "11111111-1111-4111-8111-111111111111".into(),
                profile: "22222222-2222-4222-8222-222222222222".into(),
                reminder: "33333333-3333-4333-8333-333333333333".into(),
                occurrence: format!("occ1-{}", "ab".repeat(32)),
                entity: "task:44444444-4444-4444-8444-444444444444:55555555-5555-4555-8555-555555555555:66666666-6666-4666-8666-666666666666".into(),
            },
        },
    };
    let text = json(&push);
    assert!(text.contains(r#""kind":"reminder_snooze""#), "{text}");
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    let intent = &value["body"]["intent"];
    for forbidden in ["title", "body", "workspace_name", "profile_name"] {
        assert!(intent.get(forbidden).is_none(), "{forbidden} in {text}");
    }
    assert_eq!(serde_json::from_str::<FromAgent>(&text).unwrap(), push);

    let ack = Request::AcceptReminderSnooze {
        delivery: "11111111-1111-4111-8111-111111111111".into(),
    };
    assert_eq!(serde_json::from_str::<Request>(&json(&ack)).unwrap(), ack);
}

#[test]
fn the_two_version_numbers_are_not_the_same_declaration() {
    // They started equal, which is exactly why this was written: a bump to one
    // must not be assumed to move the other, and the update conversation is the
    // bump that separated them. The frame's version belongs to `koru-ipc`,
    // which holds no constant of its own so that the store's rendezvous and
    // this stream can differ.
    //
    // Literal numbers rather than an inequality, so that bumping one is a
    // deliberate edit here as well as there -- which is what caught the
    // `OpenLink` bump that took the protocol to 8 and left the frame alone.
    assert_eq!(PROTOCOL_VERSION, 43);
    assert_eq!(FRAME_VERSION, 1);
}

#[test]
fn routed_dictation_vectors_are_exact_content_free_and_round_trip() {
    let gestures = [
        (
            DictationGesture::Press {
                key: DictationKey::D,
            },
            r#"{"kind":"dictation_gesture","route":7,"gesture":{"kind":"press","key":"d"}}"#,
        ),
        (
            DictationGesture::Release {
                key: DictationKey::M,
            },
            r#"{"kind":"dictation_gesture","route":7,"gesture":{"kind":"release","key":"m"}}"#,
        ),
        (
            DictationGesture::Cancel {
                key: DictationKey::D,
            },
            r#"{"kind":"dictation_gesture","route":7,"gesture":{"kind":"cancel","key":"d"}}"#,
        ),
    ];
    for (gesture, expected) in gestures {
        let push = Push::DictationGesture { route: 7, gesture };
        assert_eq!(json(&push), expected);
        assert_eq!(serde_json::from_str::<Push>(expected).unwrap(), push);
    }

    let states = [
        (DictationState::Refused, r#"{"kind":"refused"}"#),
        (
            DictationState::Listening {
                mode: DictationMode::PushToTalk,
            },
            r#"{"kind":"listening","mode":"push_to_talk"}"#,
        ),
        (
            DictationState::Listening {
                mode: DictationMode::Toggle,
            },
            r#"{"kind":"listening","mode":"toggle"}"#,
        ),
        (DictationState::Finalising, r#"{"kind":"finalising"}"#),
        (DictationState::Idle, r#"{"kind":"idle"}"#),
    ];
    for (state, nested) in states {
        let frame = FromShell::Request {
            id: 9,
            body: Request::DictationState { route: 7, state },
        };
        let encoded = json(&frame);
        assert_eq!(
            encoded,
            format!(
                r#"{{"kind":"request","id":9,"body":{{"kind":"dictation_state","route":7,"state":{nested}}}}}"#
            )
        );
        assert_eq!(serde_json::from_str::<FromShell>(&encoded).unwrap(), frame);
        for forbidden in ["transcript", "text", "target", "device", "error", "message"] {
            assert!(
                !encoded.contains(forbidden),
                "{forbidden} crossed: {encoded}"
            );
        }
    }
}

/// **The grant pair is two frames and no vocabulary.** A grant says the
/// supervisor launched whoever holds it and nothing more: no caller name, no
/// product, no roles, no scopes. If a permission ever appeared inside one there
/// would be two places to look when something is allowed that should not be,
/// and this is where that would be caught.
#[test]
fn a_first_party_grant_carries_a_token_an_expiry_and_no_permissions() {
    let ask = json(&FromShell::Request {
        id: 4,
        body: Request::RequestFirstPartyGrant,
    });
    assert!(
        ask.contains(r#""kind":"request_first_party_grant""#),
        "{ask}"
    );

    let grant = FirstPartyGrant::new("secret-token".into(), 1_700_000_000_000);
    let issued = json(&FromAgent::Response {
        id: 4,
        body: Response::FirstPartyGrant {
            grant: grant.clone(),
        },
    });
    assert!(issued.contains(r#""kind":"first_party_grant""#), "{issued}");
    assert!(issued.contains(r#""token":"secret-token""#), "{issued}");
    assert!(
        issued.contains(r#""expires_at_millis":1700000000000"#),
        "{issued}"
    );
    for forbidden in ["scope", "role", "product", "permission", "pid"] {
        assert!(
            !issued.contains(forbidden),
            "a grant carried `{forbidden}`: {issued}"
        );
    }

    // The token serializes, because it has to cross; it does not print.
    let printed = format!("{grant:?}");
    assert!(!printed.contains("secret-token"), "{printed}");

    let check = json(&FromShell::Request {
        id: 5,
        body: Request::ValidateFirstPartyGrant { grant },
    });
    assert!(
        check.contains(r#""kind":"validate_first_party_grant""#),
        "{check}"
    );
    let verdict = json(&FromAgent::Response {
        id: 5,
        body: Response::FirstPartyGrantValidity { valid: false },
    });
    assert!(
        verdict.contains(r#""kind":"first_party_grant_validity""#),
        "{verdict}"
    );
    // One bit, deliberately. Unknown, expired and already-spent are the same
    // answer, or a guessing caller learns which guesses are getting warmer.
    assert!(verdict.contains(r#""valid":false"#), "{verdict}");
    assert!(!verdict.contains("reason"), "{verdict}");
}

#[test]
fn account_conversation_is_typed_pre_hello_and_debug_redacts_private_fields() {
    let command = AccountCommand::CreateProfile {
        profile_id: "00000000-0000-4000-8000-000000000001".into(),
        owner_display_name: "Seeded Owner Name".into(),
        name: "Seeded Private Name".into(),
        locale: Some("seeded-locale".into()),
    };
    let request = FromShell::Request {
        id: 1,
        body: Request::Account {
            command: command.clone(),
            expected_owner_scope: Some(4),
        },
    };
    assert_eq!(
        serde_json::from_str::<FromShell>(&json(&request)).unwrap(),
        request
    );
    let debug = format!("{command:?}");
    assert!(!debug.contains("Seeded Private Name"));
    assert!(!debug.contains("Seeded Owner Name"));
    assert!(!debug.contains("seeded-locale"));

    let url = BrowserUrl("https://example.test/?state=seeded-secret".into());
    assert_eq!(format!("{url:?}"), "BrowserUrl(REDACTED)");
    let browser = Push::AccountBrowser {
        owner_scope: 4,
        generation: 8,
        attempt: 2,
        url,
    };
    assert_eq!(
        serde_json::from_str::<Push>(&json(&browser)).unwrap(),
        browser
    );
    assert!(!format!("{browser:?}").contains("seeded-secret"));
}

#[test]
fn enrollment_retry_and_cancel_are_distinct_typed_commands() {
    for (command, kind) in [
        (
            AccountCommand::RetryEnrollment { profile_id: None },
            "retry_enrollment",
        ),
        (
            AccountCommand::RetryEnrollment {
                profile_id: Some("10000000-0000-4000-8000-000000000001".into()),
            },
            "retry_enrollment",
        ),
        (AccountCommand::CancelEnrollment, "cancel_enrollment"),
    ] {
        let encoded = json(&command);
        assert!(
            encoded.contains(&format!(r#""kind":"{kind}""#)),
            "{encoded}"
        );
        assert_eq!(
            serde_json::from_str::<AccountCommand>(&encoded).unwrap(),
            command
        );
    }
}

#[test]
fn profile_set_wire_preserves_scope_members_and_independent_progress() {
    let a = "10000000-0000-4000-8000-000000000001";
    let b = "10000000-0000-4000-8000-000000000002";
    for profile_ids in [vec![], vec![a.to_owned()], vec![a.to_owned(), b.to_owned()]] {
        let request = Request::Account {
            command: AccountCommand::SelectProfiles { profile_ids },
            expected_owner_scope: Some(9),
        };
        let encoded = json(&request);
        assert!(encoded.contains("select_profiles"));
        assert_eq!(serde_json::from_str::<Request>(&encoded).unwrap(), request);
    }
    assert!(
        serde_json::from_str::<AccountCommand>(&format!(
            r#"{{"kind":"select_profile","profile_id":"{a}"}}"#
        ))
        .is_err()
    );
    let rows = vec![
        AccountProfileEnrollment {
            profile_id: a.into(),
            state: EnrollmentState::Waiting,
            failure_code: Some("no_peer".into()),
            retry_at_millis: Some(45),
        },
        AccountProfileEnrollment {
            profile_id: b.into(),
            state: EnrollmentState::Complete,
            failure_code: None,
            retry_at_millis: None,
        },
    ];
    let state = AccountState {
        generation: 7,
        active_scope: 9,
        enrollments: rows.clone(),
        ..Default::default()
    };
    assert_eq!(
        serde_json::from_str::<AccountState>(&json(&state)).unwrap(),
        state
    );
    let result = Push::AccountResult {
        owner_scope: 9,
        generation: 7,
        result: AccountOperationResult::ProfilesSelected { profiles: rows },
    };
    assert_eq!(
        serde_json::from_str::<Push>(&json(&result)).unwrap(),
        result
    );
    assert_ne!(PROTOCOL_VERSION, 35);
    assert_ne!(PROTOCOL_VERSION, 32);
    assert_ne!(PROTOCOL_VERSION, 28);
}

#[test]
fn profile_choices_are_distinct_from_the_full_management_catalog() {
    for (command, kind) in [
        (AccountCommand::ListProfiles, "list_profiles"),
        (AccountCommand::ListProfileChoices, "list_profile_choices"),
    ] {
        let encoded = json(&command);
        assert!(encoded.contains(&format!(r#""kind":"{kind}""#)));
        assert_eq!(
            serde_json::from_str::<AccountCommand>(&encoded).unwrap(),
            command
        );
    }
}

#[test]
fn profile_lifecycle_commands_and_results_are_typed_and_debug_redacted() {
    let profile = "10000000-0000-4000-8000-000000000001".to_owned();
    for (command, kind) in [
        (
            AccountCommand::DeleteProfile {
                profile_id: profile.clone(),
                expected_lifecycle_revision: 2,
            },
            "delete_profile",
        ),
        (
            AccountCommand::InspectProfileLifecycle {
                profile_id: profile.clone(),
            },
            "inspect_profile_lifecycle",
        ),
        (
            AccountCommand::AcknowledgeProfileCleanup {
                profile_id: profile.clone(),
                stage: ProfileCleanupStage::ProfileRemoved,
            },
            "acknowledge_profile_cleanup",
        ),
        (
            AccountCommand::ReconcileProfileLifecycles,
            "reconcile_profile_lifecycles",
        ),
    ] {
        let encoded = serde_json::to_string(&command).unwrap();
        assert!(encoded.contains(&format!(r#""kind":"{kind}""#)));
        assert_eq!(
            serde_json::from_str::<AccountCommand>(&encoded).unwrap(),
            command
        );
        assert!(!format!("{command:?}").contains(&profile));
    }

    let lifecycle = AccountProfileLifecycleRow {
        profile_id: profile.clone(),
        lifecycle_state: AccountProfileLifecycleState::Deleted,
        lifecycle_revision: 3,
        operation_id: Some("20000000-0000-4000-8000-000000000002".into()),
        cleanup_stage: Some(ProfileCleanupStage::HostedCommitted),
        failure_code: Some("account_unavailable".into()),
    };
    let result = AccountOperationResult::ProfileDeleted {
        lifecycle,
        exact_retry: true,
    };
    let encoded = serde_json::to_string(&result).unwrap();
    assert_eq!(
        serde_json::from_str::<AccountOperationResult>(&encoded).unwrap(),
        result
    );
    assert!(!format!("{result:?}").contains(&profile));
    assert!(!encoded.contains("email"));
    assert!(!encoded.contains("display_name"));
}

#[test]
fn ai_chat_control_is_typed_and_keeps_exact_link_ids() {
    let request = AiChatRequest::OpenChat {
        chat: "00000000-0000-4000-8000-000000000001".into(),
        block: Some("00000000-0000-4000-8000-000000000002".into()),
    };
    let text = json(&request);
    assert!(text.contains(r#""kind":"open_chat""#), "{text}");
    assert!(text.contains(r#""chat":"00000000-0000-4000-8000-000000000001""#));
    assert_eq!(
        serde_json::from_str::<AiChatRequest>(&text).unwrap(),
        request
    );

    let response = AiChatResponse::Accepted {
        pid: 42,
        focused: true,
    };
    assert_eq!(
        serde_json::from_str::<AiChatResponse>(&json(&response)).unwrap(),
        response
    );
    assert_eq!(AI_CHAT_PROTOCOL_VERSION, 1);
}

#[test]
fn link_codes_are_redacted_from_debug_output() {
    let request = Request::AcceptLinkCode {
        code: LinkCode("koru-link-v1:do-not-print-me".into()),
    };
    let debug = format!("{request:?}");
    assert!(debug.contains("REDACTED"));
    assert!(!debug.contains("do-not-print-me"));
}

#[test]
fn signed_in_identity_is_correlated_typed_and_debug_redacted() {
    let command = AccountCommand::InspectAccountSession;
    let encoded = json(&command);
    assert!(encoded.contains(r#""kind":"inspect_account_session""#));
    assert_eq!(
        serde_json::from_str::<AccountCommand>(&encoded).unwrap(),
        command
    );

    let result = AccountOperationResult::SignedInIdentity {
        provider: "google".into(),
        verified_email: "private-session@example.test".into(),
    };
    let encoded = json(&result);
    assert!(encoded.contains("private-session@example.test"));
    assert_eq!(
        serde_json::from_str::<AccountOperationResult>(&encoded).unwrap(),
        result
    );
    let debug = format!("{result:?}");
    assert!(!debug.contains("private-session@example.test"));
    assert!(!debug.contains("google"));
}

#[test]
fn profile_authorization_rows_and_removal_are_typed_and_redacted() {
    let command = AccountCommand::RemoveProfileAuthorization {
        profile_id: "00000000-0000-4000-8000-000000000001".into(),
        auth_identity_id: "00000000-0000-4000-8000-000000000002".into(),
        confirm_profile_offline_only: true,
    };
    let encoded = json(&command);
    assert!(encoded.contains("confirm_profile_offline_only"));
    assert_eq!(
        serde_json::from_str::<AccountCommand>(&encoded).unwrap(),
        command
    );
    assert!(!format!("{command:?}").contains("00000000"));

    let row = AccountProfileRow {
        koru_identity_id: "koru-identity-private".into(),
        profile_id: "profile-private".into(),
        name: Some("Private profile".into()),
        locale: Some("en-US".into()),
        auth_identities: vec![AccountAuthIdentityRow {
            auth_identity_id: "identity-private".into(),
            provider: "google".into(),
            verified_email: Some("private@example.test".into()),
        }],
    };
    let encoded = json(&row);
    assert_eq!(
        serde_json::from_str::<AccountProfileRow>(&encoded).unwrap(),
        row
    );
    let debug = format!("{row:?} {:?}", row.auth_identities[0]);
    assert!(!debug.contains("Private profile"));
    assert!(!debug.contains("private@example.test"));
    assert!(!debug.contains("koru-identity-private"));

    let result = AccountOperationResult::ProfileAuthorizationRemoved {
        profile_id: "profile-private".into(),
        profile_offline_only: true,
        already_removed: false,
    };
    assert_eq!(
        serde_json::from_str::<AccountOperationResult>(&json(&result)).unwrap(),
        result
    );
}

#[test]
fn staged_google_candidate_and_owner_ack_are_typed_and_redacted() {
    let candidate = AccountOperationResult::AddGoogleCandidate {
        candidate_id: "candidate-secret".into(),
        profile_id: "profile-private".into(),
        verified_email: "person@example.test".into(),
        expires_at_millis: 123,
    };
    assert_eq!(
        serde_json::from_str::<AccountOperationResult>(&json(&candidate)).unwrap(),
        candidate
    );
    let debug = format!("{candidate:?}");
    for secret in ["candidate-secret", "profile-private", "person@example.test"] {
        assert!(!debug.contains(secret));
    }

    let confirmed = AccountOperationResult::GoogleAuthorizationAdded {
        koru_identity_id: "koru-identity-private".into(),
        profile_id: "profile-private".into(),
        already_authorized: false,
    };
    assert_eq!(
        serde_json::from_str::<AccountOperationResult>(&json(&confirmed)).unwrap(),
        confirmed
    );
    let debug = format!("{confirmed:?}");
    assert!(!debug.contains("koru-identity-private"));
    assert!(!debug.contains("profile-private"));
}

#[test]
fn a_build_without_koru_cloud_says_not_configured_not_unavailable() {
    let state = AccountState::not_configured();
    assert!(json(&state).contains(r#""stage":"not_configured""#));
    assert_eq!(
        serde_json::from_str::<AccountState>(&json(&state)).unwrap(),
        state
    );
    // Nothing went wrong, so nothing says it did, and there is no capability.
    assert_eq!(state.failure_code, None);
    assert_eq!(state.retry_at_millis, None);
    assert!(!state.capability);

    assert_eq!(
        AccountState::without_capability(false).stage,
        AccountStage::NotConfigured
    );
    // A build that has a Cloud and no capability is the old answer.
    assert_eq!(
        AccountState::without_capability(true),
        AccountState::default()
    );
    assert_eq!(AccountState::default().stage, AccountStage::Unavailable);
}
