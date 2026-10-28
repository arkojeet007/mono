// SPDX-License-Identifier: MPL-2.0

//! What differs between a document that *is* the window and one that is a tab.

use super::*;
use koru_core::node::{AttrValue, Kind, Node};
use koru_core::{Heading, Mark, MarkSet, WorkspaceId};
use koru_draw::theme::ViewMode;
use koru_window::snap;
use koru_workspace_host::{
    DocViewPrefs, PasteTarget, PastedImage, RichBlock, RichListSpec, RichTableBlock, RichTextBlock,
    RichTextKind, RichTextLink, RichTextRun, Settings, View,
};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

/// The button's rect. The caption always offers one now: `theme::window_menu`
/// and the mutex the tests flipping it queued behind are both gone, and the
/// caption is four stable controls ([ADR-150](../../../docs/decisions/README.md)).
fn menu_button(view: &BuiltInDocument) -> Rect {
    caption::window_menu_button(view.editor.view.viewport.0).expect("a ⋮ button")
}

fn document() -> (WorkspaceDoc, BuiltInDocument) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "spec.md").expect("a document");
    let view = BuiltInDocument::open(&ws, doc);
    (ws, view)
}

#[test]
fn document_tabs_do_not_reserve_a_status_bar() {
    let (ws, view) = document();
    assert_eq!(view.editor.view.status_h, 0.0);
    assert_eq!(view.owns_the_window(&ws).editor.view.status_h, 0.0);
}

#[test]
fn the_first_paragraph_teaches_dictation_without_hard_coding_its_chord() {
    let chord = koru_keys::registry::label_of(koru_keys::ShortcutId::Dictate);
    assert_eq!(
        crate::view::body_placeholder(true, Some(chord.clone())),
        format!("Start typing, or press {chord} to dictate…")
    );
    assert_eq!(
        crate::view::body_placeholder(false, Some(chord)),
        "Start typing…"
    );
    assert_eq!(crate::view::body_placeholder(true, None), "Start typing…");
}

#[test]
fn ctrl_f_opens_local_search_and_an_outside_press_dismisses_then_acts() {
    let (mut ws, mut view) = document();
    assert!(view.key(&mut ws, &ch("f"), Some("f"), koru_keys::PRIMARY,));
    assert!(view.search.is_open());
    assert!(!view.editor.view.active, "the query owns keyboard focus");

    // Well below the top-right panel: the press closes search and continues
    // into the editor, which restores an active document caret.
    assert!(view.mouse_down(&mut ws, 300.0, 300.0, false));
    assert!(!view.search.is_open());
    assert!(view.editor.view.active);
}

#[test]
fn standalone_search_highlights_and_selects_the_caption_title() {
    let (mut ws, view) = document();
    let mut view = view.owns_the_window(&ws);

    assert!(view.key(&mut ws, &ch("f"), Some("f"), koru_keys::PRIMARY,));
    for letter in ["s", "p", "e", "c"] {
        assert!(view.key(&mut ws, &ch(letter), Some(letter), ModifiersState::empty()));
    }

    let root = view.editor.body.root.expect("a document title");
    assert_eq!(view.editor.view.search_highlights.len(), 1);
    assert_eq!(view.editor.view.search_highlights[0].node, root);
    assert_eq!(view.editor.view.search_highlights[0].range, 0..4);

    assert!(view.key(
        &mut ws,
        &Key::Named(NamedKey::Escape),
        None,
        ModifiersState::empty(),
    ));
    assert!(!view.search.is_open());
    let Overlay::Name(prompt) = &view.editor.view.overlay else {
        panic!("closing search should select a caption match in the rename field");
    };
    assert_eq!(prompt.input.selected_text(), Some("spec"));
}

/// The container draws the caption, the tabs and the window buttons; a document
/// inside one is a *tab*, which does not even reach the window's edge. A second
/// title bar there would be both wrong and unusable — and it is the default,
/// because forgetting to turn a thing off is how it ships.
#[test]
fn a_document_in_a_container_tab_draws_no_title_bar_of_its_own() {
    let (_ws, view) = document();
    assert_eq!(
        view.chrome_h(),
        0.0,
        "a tab opens straight onto the page: no caption, and no toolbar either"
    );
    assert_eq!(view.window_chrome(), None);
    assert_eq!(
        view.caption_hit(theme::MARGIN_X, 4.0),
        None,
        "the top of a tab is the document, not a caption"
    );
    assert_eq!(
        view.editor.view.status_h, 0.0,
        "a document gives the former status-bar height back to its body"
    );
}

/// On its own it is the window, so it draws the title bar the shell does not.
#[test]
fn a_document_that_owns_its_window_draws_one() {
    let (ws, view) = document();
    let view = view.owns_the_window(&ws);
    assert_eq!(view.chrome_h(), theme::CAPTION_H);
    assert_eq!(
        view.window_chrome(),
        Some(draw::WindowChrome {
            maximized: false,
            pinned: false,
            menu_morph: 0.0,
            menu_open: false,
            title: "spec.md".to_string(),
            search: None,
            offline: None,
        })
    );
    assert_eq!(
        view.caption_hit(theme::MARGIN_X, 4.0),
        Some(CaptionHit::Caption)
    );
}

