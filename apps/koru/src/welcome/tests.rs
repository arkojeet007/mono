// SPDX-License-Identifier: MPL-2.0

//! The Welcome page: who it greets, what each row does, what is counted, and
//! that what is drawn is exactly what can be pressed.

use super::layout::Layout;
use super::rows::Section;
use super::*;
use koru_core::WorkspaceId;
use koru_draw::inspect;

/// Serializes the process-global Guide hint switch across these tests.
static GUIDE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn guide() -> std::sync::MutexGuard<'static, ()> {
    GUIDE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn doc() -> WorkspaceDoc {
    WorkspaceDoc::create(WorkspaceId::new(), "t").expect("create")
}

/// A page tall enough that nothing scrolls, so a rect's centre is a click.
fn page() -> WelcomeView {
    let mut view = WelcomeView::first_minutes("Ana Lima", "Team Roadmap");
    view.set_identity(WelcomeIdentity {
        owner: Some("Ana Lima".into()),
        workspace: "Team Roadmap".into(),
        profile: "Work".into(),
    });
    view.set_viewport(1000.0, 4000.0);
    view
}

fn laid(view: &mut WelcomeView) -> &Layout {
    view.ensure_laid()
}

fn rect_of(view: &mut WelcomeView, target: Target) -> Rect {
    laid(view)
        .hits
        .iter()
        .find(|(t, _)| *t == target)
        .unwrap_or_else(|| panic!("{target:?} is on the page"))
        .1
}

fn row_rect(view: &mut WelcomeView, row: RowId) -> Rect {
    laid(view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|r| r.id == row)
        .unwrap_or_else(|| panic!("{row:?} is drawn"))
        .rect
}

fn press(view: &mut WelcomeView, rect: Rect) -> bool {
    let (x, y) = rect.centre();
    View::mouse_down(view, &mut doc(), x, y - view.scroll, false)
}

fn key(view: &mut WelcomeView, named: NamedKey, mods: ModifiersState) -> bool {
    View::key(view, &mut doc(), &Key::Named(named), None, mods)
}

// --------------------------------------------------------- what is counted

#[test]
fn only_rows_with_an_action_tick_and_only_checklist_rows_count() {
    let view = page();
    assert_eq!(view.progress_of(Section::FirstMinutes), (0, 6));
    assert_eq!(view.progress_of(Section::Further), (0, 3));
    assert_eq!(
        view.progress_of(Section::Devices),
        (0, 0),
        "devices have no count"
    );
    for inert in [RowId::TaskList, RowId::Watch, RowId::OwnApps] {
        assert!(!inert.ticks() && !inert.counted(), "{inert:?}");
    }
    assert!(RowId::Mcp.ticks() && RowId::Mcp.counted());
    for device in [RowId::Phone, RowId::Computer] {
        assert!(device.ticks() && !device.counted(), "{device:?}");
    }
    assert_eq!(
        RowId::persisted(),
        [
            "first-document",
            "documents",
            "files",
            "learn",
            "reminder",
            "feedback",
            "phone",
            "computer",
            "ai",
            "mcp",
            "terminal-agent"
        ]
    );
}

#[test]
fn inert_rows_are_drawn_without_a_tick_circle() {
    let mut view = page();
    for row in laid(&mut view).sections.iter().flat_map(|s| s.rows.iter()) {
        assert_eq!(row.tick.is_some(), row.id.ticks(), "{:?}", row.id);
    }
}

