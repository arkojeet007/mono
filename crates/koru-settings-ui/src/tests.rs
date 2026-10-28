// SPDX-License-Identifier: MPL-2.0

//! **The gates SP-7 named**, and the helpers its sibling suites press with.
//!
//! Nothing here dials a Gateway or resolves a path: every surface is built with
//! [`SettingsLinks::disabled`], and every section arrives through the host
//! contract. That is not only a test convenience — it is the contract a host
//! under `cfg(test)` uses too, because a dependency is never compiled with
//! `cfg(test)` and a guard inside this crate could not cover `apps/koru`.

use super::*;
use koru_settings::TerminalCursorWidth;

/// **The process globals this surface writes are shared by the whole binary.**
/// `koru_draw::theme`'s setters are atomics, so the tests that move one take
/// this — otherwise a test asserting the density is Compact fails whenever the
/// test setting it to Comfortable happens to be running beside it.
pub(crate) static GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A surface with a viewport, no links, and nothing fed yet.
pub(crate) fn surface(sections: Sections) -> SettingsSurface {
    let mut surface = SettingsSurface::new(sections);
    surface.set_viewport(1000.0, 700.0);
    surface.relayout();
    surface
}

/// A full reading, the shape a host takes one.
pub(crate) fn voice_facts() -> VoiceFacts {
    VoiceFacts {
        blocked: None,
        languages: vec![
            (
                DictationLanguage::default(),
                "Detect automatically".to_owned(),
            ),
            (DictationLanguage::parse("en"), "English".to_owned()),
        ],
        inputs: vec![
            (DictationInput::default(), "System default".to_owned()),
            (DictationInput::parse("Yeti Nano"), "Yeti Nano".to_owned()),
        ],
        input_note: None,
        tiers: vec![
            (DictationTier::Compact, "Compact".to_owned()),
            (DictationTier::HighAccuracy, "High accuracy".to_owned()),
        ],
        tier_notes: vec![
            (
                DictationTier::Compact,
                "Compact — 57 MB. Words appear when you finish speaking.".to_owned(),
            ),
            (
                DictationTier::HighAccuracy,
                "High accuracy — Words appear as you speak.".to_owned(),
            ),
        ],
        model_line: "Installed · 648 MB".to_owned(),
        model_action: "Remove".to_owned(),
        model_removes: true,
        consent_granted: true,
        consent_line: "You agreed to voice input on this machine.".to_owned(),
        consent_action: "Revoke".to_owned(),
        chord: Some("Ctrl+Shift+D".to_owned()),
    }
}

pub(crate) fn renderer_row() -> RendererRow {
    RendererRow {
        label: "Renderer".to_owned(),
        note: "Applies the next time Koru starts.".to_owned(),
        options: vec![
            ("auto".to_owned(), "Auto".to_owned()),
            ("dx12".to_owned(), "DirectX 12".to_owned()),
            ("vulkan".to_owned(), "Vulkan".to_owned()),
            ("gl".to_owned(), "OpenGL".to_owned()),
        ],
        current: "auto".to_owned(),
    }
}

/// Koru's surface: every section, every host reading fed.
pub(crate) fn koru() -> SettingsSurface {
    let mut surface = surface(Sections::koru());
    surface.set_voice_facts(voice_facts());
    surface.set_renderer(renderer_row());
    surface.set_ai_slot(AiSlot {
        note: "OpenAI · a key is configured".to_owned(),
        action: Some("Open AI setup…".to_owned()),
    });
    surface.relayout();
    surface
}

/// Every sentence the **showing page** puts on the screen: its title and
/// description, notes, card titles and asides, row names and hints.
pub(crate) fn page_lines(surface: &SettingsSurface) -> Vec<String> {
    let mut out = Vec::new();
    for block in &surface.laid.blocks {
        match block {
            Block::Title {
                title, description, ..
            } => {
                out.push(title.clone());
                out.push(description.clone());
            }
            Block::Note { text, .. } => out.push(text.clone()),
            Block::Tiles { title, .. } => out.extend(title.clone()),
            Block::Card(card) => {
                out.extend(card.title.clone());
                out.extend(card.aside.clone());
                for row in &card.rows {
                    out.push(row.label.clone());
                    out.extend(row.hint.clone());
                    out.extend(row.link.as_ref().map(|(text, _)| text.clone()));
                    out.extend(row.chip.clone());
                }
            }
            Block::Caption { title, link, .. } => {
                out.push(title.clone());
                out.extend(link.as_ref().map(|(text, _, _)| text.clone()));
            }
            Block::Host => {}
        }
    }
    out
}