/// **The window somebody put a document into is not the window that gets to
/// stay quiet about the store.** It draws its own caption instead of the
/// container's, so for a while it was the only one with no Offline chip.
#[test]
fn a_popped_out_document_says_when_the_store_is_away() {
    let (ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    View::set_viewport(&mut view, 1000.0, 700.0);
    assert!(view.offline_chip().is_none(), "nothing to say while fine");

    assert!(View::set_store_trouble(&mut view, Some(offline(2))));
    let chip = view.offline_chip().expect("a chip while offline");

    // The wording comes from `Unconfirmed`, so this window and the container
    // cannot describe one state two ways.
    let chrome = view.window_chrome().expect("chrome");
    let (detail, open) = chrome.offline.expect("something to say");
    assert!(detail.contains("2 documents have changes"), "{detail}");
    assert!(!open);

    // Pressing it opens the detail, and pressing it again puts it away.
    let (cx, cy) = chip.centre();
    let mut ws = ws;
    view.mouse_down(&mut ws, cx, cy, false);
    assert!(view.window_chrome().expect("chrome").offline.expect("q").1);
    view.mouse_down(&mut ws, cx, cy, false);
    assert!(!view.window_chrome().expect("chrome").offline.expect("q").1);

    // ...and so does a press anywhere else, which is the half that is easy to
    // leave out — a popover that outlives a click in the page is one somebody
    // has to hunt for.
    view.mouse_down(&mut ws, cx, cy, false);
    assert!(view.window_chrome().expect("chrome").offline.expect("q").1);
    view.mouse_down(&mut ws, 400.0, 400.0, false);
    assert!(!view.window_chrome().expect("chrome").offline.expect("q").1);

    // And it goes away entirely when the store comes back.
    assert!(View::set_store_trouble(&mut view, None));
    assert!(view.offline_chip().is_none());
}

/// Trouble with `n` documents unconfirmed and all of it safely buffered.
fn offline(documents: usize) -> koru_workspace::StoreTrouble {
    store_trouble(documents, false, false, false)
}

/// Trouble in a given shape, with a fixed diagnostic so the wording is the only
/// thing that varies.
fn store_trouble(
    documents: usize,
    document_list: bool,
    not_buffered: bool,
    fatal: bool,
) -> koru_workspace::StoreTrouble {
    koru_workspace::StoreTrouble {
        message: "the store went away".to_owned(),
        owed: 0,
        since: Some(std::time::Instant::now()),
        fatal,
        unconfirmed: koru_workspace::Unconfirmed {
            documents,
            document_list,
            not_buffered: not_buffered.then(|| "the outbox is full".to_owned()),
        },
    }
}

/// **Both captions have to say the same thing about one store.**
///
/// This window's caption is not the container's, which is how it came to be the
/// one window in Koru that never mentioned the store being away. The words are
/// `koru_i18n::store`'s now and neither app owns a sentence, but each still
/// maps `StoreTrouble` into `koru_i18n::store::Trouble` by hand, and that map is
/// what can go wrong.
#[test]
fn the_two_captions_say_the_same_thing_about_the_store() {
    // **The same literal is asserted in `apps/koru/src/mainview/tests.rs`**,
    // in the test of the same name. Two literals rather than one constant,
    // because the two apps share no crate to put one in — and a golden that has
    // to be edited in two files is the alarm this test exists to be.
    assert_eq!(
        caption::store_detail(koru_i18n::Lang::En, &store_trouble(2, true, true, false)),
        "2 documents have changes, and so does this workspace's list of \
         documents, and they could not be saved on this computer either. Koru \
         is retrying automatically. Last error: the store went away."
    );

    // ...and the map under it is field for field, in all eight languages and
    // for every shape. A swapped `document_list`/`not_buffered` still renders a
    // real sentence, so a spot check in English would not find it.
    for lang in koru_i18n::Lang::ALL {
        for documents in [0, 1, 5] {
            for document_list in [false, true] {
                for not_buffered in [false, true] {
                    for fatal in [false, true] {
                        let trouble = store_trouble(documents, document_list, not_buffered, fatal);
                        assert_eq!(
                            caption::store_detail(lang, &trouble),
                            koru_i18n::store::popover_detail(
                                lang,
                                koru_i18n::store::Trouble {
                                    documents,
                                    document_list,
                                    not_buffered,
                                    fatal,
                                    message: &trouble.message,
                                }
                            ),
                            "{} disagreed at {documents}/{document_list}/\
                             {not_buffered}/{fatal}",
                            lang.tag()
                        );
                    }
                }
            }
        }
    }
}

/// A document *inside a tab* must not draw a second chip: the container's
/// caption above it already has one, and the same fact said twice in one window
/// reads as two problems.
#[test]
fn a_document_in_a_tab_draws_no_chip_of_its_own() {
    let (ws, mut view) = document();
    View::set_viewport(&mut view, 1000.0, 700.0);
    assert!(View::set_store_trouble(&mut view, Some(offline(1))));
    assert!(view.offline_chip().is_none());
    assert!(view.window_chrome().is_none());
    let _ = ws;
}

/// The rows move down with the chrome. `chrome_h` reaches `ViewState` at
/// construction, and `Editing::top_inset` reads it every reflow — so a title bar
/// that was not accounted for would draw over the first line.
#[test]
fn the_rows_start_below_whatever_chrome_there_is() {
    let (_ws, tab) = document();
    let (ws2, window) = document();
    let window = window.owns_the_window(&ws2);
    assert_eq!(
        window.view().chrome_h - tab.view().chrome_h,
        theme::CAPTION_H
    );
}

/// Every window button asks for its own thing, and the bare row drags. A close
/// button that minimised would be found immediately; a drag that closed would
/// not be found until someone lost their work to it.
///
/// Overflow is not in the list: it opens a menu rather than raising a request,
/// and pin is a row inside that menu now rather than a button of its own
/// ([ADR-150](../../../docs/decisions/README.md)). What it does is
/// `pressing_the_menu_button_again_closes_the_window`, below.
#[test]
fn each_part_of_the_title_bar_asks_for_its_own_window_action() {
    let (mut ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    let w = view.editor.view.viewport.0;
    let cap = caption::strip(w);

    let expected = [
        (CaptionAction::Minimise, WindowRequest::Minimise),
        (CaptionAction::ToggleMaximise, WindowRequest::ToggleMaximise),
        (CaptionAction::Close, WindowRequest::Close),
    ];
    for (which, request) in expected {
        let r = koru_ui::caption::controls(cap)
            .into_iter()
            .find(|control| control.action == which)
            .map(|control| control.rect)
            .expect("a rect");
        assert!(view.mouse_down(&mut ws, r.x + r.w * 0.5, r.y + r.h * 0.5, false));
        assert_eq!(view.window_request.take(), Some(request), "{which:?}");
    }

    assert!(view.mouse_down(&mut ws, theme::MARGIN_X, cap.h * 0.5, false));
    assert_eq!(view.window_request.take(), Some(WindowRequest::Drag));
}

/// With the menu on, the same four actions are reachable — as *rows* now, and
/// each still raising the request its button did. This is the test that says the
/// menu and the buttons are one feature rather than two.
#[test]
fn every_window_menu_row_asks_for_what_its_button_did() {
    // Driven off the row list itself, so a row added in one place and not
    // the other fails here rather than going quietly untested.
    let expected: Vec<WindowRequest> = window_menu::rows(false, false)
        .into_iter()
        .map(|(_, c)| match c.expect("every row acts") {
            window_menu::Choice::TogglePin => WindowRequest::TogglePin,
            window_menu::Choice::Minimise => WindowRequest::Minimise,
            window_menu::Choice::ToggleMaximise => WindowRequest::ToggleMaximise,
            window_menu::Choice::SnapLeft => {
                WindowRequest::Place(snap::Placement::Half(snap::Side::Left))
            }
            window_menu::Choice::SnapRight => {
                WindowRequest::Place(snap::Placement::Half(snap::Side::Right))
            }
            window_menu::Choice::Close => WindowRequest::Close,
        })
        .collect();
    for (i, request) in expected.into_iter().enumerate() {
        let (mut ws, view) = document();
        let mut view = view.owns_the_window(&ws);
        let btn = menu_button(&view);
        assert!(view.mouse_down(&mut ws, btn.x + btn.w * 0.5, btn.y + btn.h * 0.5, false));
        assert!(view.window_menu_open(), "the press opened the menu");
        assert_eq!(view.window_request.take(), None, "opening asks for nothing");

        let vp = view.editor.view.viewport;
        let rows = view.menu_shape();
        let r = view
            .menu
            .expect("open")
            .menu()
            .row_rect(&rows, vp, i)
            .centre();
        assert!(view.mouse_down(&mut ws, r.0, r.1, false));
        assert_eq!(view.window_request.take(), Some(request), "row {i}");
    }
}

/// The ✕ the ⋮ button becomes closes the window, which is what an ✕ in that
/// corner means everywhere — and what its red hover promises. A press there must
/// not be eaten as "dismiss the menu".
#[test]
fn pressing_the_menu_button_again_closes_the_window() {
    let (mut ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    let btn = menu_button(&view);
    let (cx, cy) = (btn.x + btn.w * 0.5, btn.y + btn.h * 0.5);

    assert!(view.mouse_down(&mut ws, cx, cy, false));
    assert!(view.window_menu_open());

    assert!(view.mouse_down(&mut ws, cx, cy, false));
    assert!(!view.window_menu_open(), "and the menu went with it");
    assert_eq!(view.window_request.take(), Some(WindowRequest::Close));
}

/// Hovering opens it, moving away closes it, and the walk down into the panel
/// keeps it up — the same three rules the container follows.
#[test]
fn the_menu_button_opens_on_hover_and_closes_when_the_pointer_leaves() {
    let (ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    let btn = menu_button(&view);
    let (cx, cy) = (btn.x + btn.w * 0.5, btn.y + btn.h * 0.5);

    assert!(view.mouse_move(cx, cy), "the move earns a frame");
    assert!(view.window_menu_open());

    // Into the panel: still open, or no row could ever be reached.
    let vp = view.editor.view.viewport;
    let rows = view.menu_shape();
    for i in 0..rows.len() {
        let r = view
            .menu
            .expect("open")
            .menu()
            .row_rect(&rows, vp, i)
            .centre();
        view.mouse_move(r.0, r.1);
        assert!(
            view.window_menu_open(),
            "row {i} closed the menu under itself"
        );
    }

    // Away from both: it goes.
    assert!(view.mouse_move(vp.0 * 0.4, vp.1 * 0.8));
    assert!(!view.window_menu_open());
}

/// Close owns the corner and overflow takes the slot the pin had.
///
/// This asserted the opposite while the experimental one-button caption was on
/// by default: the ⋮ inherited the corner because it was the only button
/// there. With four stable controls the throw-into-the-corner target is close
/// again ([ADR-150](../../../docs/decisions/README.md)), which is what it means
/// everywhere else on the desktop.
#[test]
fn close_owns_the_corner_and_overflow_takes_the_pin_slot() {
    let (ws, view) = document();
    let view = view.owns_the_window(&ws);
    let w = view.editor.view.viewport.0;
    assert_eq!(
        caption::hit(w - 1.0, 1.0, w),
        Some(CaptionHit::Control(CaptionAction::Close))
    );
    let menu = menu_button(&view);
    assert_eq!(menu.x + menu.w, w - theme::CAPTION_BTN * 3.0);
}

/// A double click on the bare row maximises, the way a title bar does — and it
/// must not reach the rename prompt, which is what a double click means in the
/// chrome one row lower.
#[test]
fn double_clicking_the_title_bar_maximises() {
    let (ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    assert!(view.mouse_double_click(&ws, theme::MARGIN_X, theme::CAPTION_H * 0.5));
    assert_eq!(
        view.window_request.take(),
        Some(WindowRequest::ToggleMaximise)
    );
}

/// A mixed text/image paste (a Word copy) lands its paragraphs and pictures in
/// order as a flat body, and URLs in the text blocks become link pills — the
/// same rule an ordinary text paste follows. The image hash is a stand-in: a
/// node only stores it, so the bytes need not exist.
#[test]
fn rich_paste_interleaves_text_and_images_in_order() {
    fn img(hash: &str) -> PastedImage {
        PastedImage {
            hash: hash.into(),
            mime: "image/png".into(),
            width: Some(1),
            height: Some(1),
            name: None,
        }
    }
    let (mut ws, mut view) = document();
    let blocks = vec![
        RichBlock::Text(RichTextBlock::from_text("hello")),
        RichBlock::Image(img("aaaa")),
        RichBlock::Text(RichTextBlock::from_text("see https://example.com here")),
        RichBlock::Image(img("bbbb")),
    ];
    View::paste_rich(&mut view, &mut ws, None, &blocks);

    // The last `blocks.len()` rows are the paste, whatever the body opened with.
    let rows = &view.editor.body.rows;
    let tail: Vec<_> = rows.iter().rev().take(blocks.len()).rev().collect();
    assert_eq!(tail.len(), blocks.len());
    assert_eq!(tail[0].kind, Kind::Paragraph);
    assert_eq!(tail[0].text, "hello");
    assert_eq!(tail[1].kind, Kind::Image);
    assert_eq!(tail[2].kind, Kind::Paragraph);
    assert!(
        !tail[2].text.contains("example"),
        "the URL was replaced by a link pill: {:?}",
        tail[2].text
    );
    assert_eq!(tail[3].kind, Kind::Image);

    // The link pill is a URL attribute on the paragraph node.
    let linked = ws.read(tail[2].id).expect("row exists");
    assert!(
        linked
            .content
            .attrs()
            .iter()
            .any(|a| matches!(a.value, AttrValue::Url(_))),
        "the pasted URL became a link"
    );
}

/// Rich clipboard data used to lose the block selection when it crossed the
/// editor/shell boundary, so the decoded blocks were inserted beside the old
/// ones. They replace the captured roots at the first selected slot.
#[test]
fn rich_paste_replaces_selected_blocks() {
    let (mut ws, mut view) = document();
    let first = view.editor.body.first_body_row();
    let keep = view.editor.body.rows[first].id;
    let parent = ws.parent(keep).unwrap();
    ws.set_text(keep, "keep").unwrap();
    let one = ws.insert(parent, Node::paragraph("one").unwrap()).unwrap();
    let two = ws.insert(parent, Node::paragraph("two").unwrap()).unwrap();
    let tail = ws.insert(parent, Node::paragraph("tail").unwrap()).unwrap();
    view.editor.body.rebuild(&ws);
    ws.enable_undo();

    View::paste_rich(
        &mut view,
        &mut ws,
        Some(PasteTarget {
            anchor: one,
            replacing: vec![one, two],
        }),
        &[RichBlock::Text(RichTextBlock::from_text("replacement"))],
    );

    let rows: Vec<_> = view
        .editor
        .body
        .rows
        .iter()
        .skip(view.editor.body.first_body_row())
        .map(|row| (row.id, row.text.as_str()))
        .collect();
    assert_eq!(
        rows,
        vec![(keep, "keep"), (rows[1].0, "replacement"), (tail, "tail")]
    );
    let pasted = rows[1].0;
    assert!(ws.read(one).is_err());
    assert!(ws.read(two).is_err());

    assert!(
        view.key(&mut ws, &ch("z"), Some("z"), koru_keys::PRIMARY,),
        "replacement is one undo step"
    );
    let restored: Vec<_> = view
        .editor
        .body
        .rows
        .iter()
        .skip(view.editor.body.first_body_row())
        .map(|row| (row.id, row.text.as_str()))
        .collect();
    assert_eq!(
        restored,
        vec![(keep, "keep"), (one, "one"), (two, "two"), (tail, "tail")]
    );

    assert!(view.key(&mut ws, &ch("y"), Some("y"), koru_keys::PRIMARY,));
    let redone: Vec<_> = view
        .editor
        .body
        .rows
        .iter()
        .skip(view.editor.body.first_body_row())
        .map(|row| (row.id, row.text.as_str()))
        .collect();
    assert_eq!(
        redone,
        vec![(keep, "keep"), (pasted, "replacement"), (tail, "tail")]
    );
}

/// Image-only clipboard acquisition is asynchronous, but it follows the same
/// replacement contract and must not leave the selected text blocks behind.
#[test]
fn image_paste_replaces_selected_blocks() {
    let (mut ws, mut view) = document();
    let first = view.editor.body.first_body_row();
    let one = view.editor.body.rows[first].id;
    let parent = ws.parent(one).unwrap();
    ws.set_text(one, "one").unwrap();
    let two = ws.insert(parent, Node::paragraph("two").unwrap()).unwrap();
    let tail = ws.insert(parent, Node::paragraph("tail").unwrap()).unwrap();
    view.editor.body.rebuild(&ws);
    ws.enable_undo();

    View::paste_images(
        &mut view,
        &mut ws,
        Some(PasteTarget {
            anchor: one,
            replacing: vec![one, two],
        }),
        &[PastedImage {
            hash: "aaaa".into(),
            mime: "image/png".into(),
            width: Some(1),
            height: Some(1),
            name: None,
        }],
    );

    let rows = &view.editor.body.rows[view.editor.body.first_body_row()..];
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].kind, Kind::Image);
    let pasted = rows[0].id;
    assert_eq!(rows[1].id, tail);
    assert!(ws.read(one).is_err());
    assert!(ws.read(two).is_err());

    assert!(view.key(&mut ws, &ch("z"), Some("z"), koru_keys::PRIMARY,));
    let restored: Vec<_> = view
        .editor
        .body
        .rows
        .iter()
        .skip(view.editor.body.first_body_row())
        .map(|row| (row.id, row.text.as_str()))
        .collect();
    assert_eq!(restored, vec![(one, "one"), (two, "two"), (tail, "tail")]);

    assert!(view.key(&mut ws, &ch("y"), Some("y"), koru_keys::PRIMARY,));
    let redone = &view.editor.body.rows[view.editor.body.first_body_row()..];
    assert_eq!(redone.len(), 2);
    assert_eq!(redone[0].id, pasted);
    assert_eq!(redone[0].kind, Kind::Image);
    assert_eq!(redone[1].id, tail);
}

/// Structured HTML (ChatGPT headings, lists, or embedded images) takes the
/// rich-paste path rather than `paste_plain_text`. The insertion point belongs
/// after the payload just as it does for plain multi-paragraph text: focusing
/// the final row without placing its caret leaves it at byte zero.
#[test]
fn rich_paste_places_the_caret_at_the_end_of_the_last_paragraph() {
    let (mut ws, mut view) = document();
    let blocks = vec![
        RichBlock::Text(RichTextBlock {
            text: "A heading".into(),
            list: None,
            heading: Some(Heading::H2),
            kind: RichTextKind::Paragraph,
            runs: Vec::new(),
            links: Vec::new(),
        }),
        RichBlock::Text(RichTextBlock::from_text("First paragraph")),
        RichBlock::Text(RichTextBlock::from_text("Last paragraph")),
    ];

    View::paste_rich(&mut view, &mut ws, None, &blocks);

    let focused = view.editor.body.focused().expect("the last row is focused");
    assert_eq!(focused.text, "Last paragraph");
    assert_eq!(focused.caret(), focused.text.len());
}

/// The shell already identifies `<ol>/<ul>` items and their nesting. The
/// document must consume that metadata: dropping it here was why a numbered
/// ChatGPT list arrived as a run of ordinary paragraphs.
#[test]
fn rich_paste_rebuilds_numbered_and_nested_bullet_lists() {
    let (mut ws, mut view) = document();
    let list = |text: &str, style, depth| {
        RichBlock::Text(RichTextBlock {
            text: text.into(),
            list: Some(RichListSpec { style, depth }),
            heading: None,
            kind: RichTextKind::Paragraph,
            runs: Vec::new(),
            links: Vec::new(),
        })
    };
    let blocks = vec![
        list("one", koru_core::ListStyle::Numbered, 0),
        list("nested", koru_core::ListStyle::Bullet, 1),
        list("two", koru_core::ListStyle::Numbered, 0),
        RichBlock::Text(RichTextBlock::from_text("after")),
    ];

    View::paste_rich(&mut view, &mut ws, None, &blocks);

    let rows: Vec<_> = view
        .editor
        .body
        .rows
        .iter()
        .filter(|row| !row.is_title)
        .map(|row| (row.text.as_str(), row.list_style, row.depth, row.list_index))
        .collect();
    assert_eq!(
        rows,
        vec![
            ("", None, 0, 0),
            ("one", Some(koru_core::ListStyle::Numbered), 1, 1),
            ("nested", Some(koru_core::ListStyle::Bullet), 2, 1),
            ("two", Some(koru_core::ListStyle::Numbered), 1, 2),
            ("after", None, 0, 0),
        ]
    );
    assert_eq!(
        view.editor.body.focused().map(|row| row.text.as_str()),
        Some("after"),
        "the caret lands on the final visible block, not a hidden list container"
    );
}

#[test]
fn rich_paste_preserves_a_semantic_blockquote() {
    let (mut ws, mut view) = document();
    let blocks = vec![RichBlock::Text(RichTextBlock {
        text: "Efficiency is a means, not the objective.".into(),
        list: None,
        heading: None,
        kind: RichTextKind::Quote,
        runs: Vec::new(),
        links: Vec::new(),
    })];

    View::paste_rich(&mut view, &mut ws, None, &blocks);

    let quoted = view
        .editor
        .body
        .rows
        .iter()
        .find(|row| row.text == "Efficiency is a means, not the objective.")
        .expect("a pasted quote row");
    assert_eq!(quoted.kind, Kind::Quote);
}

#[test]
fn rich_paste_preserves_a_markdown_table_as_one_table_node() {
    let (mut ws, mut view) = document();
    let source = "| **App** | Notes |\n| --- | ---: |\n| Koru | `fast` |";
    let blocks = vec![RichBlock::Table(RichTableBlock {
        source: source.into(),
    })];

    View::paste_rich(&mut view, &mut ws, None, &blocks);

    let table = view
        .editor
        .body
        .rows
        .iter()
        .find(|row| row.kind == Kind::Table)
        .expect("one table row");
    assert_eq!(table.text, source);
    assert!(
        table
            .table
            .as_ref()
            .is_some_and(|visual| visual.grid.is_some())
    );
    let visual = table.table.as_ref().unwrap();
    let grid = visual.grid.as_ref().unwrap();
    assert!(
        (table.height - grid.height).abs() < 0.01,
        "grid mode reserves the rendered grid height, not the hidden Markdown layout"
    );
    assert_eq!(view.editor.view.table_source, None);
    assert!(
        (grid.height - grid.row_heights.iter().sum::<f32>()).abs() < 0.01,
        "floating controls reserve no strip above the table"
    );
}

/// The controls **claim** the pointer once revealed — they take it off the
/// text — and they do not claim it during the deliberate-hover wait, while they
/// are invisible. That is the whole assertion now that Koru does not use the
/// hand cursor: what separates a live control from the words behind it is
/// I-beam versus plain arrow, not arrow versus hand.
#[test]
fn table_controls_claim_the_pointer_and_switch_between_grid_and_source() {
    let (mut ws, mut view) = document();
    let blocks = vec![RichBlock::Table(RichTableBlock {
        source: "| App | Notes |\n| --- | --- |\n| Koru | fast |".into(),
    })];
    View::paste_rich(&mut view, &mut ws, None, &blocks);

    let (id, layout_point, mode_point) = {
        let row = view
            .editor
            .body
            .rows
            .iter()
            .find(|row| row.kind == Kind::Table)
            .unwrap();
        let visual = row.table.as_ref().unwrap();
        let grid = visual.grid.as_ref().unwrap();
        let layout = koru_editor::table::control_rect(row, row.y);
        let mode = koru_editor::table::mode_control_rect(row, visual, row.y);
        assert!(
            layout.x < mode.x,
            "the width control precedes the mode control"
        );
        assert!(
            (mode.x + mode.w - (row.text_x() + grid.viewport_width)).abs() < 0.01,
            "the controls are anchored to the table's top-right edge"
        );
        (
            row.id,
            (layout.x + layout.w / 2.0, layout.y + layout.h / 2.0),
            (mode.x + mode.w / 2.0, mode.y + mode.h / 2.0),
        )
    };
    let content_x = view.editor.view.content_x;
    let scroll = view.editor.view.scroll;

    view.mouse_move(content_x + layout_point.0, layout_point.1 - scroll);
    assert_eq!(
        View::cursor_icon(&view),
        CursorIcon::Text,
        "the invisible control is not clickable during the deliberate-hover wait"
    );
    let reveal_at = View::maintenance_deadline(&view).expect("one delayed reveal wake");
    assert!(View::maintain(&mut view, &mut ws, reveal_at));
    assert!(View::animating(&view), "the controls are fading in");
    assert!(View::maintain(
        &mut view,
        &mut ws,
        reveal_at + std::time::Duration::from_millis(100)
    ));
    view.mouse_move(content_x + layout_point.0, layout_point.1 - scroll);
    assert_eq!(View::cursor_icon(&view), koru_ui::cursor::PRESS);
    view.mouse_move(content_x + mode_point.0, mode_point.1 - scroll);
    assert_eq!(View::cursor_icon(&view), koru_ui::cursor::PRESS);
    assert!(view.mouse_down(
        &mut ws,
        content_x + mode_point.0,
        mode_point.1 - scroll,
        false
    ));
    assert_eq!(view.editor.view.table_source, Some(id));

    let source_point = {
        let row = view
            .editor
            .body
            .rows
            .iter()
            .find(|row| row.id == id)
            .unwrap();
        let visual = row.table.as_ref().unwrap();
        let mode = koru_editor::table::mode_control_rect(row, visual, row.y);
        (mode.x + mode.w / 2.0, mode.y + mode.h / 2.0)
    };
    let source_scroll = view.editor.view.scroll;
    assert!(view.mouse_down(
        &mut ws,
        content_x + source_point.0,
        source_point.1 - source_scroll,
        false
    ));
    assert_eq!(view.editor.view.table_source, None);

    view.mouse_move(0.0, 0.0);
    assert!(
        View::animating(&view),
        "leaving starts the standard fade-out"
    );
    assert!(View::maintain(
        &mut view,
        &mut ws,
        std::time::Instant::now() + std::time::Duration::from_millis(75)
    ));
    assert_eq!(
        view.editor.view.table_controls_node, None,
        "the faded controls stop asking for frames"
    );
}

#[test]
fn right_click_anywhere_on_a_table_block_opens_the_table_menu() {
    let (mut ws, mut view) = document();
    View::set_viewport(&mut view, 1000.0, 700.0);
    View::paste_rich(
        &mut view,
        &mut ws,
        None,
        &[RichBlock::Table(RichTableBlock {
            source: "| hello | world |\n| --- | --- |\n| this | is cool |".into(),
        })],
    );
    let id = view
        .editor
        .body
        .rows
        .iter()
        .find(|row| row.kind == Kind::Table)
        .unwrap()
        .id;
    ws.set_table_layout(id, koru_core::TableLayout::Extended)
        .unwrap();
    view.editor.body.refresh(&ws, id);
    view.editor.reflow();

    let (x, y) = {
        let row = view
            .editor
            .body
            .rows
            .iter()
            .find(|row| row.id == id)
            .unwrap();
        let visual = row.table.as_ref().unwrap();
        let grid = visual.grid.as_ref().unwrap();
        assert!(grid.width < grid.viewport_width);
        let blank_x = row.text_x() + grid.width + (grid.viewport_width - grid.width) * 0.5;
        let body_y = row.y + grid.row_heights[0] + grid.row_heights[1] * 0.5;
        (
            view.editor.view.content_x + blank_x,
            body_y - view.editor.view.scroll,
        )
    };

    assert!(View::context_menu(&mut view, &mut ws, x, y));
    assert!(view.menu_shape().iter().any(|row| matches!(
        row,
        menu::Row::Item { label, .. } if label.as_ref() == "Edit Markdown"
    )));
    assert!(
        view.editor
            .view
            .table_cell
            .is_some_and(|(table, row, column)| table == id && row == 1 && column == 1),
        "the blank area targets the nearest cell in the clicked table row"
    );
}

#[test]
fn invalid_pasted_table_stays_in_source_mode_with_an_error() {
    let (mut ws, mut view) = document();
    let source = "| App | Notes |\n| --- | broken |\n| Koru | fast |";
    let blocks = vec![RichBlock::Table(RichTableBlock {
        source: source.into(),
    })];

    View::paste_rich(&mut view, &mut ws, None, &blocks);

    let table = view
        .editor
        .body
        .rows
        .iter()
        .find(|row| row.kind == Kind::Table)
        .expect("one table row");
    assert_eq!(table.text, source, "invalid syntax is never discarded");
    assert_eq!(view.editor.view.table_source, Some(table.id));
    assert!(
        view.editor
            .view
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("Table line 2"))
    );
}

#[test]
fn rich_paste_applies_bold_and_inline_code_inside_a_numbered_item() {
    let (mut ws, mut view) = document();
    let text = "Identify canonical documents. For example: architecture/README.md";
    let bold_end = "Identify canonical documents.".len();
    let code_start = text.find("architecture/README.md").unwrap();
    let blocks = vec![RichBlock::Text(RichTextBlock {
        text: text.into(),
        list: Some(RichListSpec {
            style: koru_core::ListStyle::Numbered,
            depth: 0,
        }),
        heading: None,
        kind: RichTextKind::Paragraph,
        runs: vec![
            RichTextRun {
                start: 0,
                end: bold_end,
                marks: MarkSet {
                    bold: true,
                    ..Default::default()
                },
            },
            RichTextRun {
                start: code_start,
                end: text.len(),
                marks: MarkSet {
                    code: true,
                    ..Default::default()
                },
            },
        ],
        links: Vec::new(),
    })];

    View::paste_rich(&mut view, &mut ws, None, &blocks);

    let row = view
        .editor
        .body
        .rows
        .iter()
        .find(|row| row.text == text)
        .expect("the numbered item was pasted");
    let runs = ws.runs(row.id).unwrap();
    assert!(
        runs.iter()
            .any(|run| { run.text == "Identify canonical documents." && run.bold })
    );
    assert!(
        runs.iter()
            .any(|run| run.text == "architecture/README.md" && run.code)
    );
}

#[test]
fn rich_paste_builds_labelled_links_and_language_tagged_code_blocks() {
    let (mut ws, mut view) = document();
    let blocks = vec![
        RichBlock::Text(RichTextBlock {
            text: "Read the review.".into(),
            list: None,
            heading: None,
            kind: RichTextKind::Paragraph,
            runs: Vec::new(),
            links: vec![RichTextLink {
                start: 5,
                end: 15,
                url: "https://example.com/review".into(),
            }],
        }),
        RichBlock::Text(RichTextBlock {
            text: "fn main() {}".into(),
            list: None,
            heading: None,
            kind: RichTextKind::Code {
                language: Some("rust".into()),
            },
            runs: Vec::new(),
            links: Vec::new(),
        }),
    ];

    View::paste_rich(&mut view, &mut ws, None, &blocks);

    let linked = view
        .editor
        .body
        .rows
        .iter()
        .find_map(|row| {
            let node = ws.read(row.id).ok()?;
            node.content.attrs().iter().find_map(|attr| {
                matches!(&attr.value, AttrValue::Url(url) if url == "https://example.com/review")
                    .then(|| (attr.label.clone(), row.kind))
            })
        })
        .expect("a labelled link pill");
    assert_eq!(linked.0.as_deref(), Some("the review"));
    assert_eq!(linked.1, Kind::Paragraph);

    let code = view
        .editor
        .body
        .rows
        .iter()
        .find(|row| row.kind == Kind::Code)
        .expect("a code block");
    assert_eq!(code.text, "fn main() {}");
    assert_eq!(
        ws.read(code.id).unwrap().meta.language.as_deref(),
        Some("rust")
    );
}

/// A right-click inside selected text keeps that range intact, opens the edit
/// menu with the existing formatting bubble above it, and routes a toolbar
/// press through the same command action as its keyboard shortcut.
#[test]
fn document_context_toolbar_preserves_and_formats_the_selection() {
    let (mut ws, mut view) = document();
    View::set_viewport(&mut view, 900.0, 700.0);
    let i = view.editor.body.first_body_row();
    let id = view.editor.body.rows[i].id;
    ws.set_text(id, "hello world").unwrap();
    view.editor.body.refresh(&ws, id);
    view.editor.body.focus = i;
    view.editor.place_selection_pair((0, 5));
    view.editor.reflow();

    let selected = Editing::selection_rects(&view.editor)
        .into_iter()
        .next()
        .expect("the selected word has geometry");
    let (x, y) = selected.centre();
    assert!(View::context_menu(&mut view, &mut ws, x, y));
    assert!(matches!(view.menu, Some(OpenMenu::Context(_))));
    assert_eq!(view.editor.body.focused().unwrap().range(), 0..5);

    let toolbar = view
        .context_toolbar_panel()
        .expect("formatting toolbar accompanies the context menu");
    let bold = bubble::button_rect(toolbar, bubble::Button::Cmd(docview::Command::Bold))
        .expect("bold is in the shared toolbar");
    assert!(view.mouse_down(&mut ws, bold.centre().0, bold.centre().1, false));

    assert!(
        view.menu.is_none(),
        "a direct toolbar command dismisses the menu"
    );
    assert!(
        ws.marks_in(id, 0..5).unwrap_or_default().has(Mark::Bold),
        "the preserved selection was formatted"
    );
}

fn document_with_pill() -> (WorkspaceDoc, BuiltInDocument, koru_core::AttrId, NodeId) {
    use koru_core::node::Node;
    use koru_core::schema::Parent;
    use koru_core::{Attr, AttrValue, Ref};

    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "spec.md").expect("a document");
    let target = koru_workspace::document(&mut ws, "Brief").expect("target");
    let para = ws
        .insert(Parent::Node(doc), Node::paragraph("see ").unwrap())
        .unwrap();
    let attr = ws
        .insert_pill(para, 4, &Attr::new("ref", AttrValue::Ref(Ref::new(target))))
        .unwrap();
    let mut view = BuiltInDocument::open(&ws, doc);
    View::set_viewport(&mut view, 900.0, 700.0);
    view.editor.reflow();
    (ws, view, attr, target)
}

fn pill_centre(view: &BuiltInDocument, attr: koru_core::AttrId) -> (f32, f32) {
    let row = view
        .editor
        .body
        .rows
        .iter()
        .position(|r| r.pills.contains_key(&attr))
        .expect("pill row");
    let r = &view.editor.body.rows[row];
    let ox = view.editor.view.content_x + r.text_x();
    let oy = r.y - view.editor.view.scroll;
    let mut point = None;
    koru_editor::rowlayout::visit_pills(
        r,
        &view.editor.view.engine.pills,
        |a, _, _, px, py, pw, ph| {
            if a == attr {
                point = Some((ox + px + pw * 0.5, oy + py + ph * 0.5));
            }
        },
    );
    point.expect("the pill has a rect")
}

#[test]
fn document_pill_context_menu_offers_the_three_open_destinations() {
    let (mut ws, mut view, attr, target) = document_with_pill();
    let (x, y) = pill_centre(&view, attr);
    assert!(View::context_menu(&mut view, &mut ws, x, y));
    let shape = view.menu_shape();
    assert!(matches!(&shape[0], menu::Row::Item { label, .. } if label.as_ref() == "Open"));
    assert!(matches!(
        &shape[1],
        menu::Row::Item { label, .. } if label.as_ref() == "Open in new tab"
    ));
    assert!(matches!(
        &shape[2],
        menu::Row::Item { label, .. } if label.as_ref() == "Open in new window"
    ));

    view.choose_menu(&mut ws, 0, None);
    assert_eq!(
        view.editor.view.wants_open,
        Some((target, OpenWhere::SameTab))
    );
}

#[test]
fn document_pill_context_menu_open_in_new_tab_asks_for_a_background_tab() {
    let (mut ws, mut view, attr, target) = document_with_pill();
    let (x, y) = pill_centre(&view, attr);
    assert!(View::context_menu(&mut view, &mut ws, x, y));
    view.choose_menu(&mut ws, 1, None);
    assert_eq!(
        view.editor.view.wants_open,
        Some((target, OpenWhere::BackgroundTab))
    );
}

#[test]
fn document_pill_context_menu_open_in_new_window_asks_for_a_new_window() {
    let (mut ws, mut view, attr, target) = document_with_pill();
    let (x, y) = pill_centre(&view, attr);
    assert!(View::context_menu(&mut view, &mut ws, x, y));
    view.choose_menu(&mut ws, 2, None);
    assert_eq!(
        view.editor.view.wants_open,
        Some((target, OpenWhere::NewWindow))
    );
}

#[test]
fn ctrl_enter_on_a_selected_document_pill_asks_for_a_new_tab() {
    let (mut ws, mut view, attr, target) = document_with_pill();
    let row = view
        .editor
        .body
        .rows
        .iter()
        .position(|r| r.pills.contains_key(&attr))
        .expect("pill row");
    view.editor.body.focus = row;
    view.editor.view.active = true;
    view.editor.view.selected_pill = Some((row, attr));
    let before = view.editor.body.rows.len();

    assert!(view.key(
        &mut ws,
        &Key::Named(NamedKey::Enter),
        None,
        koru_keys::PRIMARY,
    ));

    assert_eq!(
        view.editor.view.wants_open,
        Some((target, OpenWhere::BackgroundTab))
    );
    assert_eq!(
        view.editor.body.rows.len(),
        before,
        "Ctrl+Enter on the pill must not insert a paragraph"
    );
}

#[test]
fn ordinary_context_menu_does_not_offer_open_destinations() {
    let (mut ws, mut view) = document();
    View::set_viewport(&mut view, 900.0, 700.0);
    let i = view.editor.body.first_body_row();
    let y = view.editor.body.rows[i].y + 4.0;
    assert!(View::context_menu(&mut view, &mut ws, 400.0, y));
    assert!(!view.menu_shape().iter().any(|row| matches!(
        row,
        menu::Row::Item { label, .. }
            if matches!(label.as_ref(), "Open" | "Open in new tab" | "Open in new window")
    )));
}

#[test]
fn typing_dismisses_the_selection_bubble() {
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let row = view.editor.body.first_body_row();
    let id = view.editor.body.rows[row].id;
    ws.set_text(id, "hello").unwrap();
    view.editor.body.refresh(&ws, id);
    view.editor.body.focus = row;
    view.editor.place_selection_pair((0, 5));
    view.editor.reflow();
    view.place_bubble();
    assert!(
        view.bubble_panel().is_some(),
        "the selection opened a bubble"
    );

    assert!(view.key(&mut ws, &ch("x"), Some("x"), ModifiersState::empty()));
    assert!(view.bubble_panel().is_none(), "typing retires the bubble");
}

/// A submenu replaces the clipboard menu, not the toolbar that opened it. The
/// previous state tied the toolbar to `OpenMenu::Context`, so opening a
/// submenu changed the enum variant and made the toolbar disappear
/// immediately. Driven through the block-type button since the `⋯` left the
/// layout (its two commands do not work yet — see `bubble::LAYOUT`).
#[test]
fn context_submenu_keeps_the_right_click_toolbar_open() {
    let (mut ws, mut view) = document();
    View::set_viewport(&mut view, 900.0, 700.0);
    let i = view.editor.body.first_body_row();
    let y = view.editor.body.rows[i].y + 4.0;
    assert!(View::context_menu(&mut view, &mut ws, 400.0, y));

    let toolbar = view
        .context_toolbar_panel()
        .expect("right-click opened the toolbar");
    let block = bubble::button_rect(toolbar, bubble::Button::BlockType).expect("block-type button");
    assert!(view.mouse_down(&mut ws, block.centre().0, block.centre().1, false));

    assert!(matches!(view.menu, Some(OpenMenu::ContextBlock { .. })));
    assert_eq!(view.context_toolbar_panel(), Some(toolbar));
}

/// Ctrl+Enter is `ToggleDone`, an outline-scoped action the document view used
/// to filter out before `act` ran. On a code block it must leave the block —
/// so the document view routes it through rather than dropping it.
#[test]
fn ctrl_plus_enter_exits_a_code_block_via_the_document_key() {
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    // A body row, never the title: row 0 *is* the document node, and
    // relabelling that into a code block would outdent the whole body.
    view.editor.body.focus = view.editor.body.first_body_row();
    let id = view.editor.body.focused().unwrap().id;
    ws.set_kind(id, Kind::Code).unwrap();
    ws.set_text(id, "fn main() {}").unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.place_caret(4);
    view.editor.reflow();

    let handled = view.key(
        &mut ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        koru_keys::PRIMARY,
    );
    assert!(handled);
    assert_eq!(
        view.editor.body.rows.len(),
        view.editor.body.first_body_row() + 2,
        "Ctrl+Enter created a paragraph below the block"
    );
    assert_eq!(
        view.editor.body.rows[view.editor.body.focus - 1].id,
        id,
        "and it sits directly below it"
    );
    assert_eq!(
        view.editor.body.focused().unwrap().kind,
        Kind::Paragraph,
        "the caret left the code block"
    );
    assert_eq!(
        ws.read(id).unwrap().kind,
        Kind::Code,
        "the code block itself is untouched"
    );
}

/// **The document view lets the tick chord through to the editor.**
///
/// The routing half of Q-T2, and the half that could break without anything
/// else noticing: this view claims `Ctrl+Enter` per block kind, so a shifted
/// sibling arriving at the same `resolve` is exactly where a new chord gets
/// eaten. `Action::TickCheckbox` is `Scope::Shared`, so it has to fall past the
/// structural filter and reach `Editing::key`.
#[test]
fn ctrl_shift_enter_ticks_a_checklist_row_via_the_document_key() {
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let row = view.editor.body.first_body_row();
    let id = view.editor.body.rows[row].id;
    ws.wrap_in_list_block(id, koru_core::ListStyle::Checkbox)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(id);
    view.editor.reflow();
    assert!(
        view.editor.body.focused().unwrap().is_task(),
        "the row draws a box"
    );
    let rows_before = view.editor.body.rows.len();

    let handled = view.key(
        &mut ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        koru_keys::PRIMARY_SHIFT,
    );
    assert!(handled, "the view did not swallow it");
    assert!(view.editor.body.row(id).unwrap().done, "the box is ticked");
    assert_eq!(
        view.editor.body.rows.len(),
        rows_before,
        "and no paragraph was opened — that is the unshifted chord's job"
    );
}

#[test]
fn ctrl_plus_enter_opens_a_paragraph_after_a_table_via_the_document_key() {
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let row = view.editor.body.first_body_row();
    let table = view.editor.body.rows[row].id;
    ws.set_kind(table, Kind::Table).unwrap();
    ws.set_text(
        table,
        "| hello | world |\n| --- | --- |\n| this | is cool |",
    )
    .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(table);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Enter), None, koru_keys::PRIMARY,));
    assert_eq!(view.editor.body.focused().unwrap().kind, Kind::Paragraph);
    assert!(view.editor.body.focused().unwrap().text.is_empty());
    assert_eq!(view.editor.body.rows[view.editor.body.focus - 1].id, table);
    assert_eq!(ws.read(table).unwrap().kind, Kind::Table);
}