#[test]
fn the_header_counts_and_a_finished_checklist_folds_until_shown() {
    let mut view = page();
    let first = |view: &mut WelcomeView| {
        let section = &laid(view).sections[0];
        (
            section.progress.as_ref().map(|(text, _)| text.clone()),
            section.toggle.map(|(label, _)| label),
            section.folded.is_some(),
        )
    };
    assert_eq!(first(&mut view), (Some("0 of 6 done".into()), None, false));
    assert!(
        laid(&mut view).sections[1].progress.is_none(),
        "Connect your devices has no count"
    );

    view.set_done(["first-document", "documents", "files", "learn"]);
    assert_eq!(first(&mut view), (Some("4 of 6 done".into()), None, false));

    view.set_done(["reminder", "feedback"]);
    assert_eq!(
        first(&mut view),
        (Some("6 of 6 done".into()), Some(SHOW), true),
        "all counted rows done: one line and a Show link"
    );
    assert_eq!(Section::FirstMinutes.done_line(), FIRST_MINUTES_DONE);

    let toggle = rect_of(&mut view, Target::Toggle(Section::FirstMinutes));
    assert!(press(&mut view, toggle));
    assert_eq!(
        first(&mut view),
        (Some("6 of 6 done".into()), Some(HIDE), false)
    );
    let toggle = rect_of(&mut view, Target::Toggle(Section::FirstMinutes));
    assert!(press(&mut view, toggle));
    assert!(first(&mut view).2, "Hide folds it again");
    assert!(
        view.take_wants().is_empty(),
        "showing and hiding asks for nothing"
    );

    view.set_done(["ai", "mcp", "terminal-agent"]);
    let further = &laid(&mut view).sections[2];
    assert!(further.folded.is_some());
    assert_eq!(Section::Further.done_line(), FURTHER_DONE);
}

// ------------------------------------------------------------ the actions

#[test]
fn every_action_raises_its_existing_command_and_ticks_its_row() {
    let _hints = guide();
    let cases: [(Target, Option<WelcomeWant>, Option<RowId>); 14] = [
        (Target::Customize, Some(WelcomeWant::Settings), None),
        (
            Target::Row(RowId::FirstDocument),
            Some(WelcomeWant::NewDocument),
            Some(RowId::FirstDocument),
        ),
        (Target::MicLink, Some(WelcomeWant::VoiceSettings), None),
        (
            Target::Import("markdown"),
            Some(WelcomeWant::Import("markdown")),
            Some(RowId::Documents),
        ),
        (
            Target::ChooseFiles,
            Some(WelcomeWant::ImportFiles),
            Some(RowId::Files),
        ),
        (Target::Row(RowId::Learn), None, Some(RowId::Learn)),
        (
            Target::Row(RowId::Reminder),
            Some(WelcomeWant::Reminders),
            Some(RowId::Reminder),
        ),
        (
            Target::FeedbackForm,
            Some(WelcomeWant::Feedback),
            Some(RowId::Feedback),
        ),
        (
            Target::Email,
            Some(WelcomeWant::Email),
            Some(RowId::Feedback),
        ),
        (
            Target::Row(RowId::Computer),
            Some(WelcomeWant::LinkDevice),
            Some(RowId::Computer),
        ),
        (Target::PairQr, Some(WelcomeWant::LinkDevice), None),
        (
            Target::Row(RowId::Ai),
            Some(WelcomeWant::AiSetup),
            Some(RowId::Ai),
        ),
        (
            Target::CopyMcp,
            Some(WelcomeWant::CopyMcpCommand),
            Some(RowId::Mcp),
        ),
        (
            Target::Row(RowId::TerminalAgent),
            Some(WelcomeWant::Agents),
            Some(RowId::TerminalAgent),
        ),
    ];
    for (target, want, tick) in cases {
        let mut view = page();
        let rect = rect_of(&mut view, target);
        assert!(press(&mut view, rect), "{target:?} took the press");
        assert_eq!(
            view.take_wants(),
            want.into_iter().collect::<Vec<_>>(),
            "{target:?}"
        );
        assert_eq!(
            view.take_newly_done(),
            tick.map(RowId::key).into_iter().collect::<Vec<_>>(),
            "{target:?}"
        );
        assert!(view.take_wants().is_empty(), "{target:?}: taken once");
    }
}

#[test]
fn every_import_chip_names_its_registered_format() {
    let mut view = page();
    let chips: Vec<(&str, String)> = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .flat_map(|r| r.chips.iter())
        .map(|(id, label, _)| (*id, label.clone()))
        .collect();
    let labels: Vec<&str> = chips.iter().map(|(_, label)| label.as_str()).collect();
    assert_eq!(
        labels,
        [
            "Markdown",
            "Evernote",
            "TextBundle",
            "Microsoft OneNote",
            "Google Docs",
            "Notion"
        ]
    );
    for (id, _) in chips {
        let mut view = page();
        let rect = rect_of(&mut view, Target::Import(id));
        assert!(press(&mut view, rect));
        assert_eq!(view.take_wants(), [WelcomeWant::Import(id)]);
    }
}

