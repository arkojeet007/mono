// SPDX-License-Identifier: MPL-2.0

//! The Linux menu, [`linux_menu`], held as a value.
//!
//! The tray is a surface no test can open, so everything it says is decided
//! by a pure function and asserted here — on every platform, because the
//! function has no platform code and a drift found on Windows CI is still a
//! drift.

use super::*;
use crate::registry::{ConnectionId, OpenWorkspace};

/// A Linux menu with everything switched on and something in every list: the
/// fixture each test below starts from, so what a test is about is the one
/// field it changes.
fn full() -> MenuState {
    MenuState {
        lang: koru_i18n::Lang::En,
        surfaces: Surfaces {
            ai: true,
            reminders: true,
            agents: true,
        },
        shortcut: Shortcut::Unknown,
        ai_shortcut: None,
        store_running: None,
        // Installed with a worker holding the model, so every voice action is
        // pressable — the state that offers the most.
        voice: VoiceMenu {
            row: VoiceRow::Installed,
            installed: vec![koru_voice_models::ModelTier::Compact],
            worker: Some(VoiceWorker {
                state: koru_voice::State::Idle,
                model_loaded: true,
                last_error: None,
            }),
            devices: vec![koru_voice::InputDevice::new("USB_Headset", true)],
            input: koru_voice::InputChoice::SystemDefault,
        },
        dictation_shortcut: Shortcut::Unknown,
        workspaces: vec![OpenWorkspace {
            connection: ConnectionId(7),
            label: "Notes — Personal".to_owned(),
        }],
        agents: vec![
            AgentsLine::Separator,
            AgentsLine::Header("Koru".to_owned()),
            AgentsLine::Session {
                session: koru_session_protocol::SessionId::mint(),
                label: "Claude Code".to_owned(),
            },
        ],
        relay: Some(true),
        relay_status: koru_network_iroh::RelayConnectionStatus::Off,
        about: about_line("0.3.1", 42, "release"),
        about_enabled: true,
    }
}

/// Every ask in the menu, submenus included, with whether its row is
/// pressable.
fn asks(rows: &[MenuRow]) -> Vec<(TrayAsk, bool)> {
    let mut out = Vec::new();
    for row in rows {
        match row {
            MenuRow::Separator => {}
            MenuRow::Item {
                ask: Some(ask),
                enabled,
                ..
            } => out.push((ask.clone(), *enabled)),
            MenuRow::Item { ask: None, .. } => {}
            MenuRow::Check { ask, enabled, .. } => out.push((ask.clone(), *enabled)),
            MenuRow::Submenu { rows, .. } => out.extend(asks(rows)),
        }
    }
    out
}

/// The row (or submenu) with this label, searched depth first.
fn find<'a>(rows: &'a [MenuRow], wanted: &str) -> Option<&'a MenuRow> {
    rows.iter().find_map(|row| match row {
        MenuRow::Item { label, .. } | MenuRow::Check { label, .. } if label == wanted => Some(row),
        MenuRow::Submenu { label, .. } if label == wanted => Some(row),
        MenuRow::Submenu { rows, .. } => find(rows, wanted),
        _ => None,
    })
}

fn submenu<'a>(rows: &'a [MenuRow], wanted: &str) -> &'a [MenuRow] {
    match find(rows, wanted) {
        Some(MenuRow::Submenu { rows, .. }) => rows,
        other => panic!("no submenu {wanted:?}: {other:?}"),
    }
}

fn labels(rows: &[MenuRow]) -> Vec<&str> {
    rows.iter()
        .map(|row| match row {
            MenuRow::Separator => "—",
            MenuRow::Item { label, .. }
            | MenuRow::Check { label, .. }
            | MenuRow::Submenu { label, .. } => label.as_str(),
        })
        .collect()
}

/// **The rule.** Quit is the last row whatever else is switched off, because
/// an agent with no window is stoppable from here or from nowhere.
#[test]
fn quit_is_last_and_offered_in_every_state() {
    let mut bare = full();
    bare.surfaces = Surfaces {
        ai: false,
        reminders: false,
        agents: false,
    };
    bare.voice = voice_state_switched_off(koru_voice::Off::Platform);
    bare.workspaces.clear();
    bare.agents.clear();
    bare.relay = None;
    bare.about_enabled = false;
    for state in [full(), bare] {
        let rows = linux_menu(&state);
        assert_eq!(
            rows.last(),
            Some(&MenuRow::action("Quit Koru Agent", TrayAsk::Quit))
        );
    }
}

