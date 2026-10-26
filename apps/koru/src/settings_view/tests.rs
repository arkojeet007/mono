// SPDX-License-Identifier: MPL-2.0

//! **The seam, and the behavioural gate.**
//!
//! The rows themselves are proved in `koru-settings-ui`'s own suite. What is
//! proved here is that Koru's Settings tab still *is* those rows: that the four
//! values the container owns a copy of round-trip through `Settings` exactly as
//! they did, that every host reading reaches the surface, and that the effects
//! that were never a view's still are not.

use super::*;
use crate::mainview::tab::{Tab, TabView};
use koru_draw::theme::ViewMode;
use koru_settings_ui::Availability;

/// The process globals this view writes are shared by the whole binary.
static GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn settings_view() -> SettingsView {
    let mut view = SettingsView::new();
    View::set_viewport(&mut view, 1000.0, 700.0);
    view.surface_mut().relayout();
    view
}

/// **The tab is its own kind now.** Until SP-7 `Tab::settings()` was a
/// `WelcomeView` with a flag set, and Welcome and Settings shared one set of
/// rects — so the only thing keeping blank space on Welcome from mutating a
/// hidden preference was an `if` in every hit test.
#[test]
fn the_settings_tab_is_not_a_welcome_tab() {
    let tab = Tab::settings();
    assert_eq!(tab.label, "Settings");
    assert!(matches!(tab.view, TabView::Settings(_)));
    assert!(matches!(Tab::welcome().view, TabView::Welcome(_)));
}

/// Koru's reminder rows are a host-owned section of the dedicated Settings
/// surface. They reserve real content height in that surface instead of
/// reviving Welcome's separate scroll model or drawing over shared rows.
#[test]
fn reminders_reserve_the_shared_settings_host_slot() {
    let mut view = settings_view();
    view.surface.show_page(koru_settings_ui::PageId::Reminders);
    view.surface.relayout();
    let without_reminders = view.surface.content_bottom();

    view.layout_reminders();
    view.surface.relayout();

    let slot = view
        .surface
        .host_section_rect()
        .expect("Koru Settings has a reminder host slot");
    assert!(slot.h > 0.0);
    assert!(view.surface.content_bottom() > without_reminders);
}

#[test]
fn end_of_day_navigation_focuses_the_field_and_reveals_its_host_rect() {
    let mut view = settings_view();
    View::set_viewport(&mut view, 1000.0, 260.0);
    let profile = koru_store_core::ids::ProfileId::new();
    let mut model = crate::reminder_settings::ReminderPreferences::default();
    let generation = match model.select_profile(profile)[0] {
        crate::reminder_settings::Effect::LoadSettings { generation, .. } => generation,
        _ => unreachable!(),
    };
    model.settings_loaded(
        profile,
        generation,
        Ok(koru_store_core::reminders::ReminderSettings::default()),
    );
    view.set_reminder_preferences(&model);

    view.surface.show_page(koru_settings_ui::PageId::Appearance);
    assert!(view.reveal_reminder_end_of_day());
    assert!(view.reminders.end_of_day_focused());
    assert!(view.end_of_day_on_screen(), "on its page and in view");
}

/// **The four values the container owns a copy of.** Everything else this page
/// draws reads a process global, so a change made in another window repaints it
/// for free. These four do not — and before they were adopted, the next
/// `capture` wrote the stale copy straight back over the authoritative change,
/// which in two windows is a loop neither one wins.
#[test]
fn a_change_made_in_another_window_reaches_the_fields_this_one_holds() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut view = settings_view();
    let settings = Settings {
        view_mode: ViewMode::Page,
        pinned_show_title: true,
        canvas_bg: Some([0x11, 0x22, 0x33, 0xFF]),
        close_behavior: koru_window::CloseBehavior::Quit,
        ..Settings::default()
    };
    View::adopt_preferences(&mut view, &settings);
    assert_eq!(view.view_mode(), ViewMode::Page);
    assert!(view.pinned_show_title());
    assert_eq!(view.canvas_background(), Some([0x11, 0x22, 0x33, 0xFF]));
    assert_eq!(view.close_behavior(), koru_window::CloseBehavior::Quit);

    // ...and the capture that follows reports what it was told, rather than
    // undoing it.
    let mut back = Settings::default();
    View::capture(&view, &mut back);
    assert_eq!(back.view_mode, settings.view_mode);
    assert_eq!(back.pinned_show_title, settings.pinned_show_title);
    assert_eq!(back.canvas_bg, settings.canvas_bg);
}