#[test]
fn rows_with_nothing_behind_them_do_nothing() {
    let mut view = page();
    for row in [
        RowId::TaskList,
        // These rows act only through their links, chips and buttons.
        RowId::Documents,
        RowId::Files,
        RowId::Feedback,
        RowId::Mcp,
    ] {
        let rect = row_rect(&mut view, row);
        // The left edge of the row: no link, chip or button lives there.
        let x = rect.x + 4.0;
        let y = rect.y + 4.0;
        assert!(
            !View::mouse_down(&mut view, &mut doc(), x, y, false),
            "{row:?} took a press"
        );
    }
    assert!(view.take_wants().is_empty());
    assert!(view.take_newly_done().is_empty());
}

/// *Phone*, *Watch* and *Write your own apps* are hidden for now, and the
/// feedback sentence offers the form and email only.
#[test]
fn the_hidden_rows_and_chat_are_not_on_the_page() {
    let mut view = page();
    let drawn: Vec<RowId> = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter().map(|r| r.id))
        .collect();
    for hidden in [RowId::Phone, RowId::Watch, RowId::OwnApps] {
        assert!(!drawn.contains(&hidden), "{hidden:?} is drawn");
    }
    assert_eq!(Section::Devices.rows(), [RowId::Computer]);
    let feedback: String = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|r| r.id == RowId::Feedback)
        .expect("drawn")
        .sentence
        .iter()
        .map(|run| run.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    assert!(!feedback.contains("chat"), "{feedback}");
    assert!(feedback.contains("feedback form") && feedback.contains("email"));
}

/// **The MCP command**: the bridge beside this executable, `serve --stdio`,
/// shown on the row and copied whole by its chip and by *Copy*.
#[test]
fn the_mcp_row_shows_the_command_and_copies_it() {
    let mut view = page();
    let (shown, _) = laid(&mut view)
        .sections
        .iter()
        .flat_map(|s| s.rows.iter())
        .find(|r| r.id == RowId::Mcp)
        .and_then(|r| r.code.clone())
        .expect("the command is drawn");
    assert!(shown.ends_with("serve --stdio"), "{shown}");
    assert_eq!(view.mcp_command, mcp::command());
    let copy_targets = laid(&mut view)
        .hits
        .iter()
        .filter(|(t, _)| *t == Target::CopyMcp)
        .count();
    assert_eq!(copy_targets, 2, "the chip and the button");
}

#[test]
fn the_mcp_command_quotes_paths_with_spaces_and_carries_the_root() {
    use std::path::Path;
    assert_eq!(
        mcp::command_line(Path::new("/opt/koru/koru-mcp"), None),
        "/opt/koru/koru-mcp serve --stdio"
    );
    assert_eq!(
        mcp::command_line(
            Path::new(r"C:\Program Files\Koru\koru-mcp.exe"),
            Some(Path::new(r"D:\dev data")),
        ),
        r#""C:\Program Files\Koru\koru-mcp.exe" serve --stdio --root "D:\dev data""#
    );
}

#[test]
fn a_long_command_is_shortened_in_the_middle_to_fit() {
    let mut engine = TextEngine::new();
    let long = format!(
        "{}koru-mcp.exe serve --stdio",
        r"C:\Users\someone\AppData\Local\Programs\Koru\".repeat(4)
    );
    let max = layout::key_w(&mut engine, "koru-mcp.exe serve --stdio") + 120.0;
    let shown = layout::elide_middle(&mut engine, &long, max);
    assert!(layout::key_w(&mut engine, &shown) <= max);
    assert!(
        shown.contains('\u{2026}') && shown.ends_with("--stdio"),
        "{shown}"
    );
    assert_eq!(
        layout::elide_middle(&mut engine, "koru-mcp serve --stdio", 10_000.0),
        "koru-mcp serve --stdio",
        "what fits is left alone"
    );
}