#[test]
fn ctrl_plus_enter_opens_a_paragraph_after_prose_without_splitting_it() {
    use koru_editor::Editing;
    let (mut ws, mut view) = laid_out();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let current = view.editor.body.rows[first].id;
    let following = view.editor.body.rows[first + 1].id;
    view.editor.body.focus = first;
    view.editor.place_caret(1);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Enter), None, koru_keys::PRIMARY,));
    assert_eq!(ws.read(current).unwrap().content.as_text(), Some("one"));
    let paragraph = view.editor.body.focused().expect("new paragraph");
    assert_eq!(paragraph.kind, Kind::Paragraph);
    assert!(paragraph.text.is_empty());
    assert_eq!(
        view.editor.body.rows[view.editor.body.focus - 1].id,
        current
    );
    assert_eq!(
        view.editor.body.rows[view.editor.body.focus + 1].id,
        following
    );
}

#[test]
fn alt_plus_enter_opens_a_paragraph_before_prose_without_splitting_it() {
    use koru_editor::Editing;
    let (mut ws, mut view) = laid_out();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let current = view.editor.body.rows[first].id;
    view.editor.body.focus = first;
    view.editor.place_caret(1);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Enter), None, ModifiersState::ALT,));
    assert_eq!(ws.read(current).unwrap().content.as_text(), Some("one"));
    let paragraph = view.editor.body.focused().expect("new paragraph");
    assert_eq!(paragraph.kind, Kind::Paragraph);
    assert!(paragraph.text.is_empty());
    assert_eq!(
        view.editor.body.rows[view.editor.body.focus + 1].id,
        current
    );
}

#[test]
fn enter_on_a_focused_rendered_table_opens_prose_without_splitting_it() {
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let row = view.editor.body.first_body_row();
    let table = view.editor.body.rows[row].id;
    let source = "| hello | world |\n| --- | --- |\n| this | is cool |";
    ws.set_kind(table, Kind::Table).unwrap();
    ws.set_text(table, source).unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(table);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Enter), None, NONE));
    assert_eq!(view.editor.body.focused().unwrap().kind, Kind::Paragraph);
    assert_eq!(ws.read(table).unwrap().content.as_text(), Some(source));
}

/// Nested empty Enter / Shift+Tab must keep the row above following siblings
/// that were under the same parent — not jump past them (plain tree outdent).
#[test]
fn empty_enter_outdents_in_place_above_following_siblings() {
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let one = view.editor.body.rows[first].id;
    let koru_core::schema::Parent::Node(document) = ws.parent(one).unwrap() else {
        panic!("body paragraph under the document");
    };
    ws.set_text(one, "one").unwrap();
    let two = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("two").unwrap(),
        )
        .unwrap();
    let three = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("three").unwrap(),
        )
        .unwrap();
    let empty = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("").unwrap(),
        )
        .unwrap();
    let four = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("four").unwrap(),
        )
        .unwrap();
    ws.wrap_in_list_block_many(
        &[one, two, three, empty, four],
        koru_core::ListStyle::Bullet,
    )
    .unwrap();
    // Nest empty and four under three the way Tab would.
    ws.indent(empty).unwrap();
    ws.indent(four).unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(empty);
    view.editor.place_caret(0);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Enter), None, NONE));

    let texts: Vec<_> = view
        .editor
        .body
        .rows
        .iter()
        .skip(view.editor.body.first_body_row())
        .map(|r| (r.text.as_str(), r.depth))
        .collect();
    // Depths are tree depth under the document (ListBlock adds one level).
    assert_eq!(
        texts,
        vec![("one", 1), ("two", 1), ("three", 1), ("", 1), ("four", 2),],
        "empty stays above four at three's level; four nests under empty"
    );
    assert_eq!(view.editor.body.focused_id(), Some(empty));
    assert_eq!(
        ws.children(koru_core::schema::Parent::Node(empty)).unwrap(),
        vec![four]
    );
}

#[test]
fn shift_tab_on_a_nested_item_keeps_following_siblings_below_it() {
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let three = view.editor.body.rows[first].id;
    let koru_core::schema::Parent::Node(document) = ws.parent(three).unwrap() else {
        panic!("body paragraph under the document");
    };
    ws.set_text(three, "three").unwrap();
    let empty = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("").unwrap(),
        )
        .unwrap();
    let four = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("four").unwrap(),
        )
        .unwrap();
    ws.wrap_in_list_block_many(&[three, empty, four], koru_core::ListStyle::Bullet)
        .unwrap();
    ws.indent(empty).unwrap();
    ws.indent(four).unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(empty);
    view.editor.place_caret(0);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT));

    let texts: Vec<_> = view
        .editor
        .body
        .rows
        .iter()
        .skip(view.editor.body.first_body_row())
        .map(|r| (r.text.as_str(), r.depth))
        .collect();
    assert_eq!(
        texts,
        vec![("three", 1), ("", 1), ("four", 2)],
        "Shift+Tab matches empty Enter"
    );
}

#[test]
fn empty_enter_on_an_outermost_item_extracts_in_place_not_past_the_list() {
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let one = view.editor.body.rows[first].id;
    let koru_core::schema::Parent::Node(document) = ws.parent(one).unwrap() else {
        panic!("body paragraph under the document");
    };
    ws.set_text(one, "one").unwrap();
    let empty = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("").unwrap(),
        )
        .unwrap();
    let four = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("four").unwrap(),
        )
        .unwrap();
    ws.wrap_in_list_block_many(&[one, empty, four], koru_core::ListStyle::Numbered)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(empty);
    view.editor.place_caret(0);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Enter), None, NONE));

    let siblings = ws
        .children(koru_core::schema::Parent::Node(document))
        .unwrap();
    assert_eq!(
        siblings[1], empty,
        "empty stayed between the two list pieces"
    );
    assert_eq!(view.editor.body.focused().unwrap().list_style, None);
    let after = siblings[2];
    assert_eq!(
        ws.children(koru_core::schema::Parent::Node(after)).unwrap(),
        vec![four]
    );
    assert_eq!(
        ws.read(after).unwrap().meta.list_style,
        Some(koru_core::ListStyle::Numbered)
    );
    // Suffix renumbers from 1 in the body projection.
    let four_row = view
        .editor
        .body
        .rows
        .iter()
        .find(|r| r.id == four)
        .expect("four still visible");
    assert_eq!(four_row.list_index, 1);
}

#[test]
fn shift_tab_keeps_the_first_outermost_bullet_a_bullet() {
    use koru_core::schema::Parent;
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let row = view.editor.body.first_body_row();
    let item = view.editor.body.rows[row].id;
    ws.set_text(item, "first").unwrap();
    let list = ws
        .wrap_in_list_block(item, koru_core::ListStyle::Bullet)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(item);
    view.editor.place_caret(0);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT,));
    assert_eq!(ws.parent(item).unwrap(), Parent::Node(list));
    assert_eq!(
        view.editor.body.focused().unwrap().list_style,
        Some(koru_core::ListStyle::Bullet)
    );
    assert!(ws.read(list).is_ok(), "the ListBlock stays in place");
}

#[test]
fn shift_tab_keeps_outer_numbered_list_items_numbered() {
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let one = view.editor.body.rows[first].id;
    let koru_core::schema::Parent::Node(document) = ws.parent(one).unwrap() else {
        panic!("the paragraph belongs to its document")
    };
    let two = ws
        .insert(
            koru_core::schema::Parent::Node(document),
            koru_core::node::Node::paragraph("two").unwrap(),
        )
        .unwrap();
    let list = ws
        .wrap_in_list_block_many(&[one, two], koru_core::ListStyle::Numbered)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(two);

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT));
    assert_eq!(
        ws.parent(two).unwrap(),
        koru_core::schema::Parent::Node(list)
    );
    assert_eq!(
        ws.read(list).unwrap().meta.list_style,
        Some(koru_core::ListStyle::Numbered)
    );
}

#[test]
fn backspace_unlists_the_first_document_paragraph_before_the_title_seam() {
    use koru_editor::Editing;
    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let row = view.editor.body.first_body_row();
    let item = view.editor.body.rows[row].id;
    let document = ws.parent(item).unwrap();
    ws.set_text(item, "first").unwrap();
    let list = ws
        .wrap_in_list_block(item, koru_core::ListStyle::Bullet)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(item);
    view.editor.place_caret(0);
    view.editor.reflow();

    assert!(view.key(&mut ws, &named(NamedKey::Backspace), None, NONE));
    assert_eq!(ws.parent(item).unwrap(), document);
    assert_eq!(view.editor.body.focused().unwrap().text, "first");
    assert_eq!(view.editor.body.focused().unwrap().list_style, None);
    assert!(ws.read(list).is_err(), "the empty ListBlock was removed");
}

/// F7 steps the document through Page → Narrow → Full → Page. It is the only
/// way to change the mode from the keyboard, and the only place F7 is claimed.
#[test]
fn f7_cycles_the_view_mode() {
    let (mut ws, mut view) = document();
    let f7 = |view: &mut BuiltInDocument, ws: &mut WorkspaceDoc| {
        view.key(ws, &Key::Named(NamedKey::F7), None, ModifiersState::empty());
    };
    assert_eq!(
        view.editor.view.view_mode,
        ViewMode::Full,
        "the default is Full"
    );
    f7(&mut view, &mut ws);
    assert_eq!(view.editor.view.view_mode, ViewMode::Page);
    f7(&mut view, &mut ws);
    assert_eq!(view.editor.view.view_mode, ViewMode::Narrow);
    f7(&mut view, &mut ws);
    assert_eq!(view.editor.view.view_mode, ViewMode::Full);
    f7(&mut view, &mut ws);
    assert_eq!(view.editor.view.view_mode, ViewMode::Page, "it wraps");
}

/// A mode and page colour set on one view reach a fresh view on the same
/// document through `capture`/`restore` — and a document on the defaults leaves
/// no entry behind.
#[test]
fn view_mode_and_page_colour_round_trip_through_settings() {
    let ws_id = WorkspaceId::new();
    let mut ws = WorkspaceDoc::create(ws_id, "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "spec.md").expect("a document");
    let mut view = BuiltInDocument::open(&ws, doc);
    view.editor.view.view_mode = ViewMode::Page;
    view.editor.view.canvas_bg = Some([0xF5, 0xEF, 0xDF, 0xFF]);

    let mut settings = Settings::default();
    settings.workspace = Some(ws_id);
    view.capture(&mut settings);
    let prefs = settings
        .doc_view_prefs
        .iter()
        .find_map(|(_, e)| e.iter().find(|(n, _)| *n == doc))
        .map(|(_, p)| *p);
    assert_eq!(
        prefs,
        Some(DocViewPrefs {
            mode: Some(ViewMode::Page),
            canvas_bg: Some([0xF5, 0xEF, 0xDF, 0xFF]),
        })
    );

    // A fresh view on the same document restores the override, not the default.
    let mut fresh = BuiltInDocument::open(&ws, doc);
    // The workspace handed to `restore` is ignored here — a document reads its
    // override out of `Settings`, keyed by the node it is rooted at — so an
    // empty one is enough, and moving `ws` into a `Workspace` would take it away
    // from the assertions below.
    fresh.restore(
        &koru_workspace::Workspace::new(
            koru_core::schema::WorkspaceDoc::create(koru_core::WorkspaceId::new(), "elsewhere")
                .unwrap(),
        ),
        &settings,
    );
    assert_eq!(fresh.editor.view.view_mode, ViewMode::Page);
    assert_eq!(fresh.editor.view.canvas_bg, Some([0xF5, 0xEF, 0xDF, 0xFF]));

    // And a document left on the global defaults records nothing.
    let clean = BuiltInDocument::open(&ws, doc);
    let mut clean_settings = Settings::default();
    clean_settings.workspace = Some(ws_id);
    clean.capture(&mut clean_settings);
    assert!(clean_settings.doc_view_prefs.is_empty());
}

// ------------------------------------------------- the gutter, and the menus

use koru_editor::components::Overlay;

/// No modifiers, the way most keys arrive.
const NONE: ModifiersState = ModifiersState::empty();

fn named(key: NamedKey) -> Key {
    Key::Named(key)
}

fn ch(s: &str) -> Key {
    Key::Character(s.into())
}

/// A document with two paragraphs under its name, laid out.
fn laid_out() -> (WorkspaceDoc, BuiltInDocument) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    // Built directly rather than through `koru_workspace::document`, which
    // seeds an empty first line — this wants a body whose rows are exactly the
    // two it names.
    let doc = ws
        .insert(
            koru_core::schema::Parent::Root,
            koru_core::node::Node::document("spec.md").unwrap(),
        )
        .unwrap();
    for text in ["one", "two"] {
        ws.insert(
            koru_core::schema::Parent::Node(doc),
            koru_core::node::Node::paragraph(text).unwrap(),
        )
        .unwrap();
    }
    let mut view = BuiltInDocument::open(&ws, doc);
    Editing::set_viewport(&mut view, 900.0, 560.0);
    view.reflow();
    (ws, view)
}

/// **A quote takes highlighting from every surface that offers it** — the
/// keyboard and the bubble the selection raises.
///
/// Driven the way a user arrives there: `> ` makes the row quoted prose, a
/// double-click selects a word, and the chord is pressed as **Windows
/// delivers it** — `Character("H")`, shifted, not the lowercase the key map is
/// written with. A refusal at any link of that chain reads as "quotes cannot
/// be highlighted", which is the report this pins against.
#[test]
fn a_quote_takes_highlighting_by_chord_and_from_the_bubble() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let (ws, mut view) = laid_out();
    let mut workspace = Workspace::new(ws);
    let images = ImageCache::new();

    view.editor.view.active = true;
    // Caret at the end of the first body row ("one"); Enter opens a fresh
    // paragraph beneath it for the `> ` gesture.
    view.body_mut().focus = view.editor.body.first_body_row();
    let len = view.editor.body.rows[view.editor.body.first_body_row()]
        .text
        .len();
    view.editor.place_caret(len);
    view.reflow();
    assert!(view.key(&mut workspace.ws, &named(NamedKey::Enter), None, NONE));
    assert!(view.key(&mut workspace.ws, &ch(">"), Some(">"), NONE));
    assert!(view.key(&mut workspace.ws, &ch(" "), Some(" "), NONE));
    let first = view.editor.body.first_body_row() + 1;
    assert_eq!(
        view.editor.body.rows[first].kind,
        Kind::Quote,
        "the row became a quote"
    );
    for c in ["q", "u", "o", "t", "e", "d"] {
        assert!(view.key(&mut workspace.ws, &ch(c), Some(c), NONE));
    }
    view.reflow();

    // Double-click the word to select it — the mouse path, so the quote's
    // text inset is in the coordinates under test too.
    let row = &view.editor.body.rows[first];
    let x = view.editor.view.content_x + row.text_x() + 8.0;
    let y = row.y - view.editor.view.scroll + row.height / 2.0;
    assert!(view.mouse_down(&mut workspace.ws, x, y, false));
    koru_editor::Editing::mouse_up(&mut view, &mut workspace.ws);
    assert!(
        BuiltInDocument::mouse_double_click(&mut view, &workspace.ws, x, y),
        "the double-click was claimed"
    );
    view.reflow();
    let selection = view.editor.body.rows[first].range();
    assert!(!selection.is_empty(), "a word is selected: {selection:?}");

    // The chord as Windows reports it: shifted uppercase character.
    let mods = koru_keys::PRIMARY.union(ModifiersState::SHIFT);
    assert!(view.key(&mut workspace.ws, &ch("H"), Some("H"), mods));
    let id = view.editor.body.rows[first].id;
    let names = koru_draw::theme::highlight_names();
    let style = workspace
        .ws
        .style_in(id, selection.clone())
        .unwrap_or_default();
    assert_eq!(
        style.highlight.as_deref(),
        Some(names[0]),
        "the first swatch landed on the selection"
    );

    // The selection still stands, the bubble is up over the quote, and its
    // Highlight button cycles to the next swatch.
    let mut scene = Scene::new();
    View::draw(
        &mut view,
        &mut scene,
        &workspace,
        &images,
        Affine::IDENTITY,
        true,
        (0.0, 0.0),
    );
    let panel = view
        .bubble_panel()
        .expect("a selection inside a quote shows the bubble");
    let hl = bubble::button_rect(panel, bubble::Button::Cmd(docview::Command::Highlight))
        .expect("Highlight is on the bar");
    let (x, y) = hl.centre();
    assert!(
        BuiltInDocument::mouse_down(&mut view, &mut workspace.ws, x, y, false),
        "the press was claimed"
    );
    let style = workspace.ws.style_in(id, selection).unwrap_or_default();
    assert_eq!(
        style.highlight.as_deref(),
        Some(names[1]),
        "and the bubble cycles it"
    );
}

/// Highlight armed at a collapsed caret carries into what is typed next,
/// inside a quote same as anywhere else.
#[test]
fn highlight_armed_in_a_quote_carries_into_the_next_typing() {
    use koru_workspace::Workspace;
    let (ws, mut view) = laid_out();
    let mut workspace = Workspace::new(ws);

    view.editor.view.active = true;
    view.body_mut().focus = view.editor.body.first_body_row();
    let len = view.editor.body.rows[view.editor.body.first_body_row()]
        .text
        .len();
    view.editor.place_caret(len);
    view.reflow();
    assert!(view.key(&mut workspace.ws, &named(NamedKey::Enter), None, NONE));
    assert!(view.key(&mut workspace.ws, &ch(">"), Some(">"), NONE));
    assert!(view.key(&mut workspace.ws, &ch(" "), Some(" "), NONE));
    let first = view.editor.body.first_body_row() + 1;
    assert_eq!(view.editor.body.rows[first].kind, Kind::Quote);
    for c in ["a", "b"] {
        assert!(view.key(&mut workspace.ws, &ch(c), Some(c), NONE));
    }
    view.reflow();

    // Collapsed caret after "ab": Ctrl+Shift+H arms, typing carries it.
    let mods = koru_keys::PRIMARY.union(ModifiersState::SHIFT);
    assert!(
        view.key(&mut workspace.ws, &ch("H"), Some("H"), mods),
        "the chord was handled"
    );
    assert!(view.key(&mut workspace.ws, &ch("c"), Some("c"), NONE));
    let id = view.editor.body.rows[first].id;
    assert_eq!(view.editor.body.rows[first].text, "abc");
    let style = workspace.ws.style_in(id, 2..3).unwrap_or_default();
    assert!(
        style.highlight.is_some(),
        "typed text came out highlighted: {:?}",
        style.highlight
    );
}

#[test]
fn list_shortcuts_wrap_a_row_selection_in_one_list_and_restyle_it() {
    let (mut ws, mut view) = laid_out();
    let first = view.editor.body.first_body_row();
    let ids = [
        view.editor.body.rows[first].id,
        view.editor.body.rows[first + 1].id,
    ];
    let document = ws.parent(ids[0]).unwrap();
    view.editor.body.toggle_selection(first);
    view.editor.body.toggle_selection(first + 1);
    let ctrl_shift = koru_keys::PRIMARY.union(ModifiersState::SHIFT);

    assert!(view.key(&mut ws, &ch("8"), Some("8"), ctrl_shift));
    let parent = ws.parent(ids[0]).unwrap();
    assert_eq!(ws.parent(ids[1]).unwrap(), parent);
    let koru_core::schema::Parent::Node(list) = parent else {
        panic!("the selected paragraphs share a ListBlock")
    };
    let node = ws.read(list).unwrap();
    assert_eq!(node.kind, Kind::ListBlock);
    assert_eq!(node.meta.list_style, Some(koru_core::ListStyle::Bullet));
    assert_eq!(
        ws.children(koru_core::schema::Parent::Node(list)).unwrap(),
        ids
    );

    assert!(view.key(&mut ws, &ch("7"), Some("7"), ctrl_shift));
    let numbered_parent = ws.parent(ids[0]).unwrap();
    assert_eq!(ws.parent(ids[1]).unwrap(), numbered_parent);
    let koru_core::schema::Parent::Node(numbered) = numbered_parent else {
        panic!("the selected paragraphs share a numbered ListBlock")
    };
    assert_eq!(
        ws.read(numbered).unwrap().meta.list_style,
        Some(koru_core::ListStyle::Numbered)
    );

    assert!(view.key(&mut ws, &ch("9"), Some("9"), ctrl_shift));
    let koru_core::schema::Parent::Node(checklist) = ws.parent(ids[0]).unwrap() else {
        panic!("the selected paragraphs share a checklist ListBlock")
    };
    assert_eq!(
        ws.read(checklist).unwrap().meta.list_style,
        Some(koru_core::ListStyle::Checkbox)
    );
    assert!(view.editor.body.row(ids[0]).unwrap().is_task());

    assert!(view.key(&mut ws, &ch("9"), Some("9"), ctrl_shift));
    assert!(ws.read(checklist).is_err());
    assert_eq!(ws.parent(ids[0]).unwrap(), document);
    assert_eq!(ws.parent(ids[1]).unwrap(), document);

    assert!(view.key(&mut ws, &ch("8"), Some("8"), ctrl_shift));
    let koru_core::schema::Parent::Node(bullets) = ws.parent(ids[0]).unwrap() else {
        panic!("the selected paragraphs share a bullet ListBlock")
    };
    assert_eq!(
        ws.read(bullets).unwrap().meta.list_style,
        Some(koru_core::ListStyle::Bullet)
    );
    assert!(view.key(&mut ws, &ch("8"), Some("8"), ctrl_shift));
    assert!(ws.read(bullets).is_err());
    assert_eq!(ws.parent(ids[0]).unwrap(), document);
    assert_eq!(ws.parent(ids[1]).unwrap(), document);
}