/// Every sentence on **every page** this host has — what a person could read
/// by walking the sidebar. The original is not moved.
pub(crate) fn lines(surface: &SettingsSurface) -> Vec<String> {
    let mut walk = clone_surface(surface);
    let mut out = Vec::new();
    for page in walk.sections.pages() {
        walk.page = page;
        walk.relayout();
        out.extend(page_lines(&walk));
    }
    out
}

/// A copy of what a layout reads, for walking pages without moving the
/// original. The links are not copied — a walk never presses anything.
fn clone_surface(surface: &SettingsSurface) -> SettingsSurface {
    let mut copy = SettingsSurface::new(surface.sections);
    copy.lang = surface.lang;
    copy.viewport = surface.viewport;
    copy.global = surface.global.clone();
    copy.terminal = surface.terminal.clone();
    copy.pinned_show_title = surface.pinned_show_title;
    copy.close_behavior = surface.close_behavior;
    copy.voice_language = surface.voice_language.clone();
    copy.voice_input = surface.voice_input.clone();
    copy.voice_tier = surface.voice_tier;
    copy.voice_gesture = surface.voice_gesture;
    copy.spelling_enabled = surface.spelling_enabled;
    copy.spelling_language = surface.spelling_language.clone();
    copy.spelling_languages = surface.spelling_languages.clone();
    copy.spelling_note = surface.spelling_note.clone();
    copy.dictionary = surface.dictionary.clone();
    copy.voice = surface.voice.clone();
    copy.renderer = surface.renderer.clone();
    copy.ai = surface.ai.clone();
    copy.telemetry = surface.telemetry;
    copy.availability = surface.availability;
    copy.notice = surface.notice.clone();
    copy.host_section_height = surface.host_section_height;
    copy
}

/// A word a person can press on the showing page, and where it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Found {
    /// A segment, tile or button: hit `n` of the page.
    Hit(usize),
    /// Option `item` of dropdown `dropdown`.
    Option { dropdown: usize, item: usize },
    /// A stepper's readout: it reads as chosen and cannot be pressed.
    Readout,
}

/// Every word a widget on the showing page carries, with where pressing it
/// lands and whether it reads as chosen.
fn words(surface: &SettingsSurface) -> Vec<(String, Found, bool)> {
    fn walk(widget: &Widget, laid: &Laid, out: &mut Vec<(String, Found, bool)>) {
        match widget {
            Widget::Segmented {
                labels,
                selected,
                first,
                ..
            } => {
                for (i, label) in labels.iter().enumerate() {
                    out.push((label.clone(), Found::Hit(first + i), *selected == Some(i)));
                }
            }
            Widget::Dropdown { dropdown, .. } => {
                for (item, (_, option)) in laid.dropdowns[*dropdown].options.iter().enumerate() {
                    out.push((
                        option.label.clone(),
                        Found::Option {
                            dropdown: *dropdown,
                            item,
                        },
                        option.selected,
                    ));
                }
            }
            Widget::Button { label, hit, .. } => {
                out.push((label.clone(), Found::Hit(*hit), false));
            }
            Widget::Stepper { value, .. } => {
                out.push((value.clone(), Found::Readout, true));
            }
            Widget::Pair(a, b) => {
                walk(a, laid, out);
                walk(b, laid, out);
            }
            Widget::None | Widget::Toggle { .. } | Widget::Swatches { .. } => {}
        }
    }
    let mut out = Vec::new();
    for block in &surface.laid.blocks {
        match block {
            Block::Tiles { tiles, .. } => {
                for tile in tiles {
                    out.push((tile.label.clone(), Found::Hit(tile.hit), tile.selected));
                }
            }
            Block::Card(card) => {
                for row in &card.rows {
                    // A link ending a row's hint is pressed like any control.
                    if let Some((text, hit)) = &row.link {
                        out.push((text.clone(), Found::Hit(*hit), false));
                    }
                    walk(&row.widget, &surface.laid, &mut out);
                }
            }
            Block::Caption {
                link: Some((text, _, hit)),
                ..
            } => out.push((text.clone(), Found::Hit(*hit), false)),
            _ => {}
        }
    }
    out
}