#[test]
fn learn_your_way_around_opens_the_shortcuts_inline_and_ticks_once() {
    let _hints = guide();
    let mut view = page();
    let learn = |view: &mut WelcomeView| {
        let row = laid(view)
            .sections
            .iter()
            .flat_map(|s| s.rows.iter())
            .find(|r| r.id == RowId::Learn)
            .expect("drawn");
        (
            row.hints.len(),
            matches!(row.trailing, layout::Trailing::Caret(true, _)),
            row.rect.h,
        )
    };
    let (lines, open, closed_h) = learn(&mut view);
    assert_eq!((lines, open), (0, false));

    let rect = rect_of(&mut view, Target::Row(RowId::Learn));
    assert!(press(&mut view, rect));
    let (lines, open, open_h) = learn(&mut view);
    assert_eq!(lines, hints::hint_lines().len(), "the registry's lines");
    assert!(lines >= 4, "the Getting around hints");
    assert!(open && open_h > closed_h);
    assert_eq!(view.take_newly_done(), ["learn"]);

    let rect = rect_of(&mut view, Target::Row(RowId::Learn));
    assert!(press(&mut view, rect));
    assert_eq!(learn(&mut view).0, 0, "a second press folds it");
    assert!(view.take_newly_done().is_empty(), "already ticked");
    assert!(
        view.take_wants().is_empty(),
        "there is no shortcuts page to open"
    );
}

// ------------------------------------------------------------- the ticks

#[test]
fn ticks_are_stable_ids_taken_once_and_merged_from_the_store() {
    let mut view = page();
    view.set_done(["reminder", "a-row-from-a-newer-build"]);
    assert!(view.is_done(RowId::Reminder));
    let rect = rect_of(&mut view, Target::Row(RowId::Reminder));
    assert!(press(&mut view, rect));
    assert!(
        view.take_newly_done().is_empty(),
        "a row the Store already has is not written again"
    );
    let rect = rect_of(&mut view, Target::Row(RowId::Ai));
    assert!(press(&mut view, rect));
    assert_eq!(view.take_newly_done(), ["ai"]);
    assert!(view.take_newly_done().is_empty());
    view.set_done([]);
    assert!(view.is_done(RowId::Ai), "a tick is never taken back");
}

// ----------------------------------------------------- keyboard and scroll

#[test]
fn tab_walks_the_actions_in_page_order_and_enter_presses_the_focused_one() {
    let mut view = page();
    assert!(
        !key(&mut view, NamedKey::Enter, ModifiersState::empty()),
        "no focus yet"
    );
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::empty()));
    assert_eq!(view.focus, Some(Target::Customize));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::empty()));
    assert_eq!(view.focus, Some(Target::Row(RowId::FirstDocument)));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::empty()));
    assert_eq!(view.focus, Some(Target::MicLink));
    assert!(key(&mut view, NamedKey::Enter, ModifiersState::empty()));
    assert_eq!(view.take_wants(), [WelcomeWant::VoiceSettings]);

    let order = view.focus_order();
    assert!(order.contains(&Target::Email) && order.contains(&Target::PairQr));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::SHIFT));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::SHIFT));
    assert_eq!(view.focus, Some(Target::Customize));
    assert!(key(&mut view, NamedKey::Tab, ModifiersState::SHIFT));
    assert_eq!(
        view.focus,
        order.last().copied(),
        "Shift+Tab wraps to the end"
    );
}

#[test]
fn the_page_scrolls_to_its_last_section_and_keeps_its_hits_while_scrolled() {
    let mut view = page();
    view.set_viewport(760.0, 500.0);
    View::scroll_by(
        &mut view,
        MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -100_000.0)),
    );
    let bottom = laid(&mut view).bottom;
    assert!(bottom > 500.0, "the page is taller than the window");
    assert!(
        (view.scroll - (bottom - 500.0)).abs() < 0.01,
        "clamped to the end"
    );
    let last = laid(&mut view).sections.last().expect("a section").card;
    assert!(
        last.y + last.h <= view.scroll + 500.0,
        "the last section is on screen at the end of the scroll"
    );

    // The last section's AI row, pressed where it is shown.
    let ai = rect_of(&mut view, Target::Row(RowId::Ai));
    assert!(press(&mut view, ai));
    assert_eq!(view.take_wants(), [WelcomeWant::AiSetup]);
}

/// **Three lines a notch**, the step the document editor already uses.
#[test]
fn a_wheel_notch_moves_three_lines() {
    let mut view = page();
    view.set_viewport(1000.0, 400.0);
    View::scroll_by(&mut view, MouseScrollDelta::LineDelta(0.0, -1.0));
    let expected = theme::text_metrics::DOC_BODY.line_height * WHEEL_LINES;
    assert!(
        (view.scroll - expected).abs() < 0.01,
        "moved {}",
        view.scroll
    );
}