/// **Linux offers what Windows and macOS offer, less the two rows it leaves
/// out on purpose.** [`OFFERED`] is the list the other platforms' tests hold
/// `imp` to; this ties the Linux menu to the same list, so a row added to
/// `imp` and to `OFFERED` and forgotten here fails a test rather than a
/// person.
#[test]
fn the_linux_menu_offers_everything_offered_except_links_and_backup() {
    let left_out = [
        TrayAsk::RegisterUrlScheme,
        TrayAsk::UnregisterUrlScheme,
        TrayAsk::BackupStore,
    ];
    let expected: std::collections::BTreeSet<String> = OFFERED
        .iter()
        .filter(|ask| !left_out.contains(ask))
        .map(|ask| format!("{ask:?}"))
        .collect();
    let rows = linux_menu(&full());
    let offered: std::collections::BTreeSet<String> = asks(&rows)
        .into_iter()
        .filter(|(ask, _)| {
            !matches!(
                ask,
                TrayAsk::OpenWorkspace(_)
                    | TrayAsk::ShowAgentSession(_)
                    | TrayAsk::ChooseVoiceInput(_)
            )
        })
        .map(|(ask, _)| format!("{ask:?}"))
        .collect();
    assert_eq!(offered, expected);
}

#[test]
fn links_and_backup_are_absent_on_linux() {
    let asks = asks(&linux_menu(&full()));
    for absent in [
        TrayAsk::RegisterUrlScheme,
        TrayAsk::UnregisterUrlScheme,
        TrayAsk::BackupStore,
    ] {
        assert!(!asks.iter().any(|(ask, _)| *ask == absent), "{absent:?}");
    }
    let rows = linux_menu(&full());
    assert!(find(&rows, "Links").is_none());
    assert!(find(&rows, "Backup Store…").is_none());
}

/// The same order `imp::Tray::show` appends in.
#[test]
fn the_top_level_follows_imps_order() {
    let rows = linux_menu(&full());
    assert_eq!(
        labels(&rows),
        [
            "Koru AI Chat",
            "Search",
            koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::ReminderAll),
            "Voice input",
            "Workspaces",
            koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::TerminalAgents),
            "API Gateway",
            "Store",
            "—",
            "Data locations",
            koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::RelayUse),
            "Relay off",
            "—",
            "About Koru",
            "Check for updates",
            "—",
            "Quit Koru Agent",
        ]
    );
}

/// A surface this build hides is not in the menu at all — `imp`'s
/// `koru-features` rule.
#[test]
fn a_hidden_surface_has_no_row() {
    let mut state = full();
    state.surfaces = Surfaces {
        ai: false,
        reminders: false,
        agents: false,
    };
    let asks = asks(&linux_menu(&state));
    for hidden in [TrayAsk::AiChat, TrayAsk::Reminders, TrayAsk::NewAgent] {
        assert!(!asks.iter().any(|(ask, _)| *ask == hidden), "{hidden:?}");
    }
    assert!(
        !asks
            .iter()
            .any(|(ask, _)| matches!(ask, TrayAsk::ShowAgentSession(_)))
    );
}

/// What `imp::Tray::set_shortcut` makes the Search row say, state by state.
#[test]
fn the_search_row_says_what_was_registered() {
    let row = |shortcut: Shortcut| {
        let mut state = full();
        state.shortcut = shortcut;
        linux_menu(&state)
            .into_iter()
            .find(|row| {
                matches!(
                    row,
                    MenuRow::Item {
                        ask: Some(TrayAsk::Search),
                        ..
                    }
                )
            })
            .expect("a Search row")
    };
    let item = |label: &str, shortcut: Option<&[&str]>| MenuRow::Item {
        label: label.to_owned(),
        enabled: true,
        ask: Some(TrayAsk::Search),
        shortcut: shortcut.map(|keys| keys.iter().map(|key| (*key).to_owned()).collect()),
    };
    // Not answered yet: say nothing, which is not the same as *none*.
    assert_eq!(row(Shortcut::Unknown), item("Search", None));
    assert_eq!(
        row(Shortcut::Chord("Ctrl+Shift+K".to_owned())),
        item("Search", Some(&["Control", "Shift", "K"]))
    );
    assert_eq!(
        row(Shortcut::Absent),
        item("Search (no global shortcut)", None)
    );
    // A chord the property cannot carry is still a working shortcut, so the
    // text names it — `imp`'s rule for an unparseable accelerator.
    assert_eq!(
        row(Shortcut::Chord("Hyper+K".to_owned())),
        item("Search (Hyper+K)", None)
    );
}