/// Every pressable word on the showing page.
pub(crate) fn labels(surface: &SettingsSurface) -> Vec<String> {
    words(surface)
        .into_iter()
        .filter(|(_, found, _)| *found != Found::Readout)
        .map(|(word, _, _)| word)
        .collect()
}

pub(crate) fn find(surface: &SettingsSurface, word: &str) -> Option<Found> {
    words(surface)
        .into_iter()
        .find(|(w, found, _)| w == word && *found != Found::Readout)
        .map(|(_, found, _)| found)
}

/// The content-space rect a word is pressed at: the control itself, or for a
/// dropdown option the field that opens its list. Showing page only.
pub(crate) fn rect_of(surface: &SettingsSurface, word: &str) -> Option<Rect> {
    match find(surface, word)? {
        Found::Hit(index) => Some(surface.laid.hits[index].rect),
        Found::Option { dropdown, .. } => Some(surface.laid.dropdowns[dropdown].field),
        Found::Readout => None,
    }
}

/// Whether the word reads as the current choice on the showing page.
pub(crate) fn is_active(surface: &SettingsSurface, word: &str) -> bool {
    words(surface)
        .into_iter()
        .any(|(w, _, chosen)| w == word && chosen)
}

/// Show the first page (in sidebar order) that carries `word`.
pub(crate) fn goto(surface: &mut SettingsSurface, word: &str) -> bool {
    for page in surface.sections.pages() {
        surface.page = page;
        surface.scroll = 0.0;
        surface.relayout();
        if words(surface).iter().any(|(w, _, _)| w == word) {
            return true;
        }
    }
    false
}

/// Press the viewport point over a content-space rect, scrolling it into view
/// first as a person would.
pub(crate) fn press_rect(surface: &mut SettingsSurface, rect: Rect) -> bool {
    surface.reveal_content_rect(rect);
    let (x, y) = rect.centre();
    surface.mouse_down(x, y - surface.scroll)
}

/// Press the control carrying this word — on whichever page has it, opening
/// its dropdown first when it is an option. Panics if there is none, because a
/// test that silently pressed nothing would pass for the wrong reason.
pub(crate) fn press(surface: &mut SettingsSurface, word: &str) -> bool {
    assert!(
        goto(surface, word),
        "no control carries `{word}` on any page"
    );
    match find(surface, word).unwrap_or_else(|| panic!("`{word}` is not pressable")) {
        Found::Hit(index) => {
            let rect = surface.laid.hits[index].rect;
            press_rect(surface, rect)
        }
        Found::Option { dropdown, item } => {
            let field = surface.laid.dropdowns[dropdown].field;
            assert!(press_rect(surface, field), "the field opens its list");
            let open = surface.open.as_ref().expect("the list is open");
            let (x, y) = open.menu.item_rect(surface.viewport, item).centre();
            surface.mouse_down(x, y)
        }
        Found::Readout => unreachable!("filtered by find"),
    }
}

/// Press the control carrying `word` on one named page — for a word two pages
/// share, such as a theme's *Dark*.
pub(crate) fn press_on(surface: &mut SettingsSurface, page: PageId, word: &str) -> bool {
    surface.show_page(page);
    surface.relayout();
    match find(surface, word).unwrap_or_else(|| panic!("`{word}` is not on {page:?}")) {
        Found::Hit(index) => {
            let rect = surface.laid.hits[index].rect;
            press_rect(surface, rect)
        }
        Found::Option { dropdown, item } => {
            let field = surface.laid.dropdowns[dropdown].field;
            assert!(press_rect(surface, field), "the field opens its list");
            let open = surface.open.as_ref().expect("the list is open");
            let (x, y) = open.menu.item_rect(surface.viewport, item).centre();
            surface.mouse_down(x, y)
        }
        Found::Readout => unreachable!("filtered by find"),
    }
}

/// The switch on the row named `row` on the showing page: its hit, and
/// whether it is on.
pub(crate) fn switch(surface: &SettingsSurface, row: &str) -> Option<(usize, bool)> {
    surface.laid.blocks.iter().find_map(|block| match block {
        Block::Card(card) => card.rows.iter().find_map(|r| match &r.widget {
            Widget::Toggle { on, hit, .. } if r.label == row => Some((*hit, *on)),
            _ => None,
        }),
        _ => None,
    })
}

/// Show the page with the switch named `row`, and say whether it is on.
pub(crate) fn switch_on(surface: &mut SettingsSurface, row: &str) -> bool {
    for page in surface.sections.pages() {
        surface.page = page;
        surface.relayout();
        if let Some((_, on)) = switch(surface, row) {
            return on;
        }
    }
    panic!("no switch named `{row}` on any page")
}