#[test]
fn a_list_shortcut_changes_only_the_focused_item() {
    use koru_core::schema::Parent;
    use koru_editor::Editing;

    for (digit, requested) in [
        ("7", koru_core::ListStyle::Numbered),
        ("8", koru_core::ListStyle::Bullet),
    ] {
        let (mut ws, mut view) = document();
        view.editor.view.active = true;
        let first = view.editor.body.first_body_row();
        let first_id = view.editor.body.rows[first].id;
        let Parent::Node(document) = ws.parent(first_id).unwrap() else {
            panic!("the paragraph belongs to its document")
        };
        ws.set_text(first_id, "hello").unwrap();
        let two = ws
            .insert(
                Parent::Node(document),
                koru_core::node::Node::paragraph("two").unwrap(),
            )
            .unwrap();
        let three = ws
            .insert(
                Parent::Node(document),
                koru_core::node::Node::paragraph("three").unwrap(),
            )
            .unwrap();
        let ids = [first_id, two, three];
        ws.wrap_in_list_block_many(&ids, koru_core::ListStyle::Bullet)
            .unwrap();
        view.editor.body.rebuild(&ws);
        view.editor.body.focus_on(two);
        view.editor.reflow();

        assert!(view.key(
            &mut ws,
            &ch(digit),
            Some(digit),
            koru_keys::PRIMARY.union(ModifiersState::SHIFT),
        ));

        let first_parent = ws.parent(first_id).unwrap();
        let middle_parent = ws.parent(two).unwrap();
        let last_parent = ws.parent(three).unwrap();
        assert_eq!(
            ws.read(match first_parent {
                Parent::Node(id) => id,
                _ => panic!("first item remains a bullet"),
            })
            .unwrap()
            .meta
            .list_style,
            Some(koru_core::ListStyle::Bullet)
        );
        assert_eq!(
            ws.read(match last_parent {
                Parent::Node(id) => id,
                _ => panic!("last item remains a bullet"),
            })
            .unwrap()
            .meta
            .list_style,
            Some(koru_core::ListStyle::Bullet)
        );

        if requested == koru_core::ListStyle::Numbered {
            let Parent::Node(middle_list) = middle_parent else {
                panic!("the middle item became a numbered list")
            };
            assert_eq!(
                ws.read(middle_list).unwrap().meta.list_style,
                Some(koru_core::ListStyle::Numbered)
            );
        } else {
            assert_eq!(middle_parent, Parent::Node(document));
        }
    }
}

/// Choosing the active style from the selection bubble's list menu is a toggle,
/// just like its keyboard shortcut. It extracts only that row; the neighbouring
/// rows remain in their list.
#[test]
fn choosing_the_active_bullet_style_turns_off_the_focused_row() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    View::set_viewport(&mut view, 900.0, 700.0);
    let first = view.editor.body.first_body_row();
    let first_id = view.editor.body.rows[first].id;
    let Parent::Node(document) = ws.parent(first_id).unwrap() else {
        panic!("the paragraph belongs to its document")
    };
    ws.set_text(first_id, "one").unwrap();
    let two = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("two").unwrap(),
        )
        .unwrap();
    let three = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("three").unwrap(),
        )
        .unwrap();
    ws.wrap_in_list_block_many(&[first_id, two, three], koru_core::ListStyle::Bullet)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(two);
    view.editor.place_selection_pair((0, 3));
    view.editor.reflow();
    view.place_bubble();

    let panel = view.bubble_panel().expect("selected text has a toolbar");
    let list =
        bubble::button_rect(panel, bubble::Button::List).expect("the toolbar has a list button");
    assert!(view.mouse_down(&mut ws, list.centre().0, list.centre().1, false,));
    view.choose_menu(&mut ws, 0, None);

    assert_eq!(ws.parent(two).unwrap(), Parent::Node(document));
    for id in [first_id, three] {
        let Parent::Node(list) = ws.parent(id).unwrap() else {
            panic!("an unselected neighbour remains a bullet")
        };
        assert_eq!(
            ws.read(list).unwrap().meta.list_style,
            Some(koru_core::ListStyle::Bullet)
        );
    }
}

#[test]
fn restyling_selected_lists_reconnects_matching_numbered_neighbours() {
    use koru_core::schema::Parent;

    // The first range is the screenshot: only the middle bullet block. The
    // second also selects the numbered suffix, pinning the case where merging
    // one selected container deletes another selected container.
    for selected in [1..3, 1..4] {
        let (mut ws, mut view) = document();
        view.editor.view.active = true;
        let first = view.editor.body.first_body_row();
        let one = view.editor.body.rows[first].id;
        let Parent::Node(document) = ws.parent(one).unwrap() else {
            panic!("the paragraph belongs to its document")
        };
        ws.set_text(one, "one").unwrap();
        let mut ids = vec![one];
        for text in ["two", "three", "four"] {
            ids.push(
                ws.insert(
                    Parent::Node(document),
                    koru_core::node::Node::paragraph(text).unwrap(),
                )
                .unwrap(),
            );
        }
        ws.wrap_in_list_block_many(&ids[..1], koru_core::ListStyle::Numbered)
            .unwrap();
        ws.wrap_in_list_block_many(&ids[1..3], koru_core::ListStyle::Bullet)
            .unwrap();
        ws.wrap_in_list_block_many(&ids[3..], koru_core::ListStyle::Numbered)
            .unwrap();
        view.editor.body.rebuild(&ws);
        for id in &ids[selected.clone()] {
            let row = view.editor.body.index_of(*id).unwrap();
            view.editor.body.toggle_selection(row);
        }
        view.editor.body.focus_on(ids[selected.end - 1]);

        assert!(view.key(
            &mut ws,
            &ch("7"),
            Some("7"),
            koru_keys::PRIMARY.union(ModifiersState::SHIFT),
        ));

        let children = ws.children(Parent::Node(document)).unwrap();
        assert_eq!(children.len(), 1, "all four rows are one numbered list");
        assert_eq!(
            ws.read(children[0]).unwrap().meta.list_style,
            Some(koru_core::ListStyle::Numbered)
        );
        assert_eq!(ws.children(Parent::Node(children[0])).unwrap(), ids);
    }
}

#[test]
fn tab_and_shift_tab_indent_and_outdent_the_selected_list_items_together() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let one = view.editor.body.rows[first].id;
    let Parent::Node(document) = ws.parent(one).unwrap() else {
        panic!("the paragraph belongs to its document")
    };
    ws.set_text(one, "one").unwrap();
    let mut ids = vec![one];
    for text in ["two", "three", "four"] {
        ids.push(
            ws.insert(
                Parent::Node(document),
                koru_core::node::Node::paragraph(text).unwrap(),
            )
            .unwrap(),
        );
    }
    let list = ws
        .wrap_in_list_block_many(&ids, koru_core::ListStyle::Numbered)
        .unwrap();
    view.editor.body.rebuild(&ws);
    for id in &ids[1..3] {
        let row = view.editor.body.index_of(*id).unwrap();
        view.editor.body.toggle_selection(row);
    }
    view.editor.body.focus_on(ids[2]);

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, NONE));
    assert_eq!(ws.parent(ids[1]).unwrap(), Parent::Node(ids[0]));
    assert_eq!(ws.parent(ids[2]).unwrap(), Parent::Node(ids[0]));
    assert_eq!(
        ws.children(Parent::Node(ids[0])).unwrap(),
        vec![ids[1], ids[2]]
    );
    assert_eq!(
        ws.children(Parent::Node(list)).unwrap(),
        vec![ids[0], ids[3]]
    );

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT,));
    assert_eq!(ws.children(Parent::Node(list)).unwrap(), ids);
    assert!(
        ids[1..3]
            .iter()
            .all(|id| view.editor.body.row(*id).is_some_and(|row| row.selected))
    );

    assert!(view.key(
        &mut ws,
        &named(NamedKey::ArrowDown),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    ));
    assert_eq!(
        ws.children(Parent::Node(list)).unwrap(),
        vec![ids[0], ids[3], ids[1], ids[2]],
        "the group stays ordered when its last item is already at the bottom"
    );
}

#[test]
fn tab_moves_a_bullet_below_a_task_without_losing_its_marker() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let task = view.editor.body.rows[view.editor.body.first_body_row()].id;
    let Parent::Node(document) = ws.parent(task).unwrap() else {
        panic!("the first body row belongs to its document")
    };
    ws.set_kind(task, Kind::Task).unwrap();
    let bullet = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("bullet").unwrap(),
        )
        .unwrap();
    ws.wrap_in_list_block(bullet, koru_core::ListStyle::Bullet)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(bullet);

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, NONE));
    let Parent::Node(list) = ws.parent(bullet).unwrap() else {
        panic!("the bullet remains in a list")
    };
    assert_eq!(ws.parent(list).unwrap(), Parent::Node(task));
    assert_eq!(ws.parent(bullet).unwrap(), Parent::Node(list));
    assert_eq!(
        view.editor.body.row(bullet).unwrap().list_style,
        Some(koru_core::ListStyle::Bullet)
    );

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT));
    let Parent::Node(list) = ws.parent(bullet).unwrap() else {
        panic!("the bullet remains in a list")
    };
    assert_eq!(ws.parent(list).unwrap(), Parent::Node(document));
    assert_eq!(ws.parent(bullet).unwrap(), Parent::Node(list));
    assert_eq!(
        view.editor.body.row(bullet).unwrap().list_style,
        Some(koru_core::ListStyle::Bullet)
    );
}

#[test]
fn tab_moves_a_task_below_a_bullet_without_changing_it_to_a_bullet() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let bullet = view.editor.body.rows[view.editor.body.first_body_row()].id;
    let Parent::Node(document) = ws.parent(bullet).unwrap() else {
        panic!("the first body row belongs to its document")
    };
    let list = ws
        .wrap_in_list_block(bullet, koru_core::ListStyle::Bullet)
        .unwrap();
    let task = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::task("task").unwrap(),
        )
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(task);

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, NONE));
    assert_eq!(ws.parent(task).unwrap(), Parent::Node(bullet));
    assert!(view.editor.body.row(task).unwrap().is_task());
    assert_eq!(ws.parent(bullet).unwrap(), Parent::Node(list));

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT));
    assert_eq!(ws.parent(task).unwrap(), Parent::Node(document));
    assert!(view.editor.body.row(task).unwrap().is_task());
}

#[test]
fn shift_tab_keeps_a_checkbox_list_item_a_task_below_a_bullet() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let bullet = view.editor.body.rows[view.editor.body.first_body_row()].id;
    let Parent::Node(document) = ws.parent(bullet).unwrap() else {
        panic!("the first body row belongs to its document")
    };
    let checkbox = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("checkbox").unwrap(),
        )
        .unwrap();
    let bullet_list = ws
        .wrap_in_list_block(bullet, koru_core::ListStyle::Bullet)
        .unwrap();
    ws.wrap_in_list_block(checkbox, koru_core::ListStyle::Checkbox)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(checkbox);

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, NONE));
    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT));

    let Parent::Node(checkbox_list) = ws.parent(checkbox).unwrap() else {
        panic!("outdenting keeps the checkbox in a list")
    };
    assert_eq!(ws.parent(checkbox_list).unwrap(), Parent::Node(bullet_list));
    assert_eq!(
        ws.read(checkbox_list).unwrap().meta.list_style,
        Some(koru_core::ListStyle::Checkbox)
    );
    assert!(view.editor.body.row(checkbox).unwrap().is_task());
}

#[test]
fn tab_nests_adjacent_bullet_and_checkbox_list_blocks_by_visible_order() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let bullet = view.editor.body.rows[view.editor.body.first_body_row()].id;
    let Parent::Node(document) = ws.parent(bullet).unwrap() else {
        panic!("the first body row belongs to its document")
    };
    let checkbox = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("checkbox").unwrap(),
        )
        .unwrap();
    let nested_bullet = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("nested bullet").unwrap(),
        )
        .unwrap();
    let bullet_list = ws
        .wrap_in_list_block(bullet, koru_core::ListStyle::Bullet)
        .unwrap();
    ws.wrap_in_list_block(checkbox, koru_core::ListStyle::Checkbox)
        .unwrap();
    ws.wrap_in_list_block(nested_bullet, koru_core::ListStyle::Bullet)
        .unwrap();
    view.editor.body.rebuild(&ws);

    view.editor.body.focus_on(checkbox);
    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, NONE));
    let Parent::Node(checkbox_list) = ws.parent(checkbox).unwrap() else {
        panic!("the checkbox remains in a list")
    };
    assert_eq!(ws.parent(checkbox_list).unwrap(), Parent::Node(bullet));
    assert_eq!(
        ws.read(checkbox_list).unwrap().meta.list_style,
        Some(koru_core::ListStyle::Checkbox)
    );
    assert!(view.editor.body.row(checkbox).unwrap().is_task());

    view.editor.body.focus_on(nested_bullet);
    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, NONE));
    let Parent::Node(nested_bullet_list) = ws.parent(nested_bullet).unwrap() else {
        panic!("the nested bullet remains in a list")
    };
    assert_eq!(
        ws.parent(nested_bullet_list).unwrap(),
        Parent::Node(checkbox)
    );
    assert_eq!(ws.parent(bullet).unwrap(), Parent::Node(bullet_list));
    assert_eq!(
        view.editor.body.row(nested_bullet).unwrap().list_style,
        Some(koru_core::ListStyle::Bullet)
    );

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT));
    let Parent::Node(outdented_bullet_list) = ws.parent(nested_bullet).unwrap() else {
        panic!("outdenting retains the bullet list")
    };
    assert_eq!(
        ws.parent(outdented_bullet_list).unwrap(),
        Parent::Node(checkbox_list)
    );
    assert_eq!(
        view.editor.body.row(nested_bullet).unwrap().list_style,
        Some(koru_core::ListStyle::Bullet)
    );

    assert!(view.key(&mut ws, &named(NamedKey::Tab), None, ModifiersState::SHIFT));
    let Parent::Node(outdented_again_list) = ws.parent(nested_bullet).unwrap() else {
        panic!("outdenting again retains the bullet list")
    };
    assert_eq!(
        ws.parent(outdented_again_list).unwrap(),
        Parent::Node(bullet)
    );
    assert_eq!(
        view.editor.body.row(nested_bullet).unwrap().list_style,
        Some(koru_core::ListStyle::Bullet)
    );
}

#[test]
fn reorder_moves_a_selected_parent_with_all_of_its_list_descendants() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let group = view.editor.body.rows[first].id;
    let Parent::Node(document) = ws.parent(group).unwrap() else {
        panic!("the paragraph belongs to its document")
    };
    ws.set_text(group, "group").unwrap();
    let child = ws
        .insert(
            Parent::Node(group),
            koru_core::node::Node::paragraph("child").unwrap(),
        )
        .unwrap();
    let child_list = ws
        .wrap_in_list_block(child, koru_core::ListStyle::Bullet)
        .unwrap();
    let grandchild = ws
        .insert(
            Parent::Node(child),
            koru_core::node::Node::paragraph("grandchild").unwrap(),
        )
        .unwrap();
    let grandchild_list = ws
        .wrap_in_list_block(grandchild, koru_core::ListStyle::Bullet)
        .unwrap();
    let after = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("after").unwrap(),
        )
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(group);
    view.editor
        .body
        .extend_selection_to(view.editor.body.index_of(grandchild).unwrap());

    assert!(view.key(
        &mut ws,
        &named(NamedKey::ArrowDown),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    ));

    assert_eq!(
        ws.children(Parent::Node(document)).unwrap(),
        vec![after, group]
    );
    assert_eq!(ws.parent(child_list).unwrap(), Parent::Node(group));
    assert_eq!(ws.parent(child).unwrap(), Parent::Node(child_list));
    assert_eq!(ws.parent(grandchild_list).unwrap(), Parent::Node(child));
    assert_eq!(
        ws.parent(grandchild).unwrap(),
        Parent::Node(grandchild_list)
    );
    assert!(
        [group, child, grandchild].iter().all(|id| view
            .editor
            .body
            .row(*id)
            .is_some_and(|row| row.selected))
    );
}

#[test]
fn reorder_moves_selected_sibling_list_items_as_one_ordered_group() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let one = view.editor.body.rows[first].id;
    let Parent::Node(document) = ws.parent(one).unwrap() else {
        panic!("the paragraph belongs to its document")
    };
    ws.set_text(one, "one").unwrap();
    let mut ids = vec![one];
    for text in ["two", "three", "four"] {
        ids.push(
            ws.insert(
                Parent::Node(document),
                koru_core::node::Node::paragraph(text).unwrap(),
            )
            .unwrap(),
        );
    }
    let list = ws
        .wrap_in_list_block_many(&ids, koru_core::ListStyle::Numbered)
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(ids[1]);
    view.editor
        .body
        .extend_selection_to(view.editor.body.index_of(ids[2]).unwrap());

    assert!(view.key(
        &mut ws,
        &named(NamedKey::ArrowDown),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    ));

    assert_eq!(
        ws.children(Parent::Node(list)).unwrap(),
        vec![ids[0], ids[3], ids[1], ids[2]]
    );
    assert!(
        ids[1..3]
            .iter()
            .all(|id| view.editor.body.row(*id).is_some_and(|row| row.selected))
    );
}

#[test]
fn reorder_keeps_selected_paragraphs_and_a_complete_list_together() {
    use koru_core::schema::Parent;

    let (mut ws, mut view) = document();
    view.editor.view.active = true;
    let first = view.editor.body.first_body_row();
    let before = view.editor.body.rows[first].id;
    let Parent::Node(document) = ws.parent(before).unwrap() else {
        panic!("the paragraph belongs to its document")
    };
    ws.set_text(before, "before").unwrap();
    let mut paragraphs = Vec::new();
    for text in ["one", "two", "three"] {
        paragraphs.push(
            ws.insert(
                Parent::Node(document),
                koru_core::node::Node::paragraph(text).unwrap(),
            )
            .unwrap(),
        );
    }
    let item = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("list parent").unwrap(),
        )
        .unwrap();
    let list = ws
        .wrap_in_list_block(item, koru_core::ListStyle::Bullet)
        .unwrap();
    let child = ws
        .insert(
            Parent::Node(item),
            koru_core::node::Node::paragraph("list child").unwrap(),
        )
        .unwrap();
    let after = ws
        .insert(
            Parent::Node(document),
            koru_core::node::Node::paragraph("after").unwrap(),
        )
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(paragraphs[0]);
    view.editor
        .body
        .extend_selection_to(view.editor.body.index_of(child).unwrap());

    assert!(view.key(
        &mut ws,
        &named(NamedKey::ArrowUp),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    ));

    assert_eq!(
        ws.children(Parent::Node(document)).unwrap(),
        vec![
            paragraphs[0],
            paragraphs[1],
            paragraphs[2],
            list,
            before,
            after,
        ],
        "the selected prose and complete list move past `before` as one group"
    );
    assert_eq!(ws.parent(item).unwrap(), Parent::Node(list));
    assert_eq!(ws.parent(child).unwrap(), Parent::Node(item));
    assert!(
        paragraphs.iter().chain([item, child].iter()).all(|id| view
            .editor
            .body
            .row(*id)
            .is_some_and(|row| row.selected))
    );

    assert!(view.key(
        &mut ws,
        &named(NamedKey::ArrowDown),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    ));
    assert_eq!(
        ws.children(Parent::Node(document)).unwrap(),
        vec![
            before,
            paragraphs[0],
            paragraphs[1],
            paragraphs[2],
            list,
            after,
        ],
        "moving down keeps the same mixed selection together"
    );

    assert!(view.key(
        &mut ws,
        &named(NamedKey::ArrowDown),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    ));
    let at_bottom = vec![
        before,
        after,
        paragraphs[0],
        paragraphs[1],
        paragraphs[2],
        list,
    ];
    assert_eq!(ws.children(Parent::Node(document)).unwrap(), at_bottom);

    assert!(view.key(
        &mut ws,
        &named(NamedKey::ArrowDown),
        None,
        koru_keys::PRIMARY.union(ModifiersState::SHIFT),
    ));
    assert_eq!(
        ws.children(Parent::Node(document)).unwrap(),
        at_bottom,
        "a mixed group cannot partially rearrange after it reaches the bottom"
    );
}

/// A row's `+` in window coordinates. The lane scrolls with the content, so
/// the test has to make the same journey the click path does.
fn plus_at(view: &BuiltInDocument, row: usize) -> (f32, f32) {
    let r = Editing::plus_rect(&view.editor, row).expect("a body row has a `+`");
    (
        r.x + view.editor.view.content_x + r.w * 0.5,
        r.y - view.editor.view.scroll + r.h * 0.5,
    )
}

/// The title's `…` in window coordinates.
fn dots_at(view: &BuiltInDocument) -> (f32, f32) {
    let r = Editing::title_menu_rect(&view.editor).expect("a document has a title row");
    (
        r.x + view.editor.view.content_x + r.w * 0.5,
        r.y - view.editor.view.scroll + r.h * 0.5,
    )
}

/// The title has no `+` and no grip: it is the node the body is rooted at, so
/// there is nothing to drag it to and nothing goes above it.
#[test]
fn the_title_row_has_no_insert_button() {
    let (_ws, view) = laid_out();
    assert!(Editing::plus_rect(&view.editor, 0).is_none());
    assert!(Editing::plus_rect(&view.editor, 1).is_some());
    assert!(Editing::title_menu_rect(&view.editor).is_some());
}

/// Clicking `+` opens the block menu and **creates nothing**. Dismissing it has
/// to leave the document exactly as it was, or the button would litter empty
/// paragraphs every time somebody looked at it.
#[test]
fn the_plus_opens_the_block_menu_without_inserting_anything() {
    let (mut ws, mut view) = laid_out();
    let before = view.body().rows.len();
    let (x, y) = plus_at(&view, 1);

    assert!(view.mouse_down(&mut ws, x, y, false));
    assert!(
        matches!(view.view().overlay, Overlay::Command(_)),
        "the same menu `/` opens"
    );
    assert_eq!(view.body().rows.len(), before, "and nothing was inserted");

    view.key(&mut ws, &named(NamedKey::Escape), None, NONE);
    assert!(matches!(view.view().overlay, Overlay::None));
    assert_eq!(view.body().rows.len(), before, "Esc leaves no empty row");
}

/// Choosing a kind is what makes the block — below the row whose `+` it was.
#[test]
fn choosing_from_the_plus_menu_makes_the_block_below() {
    let (mut ws, mut view) = laid_out();
    let (x, y) = plus_at(&view, 1);
    view.mouse_down(&mut ws, x, y, false);
    step_to_divider(&mut view, &mut ws);
    view.key(&mut ws, &named(NamedKey::Enter), None, NONE);

    let kinds: Vec<_> = view.body().rows.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds[2],
        koru_core::Kind::Divider,
        "the rule landed after the row that offered it: {kinds:?}"
    );
    assert_eq!(view.body().rows[1].text, "one");
}

/// Alt-click adds above instead, which is the other half of the hint the `+`
/// shows on hover.
#[test]
fn alt_clicking_the_plus_makes_the_block_above() {
    let (mut ws, mut view) = laid_out();
    view.editor.view.mods = ModifiersState::ALT;
    let (x, y) = plus_at(&view, 2);
    view.mouse_down(&mut ws, x, y, false);
    step_to_divider(&mut view, &mut ws);
    view.key(&mut ws, &named(NamedKey::Enter), None, NONE);

    let rows = &view.body().rows;
    let two = rows.iter().position(|r| r.text == "two").expect("row two");
    // `commit_divider` opens a paragraph after the rule for the caret, so the
    // rule is two above "two" rather than one. What matters is that it landed
    // on the far side of it.
    assert!(
        rows[..two]
            .iter()
            .any(|r| r.kind == koru_core::Kind::Divider),
        "the rule sits above it"
    );
    assert_eq!(rows[1].text, "one", "and below the row before it");
}

/// Walk the open command menu to its Divider row. By search rather than by a
/// count, so re-ordering the menu does not silently retarget the test.
fn step_to_divider(view: &mut BuiltInDocument, ws: &mut WorkspaceDoc) {
    use koru_editor::components::command_menu::Command as Cmd;
    let Overlay::Command(menu) = &view.view().overlay else {
        panic!("the `+` opens the command menu");
    };
    let want = menu
        .hits
        .iter()
        .position(|c| *c == Cmd::Divider)
        .expect("the document offers dividers");
    for _ in 0..want {
        view.key(ws, &named(NamedKey::ArrowDown), None, NONE);
    }
}

