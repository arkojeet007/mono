// SPDX-License-Identifier: MPL-2.0

use super::*;
use koru_projection::model::ListedDocument;
use koru_projection::{DocumentProjection, ProjectedNode};
use koru_store_api::state::ReminderProjectionService;
use koru_store_core::{
    DocumentId, EffectiveState, EntityRef, OccurrenceKey, OccurrenceMoment, OccurrenceSource,
    OneTimeSchedule, ReminderId, ReminderReductionInput, ReminderRule, RuleRevisionId, RuleStatus,
    StoredReminder, WorkspaceId, ZonedDateTime,
};

fn node(id: &str) -> ProjectedNode {
    ProjectedNode {
        id: id.into(),
        parent: None,
        ordinal: 0,
        depth: 0,
        kind: "paragraph".into(),
        text: "Target".into(),
        heading: None,
        list_style: None,
        done: false,
        level_name: None,
        name: None,
        icon: None,
        colour: None,
        language: None,
        blob: None,
        mime: None,
        embed: None,
        runs: Vec::new(),
        attrs: Vec::new(),
        in_trash: false,
    }
}

fn reminder(workspace: WorkspaceId, document: DocumentId, node: &str, at: i64) -> StoredReminder {
    StoredReminder {
        rule: ReminderRule {
            id: ReminderId::new(),
            target: EntityRef::new(
                "node",
                workspace,
                document,
                uuid::Uuid::parse_str(node).unwrap(),
            )
            .unwrap(),
            revision: RuleRevisionId::new(),
            status: RuleStatus::Enabled,
            schedule: OneTimeSchedule::Zoned {
                value: ZonedDateTime::new(at, "UTC").unwrap(),
            },
            tone_override: None,
        },
        deleted: false,
        actions: Vec::new(),
    }
}

fn port_with(reminder: &StoredReminder, sweeps: Sweeps) -> ReadPort {
    let workspace = reminder.rule.target.workspace();
    let document = reminder.rule.target.document();
    let node_id = reminder.rule.target.node().hyphenated().to_string();
    let mut projections = Projections::in_memory().unwrap();
    projections
        .replace(
            &DocumentProjection {
                workspace,
                document,
                source_revision: vec![1],
                nodes: vec![node(&node_id)],
            },
            1,
            None,
        )
        .unwrap();
    projections
        .set_listing(
            workspace,
            "Workspace",
            &[ListedDocument {
                document,
                parent: None,
                ordinal: 0,
                title: "Document".into(),
                kind: "page".into(),
                mime: None,
                trashed_at: None,
                trashed_by: None,
            }],
            1,
        )
        .unwrap();
    ReadPort {
        projections: Mutex::new(projections),
        sweeps,
    }
}

fn visible_input(reminder: StoredReminder) -> ReminderReductionInput {
    ReminderReductionInput::new(reminder, std::collections::BTreeSet::new())
}

#[test]
fn snapshot_waits_for_a_new_generation_even_after_an_earlier_sweep() {
    let workspace = WorkspaceId::new();
    let stored = reminder(
        workspace,
        DocumentId::new(),
        "11111111-1111-4111-8111-111111111111",
        100,
    );
    let sweeps = Sweeps::answering_for_test();
    sweeps.record_for_test(workspace);
    let port = port_with(&stored, sweeps.clone());

    let snapshot = port
        .snapshot(&[visible_input(stored)], &ReminderSettings::default())
        .unwrap();
    // The earlier sweep was the first ticket; the snapshot asked for, and
    // was answered by, a second one.
    assert_eq!(sweeps.finished(workspace), 2);
    assert!(snapshot.updating_workspaces.is_empty());
}

#[test]
fn snapshot_marks_a_prior_finished_workspace_updating_until_the_new_request_finishes() {
    let workspace = WorkspaceId::new();
    let stored = reminder(
        workspace,
        DocumentId::new(),
        "22222222-2222-4222-8222-222222222222",
        100,
    );
    let sweeps = Sweeps::default();
    sweeps.record_for_test(workspace);
    let port = port_with(&stored, sweeps);

    let snapshot = port
        .snapshot(&[visible_input(stored)], &ReminderSettings::default())
        .unwrap();
    assert_eq!(snapshot.updating_workspaces, vec![workspace]);
    assert!(!snapshot.reminders[0].target.projected);
    assert_eq!(snapshot.reminders[0].target.state, TargetState::Missing);
    assert!(snapshot.reminders[0].target.due.is_none());
}