/// **The seam where the two used to drift**, Linux's half: every chord the
/// hotkey module can register is one the DBusMenu property can carry.
#[test]
fn every_shortcut_the_hotkey_can_register_is_one_dbusmenu_can_carry() {
    for action in crate::hotkey::ACTIONS {
        for label in crate::hotkey::candidates(*action) {
            assert!(dbus_shortcut(label).is_some(), "{label}");
        }
    }
    assert_eq!(dbus_shortcut("K"), None, "a key alone is not a chord");
    assert_eq!(dbus_shortcut("Ctrl+"), None);
}

#[test]
fn the_ai_chat_row_says_whether_its_shortcut_is_held() {
    let row = |available: Option<bool>| {
        let mut state = full();
        state.ai_shortcut = available;
        linux_menu(&state).remove(0)
    };
    assert!(matches!(
        row(None),
        MenuRow::Item { label, shortcut: None, ask: Some(TrayAsk::AiChat), .. } if label == "Koru AI Chat"
    ));
    assert!(matches!(
        row(Some(true)),
        MenuRow::Item { label, shortcut: Some(_), .. } if label == "Koru AI Chat"
    ));
    assert!(matches!(
        row(Some(false)),
        MenuRow::Item { label, shortcut: None, .. } if label == "Koru AI Chat (no global shortcut)"
    ));
}

/// What a switched-off installation reads — the state Linux is in until its
/// voice rung (`koru_voice::Off::Platform`).
fn voice_state_switched_off(off: koru_voice::Off) -> VoiceMenu {
    VoiceMenu {
        row: VoiceRow::Unavailable(off),
        installed: Vec::new(),
        worker: None,
        devices: Vec::new(),
        input: koru_voice::InputChoice::SystemDefault,
    }
}

/// **Linux's voice submenu is the helpers' switched-off answer**, word for
/// word: one line saying so and nothing to press. No new sentence is
/// invented for the platform.
#[test]
fn a_switched_off_voice_submenu_says_so_and_offers_nothing() {
    let mut state = full();
    state.voice = voice_state_switched_off(koru_voice::Off::Switch);
    let rows = linux_menu(&state);
    assert_eq!(
        submenu(&rows, "Voice input"),
        [MenuRow::reading(
            "Voice input is switched off on this installation"
        )]
    );
    // A platform without voice says that instead — Settings' sentence, in
    // the menu's language — rather than blaming a switch nobody set.
    state.voice = voice_state_switched_off(koru_voice::Off::Platform);
    assert_eq!(
        submenu(&linux_menu(&state), "Voice input"),
        [MenuRow::reading(koru_i18n::t(
            state.lang,
            koru_i18n::Key::VoiceNotOnLinuxYet
        ))]
    );
}

/// On Linux the reading itself is switched off, whatever the disk holds —
/// `voice_state` asks `koru_voice::available()` before it walks anything.
#[cfg(target_os = "linux")]
#[test]
fn linux_reads_voice_as_switched_off() {
    let state = voice_state(None, koru_voice::InputChoice::SystemDefault);
    assert_eq!(state, voice_state_switched_off(koru_voice::Off::Platform));
}

/// With voice on, each voice line raises the ask `imp`'s handler maps it to,
/// and the microphone picker is a submenu of ticks.
#[test]
fn the_voice_rows_raise_the_same_asks_as_imp() {
    let rows = linux_menu(&full());
    let voice = submenu(&rows, "Voice input");
    let voice_asks: Vec<TrayAsk> = asks(voice)
        .into_iter()
        .map(|(ask, _)| ask)
        .filter(|ask| !matches!(ask, TrayAsk::ChooseVoiceInput(_)))
        .collect();
    assert_eq!(
        voice_asks,
        [
            TrayAsk::Voice,
            TrayAsk::VoiceSettings,
            TrayAsk::VoiceRelease,
            TrayAsk::VoiceStop
        ]
    );
    let inputs = submenu(voice, "Input: System default");
    assert_eq!(
        inputs,
        [
            MenuRow::Check {
                label: "System default".to_owned(),
                enabled: true,
                checked: true,
                ask: TrayAsk::ChooseVoiceInput(koru_voice::InputChoice::SystemDefault),
            },
            MenuRow::Check {
                label: "USB_Headset".to_owned(),
                enabled: true,
                checked: false,
                ask: TrayAsk::ChooseVoiceInput(koru_voice::InputChoice::named("USB_Headset")),
            },
        ]
    );
}