/// The caret is a live global and `capture` reports it, which is the path the
/// shell's per-field patch reads. Unchanged from `WelcomeView`.
#[test]
fn the_caret_globals_round_trip_through_capture() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = (
        theme::caret_color(),
        theme::caret_width(),
        theme::caret_height(),
    );
    let view = settings_view();
    theme::set_caret_color(theme::CaretColor::Amber);
    theme::set_caret_width(theme::CaretWidth::Thick);
    theme::set_caret_height(theme::CaretHeight::Tall);
    let mut back = Settings::default();
    View::capture(&view, &mut back);
    assert_eq!(back.caret_color, theme::CaretColor::Amber);
    assert_eq!(back.caret_width, theme::CaretWidth::Thick);
    assert_eq!(back.caret_height, theme::CaretHeight::Tall);
    theme::set_caret_color(was.0);
    theme::set_caret_width(was.1);
    theme::set_caret_height(was.2);
}

/// **`restore` puts the caret back**, exactly as the welcome page's did — the
/// shell's `Settings` is the source and the globals are what everything draws
/// from.
#[test]
fn restore_applies_the_stored_caret_to_the_live_globals() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = theme::caret_color();
    let mut view = settings_view();
    let workspace = koru_workspace::Workspace::new(
        koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
            .expect("create"),
    );
    View::restore(
        &mut view,
        &workspace,
        &Settings {
            caret_color: theme::CaretColor::Violet,
            ..Settings::default()
        },
    );
    assert_eq!(theme::caret_color(), theme::CaretColor::Violet);
    theme::set_caret_color(was);
}

// ------------------------------------------------------- the host's readings

/// **The renderer row is the host's, in the host's own tokens.** The surface
/// never learns what a `RendererBackend` is, because `renderer.conf` is exempt
/// from the settings authority (ADR-266) and `koru-window` is its only writer.
#[test]
fn the_renderer_row_carries_this_process_own_tokens() {
    let row = renderer_row();
    if cfg!(target_os = "macos") {
        assert!(
            row.options.is_empty(),
            "macOS has one backend, so the row would control nothing"
        );
        return;
    }
    let tokens: Vec<&str> = row
        .options
        .iter()
        .map(|(token, _)| token.as_str())
        .collect();
    for backend in koru_window::RendererBackend::ALL {
        assert!(tokens.contains(&backend.key()), "{:?}", backend);
    }
    assert!(
        tokens.contains(&row.current.as_str()),
        "the current token is one of the offered ones"
    );
    assert_eq!(
        row.note,
        "Only change if you see drawing glitches. Applies next time Koru starts."
    );
}

/// **A token nothing offers writes nothing.** The passthrough is a string over
/// a seam, so it has to refuse what it does not recognise rather than guessing.
///
/// Deliberately no positive case: choosing a backend writes this machine's own
/// `renderer.conf`, and a unit test must not leave one on whoever ran the suite.
#[test]
fn an_unknown_renderer_token_writes_nothing() {
    set_renderer_backend("not-a-backend");
}

/// The AI section is a slot: Settings names it and offers the way in, and the
/// provider rows stay in Koru AI where they already are.
#[test]
fn the_ai_slot_offers_the_way_in_rather_than_the_rows() {
    let slot = ai_slot();
    assert!(!slot.note.is_empty());
    assert!(slot.action.is_some());
}

/// Every language the picker offers is one the registry will accept, and
/// *detect it* comes first — a person who has expressed no preference is not
/// saying English.
#[test]
fn the_language_options_open_with_detect_automatically() {
    let options = language_options();
    assert_eq!(options[0].0.token(), DictationLanguage::AUTO);
    assert_eq!(options[0].1, "Detect automatically");
    for (language, label) in &options {
        assert!(language.validate().is_ok(), "{label}");
    }
}