/// Press the switch on the row named `row`, on whichever page has it.
pub(crate) fn flip(surface: &mut SettingsSurface, row: &str) -> bool {
    for page in surface.sections.pages() {
        surface.page = page;
        surface.scroll = 0.0;
        surface.relayout();
        if let Some((hit, _)) = switch(surface, row) {
            let rect = surface.laid.hits[hit].rect;
            return press_rect(surface, rect);
        }
    }
    panic!("no switch named `{row}` on any page")
}

/// The swatch rects of the row named `row` on the showing page.
pub(crate) fn swatches(surface: &SettingsSurface, row: &str) -> Vec<Rect> {
    surface
        .laid
        .blocks
        .iter()
        .find_map(|block| match block {
            Block::Card(card) => card.rows.iter().find_map(|r| match &r.widget {
                Widget::Swatches { rects, .. } if r.label == row => Some(rects.clone()),
                _ => None,
            }),
            _ => None,
        })
        .unwrap_or_default()
}

// ----------------------------------------------------------------- pages

#[test]
fn koru_lists_every_page_in_sidebar_order() {
    assert_eq!(
        Sections::koru().pages(),
        vec![
            PageId::Appearance,
            PageId::Editor,
            PageId::Reminders,
            PageId::Terminal,
            PageId::System,
        ]
    );
    // The standalone hosts, while they last: no Reminders, and the Terminal
    // page only where the terminal is.
    assert!(!Sections::koru_ai().pages().contains(&PageId::Terminal));
    assert!(!Sections::terminal().pages().contains(&PageId::Reminders));
}

#[test]
fn the_sidebar_switches_pages_and_starts_each_from_the_top() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert_eq!(surface.page(), PageId::Appearance);
    let nav = surface.nav_rect();
    let system = NavList::row_rect(nav, 4);
    let (x, y) = system.centre();
    surface.scroll = 30.0;
    assert!(surface.mouse_down(x, y));
    assert_eq!(surface.page(), PageId::System);
    assert_eq!(surface.scroll, 0.0);
    surface.relayout();
    assert!(page_lines(&surface).iter().any(|line| line == "System"));
}

/// **Narrow, the nav is chips above the page**: no column beside it, the page
/// keeps the full width, and a chip switches pages like a list row does.
#[test]
fn a_narrow_window_turns_the_nav_into_chips_above_the_page() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.relayout();
    assert!(!surface.compact());
    assert!(surface.nav_chips.is_empty());
    let wide_column = surface.column_rect();
    assert!(
        wide_column.x >= NAV_W,
        "the wide page sits right of the nav column"
    );

    surface.set_viewport(COMPACT_BELOW - 40.0, 700.0);
    surface.relayout();
    assert!(surface.compact());
    assert_eq!(surface.nav_chips.len(), surface.listed().len());
    let band = surface.sidebar_rect();
    assert_eq!(band.w, COMPACT_BELOW - 40.0, "the band spans the width");
    let column = surface.column_rect();
    assert!(
        column.y >= band.y + band.h,
        "the page starts under the chips"
    );
    assert!(
        column.x < NAV_W,
        "no nav column takes width beside the page"
    );
    let search = surface.search_rect();
    assert!(search.w <= COMPACT_SEARCH_MAX);
    for chip in &surface.nav_chips {
        assert!(chip.y >= search.y + search.h && chip.y + chip.h <= band.y + band.h);
    }

    let (x, y) = surface.nav_chips[4].centre();
    assert!(surface.mouse_down(x, y));
    assert_eq!(surface.page(), PageId::System);
}

/// **The hit-gating property, kept.** A page that is not showing has no rects,
/// so nothing on it can be pressed — the same guarantee `WelcomeView` held with
/// a flag, with nothing left to forget.
#[test]
fn a_page_that_is_not_showing_has_no_pressable_rect() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::Appearance);
    surface.relayout();
    assert!(
        find(&surface, "Quit").is_none(),
        "System's rows are not here"
    );
    surface.show_page(PageId::System);
    surface.relayout();
    assert!(find(&surface, "Quit").is_some());
}

#[test]
fn a_page_this_host_does_not_have_is_refused() {
    let mut surface = SettingsSurface::new(Sections::koru_ai());
    assert!(!surface.show_page(PageId::Terminal));
    assert_ne!(surface.page(), PageId::Terminal);
}