/// Typing over an open `+` menu filters it, and leaves the document alone.
///
/// The characters go into the panel's own search box, so a half-written command
/// is never text somebody has to take back out of their page.
#[test]
fn typing_over_the_plus_menu_filters_it_without_touching_the_document() {
    let (mut ws, mut view) = laid_out();
    let before: Vec<_> = view.body().rows.iter().map(|r| r.text.clone()).collect();
    let (x, y) = plus_at(&view, 1);
    view.mouse_down(&mut ws, x, y, false);

    for c in "div".chars() {
        let s = c.to_string();
        view.key(&mut ws, &ch(&s), Some(&s), NONE);
    }
    let Overlay::Command(menu) = &view.view().overlay else {
        panic!("the menu is still open, filtered");
    };
    assert_eq!(
        menu.hits,
        vec![koru_editor::components::command_menu::Command::Divider]
    );
    let after: Vec<_> = view.body().rows.iter().map(|r| r.text.clone()).collect();
    assert_eq!(before, after, "and nothing was typed into the page");

    // And the choice still lands where the `+` said it would.
    view.key(&mut ws, &named(NamedKey::Enter), None, NONE);
    assert_eq!(view.body().rows[2].kind, koru_core::Kind::Divider);
}

/// The `…` opens the page settings, and a width row changes the mode —
/// **without closing**. Every row there is a setting whose result you are
/// looking at, so the menu stays up until it is dismissed.
#[test]
fn the_title_dots_open_the_page_menu_and_a_row_sets_the_width() {
    let (mut ws, mut view) = laid_out();
    Editing::set_view_mode(&mut view, ViewMode::Full);
    let (x, y) = dots_at(&view);
    assert!(view.mouse_down(&mut ws, x, y, false));
    assert!(view.menu.is_some(), "the page menu is open");

    // Walk to Narrow and take it. In Full there is no page-color row, so the
    // widths are rows 1, 2, 3.
    for _ in 0..2 {
        view.key(&mut ws, &named(NamedKey::ArrowDown), None, NONE);
    }
    view.key(&mut ws, &named(NamedKey::Enter), None, NONE);

    assert_eq!(view.view().view_mode, ViewMode::Narrow);
    assert!(view.menu.is_some(), "and it stayed open");

    // Esc is what closes it.
    view.key(&mut ws, &named(NamedKey::Escape), None, NONE);
    assert!(view.menu.is_none());
}

/// The width chords name a mode outright, and work with the menu open so the
/// three can be stepped through while watching the page.
#[test]
fn the_layout_chords_switch_the_mode() {
    let (mut ws, mut view) = laid_out();
    let ctrl_shift = koru_keys::PRIMARY.union(ModifiersState::SHIFT);
    // `W` closes every tab and `F` opens global search, so Full width is `G`.
    for (letter, mode) in [
        ("n", ViewMode::Narrow),
        ("g", ViewMode::Full),
        ("p", ViewMode::Page),
    ] {
        assert!(view.key(&mut ws, &ch(letter), Some(letter), ctrl_shift));
        assert_eq!(view.view().view_mode, mode, "Ctrl+Shift+{letter}");
    }

    let (x, y) = dots_at(&view);
    view.mouse_down(&mut ws, x, y, false);
    assert!(view.key(&mut ws, &ch("g"), Some("g"), ctrl_shift));
    assert_eq!(view.view().view_mode, ViewMode::Full);
    assert!(view.menu.is_some(), "and the menu is still up to show it");
}

/// Every menu's *shape* is the same with and without the document read.
///
/// `mouse_move` has no `&WorkspaceDoc`, so hover hit-tests against
/// `menu_shape` while a click hit-tests against `menu_rows`. They agree only
/// while what the caret says reaches nothing but a row's `Trailing` — this is
/// what fails if a row ever starts appearing, disappearing or changing height
/// with the selection.
#[test]
fn every_menu_lays_out_the_same_with_and_without_the_document() {
    let (mut ws, mut view) = laid_out();
    let vp = view.editor.view.viewport;
    let anchor = koru_draw::geometry::Rect::new(100.0, 100.0, 20.0, 20.0);

    for open in [
        OpenMenu::Page(page_menu::open(anchor)),
        OpenMenu::Block(blockmenu::open(anchor)),
        OpenMenu::More(blockmenu::open(anchor)),
    ] {
        view.menu = Some(open);
        let real = view.menu_rows(&ws);
        let shape = view.menu_shape();
        assert_eq!(real.len(), shape.len(), "{open:?}");
        let m = open.menu();
        for i in 0..real.len() {
            assert_eq!(
                m.row_rect(&real, vp, i),
                m.row_rect(&shape, vp, i),
                "{open:?} row {i} lands in the same place"
            );
            assert_eq!(
                real[i].selectable(),
                shape[i].selectable(),
                "{open:?} row {i} is clickable either way"
            );
        }
    }
    let _ = &mut ws;
}

/// While a bubble menu is open, moving outside the panel must not light the
/// row underneath or restore the I-beam — dismiss is a click, not a move.
#[test]
fn an_open_menu_absorbs_mouse_move_outside_its_panel() {
    let (mut ws, mut view) = laid_out();
    let anchor = koru_draw::geometry::Rect::new(100.0, 100.0, 20.0, 20.0);
    view.menu = Some(OpenMenu::Block(blockmenu::open(anchor)));
    // Seed a hover as if the pointer had been over text before the menu opened.
    view.editor.view.hover = Some(koru_editor::view_state::Hover::Text(0));
    // Far from the menu panel.
    assert!(view.mouse_move(800.0, 500.0));
    assert!(view.menu.is_some(), "move does not dismiss");
    assert!(
        view.editor.view.hover.is_none(),
        "editor hover must not stick under the menu"
    );
    let _ = &mut ws;
}

/// Every piece of chrome this view draws, encoded once.
///
/// A `vello::Scene` is an encoding buffer, not a GPU resource, so a frame can be
/// drawn headlessly — and none of the tests above draw anything at all, which
/// left the whole of `draw.rs`, `bubble.rs`, `menu.rs` and the row loop's new
/// affordances covered only by whether the app happened to be run. It asserts
/// nothing beyond "this does not panic", which is the interesting failure: an
/// out-of-range index, a negative rect, an empty layout.
#[test]
fn a_frame_with_every_menu_and_the_bubble_draws_without_panicking() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let (ws, mut view) = laid_out();
    let workspace = Workspace::new(ws);
    let images = ImageCache::new();
    let anchor = koru_draw::geometry::Rect::new(100.0, 100.0, 20.0, 20.0);

    let frame = |view: &mut BuiltInDocument, pointer: (f32, f32)| {
        let mut scene = Scene::new();
        View::draw(
            view,
            &mut scene,
            &workspace,
            &images,
            Affine::IDENTITY,
            true,
            pointer,
        );
    };

    // A bare caret: the title, its `…`, and the placeholder-free body.
    frame(&mut view, (0.0, 0.0));

    // A hovered row, so the `+` and the grip are in the scene.
    let (px, py) = plus_at(&view, 1);
    view.mouse_move(px, py);
    frame(&mut view, (px, py));

    // A selection, so the bubble is — with the pointer over one of its buttons.
    view.editor.view.active = true;
    view.body_mut().focus = 1;
    view.place_selection_pair((0, 3));
    view.reflow();
    // A frame first: the bubble's clock is started by drawing, so asking for
    // the panel before one has been drawn is asking before the selection has
    // been noticed.
    frame(&mut view, (0.0, 0.0));
    let panel = view.bubble_panel().expect("a selection has a bubble");
    frame(&mut view, panel.centre());

    // And each menu in turn, with the pointer on one of its rows.
    for open in [
        OpenMenu::Page(page_menu::open(anchor)),
        OpenMenu::Block(blockmenu::open(anchor)),
        OpenMenu::More(blockmenu::open(anchor)),
    ] {
        view.menu = Some(open);
        let rows = view.menu_shape();
        let r = open
            .menu()
            .row_rect(&rows, view.editor.view.viewport, rows.len() - 1);
        view.mouse_move(r.centre().0, r.centre().1);
        frame(&mut view, r.centre());
    }

    // An empty document: the title placeholder and the body's, together.
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(
            koru_core::schema::Parent::Root,
            koru_core::node::Node::document("").unwrap(),
        )
        .unwrap();
    let mut blank = BuiltInDocument::open(&ws, doc);
    Editing::set_viewport(&mut blank, 900.0, 560.0);
    blank.reflow();
    let empty = Workspace::new(ws);
    let mut scene = Scene::new();
    View::draw(
        &mut blank,
        &mut scene,
        &empty,
        &images,
        Affine::IDENTITY,
        true,
        (0.0, 0.0),
    );
}

/// The bubble waits for the drag that is making the selection to finish.
///
/// A selection is *made* by dragging, and every frame of that drag is a
/// different selection — one of which may be crossing into a second block,
/// where there is no bubble at all. Appearing while the pointer is still down
/// put a dark bar under it that flickered and moved; the release is the moment
/// the selection means something.
#[test]
fn the_bubble_waits_for_the_drag_to_finish() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let (mut ws, mut view) = laid_out();
    let workspace = Workspace::new(WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap());
    let images = ImageCache::new();
    let frame = |view: &mut BuiltInDocument| {
        let mut scene = Scene::new();
        View::draw(
            view,
            &mut scene,
            &workspace,
            &images,
            Affine::IDENTITY,
            true,
            (0.0, 0.0),
        );
    };

    view.editor.view.active = true;
    view.body_mut().focus = 1;
    view.place_selection_pair((0, 3));
    view.reflow();

    view.editor.view.dragging = true;
    frame(&mut view);
    assert!(
        view.bubble_panel().is_none(),
        "not while the button is down"
    );

    // The release is what shows it — no clock, no wait.
    Editing::mouse_up(&mut view, &mut ws);
    frame(&mut view);
    assert!(view.bubble_panel().is_some(), "and then it appears");
    assert!(
        !view.animating(),
        "with no animation left running — ADR-26's rule"
    );

    // Collapsing the selection takes it away again.
    view.place_selection_pair((2, 2));
    view.reflow();
    frame(&mut view);
    assert!(view.bubble_panel().is_none());
}

/// Selecting only emoji is allowed, but the formatting bubble has nothing to
/// offer — marks and TURN INTO do not apply — so it stays hidden.
#[test]
fn an_emoji_only_selection_shows_no_formatting_bubble() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(
            koru_core::schema::Parent::Root,
            koru_core::node::Node::document("e.md").unwrap(),
        )
        .unwrap();
    ws.insert(
        koru_core::schema::Parent::Node(doc),
        koru_core::node::Node::paragraph("👋🏻").unwrap(),
    )
    .unwrap();
    let mut view = BuiltInDocument::open(&ws, doc);
    Editing::set_viewport(&mut view, 900.0, 560.0);
    view.reflow();

    let workspace = Workspace::new(ws);
    let images = ImageCache::new();
    let frame = |view: &mut BuiltInDocument| {
        let mut scene = Scene::new();
        View::draw(
            view,
            &mut scene,
            &workspace,
            &images,
            Affine::IDENTITY,
            true,
            (0.0, 0.0),
        );
    };

    view.editor.view.active = true;
    // Body row is 1 (title is 0).
    view.body_mut().focus = 1;
    let len = view.body().rows[1].text.len();
    view.place_selection_pair((0, len));
    view.reflow();
    frame(&mut view);
    assert!(
        view.bubble_panel().is_none(),
        "emoji-only selection must not open the formatting bubble"
    );
    assert!(
        view.body().rows[1].selection_is_emoji_only(),
        "precondition: the selection is pure emoji"
    );
}

/// **The bubble does not move once it is up.** Its place comes from the
/// selection's glyphs, and pressing Bold re-shapes them — so a bar that
/// re-measured every frame slid sideways out from under the pointer that had
/// just pressed it.
#[test]
fn the_bubble_stays_put_when_the_words_under_it_change_width() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let (ws, mut view) = laid_out();
    let mut workspace = Workspace::new(ws);
    let images = ImageCache::new();

    view.editor.view.active = true;
    view.body_mut().focus = 1;
    view.place_selection_pair((0, 3));
    view.reflow();

    let frame = |view: &mut BuiltInDocument, workspace: &Workspace| {
        let mut scene = Scene::new();
        View::draw(
            view,
            &mut scene,
            workspace,
            &images,
            Affine::IDENTITY,
            true,
            (0.0, 0.0),
        );
    };
    frame(&mut view, &workspace);
    let before = view.bubble_panel().expect("a selection has a bubble");

    // Bold the selection: the same range, wider glyphs.
    Editing::toggle_mark(&mut view, &mut workspace.ws, koru_core::Mark::Bold);
    view.reflow();
    frame(&mut view, &workspace);
    assert_eq!(
        view.bubble_panel(),
        Some(before),
        "the same selection keeps the place it was given"
    );

    // A *different* selection is a different bubble, and may land elsewhere.
    view.place_selection_pair((0, 9));
    view.reflow();
    frame(&mut view, &workspace);
    assert!(view.bubble_panel().is_some());
}

/// A rapid second click on Bold is a double-click to the shell. The bubble must
/// swallow it the way mouse_down does, or the editor word-selects the line under
/// the bar and drops the selection the bubble is for.
#[test]
fn a_double_click_on_the_bubble_does_not_steal_the_selection() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let (ws, mut view) = laid_out();
    let mut workspace = Workspace::new(ws);
    let images = ImageCache::new();

    view.editor.view.active = true;
    view.body_mut().focus = 1;
    view.place_selection_pair((0, 3));
    view.reflow();

    let mut scene = Scene::new();
    View::draw(
        &mut view,
        &mut scene,
        &workspace,
        &images,
        Affine::IDENTITY,
        true,
        (0.0, 0.0),
    );
    let panel = view.bubble_panel().expect("a selection has a bubble");
    let bold = bubble::button_rect(panel, bubble::Button::Cmd(docview::Command::Bold))
        .expect("Bold is on the bar");
    let (x, y) = bold.centre();

    assert!(BuiltInDocument::mouse_down(
        &mut view,
        &mut workspace.ws,
        x,
        y,
        false
    ));
    assert!(
        BuiltInDocument::mouse_double_click(&mut view, &workspace.ws, x, y),
        "the bubble claims the double-click"
    );
    assert!(
        Editing::has_text_selection(&view.editor),
        "the selection the bar acts on is still there"
    );
    assert_eq!(view.body().focus, 1, "focus did not jump to another row");
}

/// **A document's name takes no inline formatting, and nothing offers it any.**
///
/// The name is a name: bolding half of it, or highlighting a word, is
/// formatting every other view of that document — a tab label, a tree row, a
/// pill — renders as plain text anyway.
#[test]
fn the_title_takes_no_marks_and_shows_no_bubble() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let (ws, mut view) = laid_out();
    let mut workspace = Workspace::new(ws);
    let images = ImageCache::new();
    let doc = view.body().rows[0].id;

    view.editor.view.active = true;
    view.body_mut().focus = 0;
    view.place_selection_pair((0, 4));
    view.reflow();

    Editing::toggle_mark(&mut view, &mut workspace.ws, koru_core::Mark::Bold);
    Editing::act(&mut view, &mut workspace.ws, Action::CycleHighlight);
    Editing::act(&mut view, &mut workspace.ws, Action::ClearFormatting);
    assert!(
        workspace
            .ws
            .runs(doc)
            .unwrap_or_default()
            .iter()
            .all(|r| !r.bold && !r.italic && r.style.highlight.is_none()),
        "the title carries no marks"
    );

    let mut scene = Scene::new();
    View::draw(
        &mut view,
        &mut scene,
        &workspace,
        &images,
        Affine::IDENTITY,
        true,
        (0.0, 0.0),
    );
    assert!(
        view.bubble_panel().is_none(),
        "and nothing floats over it offering them"
    );

    // A body row is unaffected.
    view.body_mut().focus = 1;
    view.place_selection_pair((0, 3));
    view.reflow();
    let mut scene = Scene::new();
    View::draw(
        &mut view,
        &mut scene,
        &workspace,
        &images,
        Affine::IDENTITY,
        true,
        (0.0, 0.0),
    );
    assert!(view.bubble_panel().is_some());
}

/// **`Ctrl+.` summons the bubble at a bare caret**, which is the only way to
/// reach the block dropdown and the three alignments from the keyboard: none of
/// them acts on a range, so none of them is reachable through the selection
/// that otherwise conjures the bar.
///
/// The inline half is live too, and that is not a special case — an empty range
/// arms `Paragraph::pending_marks` for the next keystroke, so pressing Bold
/// here does exactly what `Ctrl+B` at the same caret already did.
#[test]
fn ctrl_period_summons_the_bubble_at_a_bare_caret() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let (ws, mut view) = laid_out();
    let mut workspace = Workspace::new(ws);
    let images = ImageCache::new();

    let frame = |view: &mut BuiltInDocument, workspace: &Workspace| {
        let mut scene = Scene::new();
        View::draw(
            view,
            &mut scene,
            workspace,
            &images,
            Affine::IDENTITY,
            true,
            (0.0, 0.0),
        );
    };

    view.editor.view.active = true;
    view.body_mut().focus = 1;
    view.place_selection_pair((2, 2));
    view.reflow();
    frame(&mut view, &workspace);
    assert!(
        view.bubble_panel().is_none(),
        "a bare caret conjures nothing on its own"
    );

    let ctrl = koru_keys::PRIMARY;
    assert!(
        view.key(&mut workspace.ws, &ch("."), Some("."), ctrl),
        "the chord is claimed by the document, not passed on"
    );
    frame(&mut view, &workspace);
    assert!(
        view.bubble_panel().is_some(),
        "Ctrl+. puts one at the caret"
    );

    // Typing retires a summoned bubble too: it is no longer an action surface
    // for the editing state once the caret starts changing the text.
    view.key(&mut workspace.ws, &ch("x"), Some("x"), NONE);
    view.reflow();
    frame(&mut view, &workspace);
    assert!(view.bubble_panel().is_none(), "typing hides it");

    assert!(
        view.key(&mut workspace.ws, &named(NamedKey::Escape), None, NONE),
        "Escape is still handled after the bubble is gone"
    );
    frame(&mut view, &workspace);
    assert!(view.bubble_panel().is_none());
}

/// The chord obeys every veto the selection does. A title takes no marks and a
/// code block is coloured by the syntax engine from its text alone, so neither
/// gets a bar offering to format it — summoned or not.
#[test]
fn a_summoned_bubble_still_refuses_the_title() {
    use koru_draw::ImageCache;
    use koru_workspace::Workspace;
    use vello::Scene;
    use vello::kurbo::Affine;

    let (ws, mut view) = laid_out();
    let mut workspace = Workspace::new(ws);
    let images = ImageCache::new();

    view.editor.view.active = true;
    view.body_mut().focus = 0;
    view.place_selection_pair((1, 1));
    view.reflow();
    view.key(&mut workspace.ws, &ch("."), Some("."), koru_keys::PRIMARY);

    let mut scene = Scene::new();
    View::draw(
        &mut view,
        &mut scene,
        &workspace,
        &images,
        Affine::IDENTITY,
        true,
        (0.0, 0.0),
    );
    assert!(
        view.bubble_panel().is_none(),
        "the document's name is a name, and nothing offers to bold it"
    );
}

/// **A document's name takes an emoji, its own attributes, and reminders, and
/// nothing else `/` offers.**
///
/// It is ordinary text — `:smile` and `Ctrl+Shift+Space` already reached it —
/// but `/` was refused outright on the grounds that a title is not a place
/// blocks are made. True of blocks, and true of pills: a name is drawn as plain
/// text in a tab, a tree row and every pill pointing at it, so an anchor
/// character in one would show as a stray glyph in all three. An emoji is
/// neither — and neither is an attribute, which is metadata on the node rather
/// than content inside the name ([ADR-210](../../../docs/decisions/README.md)).
#[test]
fn the_title_offers_emoji_and_its_attributes_from_the_slash_menu() {
    use koru_editor::components::Overlay;
    use koru_editor::components::command_menu::Command;

    let (mut ws, mut view) = laid_out();
    view.editor.view.active = true;
    view.body_mut().focus = 0;
    view.place_selection_pair((0, 0));
    view.reflow();

    view.key(&mut ws, &ch("/"), Some("/"), NONE);
    let Overlay::Command(menu) = &view.editor.view.overlay else {
        panic!("`/` opens the command menu on a title");
    };
    assert_eq!(
        menu.hits,
        vec![
            Command::Attr,
            Command::Color,
            Command::Complete,
            // Delegating writes nothing into the document, so it survives the
            // block gate and the pill gate the way Emoji and Dictate do: a
            // page's title is a reasonable place to hand its work over from.
            Command::Delegate,
            Command::Dictate,
            Command::Due,
            Command::Emoji,
            Command::Priority,
            Command::Remind,
            Command::Tag,
            // A page is task-capable (SPEC §4's `Kind::Document`), so the
            // title's `/` offers it like any other row's — and a list.
            Command::Task,
            Command::TaskList,
        ]
    );

    // `@` stays refused: a contact is a pill.
    view.editor.view.overlay = Overlay::None;
    view.place_selection_pair((0, 0));
    view.key(&mut ws, &ch("@"), Some("@"), NONE);
    assert!(
        !view.editor.view.overlay.is_open(),
        "a name carries no references"
    );

    // A body row still gets the whole menu.
    view.body_mut().focus = 1;
    view.place_selection_pair((0, 0));
    view.reflow();
    view.key(&mut ws, &ch("/"), Some("/"), NONE);
    let Overlay::Command(menu) = &view.editor.view.overlay else {
        panic!("and prose is unchanged");
    };
    assert!(menu.hits.len() > 6);
}

/// Type `text` into the view a character at a time, the way a person does.
fn type_into(view: &mut BuiltInDocument, ws: &mut WorkspaceDoc, text: &str) {
    for c in text.chars() {
        let s = c.to_string();
        view.key(ws, &ch(&s), Some(&s), NONE);
    }
}

/// Put the caret in the title row, ready for a trigger character.
fn caret_in_title(view: &mut BuiltInDocument) {
    view.editor.view.active = true;
    view.body_mut().focus = 0;
    view.place_selection_pair((0, 0));
    view.reflow();
    assert!(
        view.editor.body.focus_is_title(),
        "the caret is on the name"
    );
}

/// The value of `key` on `node`, as stored.
fn attr_of(ws: &WorkspaceDoc, node: koru_core::NodeId, key: &str) -> Option<AttrValue> {
    ws.read(node)
        .ok()?
        .content
        .attrs()
        .iter()
        .find(|a| a.role == key)
        .map(|a| a.value.clone())
}

/// **`/priority` works from inside the name, end to end.** The gate used to
/// read `attrs: !title`, so the five attribute commands were refused on the
/// one row whose node the explore sidebar was already writing to
/// ([ADR-210](../../../docs/decisions/README.md)).
///
/// Driven as a person drives it: caret on the title, `/priority`, Enter to
/// open the picker, Enter to take the top level. Then the two things that
/// prove it landed — the attribute is on the **document root**, and the title
/// row wears the chip.
#[test]
fn slash_priority_on_the_title_sets_it_on_the_document_root() {
    use koru_core::attributes::KEY_PRIORITY;
    use koru_editor::components::Overlay;

    let (mut ws, mut view) = laid_out();
    ws.enable_undo();
    let root = view.editor.body.root.expect("a document root");
    caret_in_title(&mut view);

    type_into(&mut view, &mut ws, "/priority");
    view.key(&mut ws, &named(NamedKey::Enter), None, NONE);
    assert!(
        matches!(view.editor.view.overlay, Overlay::Priority(_)),
        "the priority picker opened from the title"
    );
    view.key(&mut ws, &named(NamedKey::Enter), None, NONE);

    assert!(
        matches!(attr_of(&ws, root, KEY_PRIORITY), Some(AttrValue::Text(_))),
        "the level landed on the document's root node"
    );
    assert_eq!(
        ws.read(root).unwrap().content.as_text(),
        Some("spec.md"),
        "the /priority trigger text went back out of the name"
    );
    view.reflow();
    assert!(
        !view.editor.body.rows[0].chips.is_empty(),
        "the title row draws the chip it earned"
    );

    // **One undo step for the whole gesture.** The trigger character's
    // removal and the attribute were one `edit_group`, so a single Ctrl+Z
    // puts both back — the level is gone and the `/` is in the name again,
    // exactly the state the picker opened over.
    assert!(ws.undo().unwrap_or(false), "there was a step to undo");
    view.body_mut().rebuild(&ws);
    assert!(attr_of(&ws, root, KEY_PRIORITY).is_none());
    assert_eq!(ws.read(root).unwrap().content.as_text(), Some("/spec.md"));
}