#[test]
fn focus_scrolls_the_focused_control_into_view() {
    let mut view = page();
    view.set_viewport(760.0, 400.0);
    let order = view.focus_order();
    for _ in 0..order.len() {
        assert!(key(&mut view, NamedKey::Tab, ModifiersState::empty()));
    }
    let focus = view.focus.expect("focused");
    let rect = rect_of(&mut view, focus);
    assert!(rect.y >= view.scroll && rect.y + rect.h <= view.scroll + 400.0);
}

// --------------------------------------------------- drawing and geometry

fn capture(view: &mut WelcomeView) -> inspect::Snapshot {
    let mut canvas = Scene::new();
    let root = Affine::IDENTITY;
    let (w, h) = view.viewport;
    let frame = inspect::Frame::begin(1, root, koru_draw::scene::rect(0.0, 0.0, w, h));
    view.draw_page(&mut canvas, root);
    frame.finish()
}

#[test]
fn what_is_drawn_is_exactly_what_can_be_pressed() {
    let mut view = page();
    view.set_done(["first-document"]);
    let _ = capture(&mut view);
    let hits = laid(&mut view).hits.clone();
    assert_eq!(view.drawn, hits);
    // And again while scrolled, where a drift would show first.
    view.set_viewport(760.0, 500.0);
    View::scroll_by(&mut view, MouseScrollDelta::LineDelta(0.0, -5.0));
    let _ = capture(&mut view);
    let hits = laid(&mut view).hits.clone();
    assert_eq!(view.drawn, hits);
}

#[test]
fn the_inspector_shows_one_named_tree_with_no_loose_primitives() {
    let mut view = page();
    let snapshot = capture(&mut view);
    assert!(!snapshot.truncated);
    let roots: Vec<&str> = snapshot
        .records
        .iter()
        .filter(|r| r.parent.is_none())
        .map(|r| r.name)
        .collect();
    assert_eq!(roots, ["KoruWelcome"], "one root, everything under it");
    let page = snapshot
        .records
        .iter()
        .position(|r| r.name == "KoruWelcome")
        .expect("root");
    let loose: Vec<&str> = snapshot
        .records
        .iter()
        .filter(|r| r.parent == Some(page) && r.kind == "primitive" && r.name != "fill")
        .map(|r| r.name)
        .collect();
    assert!(
        loose.is_empty(),
        "loose primitives under the page: {loose:?}"
    );
    let count = |name: &str| snapshot.records.iter().filter(|r| r.name == name).count();
    assert_eq!(count("Welcome header"), 0, "the page has no header for now");
    assert_eq!(count("Welcome section"), 3);
    assert_eq!(count("Welcome footer"), 0, "the page has no footer");
    assert_eq!(count("Welcome pair QR"), 1);
    assert_eq!(count("KoruLogo"), 0, "no header, so no mark");
    assert_eq!(count("KoruTextTitle"), 0, "no header, so no greeting");
    assert_eq!(
        count("Welcome row"),
        11,
        "every row of all three sections but the three hidden for now"
    );
    assert_eq!(count("Welcome command"), 1, "the MCP command");
    // Every row sits inside a section, and every section inside the page.
    for (i, record) in snapshot.records.iter().enumerate() {
        if record.name == "Welcome row" {
            let parent = &snapshot.records[record.parent.expect("parented")];
            assert!(
                parent.name == "KoruCard",
                "row {i} is under {:?}",
                parent.name
            );
        }
    }
}

#[test]
fn the_column_is_centred_and_capped() {
    let mut view = page();
    view.set_viewport(2400.0, 900.0);
    let column = laid(&mut view).column;
    assert_eq!(column.w, layout::COLUMN_W, "capped");
    assert!(
        (column.x - (2400.0 - layout::COLUMN_W) / 2.0).abs() < 0.5,
        "centred"
    );
    view.set_viewport(300.0, 900.0);
    let narrow = laid(&mut view).column;
    assert!(narrow.w <= 300.0 && narrow.w > 0.0, "{}", narrow.w);
    assert!(narrow.x >= 0.0);
}

