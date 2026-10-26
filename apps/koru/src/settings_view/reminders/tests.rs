// SPDX-License-Identifier: MPL-2.0

use super::*;
use crate::reminder_settings::Effect;
use koru_agent_wire::{NotificationControlResult, ReminderDiagnostics};
use koru_store_core::ids::ProfileId;
use koru_store_core::reminders::{ReminderSettings, ToneId};

const COLUMN: Rect = Rect {
    x: 100.0,
    y: 40.0,
    w: 520.0,
    h: 900.0,
};

fn ready(tone: ToneId) -> ReminderPreferences {
    let profile = ProfileId::new();
    let mut model = ReminderPreferences::default();
    let effects = model.select_profile(profile);
    let generation = match effects[0] {
        Effect::LoadSettings { generation, .. } => generation,
        _ => unreachable!(),
    };
    model.settings_loaded(
        profile,
        generation,
        Ok(ReminderSettings {
            end_of_day: None,
            default_tone: tone,
            week_start: WeekStartPreference::System,
        }),
    );
    model
}

fn surface(model: &ReminderPreferences) -> (ReminderSurface, TextEngine) {
    let mut engine = TextEngine::new();
    let mut surface = ReminderSurface::new(&mut engine);
    surface.set_model(&mut engine, model);
    surface.layout(&mut engine, COLUMN, COLUMN.y, koru_i18n::Lang::En);
    (surface, engine)
}

fn center(rect: Rect) -> (f32, f32) {
    (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5)
}

/// The tones are one dropdown, not eleven chips: the field opens the shared
/// list, a row chooses, and the list opens on the tone in force.
#[test]
fn every_tone_is_chosen_from_the_shared_dropdown() {
    let model = ready(ToneId::Calm);
    let (mut surface, mut engine) = surface(&model);
    let field = surface.rect(Control::ToneMenu).expect("the tone field");
    for (index, tone) in TONES.into_iter().enumerate() {
        let (x, y) = center(field);
        assert!(surface.press(&mut engine, x, y), "the field opens its list");
        let menu = surface.menu.as_ref().expect("the list is open");
        assert_eq!(menu.items().len(), TONES.len());
        assert_eq!(
            menu.hover,
            TONES.iter().position(|t| *t == ToneId::Calm),
            "it opens on the tone in force"
        );
        let (x, y) = center(menu.item_rect(surface.view, index));
        assert!(surface.press(&mut engine, x, y));
        assert!(surface.menu.is_none(), "choosing closes the list");
        assert_eq!(surface.take_intent(), Some(Intent::SetDefaultTone(tone)));
    }
}

#[test]
fn the_tone_list_answers_the_keyboard_and_escape_chooses_nothing() {
    let model = ready(ToneId::System);
    let (mut surface, mut engine) = surface(&model);
    surface.focus = Some(Control::ToneMenu);
    let enter = Key::Named(NamedKey::Enter);
    assert!(surface.key(&mut engine, &enter, None, ModifiersState::default()));
    assert!(surface.menu.is_some(), "Enter on the field opens it");
    let down = Key::Named(NamedKey::ArrowDown);
    assert!(surface.key(&mut engine, &down, None, ModifiersState::default()));
    assert!(surface.key(&mut engine, &enter, None, ModifiersState::default()));
    assert_eq!(
        surface.take_intent(),
        Some(Intent::SetDefaultTone(TONES[1]))
    );
    assert!(surface.key(&mut engine, &enter, None, ModifiersState::default()));
    let escape = Key::Named(NamedKey::Escape);
    assert!(surface.key(&mut engine, &escape, None, ModifiersState::default()));
    assert!(surface.menu.is_none());
    assert_eq!(surface.take_intent(), None);
}

#[test]
fn every_week_start_choice_uses_shared_draw_and_hit_geometry() {
    let model = ready(ToneId::System);
    let (mut surface, mut engine) = surface(&model);
    let choices: Vec<_> = surface
        .targets
        .iter()
        .filter_map(|target| match target.control {
            Control::WeekStart(value) => Some((value, target.rect)),
            _ => None,
        })
        .collect();
    assert_eq!(
        choices.iter().map(|(value, _)| *value).collect::<Vec<_>>(),
        vec![
            WeekStartPreference::System,
            WeekStartPreference::Monday,
            WeekStartPreference::Sunday,
        ]
    );
    for (value, rect) in choices {
        let (x, y) = center(rect);
        assert!(surface.press(&mut engine, x, y));
        assert_eq!(surface.take_intent(), Some(Intent::SetWeekStart(value)));
    }
}