/// **The sidebar and the document write the same attribute on the same
/// node.** That disagreement *was* the bug: the explore rows' Attributes
/// submenu applies to a document's root node — which is the title row — while
/// the row itself refused the command. Two documents, one set each way, and
/// the reads have to match.
#[test]
fn the_sidebar_and_the_title_row_set_the_same_attribute() {
    use koru_core::attributes::KEY_PRIORITY;
    use koru_editor::AttrPicker;

    // Through the sidebar: the picker opens over the root with no trigger
    // text behind it.
    let (mut ws_side, mut side) = laid_out();
    let side_root = side.editor.body.root.expect("a document root");
    side.editor.view.active = true;
    assert!(side.open_document_attr_picker(&ws_side, AttrPicker::Priority));
    side.key(&mut ws_side, &named(NamedKey::Enter), None, NONE);
    let from_sidebar = attr_of(&ws_side, side_root, KEY_PRIORITY);

    // From inside the name.
    let (mut ws_doc, mut doc) = laid_out();
    let doc_root = doc.editor.body.root.expect("a document root");
    caret_in_title(&mut doc);
    type_into(&mut doc, &mut ws_doc, "/priority");
    doc.key(&mut ws_doc, &named(NamedKey::Enter), None, NONE);
    doc.key(&mut ws_doc, &named(NamedKey::Enter), None, NONE);
    let from_document = attr_of(&ws_doc, doc_root, KEY_PRIORITY);

    assert!(from_sidebar.is_some(), "the sidebar path set something");
    assert_eq!(
        from_sidebar, from_document,
        "one attribute, one node, two doors"
    );
}

/// Give the view a tag table, the way the host pushes one, and hand back the
/// one id in it.
fn seed_tag(view: &mut BuiltInDocument, name: &str) -> koru_core::ids::TagId {
    let id = koru_core::ids::TagId::new();
    view.editor.view.tags.replace([(
        id,
        koru_editor::chips::TagFace {
            name: name.to_owned(),
            color: None,
        },
    )]);
    id
}

/// **`#tag` works from inside the name, end to end.** The shorthand used to be
/// refused on a title while `/tag` was allowed there — two doors to one
/// attribute disagreeing, which is the inconsistency
/// [ADR-210](../../../docs/decisions/README.md) exists to remove.
#[test]
fn a_hash_tag_typed_in_the_name_tags_the_document() {
    use koru_editor::components::Overlay;

    let (mut ws, mut view) = laid_out();
    ws.enable_undo();
    let root = view.editor.body.root.expect("a document root");
    let tag = seed_tag(&mut view, "mvp");
    caret_at_title_end(&mut view);

    type_into(&mut view, &mut ws, " #m");
    assert!(
        matches!(view.editor.view.overlay, Overlay::Tag(_)),
        "the chooser armed inside the name"
    );
    view.key(&mut ws, &named(NamedKey::Enter), None, NONE);

    assert!(
        ws.read(root).unwrap().tag_ids().any(|t| t == tag),
        "the tag landed on the document's root node"
    );
    assert_eq!(
        ws.read(root).unwrap().content.as_text(),
        Some("spec.md "),
        "the `#m` went back out of the name (the space that armed it stays, \
         and `sanitize_title_row` trims it on the way out)"
    );
    view.reflow();
    assert!(
        !view.editor.body.rows[0].chips.is_empty(),
        "the title row draws the chip it earned"
    );

    // One undo step for the whole gesture, as everywhere else.
    assert!(ws.undo().unwrap_or(false), "there was a step to undo");
    view.body_mut().rebuild(&ws);
    assert!(!ws.read(root).unwrap().tag_ids().any(|t| t == tag));
    assert_eq!(ws.read(root).unwrap().content.as_text(), Some("spec.md #m"));
}

/// A stray `#` in a name is still a name. The chooser's own contract is the
/// guard — no table match *and* fewer than two typed characters never opens
/// it — so `C# notes` types straight through.
#[test]
fn a_stray_hash_in_a_name_opens_nothing() {
    let (mut ws, mut view) = laid_out();
    seed_tag(&mut view, "mvp");
    caret_at_title_end(&mut view);

    type_into(&mut view, &mut ws, " C# notes");
    assert!(
        !view.editor.view.overlay.is_open(),
        "a name with a sharp in it is not a tag gesture"
    );
    let root = view.editor.body.root.expect("a document root");
    assert_eq!(
        ws.read(root).unwrap().content.as_text(),
        Some("spec.md C# notes")
    );
}

/// Put the caret at the **end** of the name, where typing continues it.
fn caret_at_title_end(view: &mut BuiltInDocument) {
    view.editor.view.active = true;
    view.body_mut().focus = 0;
    let end = view.editor.body.rows[0].text.len();
    view.place_selection_pair((end, end));
    view.reflow();
    assert!(
        view.editor.body.focus_is_title(),
        "the caret is on the name"
    );
    assert_eq!(view.editor.body.rows[0].caret(), end);
}

/// Select rows `from`..=`to`, the way a drag down the page does.
fn select_rows(view: &mut BuiltInDocument, from: usize, to: usize) {
    let anchor = view.editor.body.rows[from].id;
    view.editor.body.clear_selection();
    view.editor.body.anchor = Some(anchor);
    view.editor.body.focus = to;
    view.editor.body.mark_selection();
    view.editor.view.active = true;
}

/// Right-click on row `i`, which is inside the selection and so keeps it.
fn context_menu_on_row(view: &mut BuiltInDocument, ws: &mut WorkspaceDoc, i: usize) {
    let y = view.editor.body.rows[i].y - view.editor.view.scroll + 2.0;
    assert!(View::context_menu(view, ws, 300.0, y));
}

#[test]
fn context_menu_add_reminder_routes_the_exact_right_clicked_object() {
    let (mut ws, mut view) = laid_out();
    let clicked = view.editor.body.rows[1].id;
    let focus_end = view.editor.body.rows[2].id;
    let before = view
        .editor
        .body
        .rows
        .iter()
        .map(|row| (row.id, row.text.clone()))
        .collect::<Vec<_>>();

    // Preserve a multi-row selection whose focus end is not the row being
    // right-clicked. Creation belongs to the object under the pointer.
    select_rows(&mut view, 1, 2);
    context_menu_on_row(&mut view, &mut ws, 1);
    assert_eq!(view.editor.body.focused_id(), Some(focus_end));

    let label = koru_i18n::t(
        koru_i18n::Lang::for_system(koru_window::system_locale::system_locale().as_deref()),
        koru_i18n::Key::ReminderAdd,
    );
    let add = view
        .menu_shape()
        .iter()
        .position(|row| {
            matches!(
                row,
                menu::Row::Item {
                    label: row_label,
                    enabled: true,
                    ..
                } if *row_label == label
            )
        })
        .unwrap();

    view.choose_menu(&mut ws, add, None);
    let request = Editing::take_reminder_request(&mut view).unwrap();
    assert_eq!(request.identity.node, clicked);
    assert!(request.phrase.is_empty());
    assert_eq!(request.due_anchor, None);
    assert!(view.editor.view.pending_reminder_text.is_empty());
    assert_eq!(
        view.editor
            .body
            .rows
            .iter()
            .map(|row| (row.id, row.text.clone()))
            .collect::<Vec<_>>(),
        before,
    );
}

#[test]
fn right_clicking_a_reminder_pill_opens_only_its_exact_context() {
    let (mut ws, mut view) = laid_out();
    let node = view.editor.body.rows[1].id;
    let id = "exact-reminder";
    assert!(Editing::set_reminder_chips(
        &mut view,
        std::collections::HashMap::from([(
            node,
            vec![koru_editor::reminders::InlineChip {
                reminder_id: id.into(),
                label: "Today, 6:00 PM".into(),
                paused: false,
                snoozed: false,
                attention: false,
                handled: false,
            }],
        )]),
    ));
    let anchor = Editing::reminder_anchor(&mut view, node, Some(id)).unwrap();
    let (x, y) = anchor.centre();

    assert!(View::context_menu(&mut view, &mut ws, x, y));
    assert_eq!(
        view.context_pill_kind(),
        context_menu::PillContext::Reminder {
            paused: false,
            handled: false,
        }
    );
    assert_eq!(view.menu_shape().len(), 4);
    assert_eq!(
        view.context_toolbar_panel(),
        None,
        "formatting controls do not act on reminder metadata"
    );

    view.choose_menu(&mut ws, 1, None);
    assert_eq!(
        Editing::take_reminder_context_request(&mut view),
        Some(koru_editor::reminders::ContextRequest {
            node,
            reminder_id: id.into(),
            action: koru_editor::reminders::ContextAction::Pause,
        })
    );

    assert!(Editing::set_reminder_chips(
        &mut view,
        std::collections::HashMap::from([(
            node,
            vec![koru_editor::reminders::InlineChip {
                reminder_id: id.into(),
                label: "Today, 6:00 PM".into(),
                paused: false,
                snoozed: false,
                attention: false,
                handled: true,
            }],
        )]),
    ));
    let anchor = Editing::reminder_anchor(&mut view, node, Some(id)).unwrap();
    let (x, y) = anchor.centre();
    assert!(View::context_menu(&mut view, &mut ws, x, y));
    assert_eq!(
        view.context_pill_kind(),
        context_menu::PillContext::Reminder {
            paused: false,
            handled: true,
        }
    );
    assert_eq!(view.menu_shape().len(), 3);
    view.choose_menu(&mut ws, 2, None);
    assert_eq!(
        Editing::take_reminder_context_request(&mut view),
        Some(koru_editor::reminders::ContextRequest {
            node,
            reminder_id: id.into(),
            action: koru_editor::reminders::ContextAction::Delete,
        })
    );
}

/// Whether the four attribute rows can be chosen, and what they say if not.
fn attribute_rows(view: &BuiltInDocument) -> (Vec<bool>, Vec<menu::Trailing>) {
    let shape = view.menu_shape();
    let mut enabled = Vec::new();
    let mut notes = Vec::new();
    for i in [10, 11, 12, 13] {
        match shape[i] {
            menu::Row::Item {
                enabled: on,
                trailing,
                ..
            } => {
                enabled.push(on);
                notes.push(trailing);
            }
            ref other => panic!("row {i} is not an item: {other:?}"),
        }
    }
    (enabled, notes)
}

/// **A chip is not a run of text, and its menu is not the paragraph's.**
///
/// Right-clicking a due or priority pill raised the formatting toolbar, then
/// Cut / Copy / Paste / Paste as plain / Move to document, then the four
/// attribute rows that act on the whole *block* — so the menu offered to
/// change the row's colour and to paste over it, and offered nothing at all
/// about the value under the pointer.
///
/// Two rows now, on the attachment pill's precedent, and no toolbar: edit this
/// value, and clear it. Both go through `press_chip`, which is the call a
/// left-click on the chip makes — *Edit* is that click and *Clear* is the
/// chip's own remove lane — so the menu cannot describe a gesture it does not
/// perform.
#[test]
fn right_clicking_a_chip_offers_that_chip_and_not_the_paragraph() {
    let (mut ws, mut view) = laid_out();
    let row = view.editor.body.rows[1].id;
    koru_core::tasks::set_due(
        &mut ws,
        row,
        koru_core::time::Temporal::Date(koru_core::time::DateOnly::parse("2099-01-02").unwrap()),
    )
    .expect("a due, which is one of the two ways a task is made");
    Editing::adopt(&mut view, &ws);
    view.reflow();

    // Aimed through the same geometry the renderer used, so the test cannot
    // pass by right-clicking empty space beside the chip.
    let boxes = koru_editor::chips::chip_boxes(
        &view.editor.body.rows[1],
        &mut view.editor.view.engine,
        &view.editor.view.tags,
        &view.editor.view.lists,
    );
    let chip = boxes.first().expect("the due chip").rect;
    let x = view.editor.view.content_x + chip.x + chip.w * 0.5;
    let y = chip.y + chip.h * 0.5 - view.editor.view.scroll;
    assert!(View::context_menu(&mut view, &mut ws, x, y));

    let shape = view.menu_shape();
    let labels: Vec<String> = shape
        .iter()
        .map(|row| match row {
            menu::Row::Item { label, .. } => label.to_string(),
            _ => "-".to_owned(),
        })
        .collect();
    assert_eq!(
        labels,
        vec![
            "Go to task".to_owned(),
            "Complete".to_owned(),
            "Edit due date\u{2026}".to_owned(),
            "Clear due date".to_owned(),
            "-".to_owned(),
            "Remove from Tasks".to_owned(),
            "Delete row".to_owned(),
        ],
        "the chip's own rows, and nothing about the paragraph"
    );
    assert!(
        !labels.iter().any(|l| l == "Paste" || l == "Color\u{2026}"),
        "no clipboard rows and no block attribute rows: {labels:?}"
    );

    // Clearing goes through the chip's own remove lane, so the value is gone
    // from the document rather than from the drawing. Row 3 since the due
    // chip's menu leads with *Go to task* and *Complete*.
    view.choose_menu(&mut ws, 3, None);
    assert!(
        koru_core::tasks::TaskFields::of(&ws.read(row).unwrap())
            .due
            .is_none(),
        "the due is cleared through the ordinary attribute commit"
    );
}

/// **The three selection shapes, through the right-click menu.**
///
/// The title alone is the document; body rows alone are those rows; the two
/// together have no honest answer, so the rows dim and say why. Before this
/// the mixed case applied to the body and silently skipped the document —
/// three rows selected and two of them changed
/// ([ADR-210](../../../docs/decisions/README.md)).
#[test]
fn the_context_menu_offers_attributes_only_on_an_unambiguous_selection() {
    let (mut ws, mut view) = laid_out();

    // The title alone.
    select_rows(&mut view, 0, 0);
    context_menu_on_row(&mut view, &mut ws, 0);
    let (enabled, _) = attribute_rows(&view);
    assert_eq!(enabled, vec![true; 4], "a name takes its own attributes");

    // Body rows alone, more than one of them.
    select_rows(&mut view, 1, 2);
    context_menu_on_row(&mut view, &mut ws, 2);
    let (enabled, _) = attribute_rows(&view);
    assert_eq!(
        enabled,
        vec![true; 4],
        "several rows in one undo step stays"
    );

    // The name and the body together.
    select_rows(&mut view, 0, 2);
    context_menu_on_row(&mut view, &mut ws, 1);
    let (enabled, notes) = attribute_rows(&view);
    assert_eq!(enabled, vec![false; 4], "all four dim, not tags alone");
    for note in notes {
        assert!(
            matches!(note, menu::Trailing::Note(_)),
            "a dimmed row that does not explain itself is a dead end"
        );
    }
}

/// **The refusal is not only in the drawing.** A dimmed row cannot be clicked,
/// but the picker's opener is the thing that must not fire — otherwise any
/// second route to it would apply the half-answer the menu just refused.
#[test]
fn a_mixed_selection_opens_no_attribute_picker() {
    let (ws, mut view) = laid_out();
    seed_tag(&mut view, "mvp");
    select_rows(&mut view, 0, 2);

    for which in [
        koru_editor::AttrPicker::Tags,
        koru_editor::AttrPicker::Color,
        koru_editor::AttrPicker::Due,
        koru_editor::AttrPicker::Priority,
    ] {
        view.open_selection_attr_picker(&ws, which);
        assert!(
            !view.editor.view.overlay.is_open(),
            "{which:?} opened over a mixed selection"
        );
    }

    // Body rows alone still open it — the refusal is about the shape of the
    // selection, not about there being one.
    select_rows(&mut view, 1, 2);
    view.open_selection_attr_picker(&ws, koru_editor::AttrPicker::Tags);
    assert!(view.editor.view.overlay.is_open());
}