#[test]
fn a_missing_node_in_a_current_document_is_authoritative_not_cold() {
    let workspace = WorkspaceId::new();
    let document = DocumentId::new();
    let present = reminder(
        workspace,
        document,
        "44444444-4444-4444-8444-444444444444",
        100,
    );
    let missing = reminder(
        workspace,
        document,
        "55555555-5555-4555-8555-555555555555",
        100,
    );
    let port = port_with(&present, Sweeps::answering_for_test());

    let snapshot = port
        .snapshot(&[visible_input(missing)], &ReminderSettings::default())
        .unwrap();
    assert!(snapshot.updating_workspaces.is_empty());
    assert!(snapshot.reminders[0].target.projected);
    assert_eq!(snapshot.reminders[0].target.state, TargetState::Missing);
}

#[test]
fn missed_and_upcoming_require_one_consistent_explicit_device_context() {
    let port = ReadPort {
        projections: Mutex::new(Projections::in_memory().unwrap()),
        sweeps: Sweeps::default(),
    };
    for query in [
        ReminderQuery {
            view: ReminderView::Missed,
            now_ms: None,
            device_zone: Some("UTC".into()),
        },
        ReminderQuery {
            view: ReminderView::Upcoming,
            now_ms: Some(1),
            device_zone: None,
        },
        ReminderQuery {
            view: ReminderView::Missed,
            now_ms: Some(1),
            device_zone: Some("US/Eastern".into()),
        },
    ] {
        assert_eq!(
            port.query(&[], &ReminderSettings::default(), &query)
                .unwrap_err()
                .code(),
            Code::InvalidRequest
        );
    }
    assert!(
        port.query(
            &[],
            &ReminderSettings::default(),
            &ReminderQuery {
                view: ReminderView::Active,
                now_ms: None,
                device_zone: None,
            }
        )
        .is_ok()
    );
}

#[test]
fn missed_and_upcoming_are_disjoint_while_active_is_the_deliverable_superset() {
    let workspace = WorkspaceId::new();
    let past = reminder(
        workspace,
        DocumentId::new(),
        "33333333-3333-4333-8333-333333333333",
        99,
    );
    let future = reminder(
        workspace,
        past.rule.target.document(),
        "33333333-3333-4333-8333-333333333333",
        101,
    );
    let port = port_with(&past, Sweeps::answering_for_test());
    let runner = |view| {
        let result = port
            .query(
                &[visible_input(past.clone()), visible_input(future.clone())],
                &ReminderSettings::default(),
                &ReminderQuery {
                    view,
                    now_ms: Some(100),
                    device_zone: Some("UTC".into()),
                },
            )
            .unwrap();
        result.reminders
    };
    assert_eq!(runner(ReminderView::Missed).len(), 1);
    assert_eq!(runner(ReminderView::Upcoming).len(), 1);
    assert_eq!(runner(ReminderView::Active).len(), 2);
}

#[test]
fn cleared_handled_fact_is_terminal_in_reconciliation_but_absent_from_api_history() {
    let workspace = WorkspaceId::new();
    let stored = reminder(
        workspace,
        DocumentId::new(),
        "66666666-6666-4666-8666-666666666666",
        100,
    );
    let source = match &stored.rule.schedule {
        OneTimeSchedule::Zoned { value } => OccurrenceSource::OneTime {
            moment: OccurrenceMoment::Zoned {
                value: value.clone(),
            },
        },
        _ => unreachable!(),
    };
    let occurrence = OccurrenceKey::derive(stored.rule.id, stored.rule.revision, &source);
    let input = ReminderReductionInput::new(
        stored.clone(),
        std::collections::BTreeSet::from([occurrence]),
    );
    let port = port_with(&stored, Sweeps::answering_for_test());

    let snapshot = port
        .snapshot(std::slice::from_ref(&input), &ReminderSettings::default())
        .unwrap();
    let row = &snapshot.reminders[0];
    assert!(row.stored.actions.is_empty());
    assert!(row.reduced.handled_history.is_empty());
    assert_eq!(
        row.reduced.effective,
        EffectiveState::Handled { occurrence }
    );
    let wire = serde_json::to_value(row).unwrap();
    assert_eq!(wire["stored"]["actions"], serde_json::json!([]));
    assert_eq!(wire["reduced"]["effective"]["state"], "handled");

    for view in [ReminderView::Active, ReminderView::HandledHistory] {
        let result = port
            .query(
                std::slice::from_ref(&input),
                &ReminderSettings::default(),
                &ReminderQuery {
                    view: view.clone(),
                    now_ms: None,
                    device_zone: None,
                },
            )
            .unwrap();
        assert!(result.reminders.is_empty(), "{view:?}");
    }
}