/// Search narrows the sidebar by what a page holds, not only by its name.
#[test]
fn searching_narrows_the_sidebar_by_what_a_page_holds() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let search = surface.search_rect();
    let (x, y) = search.centre();
    assert!(surface.mouse_down(x, y));
    for c in "microphone".chars() {
        let text = c.to_string();
        assert!(surface.key(
            &WinitKey::Character(text.clone().into()),
            Some(&text),
            ModifiersState::empty()
        ));
    }
    assert_eq!(surface.listed(), vec![PageId::Editor]);
    // Enter goes to the first page it kept.
    assert!(surface.key(
        &WinitKey::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    assert_eq!(surface.page(), PageId::Editor);
    assert!(PageId::System.matches(koru_i18n::Lang::En, "vulkan"));
    assert!(PageId::Appearance.matches(koru_i18n::Lang::En, "  Caret color "));
    assert!(!PageId::Terminal.matches(koru_i18n::Lang::En, "dictionary"));
}

/// `focus_search` is the chord's way in: the box takes the keyboard with its
/// query selected, so the next search replaces the last one instead of being
/// typed onto its end.
#[test]
fn focusing_the_search_box_selects_its_query_for_the_next_one() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(!surface.search_focused());
    surface.focus_search();
    assert!(surface.search_focused());
    let type_in = |surface: &mut SettingsSurface, word: &str| {
        for c in word.chars() {
            let text = c.to_string();
            assert!(surface.key(
                &WinitKey::Character(text.clone().into()),
                Some(&text),
                ModifiersState::empty()
            ));
        }
    };
    type_in(&mut surface, "microphone");
    assert_eq!(surface.listed(), vec![PageId::Editor]);
    // Focused again, the old query is selected: the new one replaces it.
    surface.focus_search();
    assert!(surface.search_focused());
    type_in(&mut surface, "vulkan");
    assert_eq!(
        surface.listed(),
        vec![PageId::System],
        "\"vulkan\", not \"microphonevulkan\""
    );
}

/// Blank space is blank space: the margins beside the page column belong to
/// nobody.
#[test]
fn blank_space_beside_the_column_mutates_nothing() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(!surface.mouse_down(NAV_W + 4.0, 300.0));
    assert!(!surface.mouse_down(996.0, 300.0));
    assert!(surface.take_requests().is_empty());
}

// ------------------------------------------------------------- dropdowns

#[test]
fn a_dropdown_opens_its_list_and_a_choice_closes_it() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let was = theme::body_font();
    let mut surface = koru();
    assert!(press(&mut surface, theme::FontChoice::Lora.label()));
    assert!(surface.open.is_none(), "choosing closes the list");
    // The first Lora on the page is the App font's; it moved, and only it.
    assert_eq!(theme::ui_font(), theme::FontChoice::Lora);
    theme::set_ui_font(was);
    theme::set_body_font(was);
}

#[test]
fn a_press_outside_an_open_list_closes_it_and_presses_nothing() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::System);
    surface.advanced_open = true;
    surface.relayout();
    let field = rect_of(&surface, "Vulkan").expect("the renderer field");
    assert!(press_rect(&mut surface, field));
    assert!(surface.open.is_some());
    let quit = rect_of(&surface, "Quit").expect("the closing row");
    let (x, y) = quit.centre();
    assert!(
        surface.mouse_down(x, y - surface.scroll),
        "the press is ours"
    );
    assert!(surface.open.is_none());
    assert!(
        surface.take_requests().is_empty(),
        "and Quit was not chosen"
    );
}

#[test]
fn the_keyboard_steps_through_an_open_list() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::System);
    surface.advanced_open = true;
    surface.relayout();
    let field = rect_of(&surface, "Vulkan").expect("the renderer field");
    assert!(press_rect(&mut surface, field));
    // Opens on the value in force (Auto, the first), steps to DirectX 12.
    let down = WinitKey::Named(NamedKey::ArrowDown);
    assert!(surface.key(&down, None, ModifiersState::empty()));
    assert!(surface.key(
        &WinitKey::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    assert_eq!(
        surface.take_requests(),
        vec![Request::SetRenderer("dx12".to_owned())]
    );
    // Escape closes without choosing.
    assert!(press_rect(&mut surface, field));
    assert!(surface.key(
        &WinitKey::Named(NamedKey::Escape),
        None,
        ModifiersState::empty()
    ));
    assert!(surface.open.is_none());
    assert!(surface.take_requests().is_empty());
}

// ----------------------------------------------------------- the two owed states

#[test]
fn a_gateway_that_is_not_answering_leaves_the_rows_read_only() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_availability(Availability::Down);
    surface.show_page(PageId::Editor);
    surface.relayout();
    let drawn = page_lines(&surface);
    assert!(
        drawn
            .iter()
            .any(|line| line == koru_i18n::t(Lang::En, Key::SettingsUnavailableReadOnly)),
        "the page must say why it is read-only: {drawn:?}"
    );
    // Visible, not gone: the rows are still there, carrying their last values.
    assert!(rect_of(&surface, "apply to all").is_some());
}