/// Greyed rather than left out when there is nothing to do — the About row's
/// rule, carried through to the Linux rows.
#[test]
fn the_memory_actions_are_greyed_with_no_worker() {
    let mut state = full();
    state.voice.worker = None;
    let rows = linux_menu(&state);
    let voice = asks(submenu(&rows, "Voice input"));
    assert!(voice.contains(&(TrayAsk::VoiceRelease, false)));
    assert!(voice.contains(&(TrayAsk::VoiceStop, false)));
    assert!(voice.contains(&(TrayAsk::Voice, true)));
}

#[test]
fn the_store_status_row_follows_supervision_and_asks_for_a_refresh() {
    for (running, label) in [
        (None, "Store running: Checking…"),
        (Some(true), "Store running: Yes"),
        (Some(false), "Store running: No"),
    ] {
        let mut state = full();
        state.store_running = running;
        let rows = linux_menu(&state);
        assert_eq!(
            submenu(&rows, "Store")[0],
            MenuRow::action(label, TrayAsk::RefreshStore)
        );
    }
}

#[test]
fn open_workspaces_follow_the_open_row() {
    let rows = linux_menu(&full());
    assert_eq!(
        submenu(&rows, "Workspaces"),
        [
            MenuRow::action("Open Workspace", TrayAsk::Restore),
            MenuRow::Separator,
            MenuRow::action("Notes — Personal", TrayAsk::OpenWorkspace(ConnectionId(7))),
        ]
    );
    let mut none = full();
    none.workspaces.clear();
    let rows = linux_menu(&none);
    assert_eq!(
        submenu(&rows, "Workspaces"),
        [MenuRow::action("Open Workspace", TrayAsk::Restore)]
    );
}

/// New is pinned at the top; a project heading is a reading, a session a
/// pressable row — [`agents_menu_lines`]' projection, drawn.
#[test]
fn the_agents_submenu_draws_the_grouped_lines() {
    let state = full();
    let session = match &state.agents[2] {
        AgentsLine::Session { session, .. } => *session,
        other => panic!("{other:?}"),
    };
    let rows = linux_menu(&state);
    let agents = submenu(
        &rows,
        koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::TerminalAgents),
    );
    assert_eq!(
        agents,
        [
            MenuRow::action(
                koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::TerminalNew),
                TrayAsk::NewAgent
            ),
            MenuRow::Separator,
            MenuRow::reading("Koru"),
            MenuRow::action("Claude Code", TrayAsk::ShowAgentSession(session)),
        ]
    );
}

#[test]
fn the_relay_switch_follows_the_network_state() {
    for state in [Some(true), Some(false), None] {
        let mut menu = full();
        menu.relay = state;
        let drawn = relay_row(state);
        let rows = linux_menu(&menu);
        assert_eq!(
            find(
                &rows,
                koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::RelayUse)
            ),
            Some(&MenuRow::Check {
                label: koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::RelayUse).to_owned(),
                enabled: drawn.enabled,
                checked: drawn.checked,
                ask: TrayAsk::ToggleRelay,
            })
        );
    }
}

/// An unpublished build's About row is readable and cannot be pressed.
#[test]
fn the_about_row_is_greyed_without_a_page() {
    let mut state = full();
    state.about_enabled = false;
    let rows = linux_menu(&state);
    assert_eq!(
        submenu(&rows, "About Koru")[0],
        MenuRow::Item {
            label: "Koru 0.3.1 · build 42".to_owned(),
            enabled: false,
            ask: Some(TrayAsk::About),
            shortcut: None,
        }
    );
}

/// `my_notes` must not draw as `mynotes` with an access key on `n`.
#[cfg(target_os = "linux")]
#[test]
fn an_underscore_in_a_label_survives_dbusmenu() {
    assert_eq!(linux::escape("my_notes"), "my__notes");
    assert_eq!(linux::escape("Search"), "Search");
}