#[test]
fn end_of_day_accepts_every_minute_and_empty_is_explicitly_unset() {
    let model = ready(ToneId::System);
    let (mut surface, mut engine) = surface(&model);
    surface.focus = Some(Control::EndOfDay);
    surface.time.set_text(&mut engine, "17:31");
    assert!(surface.key(
        &mut engine,
        &Key::Named(NamedKey::Enter),
        None,
        ModifiersState::default(),
    ));
    assert_eq!(
        surface.take_intent(),
        Some(Intent::SetEndOfDay(LocalTime::new(17, 31, 0)))
    );

    surface.time.set_text(&mut engine, "");
    surface.commit_time();
    assert_eq!(surface.take_intent(), Some(Intent::SetEndOfDay(None)));
}

#[test]
fn loading_and_failed_profile_controls_have_no_clickable_no_ops_and_failure_retries() {
    let mut model = ready(ToneId::System);
    let (mut surface, mut engine) = surface(&model);
    model.settings = SettingsState::Loading;
    surface.set_model(&mut engine, &model);
    surface.layout(&mut engine, COLUMN, COLUMN.y, koru_i18n::Lang::En);
    let (x, y) = center(surface.rect(Control::EndOfDay).unwrap());
    assert!(!surface.press(&mut engine, x, y));
    assert_eq!(surface.take_intent(), None);

    model.settings = SettingsState::Failed;
    surface.set_model(&mut engine, &model);
    surface.layout(&mut engine, COLUMN, COLUMN.y, koru_i18n::Lang::En);
    let (x, y) = center(surface.rect(Control::ToneMenu).unwrap());
    assert!(!surface.press(&mut engine, x, y));
    assert!(surface.menu.is_none(), "a disabled field opens nothing");
    let (x, y) = center(surface.rect(Control::RetrySettings).unwrap());
    assert!(surface.press(&mut engine, x, y));
    assert_eq!(surface.take_intent(), Some(Intent::RetrySettings));
}

#[test]
fn profile_change_resets_focused_time_and_purges_profile_owned_intents() {
    let mut model = ready(ToneId::System);
    let (mut surface, mut engine) = surface(&model);
    surface.focus = Some(Control::EndOfDay);
    surface.hot = Some(Control::ToneMenu);
    surface.time.set_text(&mut engine, "17:31");
    surface.time_invalid = true;
    surface.intents.push_back(Intent::SetEndOfDay(None));
    surface
        .intents
        .push_back(Intent::SetDefaultTone(ToneId::Calm));
    surface.intents.push_back(Intent::SendTest);

    let profile_b = ProfileId::new();
    let generation_b = match model.select_profile(profile_b)[0] {
        Effect::LoadSettings { generation, .. } => generation,
        _ => unreachable!(),
    };
    surface.set_model(&mut engine, &model);
    assert_eq!(surface.time.raw_text(), "");
    assert!(!surface.time_invalid);
    assert_eq!(surface.focus, None);
    assert_eq!(surface.hot, None);
    assert!(surface.targets.is_empty());
    assert_eq!(surface.take_intent(), Some(Intent::SendTest));
    assert_eq!(surface.take_intent(), None);

    assert!(model.settings_loaded(
        profile_b,
        generation_b,
        Ok(ReminderSettings {
            end_of_day: LocalTime::new(9, 5, 0),
            default_tone: ToneId::Bright,
            week_start: WeekStartPreference::System,
        })
    ));
    surface.set_model(&mut engine, &model);
    surface.layout(&mut engine, COLUMN, COLUMN.y, koru_i18n::Lang::En);
    assert_eq!(surface.time.raw_text(), "09:05");
}

#[test]
fn unavailable_agent_never_changes_the_last_device_choice() {
    let mut model = ready(ToneId::System);
    model.health = HealthState::Ready(ReminderDiagnostics {
        enabled: false,
        ..ReminderDiagnostics::default()
    });
    let (mut surface, mut engine) = surface(&model);
    assert_eq!(surface.last_enabled, Some(false));

    model.health = HealthState::Unavailable;
    surface.set_model(&mut engine, &model);
    surface.layout(&mut engine, COLUMN, COLUMN.y, koru_i18n::Lang::En);
    assert_eq!(surface.last_enabled, Some(false));
    let rect = surface.rect(Control::Delivery).unwrap();
    let (x, y) = center(rect);
    surface.press(&mut engine, x, y);
    assert_eq!(
        surface.take_intent(),
        Some(Intent::SetDeliveryEnabled(true))
    );
    assert_eq!(
        surface.last_enabled,
        Some(false),
        "intent is not a fake success"
    );
}