#[test]
fn a_read_only_surface_takes_the_press_and_changes_nothing() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_availability(Availability::Down);
    let before = theme::density();
    assert!(
        press(&mut surface, "Comfortable"),
        "the press is still ours — the pointer is over a control"
    );
    assert_eq!(theme::density(), before, "and it moved nothing");
    assert!(surface.take_requests().is_empty());
}

#[test]
fn a_read_only_surface_shows_no_press_cursor() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::Editor);
    surface.relayout();
    let rect = rect_of(&surface, "apply to all").expect("the button");
    let (x, y) = rect.centre();
    surface.mouse_move(x, y);
    assert_eq!(surface.cursor_icon(), koru_ui::cursor::PRESS);
    surface.set_availability(Availability::Down);
    assert_eq!(surface.cursor_icon(), koru_ui::cursor::IDLE);
}

/// **S23.** A write that did not land comes back as the stored value, and the
/// surface says so rather than going on showing what nobody kept.
#[test]
fn a_refused_write_reconciles_visibly_with_a_quiet_notice() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface =
        SettingsSurface::with_links(Sections::koru(), SettingsLinks::inert(false, true));
    surface.set_viewport(1000.0, 700.0);
    // The person asks for their own cursor, then a thick one.
    assert!(flip(&mut surface, "Match the editor caret"));
    surface.links.take_outstanding_terminal();
    assert!(press_on(
        &mut surface,
        PageId::Terminal,
        koru_i18n::t(Lang::En, Key::TerminalCursorThick)
    ));
    assert_eq!(surface.terminal.cursor_width, TerminalCursorWidth::Thick);
    assert!(surface.notice().is_none(), "nothing has failed yet");

    // The authority re-read the file and answered with what it kept.
    let refused = TerminalSettings::default();
    surface.adopt_terminal(&refused);
    assert_eq!(
        surface.terminal.cursor_width,
        TerminalCursorWidth::default(),
        "the control is back on the authoritative value"
    );
    let said = surface.notice().expect("a notice").text().to_owned();
    assert_eq!(said, koru_i18n::t(Lang::En, Key::SettingsNotSaved));
    surface.relayout();
    assert!(page_lines(&surface).contains(&said));
}

/// A field this surface never asked about moving is another window's change
/// arriving — convergence, not refusal, and it says nothing.
#[test]
fn another_windows_change_arriving_is_not_a_failed_write() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface =
        SettingsSurface::with_links(Sections::koru(), SettingsLinks::inert(false, true));
    surface.set_viewport(1000.0, 700.0);
    let elsewhere = TerminalSettings {
        theme: ThemeChoice::Dark,
        ..TerminalSettings::default()
    };
    assert!(surface.adopt_terminal(&elsewhere));
    assert!(
        surface.notice().is_none(),
        "nothing was outstanding, so nothing was refused"
    );
    surface.show_page(PageId::Terminal);
    surface.relayout();
    assert!(is_active(&surface, "Dark"));
}

/// A committed write clears nothing and says nothing.
#[test]
fn a_write_the_authority_kept_says_nothing() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface =
        SettingsSurface::with_links(Sections::koru(), SettingsLinks::inert(false, true));
    surface.set_viewport(1000.0, 700.0);
    surface.show_page(PageId::Terminal);
    surface.relayout();
    let dark = rect_of(&surface, "Dark").expect("the terminal theme");
    assert!(press_rect(&mut surface, dark));
    let kept = TerminalSettings {
        theme: ThemeChoice::Dark,
        ..TerminalSettings::default()
    };
    surface.adopt_terminal(&kept);
    assert!(surface.notice().is_none());
}

// ------------------------------------------------------------ the host contract