#[test]
fn a_moved_document_does_not_leave_healthy_reminders_waiting_for_projection() {
    for completed in [false, true] {
        for missing_first in [false, true] {
            let workspace = WorkspaceId::new();
            let healthy = reminder(
                workspace,
                DocumentId::new(),
                "77777777-7777-4777-8777-777777777777",
                100,
            );
            let old = reminder(
                workspace,
                DocumentId::new(),
                "88888888-8888-4888-8888-888888888888",
                100,
            );
            // `completed`: the sweep the snapshot asks for has finished.
            let sweeps = if completed {
                Sweeps::answering_for_test()
            } else {
                Sweeps::default()
            };
            let port = port_with(&healthy, sweeps);
            // The document still exists, but is now listed in a different
            // workspace. Its old EntityRef must not borrow that location's
            // projection or keep healthy siblings permanently updating.
            let destination = WorkspaceId::new();
            {
                let mut projections = port.projections.lock().unwrap();
                projections
                    .replace(
                        &DocumentProjection {
                            workspace: destination,
                            document: old.rule.target.document(),
                            source_revision: vec![1],
                            nodes: vec![node(&old.rule.target.node().to_string())],
                        },
                        1,
                        None,
                    )
                    .unwrap();
                projections
                    .set_listing(
                        destination,
                        "Destination",
                        &[ListedDocument {
                            document: old.rule.target.document(),
                            parent: None,
                            ordinal: 0,
                            title: "Moved document".into(),
                            kind: "page".into(),
                            mime: None,
                            trashed_at: None,
                            trashed_by: None,
                        }],
                        1,
                    )
                    .unwrap();
            }
            let mut inputs = vec![visible_input(healthy.clone()), visible_input(old.clone())];
            if missing_first {
                inputs.reverse();
            }
            let snapshot = port
                .snapshot(&inputs, &ReminderSettings::default())
                .unwrap();
            if completed {
                assert!(
                    snapshot.updating_workspaces.is_empty(),
                    "a completed sweep cannot wait for a document that moved away"
                );
                let live = snapshot
                    .reminders
                    .iter()
                    .find(|row| row.stored.rule.id == healthy.rule.id)
                    .unwrap();
                assert!(live.target.projected);
                assert_eq!(live.target.state, TargetState::Active);
                assert!(matches!(
                    live.reduced.effective,
                    EffectiveState::Scheduled { .. }
                ));
                let missing = snapshot
                    .reminders
                    .iter()
                    .find(|row| row.stored.rule.id == old.rule.id)
                    .unwrap();
                assert!(
                    missing.target.projected,
                    "the completed listing proves this exact target is absent"
                );
                assert_eq!(missing.target.state, TargetState::Missing);
                assert!(missing.target.due.is_none());
            } else {
                assert_eq!(snapshot.updating_workspaces, vec![workspace]);
                assert!(
                    snapshot.reminders.iter().all(|row| !row.target.projected),
                    "a cold listing cannot authorize missing-target cleanup"
                );
            }
        }
    }
}

#[test]
fn a_listed_document_without_projection_stays_uncertain_after_a_sweep() {
    let workspace = WorkspaceId::new();
    let stored = reminder(
        workspace,
        DocumentId::new(),
        "99999999-9999-4999-8999-999999999999",
        100,
    );
    let mut projections = Projections::in_memory().unwrap();
    projections
        .set_listing(
            workspace,
            "Workspace",
            &[ListedDocument {
                document: stored.rule.target.document(),
                parent: None,
                ordinal: 0,
                title: "Not projected yet".into(),
                kind: "page".into(),
                mime: None,
                trashed_at: None,
                trashed_by: None,
            }],
            1,
        )
        .unwrap();
    let port = ReadPort {
        projections: Mutex::new(projections),
        sweeps: Sweeps::answering_for_test(),
    };
    let snapshot = port
        .snapshot(&[visible_input(stored)], &ReminderSettings::default())
        .unwrap();
    assert_eq!(snapshot.updating_workspaces, vec![workspace]);
    assert!(
        !snapshot.reminders[0].target.projected,
        "listing presence without readable content cannot authorize cleanup"
    );
}