#[test]
fn rapid_device_toggle_inverts_the_latest_pending_choice() {
    let mut model = ready(ToneId::System);
    model.health = HealthState::Ready(ReminderDiagnostics {
        enabled: false,
        ..ReminderDiagnostics::default()
    });
    let (mut surface, mut engine) = surface(&model);
    let (x, y) = center(surface.rect(Control::Delivery).unwrap());
    assert!(surface.press(&mut engine, x, y));
    assert!(surface.press(&mut engine, x, y));
    assert_eq!(
        surface.take_intent(),
        Some(Intent::SetDeliveryEnabled(true))
    );
    assert_eq!(
        surface.take_intent(),
        Some(Intent::SetDeliveryEnabled(false))
    );
    assert_eq!(surface.last_enabled, Some(false));
}

#[test]
fn authorization_prompt_has_one_explicit_control() {
    let mut model = ready(ToneId::System);
    let effects = model.select_profile(ProfileId::new());
    let inspect = effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Notification { id, .. } => Some(*id),
            _ => None,
        })
        .unwrap();
    model.notification_result(
        inspect,
        NotificationControlResult::Capability {
            authorization: NotificationAuthorization::NotDetermined,
            actions: true,
            stable_replacement: true,
            withdrawal: true,
            custom_sound: true,
            activation_while_stopped: true,
        },
        koru_i18n::Lang::En,
    );
    let (mut surface, mut engine) = surface(&model);
    assert!(surface.rect(Control::AllowNotifications).is_some());

    for (control, expected) in [
        (
            Control::RefreshNotificationStatus,
            Intent::RefreshNotificationStatus,
        ),
        (Control::SendTest, Intent::SendTest),
        (Control::AllowNotifications, Intent::AllowNotifications),
    ] {
        let (x, y) = center(surface.rect(control).unwrap());
        surface.press(&mut engine, x, y);
        assert_eq!(surface.take_intent(), Some(expected));
    }
}

#[test]
fn actionable_health_failures_offer_the_direct_fix() {
    let mut model = ready(ToneId::System);
    model.health = HealthState::Ready(ReminderDiagnostics {
        enabled: true,
        failure_category: Some("ledger".to_owned()),
        ..ReminderDiagnostics::default()
    });
    let (mut ledger_surface, mut engine) = surface(&model);
    ledger_surface.state.warnings.clear();
    ledger_surface.layout(&mut engine, COLUMN, COLUMN.y, koru_i18n::Lang::En);
    assert!(ledger_surface.rect(Control::RepairDelivery).is_some());

    model.health = HealthState::Ready(ReminderDiagnostics {
        enabled: true,
        failure_category: Some("native_permission".to_owned()),
        ..ReminderDiagnostics::default()
    });
    let (mut permission_surface, mut engine) = surface(&model);
    permission_surface.state.warnings.clear();
    permission_surface.layout(&mut engine, COLUMN, COLUMN.y, koru_i18n::Lang::En);
    let direct = permission_surface
        .rect(Control::BannerOpenNotificationSettings)
        .unwrap();
    let ordinary = permission_surface
        .rect(Control::OpenNotificationSettings)
        .unwrap();
    assert_ne!(
        direct, ordinary,
        "the banner action owns its drawn hit target"
    );
}

#[test]
fn tab_and_shift_tab_walk_the_same_enabled_target_list() {
    let model = ready(ToneId::System);
    let (mut surface, mut engine) = surface(&model);
    let order: Vec<_> = surface
        .targets
        .iter()
        .map(|target| target.control)
        .collect();
    for expected in &order {
        assert!(surface.key(
            &mut engine,
            &Key::Named(NamedKey::Tab),
            None,
            ModifiersState::default(),
        ));
        assert_eq!(surface.focus, Some(*expected));
    }
    // Forward wraps to the first; reverse from there wraps to the last.
    surface.key(
        &mut engine,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::default(),
    );
    assert_eq!(surface.focus, Some(order[0]));
    assert!(surface.key(
        &mut engine,
        &Key::Named(NamedKey::Tab),
        None,
        ModifiersState::SHIFT,
    ));
    assert_eq!(
        surface.focus,
        Some(surface.targets[surface.targets.len() - 1].control)
    );
}

#[test]
fn startup_consent_has_two_explicit_answers() {
    let mut model = ready(ToneId::System);
    model.observe_reminders(true, crate::reminder_settings::StartupFacts::default());
    let (mut surface, mut engine) = surface(&model);
    for (control, answer) in [
        (Control::StartupEnable, true),
        (Control::StartupDecline, false),
    ] {
        let (x, y) = center(surface.rect(control).unwrap());
        surface.press(&mut engine, x, y);
        assert_eq!(surface.take_intent(), Some(Intent::AnswerStartup(answer)));
    }
}