#[test]
fn a_request_is_drained_once() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    assert!(press(&mut surface, "apply to all"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::ApplyLayoutToAllDocuments]
    );
    assert!(surface.take_requests().is_empty(), "and only once");
}

#[test]
fn the_renderer_row_hands_the_host_its_own_token() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    // Folded away until *Show* unfolds the Advanced card.
    assert!(!goto(&mut surface, "Vulkan"));
    assert!(press(&mut surface, "Show"));
    assert!(press(&mut surface, "Vulkan"));
    assert_eq!(
        surface.take_requests(),
        vec![Request::SetRenderer("vulkan".to_owned())],
        "the surface never learns what a RendererBackend is"
    );
}

/// A host that offers no renderer choice — macOS has one backend — gets no row
/// rather than a control with nothing to control.
#[test]
fn a_host_with_no_renderer_choice_draws_no_renderer_row() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_renderer(RendererRow::none());
    assert!(!lines(&surface).iter().any(|line| line == "Renderer"));
    assert!(!goto(&mut surface, "Vulkan"));
}

#[test]
fn the_ai_card_is_the_slot_the_host_filled() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    let drawn = lines(&surface);
    assert!(drawn.iter().any(|line| line == "Koru AI"));
    assert!(
        drawn
            .iter()
            .any(|line| line == "OpenAI · a key is configured")
    );
    assert!(press(&mut surface, "Open AI setup…"));
    assert_eq!(surface.take_requests(), vec![Request::OpenAiSetup]);
}

/// A host that fills nothing draws no AI card either — an empty card is worse
/// than no card.
#[test]
fn an_unfilled_ai_slot_draws_nothing() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let surface = surface(Sections::koru());
    assert!(!lines(&surface).iter().any(|line| line == "AI"));
}

// ------------------------------------------------------ the host's page

#[test]
fn the_host_slot_exists_only_on_the_reminders_page() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_host_section_height(137.0);
    surface.relayout();
    assert!(surface.host_section_rect().is_none(), "not on Appearance");
    surface.show_page(PageId::Reminders);
    surface.relayout();
    let slot = surface.host_section_rect().expect("the host slot");
    assert_eq!(slot.h, 137.0);
    let column = surface.column_rect();
    assert_eq!((slot.x, slot.w), (column.x, column.w));
    assert!(surface.content_bottom() > slot.y + slot.h);
}

#[test]
fn a_zero_height_host_section_still_reports_its_insertion_point() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::Reminders);
    surface.set_host_section_height(0.0);
    surface.relayout();
    let bottom = surface.content_bottom();
    let insertion = surface.host_section_rect().expect("insertion point");
    assert_eq!(insertion.h, 0.0);
    surface.set_host_section_height(f32::NAN);
    surface.relayout();
    assert_eq!(
        surface.content_bottom(),
        bottom,
        "an invalid height takes no room"
    );
}

#[test]
fn host_geometry_uses_the_shared_scroll_and_draw_clip() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.show_page(PageId::Reminders);
    surface.set_host_section_height(900.0);
    surface.relayout();
    surface.scroll_by(winit::event::MouseScrollDelta::PixelDelta(
        winit::dpi::PhysicalPosition::new(0.0, -40.0),
    ));
    assert_eq!(surface.content_y(12.0), 52.0);

    let mut scene = Scene::new();
    surface.draw(&mut scene, Affine::IDENTITY);
    let called = std::cell::Cell::new(false);
    surface.draw_host_section(&mut scene, Affine::IDENTITY, |_, _, slot| {
        called.set(true);
        assert_eq!(slot, surface.host_section_rect().expect("host slot"));
    });
    assert!(called.get());

    surface.show_page(PageId::Appearance);
    surface.relayout();
    let not_called = std::cell::Cell::new(true);
    surface.draw_host_section(&mut scene, Affine::IDENTITY, |_, _, _| {
        not_called.set(false);
    });
    assert!(not_called.get(), "no slot off the Reminders page");
}

// ----------------------------------------------------------------- scrolling

#[test]
fn the_page_scrolls_and_a_press_follows_it() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_viewport(1000.0, 300.0);
    surface.show_page(PageId::Editor);
    surface.relayout();
    let rect = rect_of(&surface, "apply to all").expect("the button");
    let (x, y) = rect.centre();
    surface.scroll_by(winit::event::MouseScrollDelta::PixelDelta(
        winit::dpi::PhysicalPosition::new(0.0, -40.0),
    ));
    assert!(
        surface.mouse_down(x, y - 40.0),
        "the control moved up by exactly what the wheel moved"
    );
    assert_eq!(
        surface.take_requests(),
        vec![Request::ApplyLayoutToAllDocuments]
    );
}