/// **The gap between the button and its panel is not "outside".** This menu
/// hangs four pixels below the button it came from, and crossing that strip was
/// closing it mid-reach — you had to be quick to get there, which is not a
/// thing an interface should ask.
#[test]
fn the_gap_between_the_button_and_its_panel_keeps_the_menu_open() {
    let (ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    let btn = menu_button(&view);
    view.mouse_move(btn.x + btn.w * 0.5, btn.y + btn.h * 0.5);
    assert!(view.window_menu_open());

    let vp = view.editor.view.viewport;
    let rows = view.menu_shape();
    let panel = view.menu.expect("open").menu().panel(&rows, vp);
    assert!(panel.y > btn.y + btn.h, "there is a gap to cross");

    // Straight down the middle of the strip between them.
    let mid_y = (btn.y + btn.h + panel.y) * 0.5;
    view.mouse_move(btn.x + btn.w * 0.5, mid_y);
    assert!(view.window_menu_open(), "the menu survived the reach");

    // The same crossing on the panel's far side, for a diagonal approach.
    view.mouse_move(panel.x + 4.0, mid_y);
    assert!(view.window_menu_open(), "a diagonal reach counts too");

    // Well below the panel is still outside.
    view.mouse_move(panel.x + 4.0, panel.y + panel.h + 40.0);
    assert!(!view.window_menu_open());
}

/// **F2 renames the document from a standalone window.** It was the container's
/// chord alone, which went unnoticed while the name was the first line of the
/// page and could be typed over; a window keeps its name in the title bar, so
/// this is the only way to change it from the window showing it.
#[test]
fn f2_opens_the_rename_prompt_in_a_standalone_window() {
    let (mut ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    assert!(
        !view.editor.view.overlay.is_open(),
        "no prompt to begin with"
    );

    assert!(view.key(
        &mut ws,
        &Key::Named(NamedKey::F2),
        None,
        ModifiersState::empty()
    ));
    assert!(
        matches!(
            view.editor.view.overlay,
            koru_editor::components::Overlay::Name(_)
        ),
        "the name prompt opened"
    );
}

/// ...and it still works with the window menu up: that menu is about the
/// window, and this is about the document inside it.
#[test]
fn f2_works_with_the_window_menu_open() {
    let (mut ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    let btn = menu_button(&view);
    view.mouse_move(btn.x + btn.w * 0.5, btn.y + btn.h * 0.5);
    assert!(view.window_menu_open());

    assert!(view.key(
        &mut ws,
        &Key::Named(NamedKey::F2),
        None,
        ModifiersState::empty()
    ));
    assert!(
        matches!(
            view.editor.view.overlay,
            koru_editor::components::Overlay::Name(_)
        ),
        "the name prompt opened"
    );
}

/// **A local rename reaches the title bar.** `adopt` is what a *peer's* rename
/// arrives through; a local one does not come back round that way, which is why
/// the container re-titles its tabs as it routes the key. The bar had the same
/// hole: the explore column updated and the window's own title did not.
#[test]
fn renaming_the_document_retitles_the_window_immediately() {
    let (mut ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    assert_eq!(view.title, "spec.md");

    let root = view.editor.body.root.expect("a root");
    ws.set_text(root, "Renamed").expect("rename");
    // A key routed through the view is what a commit rides in on — any key will
    // do here, because the refresh is not conditional on which one it was.
    view.key(
        &mut ws,
        &Key::Named(NamedKey::ArrowDown),
        None,
        ModifiersState::empty(),
    );
    assert_eq!(view.title, "Renamed", "the bar caught up");
}

/// An unnamed document says *Untitled* rather than showing an empty bar.
#[test]
fn an_unnamed_document_titles_its_window_untitled() {
    let (mut ws, view) = document();
    let mut view = view.owns_the_window(&ws);
    let root = view.editor.body.root.expect("a root");
    ws.set_text(root, "   ").expect("blank it");
    // `View::adopt`, spelled out: `Editing` names one too, and which you get
    // depends on what is imported — the hazard AGENTS.md warns about.
    View::adopt(&mut view, &ws);
    assert_eq!(view.title, "Untitled");
}

#[test]
fn insert_attachment_puts_a_pill_on_the_row_and_siblings_for_extras() {
    let (mut ws, mut view) = document();
    let root = view.editor.body.root.expect("root");
    let row = ws
        .insert(
            koru_core::schema::Parent::Node(root),
            koru_core::node::Node::paragraph("/attach").unwrap(),
        )
        .expect("row");
    View::adopt(&mut view, &ws);

    let first = koru_core::DocumentId::new();
    let second = koru_core::DocumentId::new();
    View::insert_attachment(
        &mut view,
        &mut ws,
        row,
        &[(first, "a.pdf".into()), (second, "b.png".into())],
    );

    let node = ws.read(row).expect("row");
    assert_eq!(node.content.as_text(), Some(koru_core::text::ANCHOR_STR));
    let attrs = node.content.attrs();
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].role, "attachment");
    assert_eq!(attrs[0].label.as_deref(), Some("a.pdf"));

    // A fresh document already has one empty paragraph; extras are sibling
    // rows with their own attachment pill, found by label rather than by
    // "not the trigger row".
    let children = ws.children(koru_core::schema::Parent::Node(root)).unwrap();
    let second = children.iter().copied().find(|&id| {
        ws.read(id)
            .ok()
            .and_then(|n| {
                n.content
                    .attrs()
                    .iter()
                    .find(|a| a.role == "attachment" && a.label.as_deref() == Some("b.png"))
                    .map(|_| ())
            })
            .is_some()
    });
    assert!(
        second.is_some(),
        "second file gets its own attachment pill row"
    );
}

#[test]
fn attachment_cancelled_clears_the_trigger_text() {
    let (mut ws, mut view) = document();
    let root = view.editor.body.root.expect("root");
    let row = ws
        .insert(
            koru_core::schema::Parent::Node(root),
            koru_core::node::Node::paragraph("/attach").unwrap(),
        )
        .expect("row");
    View::adopt(&mut view, &ws);
    View::attachment_cancelled(&mut view, &mut ws, row);
    assert_eq!(ws.read(row).unwrap().content.as_text(), Some(""));
}

// ---------------------------------------------------------------- completion

/// A document holding one task, and — when `child` — one unchecked checklist
/// item under it. Laid out, so a click has a rect to land in.
///
/// The task is made with `ensure_task` rather than by writing the marker,
/// because the marker key is command-only: a test that wrote it raw would be
/// testing a door the app does not have.
fn document_with_task(child: bool) -> (WorkspaceDoc, BuiltInDocument, NodeId, Option<NodeId>) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(
            koru_core::schema::Parent::Root,
            koru_core::node::Node::document("spec.md").unwrap(),
        )
        .unwrap();
    let task = ws
        .insert(
            koru_core::schema::Parent::Node(doc),
            koru_core::node::Node::task("ship it").unwrap(),
        )
        .unwrap();
    koru_core::tasks::ensure_task(&mut ws, task, koru_core::attributes::TaskReason::Explicit)
        .expect("a task");
    let item = child.then(|| {
        let item = ws
            .insert(
                koru_core::schema::Parent::Node(task),
                koru_core::node::Node::paragraph("a step").unwrap(),
            )
            .unwrap();
        // A checkbox-styled list item is SPEC §5's "unchecked checklist item",
        // which is what makes the parent a task *with structural children*.
        ws.wrap_in_list_block(item, koru_core::ListStyle::Checkbox)
            .unwrap();
        item
    });
    let mut view = BuiltInDocument::open(&ws, doc);
    Editing::set_viewport(&mut view, 900.0, 560.0);
    view.reflow();
    (ws, view, task, item)
}

fn lifecycle(ws: &WorkspaceDoc, id: NodeId) -> koru_core::attributes::Lifecycle {
    koru_core::tasks::TaskFields::of(&ws.read(id).unwrap()).lifecycle
}

/// Click the middle of `id`'s checkbox, in viewport coordinates.
fn click_checkbox(view: &mut BuiltInDocument, ws: &mut WorkspaceDoc, id: NodeId) {
    let row = view.editor.body.row(id).expect("a row for the task");
    assert!(row.is_task(), "and it draws a checkbox");
    let x = view.editor.view.content_x
        + koru_draw::geometry::checkbox_x(row.depth)
        + koru_draw::theme::CHECKBOX_SIZE / 2.0;
    let y = row.y - view.editor.view.scroll + koru_draw::theme::ROW_H / 2.0;
    assert!(View::mouse_down(view, ws, x, y, false), "the click landed");
}

/// **SPEC §5's gesture table, at the surface a person actually touches.**
///
/// A childless task and its box are one synced state in both directions; a task
/// with structural children has a box that is *only* the box, and completing it
/// is the explicit action. Both halves are one test because the difference
/// between them is the whole rule, and two tests would let one be deleted
/// without the other looking wrong.
#[test]
fn a_childless_tasks_box_completes_it_and_a_rich_tasks_box_only_ticks() {
    use koru_core::attributes::Lifecycle;

    let (mut ws, mut view, task, _) = document_with_task(false);
    click_checkbox(&mut view, &mut ws, task);
    assert_eq!(
        lifecycle(&ws, task),
        Lifecycle::Completed,
        "checking a childless task completes it"
    );
    assert!(ws.read(task).unwrap().meta.done, "and its box is checked");

    // ...and back, in the same direction the box moves.
    click_checkbox(&mut view, &mut ws, task);
    assert_eq!(
        lifecycle(&ws, task),
        Lifecycle::Open,
        "unchecking reopens it"
    );

    let (mut ws, mut view, task, _) = document_with_task(true);
    click_checkbox(&mut view, &mut ws, task);
    assert!(ws.read(task).unwrap().meta.done, "the box moves");
    assert_eq!(
        lifecycle(&ws, task),
        Lifecycle::Open,
        "but a task with structural children is only ticked, never completed"
    );
}

/// The index the Complete row sits at, asserted here rather than hard-coded
/// twice: the menu is mapped by index and a shifted map is silent.
const COMPLETE_ROW: usize = 9;

/// **Completing from the menu is the explicit action, and it cascades.**
///
/// The same click on the same row's checkbox — pinned above — moves the box and
/// leaves the task open. This is the other door, and it has to reach the
/// canonical command: the parent completes, and the unchecked step under it is
/// ticked in the same change.
#[test]
fn the_context_menu_completes_a_rich_task_and_cascades() {
    use koru_core::attributes::Lifecycle;

    let (mut ws, mut view, task, item) = document_with_task(true);
    let item = item.expect("a step under the task");
    view.editor.body.focus_on(task);
    let y = view.editor.body.row(task).expect("a row").y - view.editor.view.scroll + 2.0;
    assert!(View::context_menu(&mut view, &mut ws, 300.0, y));

    let shape = view.menu_shape();
    assert!(
        matches!(
            &shape[COMPLETE_ROW],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Complete"
        ),
        "the task offers Complete: {:?}",
        shape[COMPLETE_ROW]
    );

    view.choose_menu(&mut ws, COMPLETE_ROW, None);
    assert_eq!(lifecycle(&ws, task), Lifecycle::Completed);
    assert!(
        ws.read(task).unwrap().meta.done,
        "completing checks the box"
    );
    assert!(
        ws.read(item).unwrap().meta.done,
        "and cascades to the checkable under it"
    );
}

/// The row is a toggle over one lifecycle, so it says what it will do. A row
/// with nothing to complete dims rather than disappearing — `choice_at` is a
/// map by index, and a vanishing row would move every table command.
#[test]
fn the_menu_row_says_reopen_on_a_completed_task_and_dims_on_a_plain_row() {
    let (mut ws, mut view, task, _) = document_with_task(false);
    koru_core::tasks::complete_task(
        &mut ws,
        task,
        koru_core::tasks::LifecycleReason::ExplicitComplete,
        koru_editor::tasks::today(),
    )
    .expect("completed");
    View::adopt(&mut view, &ws);
    view.editor.body.focus_on(task);
    let y = view.editor.body.row(task).expect("a row").y - view.editor.view.scroll + 2.0;
    assert!(View::context_menu(&mut view, &mut ws, 300.0, y));
    assert!(
        matches!(
            &view.menu_shape()[COMPLETE_ROW],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Reopen"
        ),
        "a completed task offers Reopen: {:?}",
        view.menu_shape()[COMPLETE_ROW]
    );
    // And reopening from it is the explicit reopen, which never cascades down.
    view.choose_menu(&mut ws, COMPLETE_ROW, None);
    assert_eq!(lifecycle(&ws, task), koru_core::attributes::Lifecycle::Open);

    // An ordinary paragraph is not a task: the row is there, and refuses.
    let (ws, view) = laid_out();
    let mut ws = ws;
    let mut view = view;
    view.editor.body.focus_on(view.editor.body.rows[1].id);
    let y = view.editor.body.rows[1].y - view.editor.view.scroll + 2.0;
    assert!(View::context_menu(&mut view, &mut ws, 300.0, y));
    assert!(
        matches!(
            &view.menu_shape()[COMPLETE_ROW],
            menu::Row::Item {
                label,
                enabled: false,
                ..
            } if label.as_ref() == "Complete"
        ),
        "a plain row dims it: {:?}",
        view.menu_shape()[COMPLETE_ROW]
    );
}

// ------------------------------------------------ E-3: remove from Tasks

/// The index the Remove from Tasks row sits at — the design's red last row,
/// under the hairline at 13. Named once, for `COMPLETE_ROW`'s reason.
const REMOVE_ROW: usize = 17;

/// A task with a due date on it, its row rebuilt so the due chip is there to
/// be read. `spec.md` is the document, which is what the warning names.
fn document_with_a_dated_task(due: &str) -> (WorkspaceDoc, BuiltInDocument, NodeId) {
    let (mut ws, mut view, task, _) = document_with_task(false);
    koru_core::tasks::set_due(
        &mut ws,
        task,
        koru_core::time::Temporal::Date(koru_core::time::DateOnly::parse(due).unwrap()),
    )
    .expect("a due");
    view.editor.body.rebuild(&ws);
    view.reflow();
    (ws, view, task)
}

fn day(y: i32, m: u32, d: u32) -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

/// **The warning, word for word** — the design's E-3 card.
///
/// It names the task, it names the due date the removal is about, and it names
/// the document the row will still be in. It does *not* mention priority or the
/// checkbox, because both survive; naming a thing that is not happening is how
/// a warning stops being read.
///
/// The clock is pinned through `ask_remove_from_tasks_on`: "Aug 30" and "Today"
/// are the same value read on different days.
#[test]
fn the_remove_warning_names_the_task_the_due_date_and_the_document() {
    let (ws, mut view, task) = document_with_a_dated_task("2026-08-30");
    view.editor.body.focus_on(task);
    view.ask_remove_from_tasks_on(&ws, day(2026, 8, 1));

    let (confirm, subject) = view.confirm.as_ref().expect("the question is up");
    assert_eq!(
        *subject,
        Confirming::RemoveFromTasks(task),
        "and it is about the row that was clicked"
    );
    assert_eq!(confirm.title(), "Remove this task?");
    assert_eq!(
        confirm.body(),
        "\u{201C}ship it\u{201D} is due Aug 30. Removing it clears the due date on the row in \
         spec.md; the row itself stays."
    );
    assert!(
        !confirm.body().contains("priority") && !confirm.body().contains("checkbox"),
        "neither is mentioned, because neither is touched: {}",
        confirm.body()
    );

    // Asking is not doing.
    assert_eq!(lifecycle(&ws, task), koru_core::attributes::Lifecycle::Open);
    assert!(
        koru_core::tasks::is_task_node(&ws, task).unwrap(),
        "nothing has run yet"
    );
}

/// **A dateless task drops the due sentence.** SPEC §6's warning is about the
/// due date; with none there is nothing to warn about, so what is left is the
/// promise the design ends on — the row itself stays. It must not say "no due
/// date", which would be a fact about nothing.
#[test]
fn a_dateless_task_is_asked_about_without_the_due_sentence() {
    let (ws, mut view, task, _) = document_with_task(false);
    view.editor.body.rebuild(&ws);
    view.editor.body.focus_on(task);
    view.ask_remove_from_tasks_on(&ws, day(2026, 8, 1));

    let (confirm, _) = view.confirm.as_ref().expect("the question is up");
    assert_eq!(confirm.title(), "Remove this task?");
    assert_eq!(
        confirm.body(),
        "Removing \u{201C}ship it\u{201D} stops tracking it as a task in spec.md; the row \
         itself stays."
    );
    assert!(
        !confirm.body().contains("due"),
        "there is no due date to warn about: {}",
        confirm.body()
    );
}

/// **Enter means Cancel.** The confirm recipe's whole reason for existing: the
/// destructive answer has no default, so a modal appearing under somebody's
/// hands cannot read the next Enter as *yes*. Escape is the same answer, and
/// the row is still a task after both.
#[test]
fn enter_cancels_the_removal() {
    let (mut ws, mut view, task) = document_with_a_dated_task("2026-08-30");
    view.editor.body.focus_on(task);

    for key in [named(NamedKey::Enter), named(NamedKey::Escape)] {
        view.ask_remove_from_tasks_on(&ws, day(2026, 8, 1));
        assert!(view.confirm_open());
        assert!(view.key(&mut ws, &key, None, NONE), "the modal swallows it");
        assert!(!view.confirm_open(), "and closes");
        assert!(
            koru_core::tasks::is_task_node(&ws, task).unwrap(),
            "still a task after {key:?}"
        );
    }
}

/// **The whole gesture, from the menu row to one undo step.**
///
/// Tab moves the focus onto the destructive button — the only way to reach it
/// from the keyboard — and Enter then answers it. Afterwards the row is no
/// longer a task, its due is gone, and the two things SPEC §6 promises survive
/// have survived: the priority it was given and the state of its checkbox.
/// One Ctrl+Z puts all of it back together, which is what "one undo step"
/// means.
#[test]
fn removing_from_tasks_clears_the_task_and_keeps_priority_and_the_check() {
    use koru_core::attributes::{KEY_DUE, KEY_PRIORITY};

    let (mut ws, mut view, task) = document_with_a_dated_task("2026-08-30");
    ws.set_attribute(task, KEY_PRIORITY, AttrValue::Text("high".into()))
        .expect("a priority");
    view.editor.body.set_done(&mut ws, task, true);
    view.reflow();
    // Undo starts here, so the one step under test is the removal alone.
    ws.enable_undo();

    // Through the menu, the way a person reaches it.
    view.editor.body.focus_on(task);
    let y = view.editor.body.row(task).expect("a row").y - view.editor.view.scroll + 2.0;
    assert!(View::context_menu(&mut view, &mut ws, 300.0, y));
    assert!(
        matches!(
            &view.menu_shape()[REMOVE_ROW],
            menu::Row::Item {
                label,
                enabled: true,
                ..
            } if label.as_ref() == "Remove from Tasks"
        ),
        "the row: {:?}",
        view.menu_shape()[REMOVE_ROW]
    );
    view.choose_menu(&mut ws, REMOVE_ROW, None);
    assert!(view.confirm_open(), "the row asks rather than acting");
    assert!(
        koru_core::tasks::is_task_node(&ws, task).unwrap(),
        "and nothing has run"
    );

    view.key(&mut ws, &named(NamedKey::Tab), None, NONE);
    view.key(&mut ws, &named(NamedKey::Enter), None, NONE);
    assert!(!view.confirm_open());

    assert!(
        !koru_core::tasks::is_task_node(&ws, task).unwrap(),
        "the row is out of Tasks"
    );
    assert!(attr_of(&ws, task, KEY_DUE).is_none(), "and the due is gone");
    assert_eq!(
        attr_of(&ws, task, KEY_PRIORITY),
        Some(AttrValue::Text("high".into())),
        "priority is a content attribute and survives"
    );
    assert!(
        ws.read(task).unwrap().meta.done,
        "and the checkbox keeps the state it had"
    );
    // The row itself stays, which is the promise the copy makes.
    view.editor.body.rebuild(&ws);
    assert!(view.editor.body.row(task).is_some());
    assert_eq!(
        context_menu::TaskContext::of(view.editor.body.row(task).and_then(|r| r.task)),
        context_menu::TaskContext::NotATask,
        "and the menu now says so"
    );

    // **One undo step**: the marker, the lifecycle and the due come back
    // together, because the command wrote them in one atomic group.
    assert!(ws.undo().unwrap_or(false), "there was a step to undo");
    assert!(
        koru_core::tasks::is_task_node(&ws, task).unwrap(),
        "the task is back"
    );
    assert!(
        attr_of(&ws, task, KEY_DUE).is_some(),
        "with its due, in the same step"
    );
}

/// A plain row's Remove row is dimmed, and the keyboard route past the drawing
/// refuses too — the second half of every dimmed row in this menu.
#[test]
fn a_plain_row_dims_remove_from_tasks_and_choosing_it_asks_nothing() {
    let (mut ws, mut view) = laid_out();
    let row = view.editor.body.rows[1].id;
    view.editor.body.focus_on(row);
    let y = view.editor.body.rows[1].y - view.editor.view.scroll + 2.0;
    assert!(View::context_menu(&mut view, &mut ws, 300.0, y));
    assert!(
        matches!(
            &view.menu_shape()[REMOVE_ROW],
            menu::Row::Item {
                label,
                enabled: false,
                ..
            } if label.as_ref() == "Remove from Tasks"
        ),
        "a plain row dims it: {:?}",
        view.menu_shape()[REMOVE_ROW]
    );
    view.choose_menu(&mut ws, REMOVE_ROW, None);
    assert!(!view.confirm_open(), "and there is nothing to ask about");
}

/// A paragraph with a **run pill** in it — what a delegation leaves behind
/// (D8), which is a document reference wearing the run role.
fn document_with_run_pill() -> (
    WorkspaceDoc,
    BuiltInDocument,
    koru_core::AttrId,
    koru_core::DocumentId,
) {
    use koru_core::node::Node;
    use koru_core::schema::Parent;
    use koru_core::{Attr, AttrValue, DocumentReference};

    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "spec.md").expect("a document");
    // A run is a `koru-run` document, not a page in this workspace — which
    // is the whole reason opening one is not opening a document.
    let run = koru_core::DocumentId::new();
    let para = ws
        .insert(Parent::Node(doc), Node::paragraph("asked ").unwrap())
        .unwrap();
    let attr = ws
        .insert_pill(
            para,
            6,
            &Attr::new(
                koru_core::attributes::ROLE_RUN,
                AttrValue::Document(DocumentReference::new(run)),
            ),
        )
        .unwrap();
    let mut view = BuiltInDocument::open(&ws, doc);
    View::set_viewport(&mut view, 900.0, 700.0);
    view.editor.reflow();
    (ws, view, attr, run)
}

/// **A run pill's menu is the container's** (ADR-369). Right-clicking one
/// asks for it and opens nothing here: what a run produced is not something
/// this view can name, so it must not answer with the document menu — which
/// is what it did, offering *Open in new window* on a record that has no
/// page.
#[test]
fn right_clicking_a_run_pill_asks_the_container_and_opens_no_menu_here() {
    let (mut ws, mut view, attr, run) = document_with_run_pill();
    let (x, y) = pill_centre(&view, attr);
    assert!(View::context_menu(&mut view, &mut ws, x, y));
    assert!(view.menu.is_none(), "no menu opened in the document");
    assert_eq!(Editing::take_wants_run_menu(&mut view), Some(run));
    assert_eq!(
        Editing::take_wants_run_menu(&mut view),
        None,
        "the ask is taken once"
    );
    assert_eq!(
        view.context_pill_kind(),
        context_menu::PillContext::None,
        "and nothing was left staged for a menu that never opened"
    );
}

#[test]
fn right_clicking_a_schedule_pill_asks_the_container_and_opens_no_document_menu() {
    use koru_core::node::Node;
    use koru_core::schema::Parent;
    use koru_core::{Attr, AttrValue};

    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "spec.md").unwrap();
    let para = ws
        .insert(Parent::Node(doc), Node::paragraph("repeat ").unwrap())
        .unwrap();
    let schedule = "00000000-0000-0000-0000-000000000123";
    let attr = ws
        .insert_pill(
            para,
            7,
            &Attr::labelled(
                koru_core::attributes::ROLE_SCHEDULE,
                AttrValue::Text(schedule.into()),
                "Daily research",
            ),
        )
        .unwrap();
    let mut view = BuiltInDocument::open(&ws, doc);
    View::set_viewport(&mut view, 900.0, 700.0);
    view.editor.reflow();
    let presentations = std::collections::HashMap::from([(
        koru_editor::pillinfo::schedule_entity_key(schedule),
        ("Renamed research".to_owned(), true),
    )]);
    assert!(Editing::set_schedule_pills(&mut view, &presentations));
    let info = view
        .editor
        .body
        .rows
        .iter()
        .find_map(|row| row.pills.get(&attr))
        .unwrap();
    assert_eq!(info.label, "Renamed research");
    assert_eq!(info.kind, koru_draw::pill::PillKind::SchedulePaused);
    let (x, y) = pill_centre(&view, attr);

    assert!(View::context_menu(&mut view, &mut ws, x, y));
    assert!(view.menu.is_none());
    assert_eq!(
        Editing::take_wants_schedule_menu(&mut view),
        Some(schedule.into())
    );
    assert_eq!(Editing::take_wants_schedule_menu(&mut view), None);
    let row = view
        .editor
        .body
        .rows
        .iter()
        .position(|row| row.pills.contains_key(&attr))
        .unwrap();
    assert!(Editing::follow_document_pill(
        &mut view,
        &ws,
        row,
        attr,
        OpenWhere::SameTab,
    ));
    assert_eq!(
        Editing::take_wants_schedule_open(&mut view),
        Some((schedule.into(), OpenWhere::SameTab))
    );
}

/// An ordinary document pill still opens the document menu: the run role is
/// what makes the difference, not the pill.
#[test]
fn right_clicking_an_ordinary_document_pill_still_opens_its_own_menu() {
    let (mut ws, mut view, attr, _) = document_with_pill();
    let (x, y) = pill_centre(&view, attr);
    assert!(View::context_menu(&mut view, &mut ws, x, y));
    assert!(view.menu.is_some());
    assert_eq!(Editing::take_wants_run_menu(&mut view), None);
}

// ------------------------------------------- the task pill's own gestures

/// Where the due chip is on screen, and the point at the middle of its label
/// half — clear of the remove lane at its trailing end.
fn due_chip_label_point(view: &mut BuiltInDocument, task: NodeId) -> (f32, f32) {
    let (x, y, _) = due_chip_points(view, task);
    (x, y)
}

/// The label point, the lane point, and the chip's rect.
fn due_chip_points(view: &mut BuiltInDocument, task: NodeId) -> (f32, f32, f32) {
    let row = view
        .editor
        .body
        .rows
        .iter()
        .position(|r| r.id == task)
        .expect("the task row");
    let boxes = koru_editor::chips::chip_boxes(
        &view.editor.body.rows[row],
        &mut view.editor.view.engine,
        &view.editor.view.tags,
        &view.editor.view.lists,
    );
    let chip = boxes.first().expect("the due chip").rect;
    let y = chip.y + chip.h * 0.5 - view.editor.view.scroll;
    let label_x = view.editor.view.content_x + chip.x + chip.w * 0.25;
    let lane_x = view.editor.view.content_x + chip.x + chip.w - koru_draw::chip::CHIP_CLOSE_W * 0.5;
    (label_x, y, lane_x)
}

/// **Clicking the task pill goes to the task**, which is where Ctrl+Shift+E
/// lands. It used to open the date grid; editing the date moved into the
/// chip's menu.
#[test]
fn clicking_the_task_pill_asks_for_the_task_rather_than_the_date_grid() {
    let (mut ws, mut view, task) = document_with_a_dated_task("2026-08-30");
    let (x, y) = due_chip_label_point(&mut view, task);

    assert!(View::mouse_down(&mut view, &mut ws, x, y, false));

    assert_eq!(view.editor.view.wants_task_view, Some(task));
    assert!(
        !view.editor.view.overlay.is_open(),
        "and no picker opened on the way"
    );
}

/// **The pill's x asks to remove the task, and asks before doing it.**
///
/// It used to clear the due date. What it takes now — the marker, the
/// canonical lifecycle and the due — is none of it visible from the chip, so
/// it raises the same question the menu's *Remove from Tasks* row raises
/// rather than acting on a single click.
#[test]
fn the_task_pills_remove_lane_raises_the_question_and_writes_nothing_yet() {
    let (mut ws, mut view, task) = document_with_a_dated_task("2026-08-30");
    let (_, y, lane_x) = due_chip_points(&mut view, task);

    assert!(View::mouse_down(&mut view, &mut ws, lane_x, y, false));

    let (_, subject) = view.confirm.as_ref().expect("the question is up");
    assert_eq!(*subject, Confirming::RemoveFromTasks(task));
    assert!(
        koru_core::tasks::is_task_node(&ws, task).unwrap(),
        "nothing is written until it is answered"
    );
    assert!(
        koru_core::tasks::TaskFields::of(&ws.read(task).unwrap())
            .due
            .is_some(),
        "and the due is still there — the lane is not Clear any more"
    );
}

/// **The menu's *Clear due date* still clears only the date.** It and the x
/// are two gestures on this chip where they are one on every other, so the row
/// that says it empties the date has to still empty the date.
#[test]
fn the_menu_still_clears_only_the_date_where_the_lane_removes_the_task() {
    let (mut ws, mut view, task) = document_with_a_dated_task("2026-08-30");
    let (x, y) = due_chip_label_point(&mut view, task);
    assert!(View::context_menu(&mut view, &mut ws, x, y));

    // Row 3: the menu leads with *Go to task* and *Complete*.
    view.choose_menu(&mut ws, 3, None);

    assert!(view.confirm.is_none(), "clearing a date asks nothing");
    assert!(
        koru_core::tasks::TaskFields::of(&ws.read(task).unwrap())
            .due
            .is_none(),
        "the date went"
    );
    assert!(
        koru_core::tasks::is_task_node(&ws, task).unwrap(),
        "and the row is still a task"
    );
}