/// *System default* is a row somebody can choose, distinct from never having
/// chosen.
#[test]
fn the_input_options_open_with_the_system_default() {
    let options = input_options();
    assert_eq!(options[0].0.token(), DictationInput::SYSTEM_DEFAULT);
    assert_eq!(options[0].1, "System default");
}

/// **The quality note names the tier and says what choosing it gets you.**
/// Nothing here claims either tier is more accurate — that has not been
/// measured — and the size is the descriptor's or absent, never invented.
#[test]
fn each_quality_note_says_when_the_words_appear() {
    assert!(quality_note(ModelTier::Compact).contains("when you finish speaking"));
    assert!(quality_note(ModelTier::HighAccuracy).contains("as you speak"));
    assert!(quality_note(ModelTier::Compact).starts_with("Compact"));
}

/// The two vocabularies round-trip: a tier out of the registry and back is the
/// same tier, which is what stops a press writing something else.
#[test]
fn the_dictation_vocabularies_round_trip() {
    for tier in ModelTier::ALL {
        assert_eq!(tier_from(tier_of(tier)), tier);
    }
    for choice in [
        koru_window::CloseBehavior::Ask,
        koru_window::CloseBehavior::Background,
        koru_window::CloseBehavior::Quit,
    ] {
        assert_eq!(close_behavior_of(close_behavior_to(choice)), choice);
    }
}

// --------------------------------------------------------------- the states

/// **The worker would not start**, recorded rather than asked — asking means
/// starting one. A refusal that is not one of the two clears the flag, so a
/// machine that recovered does not keep an obsolete sentence.
#[test]
fn only_a_refusal_that_means_there_is_no_worker_switches_the_section_off() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut view = settings_view();
    assert!(view.note_refusal(koru_dictation::ErrorCode::WorkerUnavailable));
    assert_eq!(view.off, Some(VoiceOff::WorkerUnavailable));
    assert!(view.note_refusal(koru_dictation::ErrorCode::Busy));
    assert_eq!(view.off, None, "a busy worker is still a worker");
}

/// **A platform with no voice runtime says so in the person's language**, and
/// blames no switch: nothing a person sets turns dictation on there, so the
/// line must not point at `KORU_VOICE_DISABLED`.
#[test]
fn the_platform_line_is_localized_and_names_no_switch() {
    let line = VoiceOff::Platform.line();
    assert_eq!(
        line,
        koru_i18n::t(crate::format::lang(), koru_i18n::Key::VoiceNotOnLinuxYet)
    );
    assert!(!line.contains("KORU_VOICE_DISABLED"), "{line}");
}

/// The read-only state reaches the hosted surface, and a press changes nothing.
#[test]
fn a_gateway_that_is_not_answering_makes_the_hosted_rows_read_only() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut view = settings_view();
    view.surface_mut().set_availability(Availability::Down);
    view.surface_mut().relayout();
    let was = theme::density();
    let mut doc = koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t")
        .expect("create");
    // A press somewhere inside the column: whatever it lands on, nothing moves.
    for y in (80..600).step_by(20) {
        View::mouse_down(&mut view, &mut doc, 500.0, y as f32, false);
    }
    assert_eq!(theme::density(), was);
    assert!(view.take_requests().is_empty());
}

/// **A Settings tab never dials a Gateway under test.** A dependency is never
/// compiled with `cfg(test)`, so the guard has to be here rather than inside
/// `koru-settings-ui`.
#[test]
fn a_test_build_holds_no_live_link() {
    let view = settings_view();
    assert!(!view.surface.links().holds_global());
    assert!(view.surface.links().holds_terminal());
}

/// **The simplified product's Settings** (koru-features all off): no
/// Reminders page and no Koru AI providers card; System stays.
#[test]
fn hidden_surfaces_have_no_settings() {
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let sections = super::sections();
        assert!(!sections.ai && !sections.reminders);
        let pages = sections.pages();
        assert!(!pages.contains(&koru_settings_ui::PageId::Reminders));
        assert!(pages.contains(&koru_settings_ui::PageId::System));
    });
    assert_eq!(super::sections(), koru_settings_ui::Sections::koru());
}