#[test]
fn scrolling_never_goes_above_the_top() {
    let mut surface = SettingsSurface::new(Sections::koru());
    surface.scroll_by(winit::event::MouseScrollDelta::PixelDelta(
        winit::dpi::PhysicalPosition::new(0.0, 200.0),
    ));
    assert_eq!(surface.scroll, 0.0);
}

#[test]
fn revealing_a_host_rect_scrolls_minimally_and_clamps_to_content() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_viewport(1000.0, 260.0);
    surface.show_page(PageId::Reminders);
    surface.set_host_section_height(300.0);
    surface.relayout();
    let host = surface.host_section_rect().unwrap();
    let target = Rect::new(host.x, host.y + 220.0, 132.0, 32.0);

    assert!(surface.reveal_content_rect(target));
    assert!(target.y >= surface.scroll + 16.0);
    assert!(target.y + target.h <= surface.scroll + 260.0 - 16.0);
    assert!(!surface.reveal_content_rect(target));

    let top = Rect::new(host.x, 0.0, 132.0, 32.0);
    assert!(surface.reveal_content_rect(top));
    assert_eq!(surface.scroll, 0.0);
}

// ------------------------------------------------------------------ keyboard

/// With no list open and the search box not focused, every key belongs to the
/// host, which sees it first.
#[test]
fn the_surface_claims_no_chord_of_its_own() {
    let mut surface = SettingsSurface::new(Sections::koru());
    assert!(!surface.key(
        &WinitKey::Named(NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
}

// ------------------------------------------------------------ inspection

/// **Every control is a shared component, and every row nests under its
/// card** — what the maintainers checks in the inspector, checked here first.
#[test]
fn every_page_draws_shared_components_nested_in_cards() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    for page in [
        PageId::Appearance,
        PageId::Editor,
        PageId::Terminal,
        PageId::System,
    ] {
        surface.show_page(page);
        let capture =
            inspect::Frame::begin(1, Affine::IDENTITY, scene::rect(0.0, 0.0, 1000.0, 700.0));
        surface.draw(&mut Scene::new(), Affine::IDENTITY);
        let snapshot = capture.finish();
        let records = &snapshot.records;
        let named = |name: &str| records.iter().filter(|r| r.name == name).count();
        assert_eq!(named("KoruNavList"), 1, "{page:?}");
        assert!(named("KoruCard") >= 1, "{page:?}");
        let under = |index: usize, ancestor: &str| {
            let mut at = records[index].parent;
            while let Some(i) = at {
                if records[i].name == ancestor {
                    return true;
                }
                at = records[i].parent;
            }
            false
        };
        for (index, record) in records.iter().enumerate() {
            if record.name == "KoruCardRow" {
                assert!(under(index, "KoruCard"), "{page:?}: a row escaped its card");
            }
            if matches!(
                record.name,
                "KoruSegmented" | "KoruDropdown" | "KoruToggle" | "KoruSwatch" | "KoruStepper"
            ) {
                assert!(
                    under(index, "KoruCardRow"),
                    "{page:?}: a {} escaped its row",
                    record.name
                );
            }
        }
    }
}

/// **The nav and the page sit together**, centred as one group: in a wide
/// window the page starts right after the nav, not in the middle of what the
/// nav leaves.
#[test]
fn a_wide_window_keeps_the_page_beside_the_nav() {
    let _guard = GLOBALS.lock().unwrap_or_else(|held| held.into_inner());
    let mut surface = koru();
    surface.set_viewport(2400.0, 900.0);
    surface.relayout();
    let nav = surface.sidebar_rect();
    let column = surface.column_rect();
    assert!(
        (column.x - (nav.x + nav.w) - PAGE_MARGIN).abs() < 0.5,
        "the page starts one margin after the nav"
    );
    let left = nav.x;
    let right = 2400.0 - (column.x + column.w + PAGE_MARGIN);
    assert!(
        (left - right).abs() < 1.0,
        "the group is centred: {left} vs {right}"
    );
    // The nav's own parts move with it.
    assert!(surface.search_rect().x >= nav.x);
    assert!(surface.nav_rect().x >= nav.x);
}
