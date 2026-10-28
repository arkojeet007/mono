// SPDX-License-Identifier: MPL-2.0

use super::*;

fn displaying(payload: &str) -> koru_agent_wire::PairingView {
    koru_agent_wire::PairingView {
        state: koru_agent_wire::PairingState::Displaying,
        qr_payload: Some(payload.into()),
        expires_in_seconds: 120,
        peer_name: None,
        error_code: None,
    }
}

fn succeeded(peer: Option<&str>) -> koru_agent_wire::PairingView {
    koru_agent_wire::PairingView {
        state: koru_agent_wire::PairingState::Succeeded,
        qr_payload: None,
        expires_in_seconds: 0,
        peer_name: peer.map(str::to_owned),
        error_code: None,
    }
}

#[test]
fn the_two_tabs_use_the_agreed_copy() {
    let panel = LinkDevice::panel((900.0, 700.0));
    let tabs = LinkDevice::tabs(panel);
    assert_eq!(
        tabs.map(|(tab, _)| tab),
        [Tab::New, Tab::Enter],
        "the Linked devices tab is Manage's page now"
    );
    assert_eq!(
        pairing_label(koru_agent_wire::PairingState::Succeeded),
        "Device linked"
    );
}

#[test]
fn a_tab_is_chosen_where_it_is_drawn() {
    let mut engine = TextEngine::new();
    let mut modal = LinkDevice::new(&mut engine);
    let viewport = (900.0, 700.0);
    let (x, y) = LinkDevice::tabs(LinkDevice::panel(viewport))[1].1.centre();
    assert!(matches!(
        modal.mouse_down(&mut engine, viewport, x, y),
        Outcome::Keep
    ));
    assert_eq!(modal.tab, Tab::Enter);
}

#[test]
fn the_qr_carries_the_link_and_the_clipboard_only_the_code() {
    let mut engine = TextEngine::new();
    let mut modal = LinkDevice::new(&mut engine);
    let payload = "https://koru.do/link-device#koru-pair:v1c:example";
    modal.set_pairing(&mut engine, displaying(payload));
    assert_eq!(modal.qr, qr_matrix(payload));
    let panel = LinkDevice::panel((900.0, 700.0));
    let (x, y) = LinkDevice::action_rect(panel, true).centre();
    match modal.mouse_down(&mut engine, (900.0, 700.0), x, y) {
        Outcome::Copy(value) => assert_eq!(value, "koru-pair:v1c:example"),
        _ => panic!("copy action was not returned"),
    }
}

#[test]
fn a_payload_without_the_link_is_copied_as_it_is() {
    assert_eq!(link_code("koru-pair:v1c:example"), "koru-pair:v1c:example");
}

#[test]
fn entering_a_code_accepts_it_by_key_and_by_button() {
    let mut engine = TextEngine::new();
    let mut modal = LinkDevice::new(&mut engine);
    let viewport = (900.0, 700.0);
    modal.tab = Tab::Enter;
    // Nothing typed is nothing to accept.
    assert!(matches!(
        modal.key(
            &mut engine,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty()
        ),
        Outcome::Keep
    ));
    modal.input.set_text(&mut engine, "koru-pair:v1c:secret");
    assert!(matches!(
        modal.key(&mut engine, &Key::Named(NamedKey::Enter), None, ModifiersState::empty()),
        Outcome::Accept(ref code) if code == "koru-pair:v1c:secret"
    ));
    let (x, y) = LinkDevice::action_rect(LinkDevice::panel(viewport), false).centre();
    assert!(matches!(
        modal.mouse_down(&mut engine, viewport, x, y),
        Outcome::Accept(ref code) if code == "koru-pair:v1c:secret"
    ));
}