/// **Two ways to be rid of it, and they are different.** *Remove from Tasks*
/// leaves the writing; *Delete row* takes it. A task is a marked node, so
/// nothing but the reader knows which was meant — see the menu's own note.
#[test]
fn the_two_removal_rows_ask_two_different_questions() {
    for (row, expected) in [(5usize, "RemoveFromTasks"), (6, "DeleteRow")] {
        let (mut ws, mut view, task) = document_with_a_dated_task("2026-08-30");
        let text = ws.read(task).unwrap().content.as_text().map(str::to_owned);
        let (x, y) = due_chip_label_point(&mut view, task);
        assert!(View::context_menu(&mut view, &mut ws, x, y));

        view.choose_menu(&mut ws, row, None);

        let (_, subject) = view.confirm.as_ref().expect("row {row} asks");
        match expected {
            "RemoveFromTasks" => assert_eq!(*subject, Confirming::RemoveFromTasks(task)),
            _ => assert_eq!(*subject, Confirming::DeleteRow(task)),
        }
        assert_eq!(
            ws.read(task).unwrap().content.as_text().map(str::to_owned),
            text,
            "row {row}: nothing is written until the question is answered"
        );

        // Answer it, and the two part company.
        view.settle_confirm(&mut ws, koru_ui::confirm::Outcome::Confirm);
        match expected {
            "RemoveFromTasks" => {
                assert!(ws.read(task).is_ok(), "the row survives");
                assert!(!koru_core::tasks::is_task_node(&ws, task).unwrap());
            }
            _ => assert!(ws.read(task).is_err(), "the row is gone"),
        }
    }
}

/// **A question about one row deletes one row**, whatever else is selected.
/// A right-click inside a row selection keeps the selection, and the editor's
/// delete acts on the selection, so *Delete row* has to name its row twice:
/// in the question, and by dropping the selection before the delete. Without
/// the second, the confirmation quoted one title and the answer took the lot.
#[test]
fn delete_row_from_the_chip_menu_takes_only_the_row_it_named() {
    let (mut ws, mut view, task) = document_with_a_dated_task("2026-08-30");
    let parent = ws.parent(task).expect("the task has a parent");
    let neighbour = ws
        .insert(parent, koru_core::node::Node::paragraph("keep me").unwrap())
        .unwrap();
    view.editor.body.rebuild(&ws);
    view.reflow();
    let rows = &view.editor.body.rows;
    let i = rows
        .iter()
        .position(|r| r.id == task)
        .expect("the task row");
    let j = rows
        .iter()
        .position(|r| r.id == neighbour)
        .expect("its neighbour");
    select_rows(&mut view, i.min(j), i.max(j));
    assert!(
        view.editor.body.selected_ids().len() >= 2,
        "both rows are selected before the right-click"
    );

    let (x, y) = due_chip_label_point(&mut view, task);
    assert!(View::context_menu(&mut view, &mut ws, x, y));
    view.choose_menu(&mut ws, 6, None);
    let (_, subject) = view.confirm.as_ref().expect("Delete row asks");
    assert_eq!(*subject, Confirming::DeleteRow(task));

    view.settle_confirm(&mut ws, koru_ui::confirm::Outcome::Confirm);
    assert!(ws.read(task).is_err(), "the named row is gone");
    assert!(
        ws.read(neighbour).is_ok(),
        "the other selected row was not the question, and survives"
    );
}

// ================================== a document window is the reference host
//
// Two hosts run this editor: a window of its own (this crate's binary, and a
// popped-out `DocumentPane`), and a tab inside the container. Q-B4 found them
// disagreeing about an open picker whose anchor a peer had destroyed — the
// window closed it, the tab did not — and the fix made the tab do what the
// window already did. **So this host's behaviour is the reference**, and the
// tests below pin it here, in the crate that owns it, so a change made for the
// container's benefit fails on this side too.
//
// The pair of them driven through one set of assertions lives in the container:
// `mainview::tests::a_peer_deleting_a_pickers_anchor_closes_it_in_both_hosts`.

/// A document window over two paragraphs, with the caret in the second one and
/// a `/` picker open at the end of it.
fn a_window_with_a_picker() -> (WorkspaceDoc, BuiltInDocument, NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = ws
        .insert(
            koru_core::schema::Parent::Root,
            Node::document("Notes").unwrap(),
        )
        .expect("the document node");
    let _first = ws
        .insert(
            koru_core::schema::Parent::Node(doc),
            Node::paragraph("first").unwrap(),
        )
        .expect("a paragraph");
    let second = ws
        .insert(
            koru_core::schema::Parent::Node(doc),
            Node::paragraph("second").unwrap(),
        )
        .expect("a paragraph");
    let mut view = BuiltInDocument::open(&ws, doc).owns_the_window(&ws);
    // Sized as the shell sizes it: without a viewport nothing is shaped, and a
    // caret is placed against a layout that does not exist.
    View::set_viewport(&mut view, 1200.0, 700.0);
    assert!(Editing::reveal(&mut view, second), "reveal the second row");
    for (key, text) in [
        (Key::Named(NamedKey::End), None),
        (Key::Character(" ".into()), Some(" ")),
        (Key::Character("/".into()), Some("/")),
    ] {
        view.key(&mut ws, &key, text, ModifiersState::empty());
    }
    assert!(
        view.editor.view.overlay.is_open(),
        "typing / did not open a picker"
    );
    (ws, view, second)
}

fn a_peer(ws: &WorkspaceDoc) -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(&ws.export_snapshot().expect("a snapshot")).expect("a replica")
}

fn receive(ws: &mut WorkspaceDoc, from: &WorkspaceDoc) {
    let since = ws.version();
    let bytes = from
        .export_updates_since(&since)
        .expect("the peer's changes");
    ws.import(&bytes).expect("the import");
}

/// **The reference behaviour: a peer deletes the node the picker hangs from, and
/// the window closes the picker and says why.**
///
/// The picker's `start` is a byte offset — a rendering cache. Its `stable`
/// endpoints are Loro cursors, and after this merge they resolve to nothing at
/// all, because the container they indexed into is gone. Closing is the
/// deliberately *visible* failure: a picker that disappears can be reopened,
/// while one still anchored to a node that no longer exists would commit its
/// next pick somewhere nobody asked for.
#[test]
fn a_peer_deleting_the_pickers_anchor_closes_it_with_the_notice() {
    let (mut ws, mut view, row) = a_window_with_a_picker();

    let mut peer = a_peer(&ws);
    peer.delete(row).expect("the peer's deletion");
    receive(&mut ws, &peer);
    View::adopt(&mut view, &ws);

    assert!(
        !view.editor.view.overlay.is_open(),
        "the picker is still open over a node that no longer exists"
    );
    assert_eq!(
        view.editor.view.notice.as_deref(),
        Some("that insertion point changed elsewhere"),
        "the picker closed without saying why"
    );
}

/// …and a peer edit that leaves the anchor resolvable leaves the picker open,
/// following its text.
#[test]
fn a_peer_editing_in_front_of_the_picker_moves_it_and_leaves_it_open() {
    let (mut ws, mut view, row) = a_window_with_a_picker();
    let before = view
        .editor
        .view
        .overlay
        .trigger()
        .expect("the picker has a trigger")
        .start;

    let mut peer = a_peer(&ws);
    peer.insert_text(row, 0, "PEER ").expect("the peer's edit");
    receive(&mut ws, &peer);
    View::adopt(&mut view, &ws);

    assert!(
        view.editor.view.overlay.is_open(),
        "an edit that left the anchor alone closed the picker"
    );
    assert_eq!(
        view.editor
            .view
            .overlay
            .trigger()
            .expect("the picker still has a trigger")
            .start,
        before + "PEER ".len(),
        "the picker did not follow its anchor"
    );
}

/// **One adopt, one of each.**
///
/// A window's `adopt` re-titles the caption, re-scans the find bar and shows the
/// document to the spelling controller. All three are idempotent, so a second
/// run for one peer edit leaves no trace in the state they produce — which is
/// why they are counted (`AdoptWork`) rather than inferred. Q-B4's fix had to
/// leave these counts exactly where they were.
#[test]
fn one_adopt_is_one_title_refresh_one_search_scan_and_one_spelling_look() {
    let (mut ws, mut view, row) = a_window_with_a_picker();
    let before = view.adopt_work();

    let mut peer = a_peer(&ws);
    peer.delete(row).expect("the peer's deletion");
    receive(&mut ws, &peer);
    View::adopt(&mut view, &ws);

    let after = view.adopt_work();
    assert_eq!(
        (
            after.titles - before.titles,
            after.searches - before.searches,
            after.spellings - before.spellings,
        ),
        (1, 1, 1)
    );
}

// ------------------------------------------------------------------ diagrams

/// A document with one `mermaid` block, laid out once, and the block's node.
fn diagram_document() -> (WorkspaceDoc, BuiltInDocument, koru_core::NodeId) {
    let mut ws = WorkspaceDoc::create(WorkspaceId::new(), "t").unwrap();
    let doc = koru_workspace::document(&mut ws, "Release plan").expect("a document");
    let node = ws
        .insert(
            koru_core::schema::Parent::Node(doc),
            Node::code("flowchart LR\n  A --> B").unwrap(),
        )
        .unwrap();
    ws.set_language(node, Some(koru_editor::diagram::LANGUAGE))
        .unwrap();
    let mut view = BuiltInDocument::open(&ws, doc);
    view.editor.view.code_enabled = true;
    view.editor.body.rebuild(&ws);
    View::set_viewport(&mut view, 900.0, 560.0);
    (ws, view, node)
}

/// The worker's answer lands through the view the way the shell delivers it,
/// and an answer for a block that has since been deleted is dropped: it
/// finds no entry, changes nothing, asks for no redraw.
#[test]
fn a_diagram_reply_for_a_node_the_editor_no_longer_has_is_dropped() {
    let (mut ws, mut view, node) = diagram_document();
    let asked = view.take_diagram_requests();
    assert_eq!(asked.len(), 1, "one layout asked for the one block");
    assert_eq!(asked[0].node, node);
    let late = Err(koru_editor::diagram::Status::NotParsed {
        line: 1,
        message: "late".into(),
        code: "merman.parse.diagram_parse".into(),
    });
    // The block goes before the worker answers.
    ws.delete(node).unwrap();
    view.editor.body.rebuild(&ws);
    view.editor.reflow();
    assert!(!view.editor.view.diagrams.contains_key(&node));
    assert!(!view.diagram_result(node, asked[0].text_hash, late));
    // And a reply for a node that never was is the same nothing.
    assert!(!view.diagram_result(
        koru_core::NodeId::new(),
        asked[0].text_hash,
        Err(koru_editor::diagram::Status::Empty)
    ));
}

/// No worker runs in a test process, so the document's line to it is quietly
/// off: a maintenance pass neither sends the editor's requests nor invents
/// answers, and the block keeps its placeholder.
#[test]
fn without_a_worker_a_maintenance_pass_leaves_the_diagram_pending() {
    let (mut ws, mut view, node) = diagram_document();
    assert!(!View::maintain(&mut view, &mut ws, Instant::now()));
    let asked = view.take_diagram_requests();
    assert_eq!(asked.len(), 1, "the request is still the editor's to give");
    assert!(matches!(
        view.editor
            .view
            .diagrams
            .get(&node)
            .map(|entry| &entry.state),
        Some(koru_editor::DiagramState::Pending)
    ));
    assert!(view.drawn_diagram(node).is_none());
}

/// A view built before the worker starts — every tab the shell restores at
/// launch — still has its diagram drawn once the worker is up: the line to
/// it is looked up again on each pass, and the request the editor kept in
/// the meantime goes out on the first pass that finds it.
#[test]
fn a_view_built_before_the_worker_starts_draws_once_it_does() {
    use koru_workspace_host::diagram::{DiagramClient, DiagramWorker};
    static WORKER: std::sync::OnceLock<DiagramWorker> = std::sync::OnceLock::new();
    fn connect() -> Option<DiagramClient> {
        WORKER.get().map(DiagramWorker::client)
    }

    let (mut ws, mut view, node) = diagram_document();
    view.diagrams = crate::diagrams::Diagrams::connecting(connect);
    assert!(!View::maintain(&mut view, &mut ws, Instant::now()));
    assert!(view.drawn_diagram(node).is_none(), "no worker yet");

    let measure = std::sync::Arc::new(koru_draw::diagram::InterMeasure);
    assert!(WORKER.set(DiagramWorker::start(measure, 1, || {})).is_ok());
    let deadline = Instant::now() + std::time::Duration::from_secs(20);
    while view.drawn_diagram(node).is_none() {
        assert!(Instant::now() < deadline, "the worker never answered");
        View::maintain(&mut view, &mut ws, Instant::now());
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// *Save as…* reaches the shell only with a drawing in hand: a request
/// raised on a block that is not drawn is dropped, and the page's name — what
/// the default file name is made of — is read from the document, not from a
/// caption a tab does not draw.
#[test]
fn save_diagram_as_is_handed_over_only_with_a_drawing() {
    let (ws, mut view, node) = diagram_document();
    view.editor.view.wants_save_diagram_as = Some(koru_editor::SaveDiagramAs { node });
    assert!(
        View::take_wants_save_diagram_as(&mut view, &ws).is_none(),
        "a placeholder has no picture to save"
    );
    assert!(
        view.editor.view.wants_save_diagram_as.is_none(),
        "and the ask is spent"
    );
    assert_eq!(view.page_name(&ws), "Release plan");
}

// ------------------------------------------------------- the diagram viewer

/// A prepared drawing of a given natural size with nothing in it: the view
/// never looks inside one, only at its size.
fn prepared_drawing(
    width: f64,
    height: f64,
) -> std::sync::Arc<koru_draw::diagram::PreparedDiagram> {
    use koru_diagram::{Counts, DiagramScene, FlowchartScene, Kind as DiagramKind, SceneBody};
    std::sync::Arc::new(koru_draw::diagram::PreparedDiagram::new(
        std::sync::Arc::new(DiagramScene {
            kind: DiagramKind::Flowchart,
            bounds: vello::kurbo::Rect::new(0.0, 0.0, width, height),
            counts: Counts::Flowchart {
                nodes: 0,
                edges: 0,
                groups: 0,
            },
            acc_title: None,
            acc_descr: None,
            body: SceneBody::Flowchart(FlowchartScene {
                nodes: Vec::new(),
                edges: Vec::new(),
                clusters: Vec::new(),
            }),
        }),
    ))
}

/// The diagram document with its block drawn, the way the worker's answer
/// lands: the request taken, the result delivered.
fn drawn_diagram_document() -> (WorkspaceDoc, BuiltInDocument, koru_core::NodeId) {
    let (ws, mut view, node) = diagram_document();
    let asked = view.take_diagram_requests();
    assert_eq!(asked.len(), 1);
    assert!(view.diagram_result(node, asked[0].text_hash, Ok(prepared_drawing(400.0, 300.0))));
    assert!(view.drawn_diagram(node).is_some());
    (ws, view, node)
}

/// The viewer opens only on a drawing: a placeholder has no picture to
/// show, so the ask does nothing and the page is not covered.
#[test]
fn the_viewer_opens_only_on_a_drawing() {
    let (_ws, mut view, node) = diagram_document();
    assert!(!view.open_diagram_viewer(node));
    assert!(!view.diagram_viewer_open());
    assert_eq!(view.blockers(), Blockers::None);

    let (_ws, mut view, node) = drawn_diagram_document();
    assert!(view.open_diagram_viewer(node));
    assert!(view.diagram_viewer_open());
    assert_eq!(view.blockers(), Blockers::Modal(ModalKind::DiagramViewer));
    let viewer = view.diagram_viewer().expect("open");
    assert_eq!(viewer.node(), node);
    assert_eq!(viewer.zoom(), 1.0, "400 × 300 fits a 900 × 560 window");
}

/// The editor's ask — its magnifier, a double click — is drained after the
/// press that set it, and opens the viewer on that block.
#[test]
fn the_editors_ask_opens_the_viewer_after_the_press() {
    let (mut ws, mut view, node) = drawn_diagram_document();
    view.editor.view.wants_open_diagram = Some(node);
    // Any press drains it; this one lands on the page's air.
    view.mouse_down(&mut ws, 880.0, 540.0, false);
    assert!(view.diagram_viewer_open());
    assert_eq!(view.editor.view.wants_open_diagram, None, "spent");
}

/// The editor's *Copy image* control asks the same way, and the drawing it
/// names goes on to the shell after the press — with no viewer opened.
#[test]
fn the_editors_copy_control_hands_the_drawing_to_the_shell() {
    let (mut ws, mut view, node) = drawn_diagram_document();
    assert!(View::take_wants_copy_diagram(&mut view).is_none());
    view.editor.view.wants_copy_diagram = Some(node);
    view.mouse_down(&mut ws, 880.0, 540.0, false);
    assert_eq!(view.editor.view.wants_copy_diagram, None, "spent");
    let copy = View::take_wants_copy_diagram(&mut view).expect("the drawing to copy");
    assert_eq!(copy.prepared.natural_size().width, 400.0);
    assert!(!view.diagram_viewer_open(), "a copy opens nothing");
}

/// While the viewer is up every key is its own: `Esc` closes it and
/// nothing reaches the page; a letter that means nothing to it is
/// swallowed rather than typed into the block underneath.
#[test]
fn the_viewer_owns_the_keyboard_until_escape() {
    let (mut ws, mut view, node) = drawn_diagram_document();
    assert!(view.open_diagram_viewer(node));
    let text_before = ws.read(node).unwrap().content.as_text().unwrap().to_owned();
    assert!(view.key(&mut ws, &ch("q"), Some("q"), NONE));
    assert_eq!(
        ws.read(node).unwrap().content.as_text().unwrap(),
        text_before,
        "nothing was typed into the block"
    );
    assert!(view.diagram_viewer_open());
    assert!(view.key(&mut ws, &named(NamedKey::Escape), None, NONE));
    assert!(!view.diagram_viewer_open());
    assert_eq!(view.blockers(), Blockers::None);
}

/// `Ctrl+C` in the viewer hands the drawing to the shell for the clipboard,
/// once; the save chord raises the block's own *Save as…*, which the shell
/// drains with the drawing and the page's name as it does from the menu.
#[test]
fn the_viewer_asks_the_shell_to_copy_and_to_save() {
    let (mut ws, mut view, node) = drawn_diagram_document();
    assert!(view.open_diagram_viewer(node));
    assert!(
        View::take_wants_copy_diagram(&mut view).is_none(),
        "nothing asked yet"
    );
    assert!(view.key(&mut ws, &ch("c"), Some("c"), koru_keys::PRIMARY));
    let copy = View::take_wants_copy_diagram(&mut view).expect("the drawing to copy");
    assert_eq!(copy.prepared.natural_size().width, 400.0);
    assert!(View::take_wants_copy_diagram(&mut view).is_none(), "spent");
    assert_eq!(
        view.editor.view.wants_toast.as_deref(),
        Some(koru_ui::CopyAffordance::TOAST),
        "a copy raises the toast every copy raises"
    );
    assert_eq!(
        view.diagram_viewer().unwrap().copy_glyph(),
        koru_draw::icons::CHECK,
        "and confirms on the button"
    );

    assert!(view.key(
        &mut ws,
        &ch("x"),
        Some("x"),
        koru_keys::PRIMARY | ModifiersState::SHIFT
    ));
    let save = View::take_wants_save_diagram_as(&mut view, &ws).expect("the drawing to save");
    assert_eq!(save.title, "Release plan");
    assert!(view.diagram_viewer_open(), "asking does not close the view");
}

/// The zoom gestures the shell offers a view go to the drawing while the
/// viewer is up, and are declined — the app's to zoom — when it is not.
#[test]
fn zoom_gestures_are_the_viewers_while_it_is_up() {
    let (_ws, mut view, node) = drawn_diagram_document();
    assert!(!View::zoom_key(
        &mut view,
        koru_workspace_host::ZoomIntent::In
    ));
    assert!(view.open_diagram_viewer(node));
    assert!(View::zoom_key(
        &mut view,
        koru_workspace_host::ZoomIntent::In
    ));
    assert_eq!(
        view.diagram_viewer().unwrap().zoom(),
        crate::diagram_viewer::ZOOM_STEP
    );
    assert!(View::zoom_wheel(
        &mut view,
        winit::event::MouseScrollDelta::LineDelta(0.0, -1.0),
        450.0,
        280.0
    ));
    assert!((view.diagram_viewer().unwrap().zoom() - 1.0).abs() < 1e-9);
    // The plain wheel is the drawing's scroll, not a zoom — and the page
    // under the modal does not scroll.
    let scroll_before = view.editor.view.scroll;
    View::scroll_by(
        &mut view,
        winit::event::MouseScrollDelta::LineDelta(0.0, 1.0),
    );
    assert_eq!(view.editor.view.scroll, scroll_before);
    assert!((view.diagram_viewer().unwrap().zoom() - 1.0).abs() < 1e-9);
}

/// Lifted by a container, the viewer takes the window: its viewport is the
/// window's, and a press in the pane's coordinates is read in the window's.
/// A press that lands on the toolbar only because the window is taller
/// than the pane reaches the toolbar; in the pane's own window it would
/// have been the scrim and closed the viewer.
#[test]
fn a_lifted_viewer_takes_the_window_and_reads_presses_in_it() {
    let (mut ws, mut view, node) = drawn_diagram_document();
    assert!(view.open_diagram_viewer(node));
    assert_eq!(view.diagram_viewer().unwrap().viewport(), (900.0, 560.0));
    view.set_viewer_lift(Some(crate::ViewerLift {
        origin: (100.0, 50.0),
        size: (1200.0, 700.0),
    }));
    let viewer = view.diagram_viewer().unwrap();
    assert_eq!(viewer.viewport(), (1200.0, 700.0));
    assert_eq!(viewer.zoom(), 1.0);
    // The zoom-out button, first on the toolbar, in window coordinates —
    // below the pane's own 560 pixels.
    let bar = viewer.toolbar_rect();
    let (wx, wy) = (bar.x + 10.0, bar.y + bar.h * 0.5);
    assert!(wy > 560.0 + 50.0, "the toolbar sits past the pane: {wy}");
    let (lx, ly) = (wx - 100.0, wy - 50.0);
    assert!(view.mouse_down(&mut ws, lx, ly, false));
    assert!(View::mouse_up(&mut view, &mut ws));
    let viewer = view
        .diagram_viewer()
        .expect("still open: the toolbar was pressed");
    assert!(viewer.zoom() < 1.0, "zoomed out: {}", viewer.zoom());
    // The pane resizing does not touch a lifted viewer; lifting back down does.
    View::set_viewport(&mut view, 800.0, 500.0);
    assert_eq!(view.diagram_viewer().unwrap().viewport(), (1200.0, 700.0));
    view.set_viewer_lift(None);
    assert_eq!(view.diagram_viewer().unwrap().viewport(), (800.0, 500.0));
}

/// A click on the dimmed page closes the viewer on release; a press that
/// starts on the drawing does not, and the page underneath hears none of
/// it — the caret stays where it was.
#[test]
fn a_click_on_the_dimmed_page_closes_the_viewer() {
    let (mut ws, mut view, node) = drawn_diagram_document();
    assert!(view.open_diagram_viewer(node));
    let focus_before = view.editor.body.focused_id();
    assert!(view.mouse_down(&mut ws, 5.0, 5.0, false));
    assert!(View::mouse_up(&mut view, &mut ws));
    assert!(!view.diagram_viewer_open());
    assert_eq!(view.editor.body.focused_id(), focus_before);
}

/// The menu on a drawn block offers *Open* and *Copy image*; *Open* raises
/// the viewer and *Copy image* asks the shell, from the row's own drawing.
#[test]
fn the_menu_opens_the_viewer_and_copies_the_image() {
    let (mut ws, mut view, node) = drawn_diagram_document();
    view.editor.body.focus_on(node);
    view.editor.view.active = true;
    let context = view.focused_diagram_context().expect("a diagram row");
    assert!(context.drawn);
    assert!(view.open_diagram_viewer(context.node));
    assert!(view.diagram_viewer_open());
    assert!(view.key(&mut ws, &named(NamedKey::Escape), None, NONE));
    assert!(!view.diagram_viewer_open());
    view.wants_copy_diagram = view.drawn_diagram(context.node);
    assert!(View::take_wants_copy_diagram(&mut view).is_some());
}