#[test]
fn a_narrow_window_wraps_sentences_instead_of_overflowing() {
    let mut view = page();
    view.set_viewport(420.0, 900.0);
    let column = laid(&mut view).column;
    for row in laid(&mut view).sections.iter().flat_map(|s| s.rows.iter()) {
        for run in &row.sentence {
            assert!(
                run.x + run.w <= column.x + column.w + 0.5,
                "{:?}: '{}' runs past the column",
                row.id,
                run.text
            );
        }
    }
}

#[test]
fn the_embedded_logo_is_square() {
    let logo = koru_draw::brand::logo_brush();
    assert_eq!(logo.image.width, logo.image.height);
    assert!(logo.image.width > 0);
}

// ------------------------------------------------- shortcuts from the registry

/// The dictation chord is hidden for now; the other chords are the
/// registry's rather than strings written here.
#[test]
fn the_shortcut_lines_leave_dictation_out_and_come_from_the_registry() {
    let _hints = guide();
    let lines = hints::hint_lines();
    assert!(
        lines.iter().all(|(_, what)| !what.contains("dictate")),
        "{lines:?}"
    );
    let (chord, _) = lines
        .iter()
        .find(|(_, what)| *what == "new tab")
        .expect("the Welcome page teaches a new tab");
    assert_eq!(
        chord,
        &koru_keys::registry::label_of(koru_keys::ShortcutId::NewTab),
        "the chord was written out rather than looked up"
    );
}

/// **Only when guide hints are on** — the key chip beside *Make your first
/// document* goes with them.
#[test]
fn the_shortcut_lines_and_key_chip_go_quiet_when_guide_hints_are_off() {
    let _hints = guide();
    koru_keys::hint::set_shown(koru_keys::Section::Guide, false);
    let quiet = hints::hint_lines();
    let chip = hints::new_document_chord();
    koru_keys::hint::set_shown(koru_keys::Section::Guide, true);
    assert!(quiet.is_empty());
    assert!(chip.is_none());
    assert_eq!(
        hints::new_document_chord().as_deref(),
        Some(koru_keys::registry::label_of(koru_keys::ShortcutId::NewTab).as_str())
    );
}

// ----------------------------------------------------------- build stamp

#[test]
fn an_unpublished_build_still_stamps_when_it_was_built() {
    assert!(koru_channel::build_info().released().is_none());
    let stamp = build_stamp().expect("a build knows when it was built");
    let when = stamp
        .strip_prefix("built ")
        .unwrap_or_else(|| panic!("{stamp}"));
    assert!(chrono::NaiveDateTime::parse_from_str(when, "%Y-%m-%d %H:%M").is_ok());
}

#[test]
fn a_published_build_shows_its_release_time_in_local_time() {
    use chrono::Local;
    let shown = build::stamp(Some("2026-08-08T14:32:11Z"), None).expect("stamped");
    let when = shown
        .strip_prefix("released ")
        .unwrap_or_else(|| panic!("{shown}"));
    let expected = chrono::DateTime::parse_from_rfc3339("2026-08-08T14:32:11Z")
        .unwrap()
        .with_timezone(&Local)
        .format("%Y-%m-%d %H:%M")
        .to_string();
    assert_eq!(when, expected);
    assert_eq!(
        build::stamp(Some("whenever"), None).as_deref(),
        Some("released whenever")
    );
    assert_eq!(build::stamp(None, None), None);
}

/// **The simplified product's Welcome** (koru-features all off): no *Task
/// list*, *Reminder*, *Koru AI* or agent row; *Further* still counts its MCP
/// row.
#[test]
fn hidden_surfaces_rows_leave_the_page() {
    use super::rows::{RowId, Section};
    koru_features::testing::with(koru_features::Features::ALL_OFF, || {
        let first = Section::FirstMinutes.rows();
        assert!(!first.contains(&RowId::TaskList));
        assert!(!first.contains(&RowId::Reminder));
        assert!(first.contains(&RowId::FirstDocument));
        let further = Section::Further.rows();
        assert_eq!(
            further,
            [RowId::Mcp],
            "the row that does not hang on a hidden surface stays"
        );
        assert!(Section::Further.counts());
        assert!(Section::FirstMinutes.counts());
    });
    assert!(Section::Further.rows().contains(&RowId::Ai));
    assert!(Section::Further.counts());
}