/// maintainers' decision 12: success says *Device linked*, and *Done* is what takes
/// the person to the list.
#[test]
fn pairing_success_clears_the_secret_and_says_device_linked_with_done() {
    let mut engine = TextEngine::new();
    let mut modal = LinkDevice::new(&mut engine);
    let viewport = (900.0, 700.0);
    modal.tab = Tab::Enter;
    modal.input.set_text(&mut engine, "koru-pair:v1c:secret");
    assert!(modal.set_pairing(&mut engine, succeeded(Some("Other device"))));
    assert!(modal.linked);
    assert!(modal.input.is_empty(), "the one-use secret left the field");

    // The tabs are gone; the button that was Close is Done.
    let panel = LinkDevice::panel(viewport);
    let (x, y) = LinkDevice::tabs(panel)[0].1.centre();
    assert!(matches!(
        modal.mouse_down(&mut engine, viewport, x, y),
        Outcome::Keep
    ));
    let (x, y) = LinkDevice::close_rect(panel).centre();
    assert!(matches!(
        modal.mouse_down(&mut engine, viewport, x, y),
        Outcome::Done
    ));
    // Enter is Done too; Escape only closes.
    assert!(matches!(
        modal.key(
            &mut engine,
            &Key::Named(NamedKey::Enter),
            None,
            ModifiersState::empty()
        ),
        Outcome::Done
    ));
    assert!(matches!(
        modal.key(
            &mut engine,
            &Key::Named(NamedKey::Escape),
            None,
            ModifiersState::empty()
        ),
        Outcome::Close
    ));
}

#[test]
fn a_state_short_of_success_keeps_the_form() {
    let mut engine = TextEngine::new();
    let mut modal = LinkDevice::new(&mut engine);
    assert!(!modal.set_pairing(&mut engine, displaying("koru-pair-v1:x")));
    assert!(!modal.linked);
    let viewport = (900.0, 700.0);
    let (x, y) = LinkDevice::close_rect(LinkDevice::panel(viewport)).centre();
    assert!(matches!(
        modal.mouse_down(&mut engine, viewport, x, y),
        Outcome::Close
    ));
}

#[test]
fn copy_feedback_expires_without_a_spinning_animation() {
    let mut engine = TextEngine::new();
    let mut modal = LinkDevice::new(&mut engine);
    let now = Instant::now();
    modal.mark_copied(now);
    assert_eq!(modal.copy_label(now), "Code copied");
    assert_eq!(modal.maintenance_deadline(), Some(now + COPY_FEEDBACK));
    assert!(!modal.maintain(now + COPY_FEEDBACK - Duration::from_millis(1)));
    assert!(modal.maintain(now + COPY_FEEDBACK));
    assert_eq!(modal.copy_label(now + COPY_FEEDBACK), "Copy link code");
    assert_eq!(modal.maintenance_deadline(), None);
}

/// A device the Agent says cannot sync shows the problem instead of the
/// tabs, and *Start over* takes a confirmation that Escape backs out of.
#[test]
fn an_identity_problem_offers_start_over_behind_a_confirmation() {
    let mut engine = TextEngine::new();
    let mut modal = LinkDevice::new(&mut engine);
    let viewport = (900.0, 700.0);
    let panel = LinkDevice::panel(viewport);
    modal.set_identity_problem(Some("identity_unavailable".into()));

    let tab = LinkDevice::tabs(panel)[1].1.centre();
    assert!(matches!(
        modal.mouse_down(&mut engine, viewport, tab.0, tab.1),
        Outcome::Keep
    ));
    assert_eq!(modal.tab, Tab::New, "the tabs are not there to press");

    let start = LinkDevice::start_over_rect(panel).centre();
    assert!(matches!(
        modal.mouse_down(&mut engine, viewport, start.0, start.1),
        Outcome::Keep
    ));
    assert_eq!(modal.start_over, StartOver::Confirming);
    modal.key(
        &mut engine,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(modal.start_over, StartOver::Offered, "Escape backs out");

    modal.mouse_down(&mut engine, viewport, start.0, start.1);
    let confirm = LinkDevice::action_rect(panel, false).centre();
    assert!(matches!(
        modal.mouse_down(&mut engine, viewport, confirm.0, confirm.1),
        Outcome::StartOver
    ));
    assert_eq!(modal.start_over, StartOver::Working);
    modal.start_over_refused();
    assert_eq!(modal.start_over, StartOver::Failed);
}

/// A working identity never offers it.
#[test]
fn a_working_identity_has_no_start_over() {
    let mut engine = TextEngine::new();
    let mut modal = LinkDevice::new(&mut engine);
    let viewport = (900.0, 700.0);
    let start = LinkDevice::start_over_rect(LinkDevice::panel(viewport)).centre();
    modal.mouse_down(&mut engine, viewport, start.0, start.1);
    assert_eq!(modal.start_over, StartOver::Offered);
}