#[test]
fn translated_controls_stay_inside_the_column_without_overlapping() {
    let mut model = ready(ToneId::Urgent);
    model.health = HealthState::Ready(ReminderDiagnostics {
        enabled: true,
        failure_category: Some("ledger".to_owned()),
        ..ReminderDiagnostics::default()
    });
    for lang in koru_i18n::Lang::ALL {
        let (mut surface, mut engine) = surface(&model);
        surface.state.warnings.clear();
        surface.layout(&mut engine, COLUMN, COLUMN.y, lang);
        for (i, target) in surface.targets.iter().enumerate() {
            assert!(target.rect.x >= COLUMN.x);
            assert!(target.rect.x + target.rect.w <= COLUMN.x + COLUMN.w + 0.01);
            // Segments of one control touch by design, and their summed
            // positions differ from the next segment's by rounding, so an
            // overlap has to be more than the tolerance the column uses.
            for other in &surface.targets[i + 1..] {
                let overlaps = target.rect.x + 0.01 < other.rect.x + other.rect.w
                    && target.rect.x + target.rect.w > other.rect.x + 0.01
                    && target.rect.y + 0.01 < other.rect.y + other.rect.h
                    && target.rect.y + target.rect.h > other.rect.y + 0.01;
                assert!(
                    !overlaps,
                    "{lang:?}: {:?} overlaps {:?}",
                    target.control, other.control
                );
            }
        }
    }
}

/// *System default* says which day it means on this machine.
#[test]
fn system_week_start_names_the_day_it_resolves_to() {
    let model = ready(ToneId::System);
    let (mut surface, _) = surface(&model);
    let lang = koru_i18n::Lang::En;
    assert_eq!(
        surface.week_label(lang, WeekStartPreference::System),
        "System default",
        "unknown stays plain"
    );
    surface.set_system_week_start(Some(
        koru_workspace_host::environment::FirstDayOfWeek::Monday,
    ));
    assert_eq!(
        surface.week_label(lang, WeekStartPreference::System),
        "System default (Monday)"
    );
    assert_eq!(
        surface.week_label(lang, WeekStartPreference::Sunday),
        "Sunday"
    );
}

/// The play button previews the tone in force and changes nothing.
#[test]
fn the_play_button_previews_the_chosen_tone() {
    let model = ready(ToneId::Chime);
    let (mut surface, mut engine) = surface(&model);
    if !cfg!(windows) {
        assert!(surface.rect(Control::PreviewTone).is_none());
        assert!(surface.rect(Control::ToneMenu).is_some());
        return;
    }
    let play = surface.rect(Control::PreviewTone).expect("the play button");
    let menu = surface.rect(Control::ToneMenu).expect("the tone field");
    assert!(
        play.x + play.w <= menu.x,
        "it sits before the field, in its row"
    );
    let (x, y) = center(play);
    assert!(surface.press(&mut engine, x, y));
    assert_eq!(
        surface.take_intent(),
        Some(Intent::PreviewTone(ToneId::Chime))
    );
    assert!(surface.menu.is_none());
}

/// The page follows the design's grouping, and every feature it does not
/// picture keeps a section of its own: Schedule; Notifications on this device
/// (delivery, sound, test); Notification permission; Reminder health.
#[test]
fn the_page_groups_schedule_notifications_permission_and_health() {
    let (surface, _engine) = surface(&ready(ToneId::default()));
    let titles: Vec<&str> = surface
        .cards
        .iter()
        .filter_map(|card| card.title.as_deref())
        .collect();
    let schedule = titles
        .iter()
        .position(|t| *t == "Schedule")
        .expect("Schedule");
    let notifications = titles
        .iter()
        .position(|t| *t == "Notifications on this device")
        .expect("Notifications on this device");
    let permission = titles
        .iter()
        .position(|t| *t == "Notification permission")
        .expect("Notification permission");
    let health = titles
        .iter()
        .position(|t| *t == "Reminder health")
        .expect("Reminder health");
    assert!(schedule < notifications && notifications < permission && permission < health);

    let labels = |title: &str| -> Vec<String> {
        surface
            .cards
            .iter()
            .find(|card| card.title.as_deref() == Some(title))
            .map(|card| card.rows.iter().map(|row| row.label.clone()).collect())
            .unwrap_or_default()
    };
    assert_eq!(labels("Schedule"), ["End of day", "Week starts on"]);
    assert_eq!(
        labels("Notifications on this device"),
        ["Deliver reminders here", "Sound", "Test"]
    );
    assert_eq!(labels("Notification permission"), ["Notification status"]);
}
