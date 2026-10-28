// SPDX-License-Identifier: MPL-2.0

//! What the container adds on top of the views it hosts.
//!
//! The 2,600-line suite next door drives a bare [`Leaf`], which still believes
//! it owns the window — so every coordinate there stays window-origin and none
//! of it had to move. What is left to prove is exactly what the container
//! introduces: that the offset arrives, that tabs come and go correctly, and
//! that a tab nobody is looking at neither goes stale nor gets laid out.
//!
//! Nothing here touches disk. `Workspace::new` is in-memory; only
//! `Session::open` reaches a directory, and only `App::new` calls it.
use super::*;
use crate::mainview::harness::*;
use crate::mainview::panes::TEAR_OFF_PX;
use koru_core::node::Node;
use koru_core::schema::Parent;
use koru_editor::Editing;

#[test]
fn socials_menu_focuses_one_tab_and_keeps_it_out_of_document_reuse() {
    let (mut view, _) = container();
    assert!(view.run_menu_action(MenuSubject::NewTab, MenuAction::Socials));
    let count = view.tabs().len();
    assert!(matches!(view.tabs().active_tab().view, TabView::Socials(_)));
    assert!(!view.tabs().active_tab().is_reusable());
    assert!(view.explore.showing_socials());
    assert!(view.sidebar.is_open());
    let shared = view.explore.socials().expect("navigator owns the surface");
    assert!(view.run_menu_action(MenuSubject::NewTab, MenuAction::Socials));
    assert_eq!(view.tabs().len(), count);
    assert!(std::rc::Rc::ptr_eq(
        &shared,
        &view.explore.socials().unwrap()
    ));
    assert!(matches!(view.tabs().active_tab().view, TabView::Socials(_)));
}

#[test]
fn configured_website_effect_reaches_system_browser_not_koru_deep_link_activation() {
    struct Browser(std::cell::RefCell<Vec<String>>);
    impl koru_workspace_host::Host for Browser {
        type V = crate::manager_view::ManagerView;
        fn initial(&mut self, _: &mut Workspace) -> Self::V {
            panic!("no window")
        }
        fn duplicate(&mut self, _: &WorkspaceDoc) -> Self::V {
            panic!("no window")
        }
        fn open_document(&mut self, _: &WorkspaceDoc, _: NodeId) -> Option<Self::V> {
            panic!("no document")
        }
        fn open_uri(&self, uri: &str) {
            self.0.borrow_mut().push(uri.to_owned());
        }
        fn open_external_url(&self, _: String) -> Result<(), String> {
            panic!("not a Koru deep link")
        }
    }
    let host = Browser(Default::default());
    for origin in ["https://www.koru.do", "http://localhost:3000"] {
        for lang in koru_i18n::Lang::ALL {
            let url = crate::website::page_url(origin, lang, crate::website::Page::Dashboard);
            assert!(PaneRequest::OpenWebsite(url.clone()).dispatch_system_uri(&host));
            assert_eq!(host.0.borrow().last(), Some(&url));
        }
    }
}

#[test]
fn manager_wrapper_focus_refreshes_only_the_visible_cloud_card() {
    let mut main = MainView::new();
    main.open_manage(crate::manage::Page::Cloud);
    while View::take_pane_request(&mut main).is_some() {}
    let TabView::Manage(card) = &mut main.tabs_mut().active_mut().view else {
        panic!("manager");
    };
    card.delegate(|c| {
        c.set_account(koru_agent_wire::AccountState {
            active_scope: 9,
            active_account_id: Some("90000000-0000-4000-8000-000000000009".into()),
            stage: koru_agent_wire::AccountStage::SignedIn,
            ..Default::default()
        })
    });
    card.set_in_front(true);
    while let Some(effect) = card.take_effect() {
        if let crate::manage::profiles::Effect::Account {
            command: koru_agent_wire::AccountCommand::InspectCloudSubscription { request_id },
            ..
        } = effect
        {
            card.delegate(|c| {
                c.complete_scoped(
                    9,
                    10,
                    koru_agent_wire::AccountOperationResult::CloudSubscription {
                        request_id,
                        current: Some(true),
                    },
                )
            });
        }
    }
    let mut wrapper = crate::manager_view::ManagerView::Container(main);
    View::set_window_focused(&mut wrapper, false);
    assert!(View::take_pane_request(&mut wrapper).is_none());
    View::set_window_focused(&mut wrapper, true);
    assert!(matches!(
        View::take_pane_request(&mut wrapper),
        Some(PaneRequest::AccountTracked {
            owner_scope: 9,
            command: koru_agent_wire::AccountCommand::InspectCloudSubscription { .. },
            ..
        })
    ));
    View::set_window_focused(&mut wrapper, true);
    assert!(
        View::take_pane_request(&mut wrapper).is_none(),
        "inflight refresh is deduplicated"
    );
}

// ---------------------------------------------------------------- the offset

/// **The test the whole translation scheme rests on.**
///
/// A click at the same place inside the view rect has to land on the same
/// character whether it went through the container or straight into a bare
/// leaf. Comparing against the untranslated path is what makes this catch a
/// coordinate that was missed, rather than just asserting today's arithmetic
/// back to itself.
#[test]
fn a_click_lands_in_the_same_place_through_the_container_as_without_it() {
    let (mut v, mut workspace) = on_document();
    let doc = fill(&v, &mut workspace.ws, 6);
    let ws = &mut workspace.ws;
    v.adopt(ws);

    let view = v.focused_view_rect();
    assert!(
        view.y > 0.0,
        "the caption row gives the view a non-zero origin"
    );

    // The same view, on the same document, sized as the container sizes it and
    // driven directly. It has to be the *same kind* of leaf, or the two are
    // reading different content and the comparison proves nothing.
    let mut bare = Leaf::document(ws, doc);
    Leaf::set_viewport(&mut bare, view.w, view.h);
    Leaf::adopt(&mut bare, ws);

    // Below the document's toolbar, which the outline this used to drive did
    // not have — a click in the top 38px lands on chrome, in both paths.
    for (x, y) in [(60.0, row_y(0)), (120.0, row_y(1)), (200.0, row_y(3))] {
        v.mouse_down(ws, view.x + x, view.y + y, false);
        Leaf::mouse_down(&mut bare, ws, x, y, false);

        let TabView::Leaf(l) = &v.tabs().active_tab().view else {
            panic!("the document tab is a leaf")
        };
        assert_eq!(
            (l.body().focus, l.body().focused().map(|p| p.caret())),
            (bare.body().focus, bare.body().focused().map(|p| p.caret())),
            "click at ({x}, {y}) inside the view rect"
        );
    }
}

#[test]
fn the_leaf_is_given_the_view_rect_as_its_viewport() {
    let (v, _w) = on_document();
    let view = v.focused_view_rect();
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("leaf")
    };
    assert_eq!(l.view().viewport, (view.w, view.h));
    assert!(
        view.w < 1200.0 || view.h < 700.0,
        "the container took something"
    );
}

// ------------------------------------------------------------------ opening

#[test]
fn import_completion_actions_name_and_route_the_imported_root() {
    let (mut v, workspace) = on_document();
    let notebook = koru_core::DocumentId::new();
    assert!(View::show_import_notice(
        &mut v,
        "Import succeeded.".into(),
        koru_workspace_host::ImportNoticeTarget::Notebook(notebook),
    ));
    let (toast, payload) = v.toast.as_ref().unwrap();
    assert_eq!(toast.action(), Some("View Notebook"));
    assert!(matches!(payload, ToastPayload::Open(id) if *id == notebook));

    v.reveal_imported_document(notebook);
    assert!(v.sidebar.is_open());
    assert_eq!(
        v.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Documents
    );
    assert_eq!(
        v.explore.selected(),
        Some(koru_core::NodeId::from_uuid(notebook.as_uuid()))
    );

    let file = koru_core::DocumentId::new();
    assert!(View::show_import_notice(
        &mut v,
        "Import succeeded.".into(),
        koru_workspace_host::ImportNoticeTarget::File(file),
    ));
    let (toast, payload) = v.toast.as_ref().unwrap();
    assert_eq!(toast.action(), Some("View file"));
    assert!(matches!(payload, ToastPayload::ShowFile(id) if *id == file));

    v.reveal_imported_file(file);
    assert!(v.sidebar.is_open());
    assert_eq!(
        v.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Files
    );
    assert_eq!(
        v.explore.selected(),
        Some(koru_core::NodeId::from_uuid(file.as_uuid()))
    );
    v.refresh_explore(&workspace.ws, true);
    assert_eq!(
        v.explore.selected(),
        Some(koru_core::NodeId::from_uuid(file.as_uuid())),
        "refreshing the Files projection must not restore the active page over the selected file"
    );
}

#[test]
fn a_live_profile_deletion_notice_offers_the_profile_switcher() {
    let (mut v, _workspace) = on_document();
    assert!(View::show_notice_open_another_profile(
        &mut v,
        "A Profile was deleted from Koru".into(),
    ));
    let (toast, payload) = v.toast.as_ref().expect("a deletion notice");
    assert_eq!(toast.action(), Some("Open another Profile"));
    assert!(matches!(payload, ToastPayload::OpenProfiles));
}

#[test]
fn import_progress_updates_in_place_and_stays_closed_when_dismissed() {
    let (mut v, _workspace) = on_document();
    assert!(View::show_import_progress(
        &mut v,
        "Import started".into(),
        koru_workspace_host::ImportNoticeProgress::Indeterminate,
    ));
    assert!(matches!(
        v.toast.as_ref().map(|(_, payload)| payload),
        Some(ToastPayload::ImportProgress)
    ));
    assert_eq!(v.toast.as_ref().unwrap().0.action(), Some("Stop"));

    assert!(View::update_import_progress(
        &mut v,
        "Importing content… Converting 50%".into(),
        koru_workspace_host::ImportNoticeProgress::Determinate(0.5),
    ));
    assert_eq!(
        v.toast.as_ref().unwrap().0.message(),
        "Importing content… Converting 50%"
    );

    assert!(View::cancel_import_progress(
        &mut v,
        "Importing content… Canceling".into(),
    ));
    assert_eq!(
        v.toast.as_ref().unwrap().0.message(),
        "Importing content… Canceling"
    );
    assert_eq!(v.toast.as_ref().unwrap().0.action(), None);

    v.toast = None;
    assert!(View::update_import_progress(
        &mut v,
        "Importing content… Converting 75%".into(),
        koru_workspace_host::ImportNoticeProgress::Determinate(0.75),
    ));
    assert!(v.toast.is_none(), "a worker update must not reopen it");
}

#[test]
fn the_library_background_opens_import_as_a_submenu() {
    let (mut v, _workspace) = on_document();
    assert!(v.open_library_menu((120.0, 300.0)));
    assert_eq!(v.menu.as_ref().unwrap().items(), &[MenuAction::ImportAs]);
    assert!(v.open_submenu(0));
    assert_eq!(
        v.submenu.as_ref().unwrap().1.items(),
        context_menu::import_items()
    );
}

#[test]
fn file_rows_offer_open_in_app_and_save_as() {
    let (mut v, _workspace) = on_document();
    let file = koru_core::NodeId::new();
    v.explore.show_files();
    assert!(v.open_node_menu(vec![file], (120.0, 300.0), false));
    assert_eq!(
        v.menu.as_ref().unwrap().items(),
        context_menu::file_items(1, None)
    );

    assert!(v.run_menu_action(MenuSubject::Files(vec![file]), MenuAction::OpenFileInApp,));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::OpenFile(koru_core::DocumentId::from_uuid(
            file.as_uuid()
        )))
    );
}

/// *Open note* when the file has one, *Add note* when it does not — and both
/// are the same request, because the store answers idempotently.
#[test]
fn a_files_note_goes_through_the_shell_either_way() {
    let (mut v, _workspace) = on_document();
    let file = koru_core::NodeId::new();
    let file_doc = koru_core::DocumentId::from_uuid(file.as_uuid());
    let note = koru_core::DocumentId::new();
    v.explore.show_files();
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: file_doc,
            parent: None,
            ordinal: 0,
            title: "Report.pdf".into(),
            kind: koru_core::explore::KIND_FILE.into(),
            mime: Some("application/pdf".into()),
            note: Some(note),
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );

    assert!(v.open_node_menu(vec![file], (120.0, 300.0), false));
    assert_eq!(
        v.menu.as_ref().unwrap().items(),
        context_menu::file_items(1, Some(true)),
        "the entry says the file has a note, so the menu offers to open it"
    );

    assert!(v.run_menu_action(MenuSubject::Files(vec![file]), MenuAction::OpenNote));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::OpenFileNote(file_doc))
    );
    assert!(v.run_menu_action(MenuSubject::Files(vec![file]), MenuAction::AddNote));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::OpenFileNote(file_doc))
    );
}

#[test]
fn file_rows_rename_opens_the_inline_field() {
    let (mut v, mut workspace) = on_document();
    let file = koru_core::DocumentId::new();
    let node = koru_core::NodeId::from_uuid(file.as_uuid());
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: file,
            parent: None,
            ordinal: 0,
            title: "Shared link.url".into(),
            kind: koru_core::explore::KIND_FILE.into(),
            mime: Some("text/uri-list".into()),
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );
    v.explore.show_files();

    assert!(v.run_menu_action(MenuSubject::Files(vec![node]), MenuAction::Rename));
    assert!(
        v.workspace_rename.is_some(),
        "the name is edited in the row, like a document"
    );
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE
    ));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::RenameFile(file, "Shared link.url".into()))
    );
}

#[test]
fn file_rows_move_to_trash_through_the_manifest_path() {
    let (mut v, _workspace) = on_document();
    let file = koru_core::DocumentId::new();
    let node = koru_core::NodeId::from_uuid(file.as_uuid());
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: file,
            parent: None,
            ordinal: 0,
            title: "Report.pdf".into(),
            kind: koru_core::explore::KIND_FILE.into(),
            mime: Some("application/pdf".into()),
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );

    assert!(v.run_menu_action(MenuSubject::Files(vec![node]), MenuAction::MoveToTrash));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::TrashDocuments(vec![file]))
    );
    assert!(v.toast.is_some(), "the reversible move offers Undo");
}

#[test]
fn file_actions_carry_the_whole_selection_to_the_shell() {
    let (mut v, _workspace) = on_document();
    let first = koru_core::NodeId::new();
    let second = koru_core::NodeId::new();
    let subject = MenuSubject::Files(vec![first, second]);

    assert!(v.run_menu_action(subject.clone(), MenuAction::OpenFileInApp));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::OpenFiles(vec![
            koru_core::DocumentId::from_uuid(first.as_uuid()),
            koru_core::DocumentId::from_uuid(second.as_uuid()),
        ]))
    );

    assert!(v.run_menu_action(subject, MenuAction::SaveFileAs));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::SaveFiles(vec![
            koru_core::DocumentId::from_uuid(first.as_uuid()),
            koru_core::DocumentId::from_uuid(second.as_uuid()),
        ]))
    );
}

/// Unlabelled document pills (Notion import) must pick up explore titles when
/// the tab opens — not stay on "unavailable" until the listing changes again.
#[test]
fn opening_a_document_leaf_applies_entity_titles() {
    let (mut v, mut workspace) = on_document();
    let target = koru_core::DocumentId::new();
    let root = v.document_root().expect("document tab");
    let row = workspace
        .ws
        .insert(
            koru_core::schema::Parent::Node(root),
            koru_core::node::Node::paragraph("").unwrap(),
        )
        .expect("row");
    let attr = koru_core::Attr::new(
        "document",
        koru_core::AttrValue::Document(koru_core::DocumentReference {
            document: target,
            node: None,
        }),
    );
    workspace.ws.insert_pill(row, 0, &attr).expect("pill");

    v.entity_titles
        .insert(target.to_storage(), "Business Plan".into());
    let leaf = v.document_leaf(&workspace.ws, root);
    let key = target.to_storage();
    let label = Editing::body(&leaf)
        .rows
        .iter()
        .flat_map(|r| r.pills.values())
        .find(|info| info.entity.as_deref() == Some(key.as_str()))
        .map(|info| info.label.clone());
    assert_eq!(label.as_deref(), Some("Business Plan"));
}

/// Body attachment-pill Open/Save must use the same PaneRequest path as the
/// Explore Files menu — returning None from take_wants_file_action so the shell
/// does not double-handle (the regression fixed in 818b3eef).
#[test]
fn a_body_attachment_open_and_save_issue_pane_requests() {
    let (mut v, _workspace) = on_document();
    let file = koru_core::DocumentId::new();

    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf");
    };
    leaf.view_mut().wants_file_action = Some((file, koru_editor::FileAction::Open));
    assert_eq!(View::take_wants_file_action(&mut v), None);
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::OpenFile(file))
    );

    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf");
    };
    leaf.view_mut().wants_file_action = Some((file, koru_editor::FileAction::Save));
    assert_eq!(View::take_wants_file_action(&mut v), None);
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::SaveFile(file))
    );
}

#[test]
fn a_body_attachment_reveal_opens_files_mode_and_restore_issues_a_pane_request() {
    let (mut v, _workspace) = on_document();
    let file = koru_core::DocumentId::new();
    let node = koru_core::NodeId::from_uuid(file.as_uuid());
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: file,
            parent: None,
            ordinal: 0,
            title: "Report.pdf".into(),
            kind: koru_core::explore::KIND_FILE.into(),
            mime: Some("application/pdf".into()),
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );

    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf");
    };
    leaf.view_mut().wants_file_action = Some((file, koru_editor::FileAction::Reveal));
    assert_eq!(View::take_wants_file_action(&mut v), None);
    assert_eq!(View::take_pane_request(&mut v), None);
    assert_eq!(
        v.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Files
    );
    assert_eq!(v.explore.selected(), Some(node));

    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf");
    };
    leaf.view_mut().wants_file_action = Some((file, koru_editor::FileAction::Restore));
    assert_eq!(View::take_wants_file_action(&mut v), None);
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(PaneRequest::RestoreDocuments(vec![file]))
    );
}

#[test]
fn a_document_row_menu_offers_rename_and_opens_the_existing_prompt() {
    let (mut v, workspace) = on_document();
    let document = koru_core::DocumentId::new();
    let node = koru_core::NodeId::from_uuid(document.as_uuid());
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: document,
            parent: None,
            ordinal: 0,
            title: "Document".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );

    assert!(v.open_node_menu(vec![node], (120.0, 300.0), false));
    let menu = v.menu.as_ref().expect("the document menu opened");
    assert_eq!(menu.subject, MenuSubject::Nodes(vec![node]));
    assert!(menu.items().contains(&MenuAction::Rename));
    assert!(v.item_is_enabled(menu, MenuAction::Rename));

    assert!(v.run_menu_action(MenuSubject::Nodes(vec![node]), MenuAction::Rename,));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document,
                node: None,
            },
            match_text: None,
            // **This window's**, not merely this process's: the prompt is the
            // other half of a gesture made here. See
            // `a_second_window_does_not_take_a_gesture_raised_in_the_first`.
            disposition: koru_workspace_host::SearchTargetDisposition::SourceWindow,
        })
    );
    assert!(View::show_active_document(
        &mut v,
        &workspace.ws,
        Some(document),
    ));
    assert!(v.tabs().active_tab().view.is_renaming());
}

// ------------------------------------------- the explore row's Attributes ›

/// One manifest row, the shape `set_entries` is pushed in.
fn document_entry(id: koru_core::DocumentId, title: &str) -> koru_core::explore::ExploreEntry {
    koru_core::explore::ExploreEntry {
        id,
        parent: None,
        ordinal: 0,
        title: title.into(),
        kind: koru_core::explore::KIND_DOCUMENT.into(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    }
}

/// The four rows of the Attributes submenu, and the overlay each one owes.
const ATTRIBUTE_ROWS: [MenuAction; 4] = [
    MenuAction::AttributeTags,
    MenuAction::AttributeColor,
    MenuAction::AttributeDue,
    MenuAction::AttributePriority,
];

/// Assert the tab in front is showing the picker `action` names, over `root`.
///
/// **Over the root is half the claim.** A document-level attribute lives on
/// the document's own root node — the title row — which is the same node
/// `/tag` in the title writes to (`the_sidebar_and_the_title_row_set_the_same_attribute`
/// in `apps/koru-document`). A picker that opened over some other row would
/// set the attribute on a paragraph.
fn assert_picker_over(v: &MainView, action: MenuAction, root: NodeId) {
    use koru_editor::components::Overlay;
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("{action:?}: the tab in front is not a document");
    };
    let overlay = &leaf.view().overlay;
    let right = match (action, overlay) {
        (MenuAction::AttributeTags, Overlay::Tag(_)) => true,
        (MenuAction::AttributeColor, Overlay::Color(_)) => true,
        (MenuAction::AttributeDue, Overlay::Date(_)) => true,
        (MenuAction::AttributePriority, Overlay::Priority(_)) => true,
        _ => false,
    };
    assert!(right, "{action:?} did not open its own picker");
    assert_eq!(
        overlay.trigger().map(|t| t.node),
        Some(root),
        "{action:?} opened over the document's root node"
    );
}

/// **The case the bug was reported from.** The document you right-click in the
/// sidebar is very often the one already in front of you — and for that one the
/// shell answers `PaneRequest::Navigate` *without* `show_active_document`,
/// because `workspace.active()` does not move and there is nothing to announce.
/// The deferred picker was drained only from that announcement, so all four
/// rows closed the menu and did nothing at all.
#[test]
fn the_attributes_rows_open_a_picker_on_the_document_already_in_front() {
    for action in ATTRIBUTE_ROWS {
        let (mut v, workspace) = on_document();
        let root = v.document_root().expect("a document tab");
        let document = koru_core::DocumentId::new();
        let node = NodeId::from_uuid(document.as_uuid());
        View::set_entries(
            &mut v,
            vec![document_entry(document, "Notes")],
            Some(document),
        );
        v.tabs_mut().active_mut().document = Some(document);

        assert!(v.run_menu_action(MenuSubject::Nodes(vec![node]), action));
        assert!(matches!(
            View::take_pane_request(&mut v),
            Some(koru_workspace_host::PaneRequest::Navigate { .. })
        ));

        // All the shell does for a document it is already on: no
        // `show_active_document`, only the pane navigation.
        assert!(View::open_search_target(
            &mut v,
            &workspace.ws,
            Some(node),
            None,
            None,
            None
        ));

        assert_picker_over(&v, action, root);
        assert!(
            v.pending_document_attrs.is_none(),
            "{action:?}: the request was drained rather than left to fire later"
        );
    }
}

/// Open, but behind another tab: the shell *does* have something to announce,
/// so the picker arrives with `show_active_document` — and lands on the tab
/// that was already over the document rather than a second one.
#[test]
fn the_attributes_rows_reach_a_document_open_in_a_background_tab() {
    for action in ATTRIBUTE_ROWS {
        let (mut v, workspace) = on_document();
        let here = koru_core::DocumentId::new();
        v.tabs_mut().active_mut().document = Some(here);

        // A second document is its own `LoroDoc`, which is why it needs its own
        // `WorkspaceDoc` rather than a second root in this one.
        let mut other_ws =
            WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create");
        let other_root = a_document(&mut other_ws, "Brief");
        let other = koru_core::DocumentId::new();
        let other_node = NodeId::from_uuid(other.as_uuid());
        View::set_entries(
            &mut v,
            vec![
                document_entry(here, "Notes"),
                document_entry(other, "Brief"),
            ],
            Some(here),
        );

        // Open it, then come back — which is what leaves it in a background tab.
        assert!(View::show_active_document(&mut v, &other_ws, Some(other)));
        let other_tab = v.tabs().active();
        assert!(View::show_active_document(
            &mut v,
            &workspace.ws,
            Some(here)
        ));
        assert_ne!(v.tabs().active(), other_tab, "the other tab is behind");
        let tabs = v.tabs().len();

        assert!(v.run_menu_action(MenuSubject::Nodes(vec![other_node]), action));
        assert!(matches!(
            View::take_pane_request(&mut v),
            Some(koru_workspace_host::PaneRequest::Navigate { .. })
        ));
        assert!(View::show_active_document(&mut v, &other_ws, Some(other)));

        assert_eq!(v.tabs().len(), tabs, "{action:?} opened no second tab");
        assert_eq!(v.tabs().active(), other_tab);
        assert_picker_over(&v, action, other_root);
        assert!(v.pending_document_attrs.is_none());
    }
}

/// Not open at all: the document is loaded, a tab is minted for it, and the
/// picker opens over the root of the document that just arrived.
#[test]
fn the_attributes_rows_reach_a_document_that_is_not_open() {
    for action in ATTRIBUTE_ROWS {
        let (mut v, _workspace) = on_document();
        let here = koru_core::DocumentId::new();
        v.tabs_mut().active_mut().document = Some(here);
        let tabs = v.tabs().len();

        let mut other_ws =
            WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create");
        let other_root = a_document(&mut other_ws, "Brief");
        let other = koru_core::DocumentId::new();
        let other_node = NodeId::from_uuid(other.as_uuid());
        View::set_entries(
            &mut v,
            vec![
                document_entry(here, "Notes"),
                document_entry(other, "Brief"),
            ],
            Some(here),
        );

        assert!(v.run_menu_action(MenuSubject::Nodes(vec![other_node]), action));
        assert!(matches!(
            View::take_pane_request(&mut v),
            Some(koru_workspace_host::PaneRequest::Navigate { .. })
        ));
        assert!(View::show_active_document(&mut v, &other_ws, Some(other)));

        assert_eq!(v.tabs().len(), tabs + 1, "{action:?} opened the document");
        assert_picker_over(&v, action, other_root);
        assert!(v.pending_document_attrs.is_none());
    }
}

/// **Rename shared the bug**, and shares the fix. `rename_node` — F2, and the
/// explore row's own gesture — had already been given the already-active case
/// by hand; the *context menu*'s Rename went through `pending_document_rename`
/// with no such branch, so it did nothing on the document in front too.
#[test]
fn the_rename_row_opens_the_prompt_on_the_document_already_in_front() {
    let (mut v, workspace) = on_document();
    let document = koru_core::DocumentId::new();
    let node = NodeId::from_uuid(document.as_uuid());
    View::set_entries(
        &mut v,
        vec![document_entry(document, "Notes")],
        Some(document),
    );
    v.tabs_mut().active_mut().document = Some(document);

    assert!(v.run_menu_action(MenuSubject::Nodes(vec![node]), MenuAction::Rename));
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::Navigate { .. })
    ));
    assert!(View::open_search_target(
        &mut v,
        &workspace.ws,
        Some(node),
        None,
        None,
        None
    ));
    assert!(v.tabs().active_tab().view.is_renaming());
    assert!(v.pending_document_rename.is_none());
}

/// A shell announcement about a *different* document leaves the request
/// standing — a workspace switch lands on its default document first, and
/// dropping the gesture there is how it would silently do nothing again. What
/// it must not do is fire over that stranger.
#[test]
fn an_attributes_request_waits_for_the_document_it_named() {
    let (mut v, workspace) = on_document();
    let here = koru_core::DocumentId::new();
    v.tabs_mut().active_mut().document = Some(here);

    let mut other_ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("create");
    let other_root = a_document(&mut other_ws, "Brief");
    let other = koru_core::DocumentId::new();
    let other_node = NodeId::from_uuid(other.as_uuid());
    View::set_entries(
        &mut v,
        vec![
            document_entry(here, "Notes"),
            document_entry(other, "Brief"),
        ],
        Some(here),
    );

    assert!(v.run_menu_action(
        MenuSubject::Nodes(vec![other_node]),
        MenuAction::AttributeTags
    ));
    let _ = View::take_pane_request(&mut v);

    // The workspace's own document arrives first.
    assert!(View::show_active_document(
        &mut v,
        &workspace.ws,
        Some(here)
    ));
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("a document tab")
    };
    assert!(
        !leaf.view().overlay.is_open(),
        "nothing opened over the document nobody asked about"
    );
    assert!(
        v.pending_document_attrs.is_some(),
        "the request still stands"
    );

    assert!(View::show_active_document(&mut v, &other_ws, Some(other)));
    assert_picker_over(&v, MenuAction::AttributeTags, other_root);
}

// ------------------------------------------------- two windows, one workspace

/// The five explore-row gestures, and how each is raised.
///
/// The tab strip's *Rename* names a tab; the other four and the row menu's own
/// *Rename* name a node. All five defer, so all five owe their answer to the
/// window they were made in.
fn gesture_rows() -> [MenuAction; 5] {
    [
        MenuAction::Rename,
        MenuAction::AttributeTags,
        MenuAction::AttributeColor,
        MenuAction::AttributeDue,
        MenuAction::AttributePriority,
    ]
}

/// **Q44, half one: the ask says where it belongs.**
///
/// A gesture whose other half is waiting on this container must be answered
/// here, so it asks for its landing with `SourceWindow` rather than the
/// `CurrentWindow` a search hit uses. This is the seam — the shell's
/// `landing_order` is what honours it, and
/// `a_gesture_lands_only_in_the_window_that_asked` in `koru-workspace-host` is what
/// proves the other side.
#[test]
fn a_deferred_explore_gesture_asks_for_a_landing_in_this_window() {
    for action in gesture_rows() {
        let (mut v, _workspace) = on_document();
        let here = koru_core::DocumentId::new();
        v.tabs_mut().active_mut().document = Some(here);
        let other = koru_core::DocumentId::new();
        let other_node = NodeId::from_uuid(other.as_uuid());
        View::set_entries(
            &mut v,
            vec![
                document_entry(here, "Notes"),
                document_entry(other, "Brief"),
            ],
            Some(here),
        );

        assert!(v.run_menu_action(MenuSubject::Nodes(vec![other_node]), action));
        let Some(koru_workspace_host::PaneRequest::Navigate { disposition, .. }) =
            View::take_pane_request(&mut v)
        else {
            panic!("{action:?} did not ask the shell for its document");
        };
        assert_eq!(
            disposition,
            koru_workspace_host::SearchTargetDisposition::SourceWindow,
            "{action:?} let a second window take its landing"
        );
    }

    // The tab strip's own Rename is the same gesture wearing a different
    // subject, and used the same pending field.
    let (mut v, _workspace) = on_document();
    let document = koru_core::DocumentId::new();
    v.tabs_mut().active_mut().document = Some(document);
    View::set_entries(
        &mut v,
        vec![document_entry(document, "Notes")],
        Some(koru_core::DocumentId::new()),
    );
    let tab = v.tabs().active();
    assert!(v.run_menu_action(MenuSubject::Tab(tab), MenuAction::Rename));
    let Some(koru_workspace_host::PaneRequest::Navigate { disposition, .. }) =
        View::take_pane_request(&mut v)
    else {
        panic!("the tab menu's Rename did not ask for its document");
    };
    assert_eq!(
        disposition,
        koru_workspace_host::SearchTargetDisposition::SourceWindow
    );
}

/// **Q44, half two: the picker opens in A, and B is left alone.**
///
/// Two windows on one workspace, both showing the same document — and B has it
/// *pinned*, which under the ordinary preference order takes the landing out of
/// A's hands outright (`a_chosen_destination_prefers_the_window_already_holding_it`).
/// Right-clicking the row in A used to hand the answer to B: A's request then
/// waited, opening nothing, until some later arrival of the same document.
///
/// There is no shell here — `App` owns the panes and needs a real window — so
/// what this drives is the routing the shell now performs: the landing goes to
/// the source window and to no other. What it proves is the half that is this
/// crate's: A completes its own gesture, and B, never offered the landing, is
/// unmoved by it.
#[test]
fn a_second_window_does_not_take_a_gesture_raised_in_the_first() {
    for action in gesture_rows() {
        // One workspace document, two containers over it — which is exactly
        // what two windows on one workspace are.
        let (mut a, workspace) = on_document();
        let root = a.document_root().expect("a document tab");
        let (mut b, _) = container();
        b.open_node(&workspace.ws, root);

        let document = koru_core::DocumentId::new();
        let node = NodeId::from_uuid(document.as_uuid());
        for v in [&mut a, &mut b] {
            View::set_entries(v, vec![document_entry(document, "Notes")], Some(document));
            v.tabs_mut().active_mut().document = Some(document);
        }
        // What makes B the preferred landing under the search-result order.
        let pinned = b.tabs().active();
        assert!(b.pin_tab(pinned));

        let b_tabs = b.tabs().len();
        let b_active = b.tabs().active();

        assert!(a.run_menu_action(MenuSubject::Nodes(vec![node]), action));
        let Some(koru_workspace_host::PaneRequest::Navigate { disposition, .. }) =
            View::take_pane_request(&mut a)
        else {
            panic!("{action:?} did not ask the shell for its document");
        };
        assert_eq!(
            disposition,
            koru_workspace_host::SearchTargetDisposition::SourceWindow
        );

        // The shell offers a `SourceWindow` landing to the source and to
        // nothing else. This is that offer.
        assert!(View::open_search_target(
            &mut a,
            &workspace.ws,
            Some(node),
            None,
            None,
            None
        ));

        if action == MenuAction::Rename {
            assert!(
                a.tabs().active_tab().view.is_renaming(),
                "the prompt opened in the window that asked"
            );
            assert!(a.pending_document_rename.is_none());
        } else {
            assert_picker_over(&a, action, root);
            assert!(a.pending_document_attrs.is_none());
        }

        // B: not asked, and not moved. Its strip and its active tab are where
        // they were, and nothing is open over its page.
        assert_eq!(b.tabs().len(), b_tabs, "{action:?} opened a tab in B");
        assert_eq!(
            b.tabs().active(),
            b_active,
            "{action:?} moved B's active tab"
        );
        assert!(!b.tabs().active_tab().view.is_renaming());
        let TabView::Leaf(leaf) = &b.tabs().active_tab().view else {
            panic!("B is showing a document")
        };
        assert!(
            !leaf.view().overlay.is_open(),
            "{action:?} opened a picker in the window nobody right-clicked in"
        );

        // And it stays inert even if something else routes that document
        // through it: the gesture is state on the container that raised it,
        // not a fact about the workspace.
        assert!(View::open_search_target(
            &mut b,
            &workspace.ws,
            Some(node),
            None,
            None,
            None
        ));
        assert!(!b.tabs().active_tab().view.is_renaming());
        let TabView::Leaf(leaf) = &b.tabs().active_tab().view else {
            panic!("B is showing a document")
        };
        assert!(!leaf.view().overlay.is_open());
    }
}

/// A gesture nothing ever answers is dropped rather than kept: the shell can
/// fail to resolve the document and report it in a dialog, and without a bound
/// the request would sit here and fire on that page minutes later, when
/// somebody opened it for their own reasons. The bound is read at the next
/// arrival — there is no timer, and nothing wakes the loop for this.
#[test]
fn an_unanswered_explore_gesture_is_dropped_rather_than_kept() {
    let (mut v, workspace) = on_document();
    let here = koru_core::DocumentId::new();
    v.tabs_mut().active_mut().document = Some(here);
    let other = koru_core::DocumentId::new();
    let other_node = NodeId::from_uuid(other.as_uuid());
    View::set_entries(
        &mut v,
        vec![
            document_entry(here, "Notes"),
            document_entry(other, "Brief"),
        ],
        Some(here),
    );

    assert!(v.run_menu_action(
        MenuSubject::Nodes(vec![other_node]),
        MenuAction::AttributeTags
    ));
    assert!(v.run_menu_action(MenuSubject::Nodes(vec![other_node]), MenuAction::Rename));
    let _ = View::take_pane_request(&mut v);
    assert!(v.pending_document_attrs.is_some());
    assert!(v.pending_document_rename.is_some());

    // Nothing answered, and the wait has run out.
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(1);
    if let Some((document, kind, _)) = v.pending_document_attrs {
        v.pending_document_attrs = Some((document, kind, expired));
    }
    if let Some((document, _)) = v.pending_document_rename {
        v.pending_document_rename = Some((document, expired));
    }

    // The next arrival is somebody else's document, and it clears both.
    assert!(View::show_active_document(
        &mut v,
        &workspace.ws,
        Some(here)
    ));
    assert!(v.pending_document_attrs.is_none());
    assert!(v.pending_document_rename.is_none());
    assert!(!v.tabs().active_tab().view.is_renaming());
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("a document tab")
    };
    assert!(!leaf.view().overlay.is_open());
}

#[test]
fn a_document_row_menu_opens_in_a_new_tab() {
    let (mut v, workspace) = on_document();
    let before = v.tabs().len();
    let document = koru_core::DocumentId::new();
    let node = koru_core::NodeId::from_uuid(document.as_uuid());
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: document,
            parent: None,
            ordinal: 0,
            title: "Brief".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );

    assert!(v.open_node_menu(vec![node], (120.0, 300.0), false));
    let menu = v.menu.as_ref().expect("the document menu opened");
    assert!(menu.items().contains(&MenuAction::OpenInNewTab));
    assert!(v.item_is_enabled(menu, MenuAction::OpenInNewTab));

    assert!(v.run_menu_action(MenuSubject::Nodes(vec![node]), MenuAction::OpenInNewTab));
    assert_eq!(
        v.pending_lands.front().copied(),
        Some((document, koru_editor::OpenWhere::NewTab))
    );
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document { document: id, .. },
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
            ..
        }) if id == document
    ));
    assert!(View::show_active_document(
        &mut v,
        &workspace.ws,
        Some(document),
    ));
    assert_eq!(v.tabs().len(), before + 1);
}

#[test]
fn a_document_row_menu_opens_in_a_new_window() {
    let (mut v, _workspace) = on_document();
    let before = v.tabs().len();
    let document = koru_core::DocumentId::new();
    let node = koru_core::NodeId::from_uuid(document.as_uuid());
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: document,
            parent: None,
            ordinal: 0,
            title: "Brief".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );

    assert!(v.open_node_menu(vec![node], (120.0, 300.0), false));
    let menu = v.menu.as_ref().expect("the document menu opened");
    assert!(menu.items().contains(&MenuAction::OpenDocumentInNewWindow));
    assert!(v.item_is_enabled(menu, MenuAction::OpenDocumentInNewWindow));

    assert!(v.run_menu_action(
        MenuSubject::Nodes(vec![node]),
        MenuAction::OpenDocumentInNewWindow
    ));
    assert!(v.pending_lands.is_empty());
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::OpenDocumentsInNewWindow { documents })
            if documents == vec![document]
    ));
    assert_eq!(v.tabs().len(), before, "this window's tabs do not change");
}

/// A document pill opens a **tab**, and the shell is never asked for a window.
#[test]
fn opening_a_document_makes_a_tab_and_not_a_window() {
    let (mut v, mut workspace) = on_document();
    let doc = a_document(&mut workspace.ws, "Brief");
    let before = v.tabs().len();

    assert!(v.open_node(&workspace.ws, doc));
    assert_eq!(v.tabs().len(), before + 1);
    assert_eq!(v.document_root(), Some(doc));
    assert_eq!(
        v.take_wants_open(),
        None,
        "the container consumed it; the shell must not open a window"
    );
}

#[test]
fn opening_the_same_document_twice_focuses_the_tab_that_has_it() {
    let (mut v, mut workspace) = on_document();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.open_node(&workspace.ws, doc);
    let after_first = v.tabs().len();

    v.select(0);
    assert!(v.open_node(&workspace.ws, doc));
    assert_eq!(
        v.tabs().len(),
        after_first,
        "no second view of one document"
    );
    assert_eq!(v.document_root(), Some(doc));
}

#[test]
fn a_tab_is_titled_by_its_document_and_follows_a_rename() {
    let (mut v, mut workspace) = on_document();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.open_node(&workspace.ws, doc);
    assert_eq!(v.tabs().active_tab().label, "Brief");

    workspace.ws.set_text(doc, "Renamed").expect("rename");
    v.adopt(&workspace.ws);
    assert_eq!(v.tabs().active_tab().label, "Renamed");
}

/// A leading emoji is the tab's icon, not a second copy in the label.
#[test]
fn a_leading_title_emoji_becomes_the_tab_icon() {
    let (mut v, mut workspace) = on_document();
    let doc = a_document(&mut workspace.ws, "💻Craft.do Pricing");
    v.open_node(&workspace.ws, doc);
    let tab = v.tabs().active_tab();
    assert_eq!(tab.icon.as_deref(), Some("💻"));
    assert_eq!(tab.label, "Craft.do Pricing");
}

#[test]
fn the_taskbar_prompt_creates_a_named_document_and_opens_its_tab() {
    let (mut v, mut workspace) = container();
    let before = v.tabs().len();
    v.prompt_new_document();
    assert!(v.new_document.is_some(), "the modal is open");

    assert!(v.key(&mut workspace.ws, &ch("Roadmap"), Some("Roadmap"), NONE,));
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    ));

    assert!(v.new_document.is_none(), "commit closes the modal");

    // **The document is not made here any more.** It belongs to the workspace,
    // and under the store creating one is a round trip that registers it in the
    // manifest — so the view asks the shell, which owns both. Inserting a node
    // into the `WorkspaceDoc` this view holds would nest a page inside whatever
    // is open, which looks right in the sidebar and is somewhere else after a
    // reopen.
    //
    // So what this pins is the *request*, and the tab count is deliberately
    // unchanged: the tab opens when the shell answers. That the created document
    // is the workspace's rather than the open document's is pinned against a
    // real store in `apps/koru-store-server/tests/document_roundtrip.rs`.
    assert_eq!(v.tabs().len(), before, "no tab until the shell has made it");
    assert!(matches!(
        koru_workspace_host::View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::CreateDocument(title)) if title == "Roadmap"
    ));
    let _ = &workspace;
}

#[test]
fn the_taskbar_prompt_keeps_an_empty_name_open_and_escape_cancels() {
    let (mut v, mut workspace) = container();
    let before = v.tabs().len();
    v.prompt_new_document();

    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    ));
    assert!(
        v.new_document.is_some(),
        "Enter cannot create an unnamed document"
    );
    assert_eq!(v.tabs().len(), before);

    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        NONE,
    ));
    assert!(v.new_document.is_none());
    assert_eq!(v.tabs().len(), before, "cancelling creates no tab");
}

#[test]
fn a_new_workspace_is_named_before_the_shell_is_asked_to_create_it() {
    let (mut v, mut workspace) = container();
    assert!(v.prompt_new_workspace());
    assert!(v.new_document.is_some(), "the shared name prompt is open");

    assert!(v.key(&mut workspace.ws, &ch("Research"), Some("Research"), NONE,));
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    ));

    assert!(v.new_document.is_none());
    assert!(matches!(
        koru_workspace_host::View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::NewWorkspace(name)) if name == "Research"
    ));
}

/// The switcher's *New workspace* while another Profile's workspaces are
/// listed: the same name prompt, but the host is asked to make it **there**.
#[test]
fn a_new_workspace_in_another_profile_names_its_owner() {
    let (mut v, mut workspace) = container();
    let owner = koru_core::ProfileId::new();
    assert!(v.apply(&workspace.ws, crate::panes::Outcome::NewWorkspaceIn(owner)));
    assert!(v.new_document.is_some(), "the shared name prompt is open");

    assert!(v.key(&mut workspace.ws, &ch("Research"), Some("Research"), NONE,));
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    ));

    assert!(v.new_document.is_none());
    assert!(matches!(
        koru_workspace_host::View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::NewWorkspaceIn(o, name))
            if o == owner && name == "Research"
    ));
}

/// A cancelled *New workspace in…* prompt must not hand its owner to the next
/// workspace prompt, which belongs to the open Profile.
#[test]
fn a_cancelled_new_workspace_in_prompt_forgets_its_owner() {
    let (mut v, mut workspace) = container();
    let (_, profile, _, _) = v.pane_context.clone().expect("pane context");
    View::set_pane_context(&mut v, Vec::new(), profile, Vec::new(), None);
    assert!(v.prompt_new_workspace_in(koru_core::ProfileId::new()));
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        NONE,
    ));
    assert!(v.new_document.is_none());

    // No workspace is open, so a new document asks for one first.
    v.prompt_new_document();
    assert!(v.key(&mut workspace.ws, &ch("Research"), Some("Research"), NONE,));
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    ));
    assert!(matches!(
        koru_workspace_host::View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::NewWorkspace(name)) if name == "Research"
    ));
}

#[test]
fn document_creation_asks_for_a_workspace_when_none_is_open() {
    let (mut v, mut workspace) = container();
    let (_, profile, _, _) = v.pane_context.clone().expect("pane context");
    View::set_pane_context(&mut v, Vec::new(), profile, Vec::new(), None);

    v.prompt_new_document();
    assert!(v.key(&mut workspace.ws, &ch("Research"), Some("Research"), NONE,));
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    ));

    assert!(matches!(
        koru_workspace_host::View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::NewWorkspace(name)) if name == "Research"
    ));
}

// --------------------------------------------------------------------- tabs

/// Every tab closes now, including the last one — and what `TabSet` is left
/// holding is a fresh welcome tab rather than nothing. There is no permanent
/// tab any more: the outline was the only thing that needed one, and it is
/// becoming a `TaskGroup` node inside a document.
///
/// **This is the invariant, not the gesture.** It drives `close` directly,
/// which is what *Close All Tabs* and a pop-out use. Pressing ✕ on the only tab
/// closes the window instead — see
/// `closing_the_last_tab_by_hand_closes_the_window`.
#[test]
fn closing_the_only_tab_leaves_a_welcome_tab() {
    let (mut v, mut workspace) = on_document();
    // [Welcome, Notes] — close both, from the end.
    assert!(v.close(1));
    assert_eq!(v.tabs().len(), 1);
    assert!(v.close(0), "the last one goes too");
    assert_eq!(v.tabs().len(), 1, "and something is always showing");
    assert_eq!(v.tabs().get(0).map(|t| t.label.as_str()), Some("Welcome"));
    let _ = &mut workspace;
}

/// Closing the tab you are looking at returns along the trail of what was
/// shown — here "c" was showing just before "b" was selected — rather than to
/// whichever neighbour the indices leave. The no-trail fall-back is pinned in
/// `tabset::tests::closing_without_a_trail_focuses_the_neighbour`.
#[test]
fn closing_the_active_tab_returns_to_the_previously_shown_tab() {
    let (mut v, mut workspace) = on_document();
    for t in ["a", "b", "c"] {
        let d = a_document(&mut workspace.ws, t);
        v.open_node(&workspace.ws, d);
    }
    // welcome, workspace, a, b, c — opening each showed it, so the trail
    // reads a, b, c.
    assert_eq!(v.tabs().len(), 5);
    v.select(3); // "b"
    assert!(v.close(3));
    assert_eq!(v.tabs().active_tab().label, "c", "shown just before b was");
}

#[test]
fn ctrl_w_closes_and_ctrl_t_opens() {
    let (mut v, mut workspace) = container();
    let ws = &mut workspace.ws;
    let before = v.tabs().len();
    assert!(v.key(ws, &ch("t"), Some("t"), PRIMARY));
    assert_eq!(v.tabs().len(), before + 1);
    assert!(v.key(ws, &ch("w"), Some("w"), PRIMARY));
    assert_eq!(v.tabs().len(), before);
}

#[test]
fn ctrl_shift_a_opens_the_same_add_menu_as_the_plus() {
    let (mut v, mut workspace) = container();
    assert!(v.menu.is_none());
    assert!(v.key(&mut workspace.ws, &ch("a"), Some("a"), PRIMARY_SHIFT,));
    assert_eq!(
        v.menu.as_ref().unwrap().items(),
        context_menu::new_tab_items()
    );
}

#[test]
fn the_welcome_page_greets_the_owner_onboarding_named_for_this_profile() {
    let (mut v, _workspace) = container();
    let profile = koru_core::ProfileId::new();
    let workspace = WorkspaceId::new();
    v.set_pane_context(
        vec![koru_core::ProfileSummary {
            id: profile,
            name: "Work".into(),
            color: Some("blue".into()),
            ..Default::default()
        }],
        profile,
        vec![koru_core::profile::WorkspaceEntry::new(
            workspace, "Design", 0,
        )],
        Some(workspace),
    );
    assert!(v.show_first_minutes("Ana Lima", "Work", "Design"));
    let TabView::Welcome(welcome) = &v.tabs().active_tab().view else {
        panic!("onboarding lands on the Welcome tab");
    };
    let identity = welcome.identity();
    assert_eq!(identity.owner.as_deref(), Some("Ana Lima"));
    assert_eq!(
        identity.profile, "Work",
        "the owner's name is kept for this Profile"
    );
    assert_eq!(identity.workspace, "Design");
    // It brings the one Welcome tab forward rather than adding a second.
    assert_eq!(
        v.tabs()
            .iter()
            .filter(|t| matches!(t.view, TabView::Welcome(_)))
            .count(),
        1
    );

    // A name given for another Profile is not used to greet this one.
    assert!(v.show_first_minutes("Bo", "Personal", "Home"));
    let TabView::Welcome(welcome) = &v.tabs().active_tab().view else {
        panic!("still Welcome");
    };
    assert_eq!(welcome.identity().owner, None);
}

#[test]
fn the_add_menu_always_offers_one_welcome_row_that_selects_the_welcome_tab() {
    let (mut v, _workspace) = on_document();
    let welcome = v
        .tabs()
        .iter()
        .position(|t| matches!(t.view, TabView::Welcome(_)))
        .expect("the document fixture opens beside Welcome");
    assert!(v.open_new_tab_menu());
    let items = v.menu.as_ref().unwrap().items().to_vec();
    assert_eq!(
        items
            .iter()
            .filter(|a| **a == MenuAction::OpenWelcome)
            .count(),
        1,
        "one Welcome row, open or not"
    );
    v.menu = None;
    let before = v.tabs().len();
    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::OpenWelcome));
    assert_eq!(v.tabs().active(), welcome, "it selects the open one");
    assert_eq!(v.tabs().len(), before, "and adds nothing");

    // Where a Welcome tab can be closed, the same row brings it back.
    if v.close(welcome) {
        assert!(!v.tabs().has_welcome());
        assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::OpenWelcome));
        assert!(v.tabs().has_welcome());
    }
}

/// **The `+` menu's profile rows open a session tab in this window**
/// (TERMINATAB W4) — a real tab carrying the chosen profile as its pending
/// launch, not a request to the Agent for the separate Koru Terminal app,
/// which is what this row did before and what the index used to be discarded
/// into.
#[test]
fn the_add_menu_opens_a_session_tab_for_the_chosen_profile() {
    let (agent, sent) = crate::agent::AgentLink::for_test();
    let (mut v, _workspace) = container();
    v.agent = agent;

    // The *Shell* and *Agents* rows are parents; only their leaf
    // profiles open a session.
    assert!(!v.run_menu_action(MenuSubject::NewTab, MenuAction::NewShell));
    assert!(!v.run_menu_action(MenuSubject::NewTab, MenuAction::NewAgent));

    let shell = context_menu::shell_profile_items()
        .into_iter()
        .next()
        .expect("at least one shell profile");
    let MenuAction::TerminalProfile(index) = shell else {
        panic!("a shell profile row is a TerminalProfile action")
    };
    let before = v.tabs().len();
    assert!(v.run_menu_action(MenuSubject::NewTab, shell));
    assert_eq!(
        v.tabs().len(),
        before + 1,
        "the chosen profile opened a tab"
    );
    let expected = koru_session_profiles::built_in_profiles()
        .into_iter()
        .nth(index)
        .expect("the chosen profile exists");
    {
        let tab = v.tabs().active_tab();
        assert_eq!(tab.label, expected.fallback_display_name());
        let TabView::Session(view) = &tab.view else {
            panic!("the new tab is a session tab")
        };
        // The launch waits for the tab to settle as the active one — and it
        // carries the *chosen* profile, which the old path discarded.
        let launch = view
            .pending_launch
            .as_ref()
            .expect("the launch waits on the tab (no backend in a test)");
        assert_eq!(launch.profile.id, expected.id);
    }

    let agent_profile = context_menu::agent_profile_items()
        .into_iter()
        .next()
        .expect("at least one agent profile");
    assert!(v.run_menu_action(MenuSubject::NewTab, agent_profile));
    assert_eq!(v.tabs().len(), before + 2);
    assert!(matches!(v.tabs().active_tab().view, TabView::Session(_)));

    // And nothing went to the Agent: the tab opens in this window, not in a
    // separate process.
    assert!(
        sent.try_recv().is_err(),
        "no Agent request leaves the window"
    );
}

/// A recent document already in the strip comes forward rather than opening
/// again; a recent chat does the same even before its transcript has loaded.
#[test]
fn the_plus_menu_focuses_a_tab_already_showing_the_recent() {
    let (mut v, mut workspace) = container();
    let notes = a_document(&mut workspace.ws, "Notes");
    v.open_node(&workspace.ws, notes);
    let notes_id = koru_core::DocumentId::from_uuid(notes.as_uuid());
    let notes_tab = v.tabs().active();
    v.tabs_mut().get_mut(notes_tab).unwrap().document = Some(notes_id);

    let second = a_document(&mut workspace.ws, "Second");
    v.open_node(&workspace.ws, second);
    assert_ne!(v.tabs().active(), notes_tab, "Second is showing");
    assert!(v.run_menu_action(
        MenuSubject::NewTab,
        MenuAction::OpenRecentDocument(notes_id),
    ));
    assert_eq!(v.tabs().active(), notes_tab);

    let chat = koru_ai_ui::ChatId::new();
    let before = v.tabs().len();
    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::OpenRecentChat(chat),));
    assert_eq!(v.tabs().len(), before + 1);
    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::OpenRecentChat(chat),));
    assert_eq!(
        v.tabs().len(),
        before + 1,
        "the same chat does not mint a second tab"
    );
}

/// A Chats flyout opened before the list has landed stays empty; when the
/// snapshot is filled, the open flyout is rebuilt rather than waiting for
/// the next Add….
#[test]
fn the_plus_menu_chats_flyout_fills_when_the_list_arrives() {
    let (mut v, mut workspace) = container();
    assert!(v.key(&mut workspace.ws, &ch("a"), Some("a"), PRIMARY_SHIFT));
    let chats = v
        .menu
        .as_ref()
        .unwrap()
        .items()
        .iter()
        .position(|a| *a == MenuAction::Chats)
        .expect("Chats row");
    assert!(v.open_submenu(chats));
    assert_eq!(
        v.submenu.as_ref().unwrap().1.items(),
        &[MenuAction::NewAiChat]
    );

    let id = koru_ai_ui::ChatId::new();
    context_menu::set_plus_snapshots(vec![], vec![], vec![], vec![(id, "Chat with Qwen".into())]);
    v.rebuild_plus_chats_flyout();
    assert_eq!(
        v.submenu.as_ref().unwrap().1.items(),
        &[
            MenuAction::NewAiChat,
            MenuAction::Divider,
            MenuAction::OpenRecentChat(id),
        ]
    );
    context_menu::set_plus_snapshots(vec![], vec![], vec![], vec![]);
}

/// **A machine missing a program's executable dims that program's row** with
/// the *Not installed* note — one uniform rule for shells and agents, the
/// same as Koru Terminal's launch menus (W4.4a). The probe's answer is
/// **injected**: what is under test is the menu's reflection of an answer,
/// never what happens to be on the test machine's real `PATH` — the menus
/// are built directly rather than through `open_new_tab_menu`, whose refresh
/// would race a real probe against the injection.
#[test]
fn a_missing_program_dims_its_profile_row_shell_or_agent() {
    let (v, _workspace) = container();
    let agents = context_menu::agent_profile_items();
    let agent_menu = ContextMenu::at_width(
        MenuSubject::NewTab,
        (100.0, 100.0),
        agents.clone(),
        MenuAnchor::Cursor,
        200.0,
    );
    let shells = context_menu::shell_profile_items();
    let shell_menu = ContextMenu::at_width(
        MenuSubject::NewTab,
        (100.0, 100.0),
        shells.clone(),
        MenuAnchor::Cursor,
        200.0,
    );
    // Before any probe has answered, the last known answer is "everything":
    // the menu opens without waiting on the world and dims when told.
    assert!(
        v.enabled_items(&agent_menu).iter().all(|&row| row),
        "no answer yet dims nothing"
    );

    // This "machine": every shell present, every agent missing.
    v.profile_probe.set_for_test(
        koru_session_profiles::built_in_profiles()
            .iter()
            .map(|profile| profile.kind != koru_session_protocol::ProfileKind::Agent)
            .collect(),
    );
    let enabled = v.enabled_items(&agent_menu);
    assert!(!agents.is_empty());
    assert!(
        enabled.iter().all(|&row| !row),
        "every missing agent is dimmed: {enabled:?}"
    );
    // Dimmed, not hidden — the rows are still in the menu, and each explains
    // itself with the note the widget shows beside a disabled row.
    for action in agents {
        assert_eq!(
            koru_ui::MenuItem::disabled_reason(&action).as_deref(),
            Some("Not installed")
        );
    }
    // The shells this answer found installed stay live — presence, not kind,
    // is what enables a row.
    assert!(
        v.enabled_items(&shell_menu).iter().all(|&row| row),
        "an installed shell stays live"
    );

    // And a probed-missing shell dims exactly like a missing agent, with the
    // same note: the two surfaces match, with no shell special case.
    v.profile_probe.set_for_test(
        koru_session_profiles::built_in_profiles()
            .iter()
            .map(|_| false)
            .collect(),
    );
    assert!(!shells.is_empty());
    assert!(
        v.enabled_items(&shell_menu).iter().all(|&row| !row),
        "a missing shell is dimmed too"
    );
    for action in shells {
        assert_eq!(
            koru_ui::MenuItem::disabled_reason(&action).as_deref(),
            Some("Not installed")
        );
    }
}

/// **Only the active session tab is live** — the active-tab-exclusive rule.
/// Switching to a document tab deactivates the session surface (no blink
/// deadline, no timer wake); switching back reactivates it. The controller
/// lease follows the same reconciliation, exercised here as far as a container
/// without a live host can be.
#[test]
fn a_session_tab_is_active_exactly_while_it_is_the_tab_showing() {
    let (mut v, mut workspace) = container();
    let shell = context_menu::shell_profile_items()
        .into_iter()
        .next()
        .expect("at least one shell profile");
    assert!(v.run_menu_action(MenuSubject::NewTab, shell));
    let session_tab = v.tabs().active();
    View::maintain(&mut v, &mut workspace.ws, std::time::Instant::now());
    {
        let TabView::Session(view) = &v.tabs().active_tab().view else {
            panic!("a session tab is active")
        };
        assert!(view.surface.active, "the shown session surface is active");
    }

    // Switch to the welcome tab: the session surface deactivates and costs no
    // maintenance deadline at all.
    v.select(0);
    View::maintain(&mut v, &mut workspace.ws, std::time::Instant::now());
    {
        let Some(tab) = v.tabs().get(session_tab) else {
            panic!("the session tab is still open")
        };
        let TabView::Session(view) = &tab.view else {
            panic!("the session tab is still a session tab")
        };
        assert!(!view.surface.active, "a hidden session surface is inactive");
        assert_eq!(
            View::maintenance_deadline(view),
            None,
            "an inactive session tab sets no timer wake"
        );
    }

    // And back: it reactivates.
    v.select(session_tab);
    View::maintain(&mut v, &mut workspace.ws, std::time::Instant::now());
    let TabView::Session(view) = &v.tabs().get(session_tab).expect("open").view else {
        panic!("still a session tab")
    };
    assert!(view.surface.active);
}

// ------------------------------------------------------- Ctrl+S, a snapshot

/// A container showing a document the history panel could actually address.
///
/// `on_document` leaves both the tab's `document` and `active_document` unset —
/// its workspace has no manifest, so no tab carries a `DocumentId` — and Ctrl+S
/// deliberately refuses that state, because a snapshot needs a document the
/// store can name. **The same id in both places**, which is what the shell's
/// `show_active_document` establishes in the running app and what
/// `prompt_save_snapshot` insists on.
fn on_a_savable_document() -> (MainView, koru_workspace::Workspace) {
    let (mut v, w) = on_document();
    let id = koru_core::DocumentId::new();
    let i = v.tabs().active();
    v.tabs_mut().get_mut(i).expect("the active tab").document = Some(id);
    v.active_document = Some(id);
    (v, w)
}

/// Ctrl+S, then the maintenance pass its answer arrives in.
///
/// **Two steps, because the press asks the store which modal to open.**
/// Whether the document has changed since its newest marker is the store's
/// fact rather than the view's guess, so `prompt_save_snapshot` asks and the
/// modal is built when the answer lands. With no store client in a container
/// fixture the panel settles on the save modal immediately, so this is one
/// extra tick and not a wait.
fn ctrl_s(v: &mut MainView, workspace: &mut koru_workspace::Workspace) -> bool {
    let handled = v.key(&mut workspace.ws, &ch("s"), Some("s"), PRIMARY);
    View::maintain(v, &mut workspace.ws, std::time::Instant::now());
    handled
}

#[test]
fn ctrl_s_opens_the_snapshot_prompt_on_a_document_tab() {
    let (mut v, mut workspace) = on_a_savable_document();
    assert!(v.snapshot_prompt.is_none());
    assert!(ctrl_s(&mut v, &mut workspace));
    assert!(v.snapshot_prompt.is_some(), "Ctrl+S asks what to call it");
    // And it owns the keyboard while it is up, so a keystroke cannot reach the
    // document behind the scrim.
    assert_eq!(
        v.blockers(),
        koru_workspace_host::routing::Blockers::Prompt(
            koru_workspace_host::routing::PromptKind::Snapshot
        )
    );
}

/// **A no-op on every tab that is not a document**, which is the answer the
/// packet asked for and the one that cannot surprise anybody: there is no
/// document on the welcome screen to take a snapshot of.
#[test]
fn ctrl_s_does_nothing_on_a_welcome_tab() {
    let (mut v, mut workspace) = container();
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Welcome(_)),
        "the fixture opens on the welcome screen"
    );
    assert!(!ctrl_s(&mut v, &mut workspace));
    assert!(v.snapshot_prompt.is_none());
}

/// The Trash is a listing, not a document. Same answer, and it is worth pinning
/// separately: the Trash tab is a different `TabView` arm and a future arm would
/// have to make the same choice deliberately.
#[test]
fn ctrl_s_does_nothing_on_a_trash_tab() {
    let (mut v, mut workspace) = on_a_savable_document();
    v.open_trash_tab();
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Trash(_)),
        "the Trash tab is showing"
    );
    assert!(!ctrl_s(&mut v, &mut workspace));
    assert!(v.snapshot_prompt.is_none());
}

/// **`active_document` alone is not the test.** It keeps naming the last
/// document opened while a welcome or Trash tab is in front, so a gate written
/// on it by itself would open the prompt over a tab with nothing to snapshot.
#[test]
fn a_stale_active_document_does_not_make_a_welcome_tab_savable() {
    let (mut v, mut workspace) = on_a_savable_document();
    v.select(0);
    assert!(matches!(v.tabs().active_tab().view, TabView::Welcome(_)));
    assert!(
        v.active_document.is_some(),
        "the id is still there — that is the point"
    );
    assert!(!ctrl_s(&mut v, &mut workspace));
    assert!(v.snapshot_prompt.is_none());
}

/// **And `TabView::Leaf` is not the test either.** A draft is a leaf over a
/// private in-memory workspace with no document in anybody's library yet, so a
/// gate written on the arm would have snapshotted whatever `active_document`
/// still pointed at — a different document from the one on screen, silently.
#[test]
fn ctrl_s_does_nothing_on_a_draft_tab() {
    let (mut v, mut workspace) = on_a_savable_document();
    // Ctrl+T opens a draft, which is how the app makes one.
    assert!(v.key(&mut workspace.ws, &ch("t"), Some("t"), PRIMARY));
    assert!(
        v.tabs().active_tab().draft.is_some(),
        "the new tab is a draft"
    );
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Leaf(_)),
        "a draft is a leaf — that is the trap"
    );
    assert!(
        v.tabs().active_tab().document.is_none(),
        "and it names no store document"
    );
    assert!(!ctrl_s(&mut v, &mut workspace));
    assert!(v.snapshot_prompt.is_none());
}

/// Escape closes it and writes nothing. Enter is covered where the declaration
/// lives, in `snapshot_prompt`’s tests.
#[test]
fn escape_closes_the_snapshot_prompt() {
    let (mut v, mut workspace) = on_a_savable_document();
    assert!(ctrl_s(&mut v, &mut workspace));
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        NONE
    ));
    assert!(v.snapshot_prompt.is_none());
    assert_eq!(v.blockers(), koru_workspace_host::routing::Blockers::None);
}

/// Enter closes the prompt through the container, which is the half
/// `snapshot_prompt`’s own tests cannot see: the modal answers `Save`, and the
/// container is what puts it away and hands the name to the history panel.
#[test]
fn enter_closes_the_snapshot_prompt_through_the_container() {
    let (mut v, mut workspace) = on_a_savable_document();
    assert!(ctrl_s(&mut v, &mut workspace));
    for c in ["d", "r", "a", "f", "t"] {
        assert!(v.key(&mut workspace.ws, &ch(c), Some(c), NONE));
    }
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE
    ));
    assert!(v.snapshot_prompt.is_none());
    assert_eq!(v.blockers(), koru_workspace_host::routing::Blockers::None);
}

/// **Ctrl+Shift+S is still Strikethrough's.** The container resolves first, so
/// a shifted arm here would take it from the editor silently — the trap
/// `mainview::keys` warns about. Asserted at the container rather than only in
/// `keys::tests` because this is the layer that would do the stealing.
#[test]
fn ctrl_shift_s_does_not_open_the_snapshot_prompt() {
    let (mut v, mut workspace) = on_a_savable_document();
    v.key(&mut workspace.ws, &ch("s"), Some("s"), PRIMARY_SHIFT);
    // Through the maintenance pass too: nothing must appear a tick later
    // either, which is where the prompt is built now.
    View::maintain(&mut v, &mut workspace.ws, std::time::Instant::now());
    assert!(
        v.snapshot_prompt.is_none(),
        "the shifted form belongs to the editor"
    );
}

#[test]
fn ctrl_digits_select_by_position_and_a_missing_tab_is_ignored() {
    let (mut v, mut workspace) = on_document();
    let ws = &mut workspace.ws;
    v.select(0);
    assert!(v.key(ws, &ch("2"), Some("2"), PRIMARY));
    assert_eq!(v.tabs().active(), 1);
    assert!(
        !v.key(ws, &ch("9"), Some("9"), PRIMARY),
        "there is no ninth tab"
    );
    assert_eq!(v.tabs().active(), 1, "and the selection did not move");
}

/// Ctrl+Tab reaches the container as a direction, because the shell is the only
/// place that can tell it apart from Ctrl+I. See `keys::tests`.
#[test]
fn cycle_tab_wraps_in_both_directions() {
    let (mut v, _w) = on_document();
    assert_eq!(v.tabs().len(), 2);
    v.select(0);
    assert_eq!(v.tabs().active(), 0);
    assert!(v.cycle_tab(1));
    assert_eq!(v.tabs().active(), 1);
    assert!(v.cycle_tab(1));
    assert_eq!(v.tabs().active(), 0, "wraps forward");
    assert!(v.cycle_tab(-1));
    assert_eq!(v.tabs().active(), 1, "and backward");
}

// (A lone tab having nowhere to cycle to is `tabset::tests`' — reaching into
// the list to build that case is exactly what `TabSet` now prevents.)

fn four_tabs() -> (MainView, koru_workspace::Workspace) {
    let (mut v, mut workspace) = on_document();
    for t in ["a", "b"] {
        let d = a_document(&mut workspace.ws, t);
        v.open_node(&workspace.ws, d);
    }
    (v, workspace)
}

fn labels(v: &MainView) -> Vec<String> {
    v.tabs().iter().map(|t| t.label.clone()).collect()
}

fn tab_rects(v: &mut MainView) -> Vec<tabs::TabRect> {
    v.ensure_tab_widths();
    tabs::layout(v.strip(), &v.tab_widths().clone(), v.tab_scroll())
}

/// A window whose focused strip overflows, so the tab scrollbar has a track
/// and a thumb to grab.
fn overflowing_tabs() -> (MainView, koru_workspace::Workspace) {
    let (mut v, mut workspace) = on_document();
    for i in 0..12 {
        let d = a_document(&mut workspace.ws, Box::leak(i.to_string().into_boxed_str()));
        v.open_node(&workspace.ws, d);
    }
    v.ensure_tab_widths();
    (v, workspace)
}

/// Pressing the scrollbar's thumb grabs it, and the release — wherever it
/// lands — lets it go.
#[test]
fn releasing_the_tab_scrollbar_ends_its_drag() {
    let (mut v, mut workspace) = overflowing_tabs();
    let strip = v.strip();
    let thumb = tabs::scroll_thumb(strip, &v.tab_widths(), v.tab_scroll()).expect("a thumb");
    let (x, y) = thumb.centre();

    v.mouse_down(&mut workspace.ws, x, y, false);
    assert!(v.tab_scroll_drag().is_some(), "the press grabbed the thumb");
    v.mouse_move(x + 50.0, y);
    v.mouse_up(&mut workspace.ws);
    assert!(
        v.tab_scroll_drag().is_none(),
        "the release ended the drag — a stuck drag keeps scrolling with every move"
    );
}

/// The same gesture in a split: the press is on the *unfocused* pane's
/// scrollbar, and the release lands over the other pane. The drag lives in
/// the pane the press grabbed, so the release must find it there — not in
/// whichever pane is focused when the button goes up.
#[test]
fn releasing_a_tab_scrollbar_in_the_other_pane_ends_its_drag() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let (mut v, mut workspace) = overflowing_tabs();
    // Split the active tab out to the right; the overflowing strip stays left.
    let right = PanePath::root().child(1);
    let active = v.tabs().active_tab().id;
    let tab = v
        .panes
        .take_tab_by_id(&PanePath::root(), active)
        .expect("took the tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, tab);
    let left_rect = v
        .panes
        .leaf_rects(v.content())
        .into_iter()
        .find(|(p, _)| *p == PanePath::root().child(0))
        .map(|(_, r)| r)
        .expect("the left pane");

    // The press lands on the LEFT pane's scrollbar while the right is focused.
    v.panes.set_focus(right.clone());
    let left_header = Rect::new(left_rect.x, left_rect.y, left_rect.w, theme::CAPTION_H);
    let widths = v
        .panes
        .group(&PanePath::root().child(0))
        .unwrap()
        .widths
        .clone();
    let thumb = tabs::scroll_thumb(left_header, &widths, 0.0).expect("a thumb");
    let (x, y) = thumb.centre();
    v.mouse_down(&mut workspace.ws, x, y, false);
    v.mouse_move(x + 40.0, y);
    // Release over the right pane.
    v.mouse_up(&mut workspace.ws);

    let left = v.panes.group(&PanePath::root().child(0)).unwrap();
    assert!(
        left.scroll_drag.is_none(),
        "the drag stayed stuck in the pane it started in"
    );
}

#[test]
fn dragging_a_tab_past_its_neighbour_reorders_it() {
    let (mut v, mut workspace) = four_tabs();
    let strip = v.strip();
    assert_eq!(labels(&v), ["Welcome", "Notes", "a", "b"]);

    // Press the last tab, drag it left over the third.
    let rects = tab_rects(&mut v);
    let (from, to) = (3usize, 2usize);
    v.mouse_down(
        &mut workspace.ws,
        rects[from].body.centre().0,
        strip.centre().1,
        false,
    );
    assert_eq!(v.group_drag().as_ref().map(|d| d.index), Some(from));
    v.mouse_move(rects[to].body.x + 2.0, strip.centre().1);
    assert_eq!(labels(&v), ["Welcome", "Notes", "b", "a"]);
    v.mouse_up(&mut workspace.ws);
    assert!(v.group_drag().is_none(), "the drag ends on release");
}

/// **The tab travels with the pointer**, and from the point it was grabbed —
/// picking it up off-centre and moving one pixel must not jerk it so that the
/// cursor is suddenly somewhere else on it.
///
/// (Grabbed near the *left* edge on purpose: the right end is the close box,
/// where a press closes the tab rather than picking it up.)
#[test]
fn a_carried_tab_keeps_the_grip_it_was_picked_up_by() {
    let (mut v, mut workspace) = four_tabs();
    let strip = v.strip();
    let rects = tab_rects(&mut v);
    let grab_at = rects[2].body.x + 12.0;
    v.mouse_down(&mut workspace.ws, grab_at, strip.centre().1, false);
    let (i, dx) = v.carry_offset().expect("carried from the press");
    assert_eq!(i, 2);
    assert!(dx.abs() < 0.01, "at rest where it sits, not {dx}");

    // Nudge right by less than half a tab: it follows exactly, and nothing swaps.
    v.mouse_move(grab_at + 9.0, strip.centre().1);
    let (i, dx) = v.carry_offset().expect("still carried");
    assert_eq!(i, 2);
    assert!((dx - 9.0).abs() < 0.01, "moved {dx}, not 9");
    assert_eq!(labels(&v), ["Welcome", "Notes", "a", "b"], "no swap yet");
}

/// The right-hand end of a tab is its close box, and a press there closes it
/// rather than picking it up. Worth pinning, because the ✕ is invisible until
/// hovered — what is drawn and what is clickable have to agree about where it is.
#[test]
fn pressing_the_close_box_closes_rather_than_starting_a_drag() {
    let (mut v, mut workspace) = four_tabs();
    let rects = tab_rects(&mut v);
    assert!(rects[2].close.w > 0.0, "the fixture is wide enough");
    let (cx, cy) = rects[2].close.centre();
    v.mouse_down(&mut workspace.ws, cx, cy, false);
    assert_eq!(labels(&v), ["Welcome", "Notes", "b"]);
    assert!(v.group_drag().is_none(), "and nothing is being carried");
}

/// **The swap is judged on the tab, not the pointer.** Two drags that carry the
/// tab to the same place must swap at the same moment however it was gripped —
/// which is only true if the grip is subtracted out first.
#[test]
fn the_swap_follows_the_tab_rather_than_the_pointer() {
    // The pointer position at which tab 3 first takes slot 2, for a given grip.
    fn swaps_at(grip: f32) -> f32 {
        let (mut v, mut workspace) = four_tabs();
        let strip = v.strip();
        let rects = tab_rects(&mut v);
        v.mouse_down(
            &mut workspace.ws,
            rects[3].body.x + grip,
            strip.centre().1,
            false,
        );
        let mut x = rects[3].body.x + grip;
        while v.tabs().active() == 3 && x > strip.x - 400.0 {
            x -= 1.0;
            v.mouse_move(x, strip.centre().1);
        }
        assert_eq!(v.tabs().active(), 2, "grip {grip} never swapped");
        // Where the *tab* had got to, which is what should be grip-independent.
        x - grip
    }

    let by_the_edge = swaps_at(4.0);
    let by_the_middle = swaps_at(70.0);
    assert!(
        (by_the_edge - by_the_middle).abs() <= 1.0,
        "the tab swapped at {by_the_edge} gripped by its edge and {by_the_middle} \
         gripped in the middle — the pointer is leaking into the rule"
    );
}

/// A drag that wanders off the strip must not carry the tab over the window
/// buttons or off the left edge — there is nowhere out there to drop it.
#[test]
fn a_carried_tab_stays_inside_the_strip() {
    let (mut v, mut workspace) = four_tabs();
    let strip = v.strip();
    let rects = tab_rects(&mut v);
    v.mouse_down(
        &mut workspace.ws,
        rects[3].body.centre().0,
        strip.centre().1,
        false,
    );
    for x in [-4000.0, 4000.0] {
        v.mouse_move(x, strip.centre().1);
        let (i, dx) = v.carry_offset().expect("still carried");
        let slot = tab_rects(&mut v)[i].body.x;
        let left = slot + dx;
        assert!(left >= strip.x - 0.01, "x={x} put it at {left}");
        assert!(
            // Bounded by the lane, not the `+` — the button travels with the
            // tabs, so its x is not a fixed limit.
            left + rects[3].body.w <= tabs::lane(strip).w + strip.x + 0.01,
            "x={x} put it past the lane"
        );
    }
}

/// Releasing does not teleport the tab into its slot — it eases in, the same
/// way its neighbours have been.
#[test]
fn releasing_a_tab_lets_it_drop_into_its_slot() {
    let (mut v, mut workspace) = four_tabs();
    let strip = v.strip();
    let rects = tab_rects(&mut v);
    v.mouse_down(
        &mut workspace.ws,
        rects[3].body.centre().0,
        strip.centre().1,
        false,
    );
    // Off-centre by a few pixels, not far enough to swap.
    v.mouse_move(rects[3].body.centre().0 + 7.0, strip.centre().1);
    v.mouse_up(&mut workspace.ws);
    assert!(v.group_drag().is_none());
    assert!(v.animating(), "it has somewhere left to travel");
}

// ------------------------------------------------------------------ pointer

/// The leaf's `hover_at` resolves a row from `y` alone, so a pointer parked
/// over the chrome would otherwise leave a row lit and wear an I-beam.
#[test]
fn a_pointer_over_the_chrome_does_not_wear_the_ibeam() {
    let (mut v, mut workspace) = on_document();
    fill(&v, &mut workspace.ws, 1);
    v.adopt(&workspace.ws);

    // Past the checkbox column (`text_x` is MARGIN_X + GUTTER) and below
    // MARGIN_TOP, so this is genuinely over the row's text.
    let c = v.content();
    v.mouse_move(c.x + 70.0, c.y + row_y(0));
    assert_eq!(v.cursor_icon(), CursorIcon::Text, "over a row");

    // Onto the caption, at the same height a row would be if it were not there.
    v.mouse_move(v.regions.caption.centre().0, v.regions.caption.centre().1);
    assert_ne!(v.cursor_icon(), CursorIcon::Text, "over the caption");
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("leaf")
    };
    assert_eq!(l.view().hover, None, "the row highlight was dropped");
}

/// A text selection, a row reorder, a thumb drag and an image resize all
/// deliberately follow the pointer outside the content rect. Cutting them off
/// at the region edge would be a regression against the editor's own behaviour.
#[test]
fn a_drag_that_leaves_the_content_rect_still_reaches_the_leaf() {
    let (mut v, mut workspace) = on_document();
    fill(&v, &mut workspace.ws, 4);
    v.adopt(&workspace.ws);

    // In the text column, not the checkbox one — a press on the box toggles it
    // rather than beginning a selection.
    let c = v.content();
    v.mouse_down(&mut workspace.ws, c.x + 70.0, c.y + row_y(0), false);
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("leaf")
    };
    assert!(l.view().dragging, "the press began a selection");

    // Now leave the content rect entirely — upward, over the caption.
    v.mouse_move(c.x + 70.0, 2.0);
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("leaf")
    };
    assert!(l.view().dragging, "the drag survived leaving the band");
}

/// The wheel arrives without coordinates, so it has to be routed on the last
/// pointer position — and must not scroll the document from over the chrome.
#[test]
fn the_wheel_only_reaches_the_view_it_is_over() {
    let (mut v, mut workspace) = on_document();
    fill(&v, &mut workspace.ws, 80);
    v.adopt(&workspace.ws);
    let scroll = |v: &MainView| match &v.tabs().active_tab().view {
        TabView::Leaf(l) => l.view().scroll,
        _ => 0.0,
    };
    assert_eq!(scroll(&v), 0.0);

    let down = MouseScrollDelta::LineDelta(0.0, -5.0);
    v.mouse_move(v.regions.caption.centre().0, v.regions.caption.centre().1);
    v.scroll_by(down);
    assert_eq!(scroll(&v), 0.0, "the wheel over the caption did nothing");

    let c = v.content();
    v.mouse_move(c.centre().0, c.centre().1);
    v.scroll_by(down);
    assert!(scroll(&v) > 0.0, "over the content it scrolled");
}

// ---------------------------------------------------------------- housekeeping

/// A node deleted here or by another peer must not leave a tab pointing at
/// nothing.
#[test]
fn a_tab_whose_document_was_deleted_closes_itself() {
    let (mut v, mut workspace) = on_document();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.open_node(&workspace.ws, doc);
    let before = v.tabs().len();

    workspace.ws.delete(doc).expect("delete");
    v.adopt(&workspace.ws);
    assert_eq!(v.tabs().len(), before - 1);
    assert!(
        v.tabs().active() < v.tabs().len(),
        "the active index stayed in range"
    );
}

/// `show_active_document` records the active document, so the welcome screen
/// has something to offer — most recent first, deduplicated.
#[test]
fn the_active_document_is_recorded_as_recent() {
    let (mut v, workspace) = container();
    let a = koru_core::DocumentId::new();
    let b = koru_core::DocumentId::new();
    v.show_active_document(&workspace.ws, Some(a));
    v.show_active_document(&workspace.ws, Some(b));
    assert_eq!(v.recents, vec![b, a], "most recent first");

    // Re-activating moves it up rather than listing it twice.
    v.show_active_document(&workspace.ws, Some(a));
    assert_eq!(v.recents, vec![a, b]);
}

/// A container with nothing animating must not ask for frames, or the loop
/// spins at frame rate forever.
#[test]
fn a_still_container_is_not_animating() {
    let (v, _w) = container();
    assert!(!v.animating());
}

// ------------------------------------------------- clicking inside a document
//
// Only a `Kind::Document` can root a tab. Everything else in the explore tree is
// a *place in* one, so clicking it opens that document and lands on it.

/// A document with `lines` paragraphs and an image at the end.
fn illustrated(ws: &mut WorkspaceDoc, title: &str, lines: usize) -> (NodeId, NodeId) {
    let doc = a_document(ws, title);
    for i in 0..lines {
        ws.insert(
            Parent::Node(doc),
            Node::paragraph(format!("line {i}")).unwrap(),
        )
        .unwrap();
    }
    let img = ws
        .insert(
            Parent::Node(doc),
            Node::image(
                koru_core::node::BlobHash("hash".into()),
                "image/png",
                Some(800),
                Some(600),
            )
            .unwrap(),
        )
        .unwrap();
    (doc, img)
}

fn active_scroll(v: &MainView) -> f32 {
    v.tabs().active_tab().view.scroll()
}

/// **The bug this fixes.** Rooting a tab at an image gave `walk_under` nothing
/// and a blank page; it has to open the document the image lives in.
#[test]
fn clicking_an_image_opens_its_document_and_scrolls_to_it() {
    let (mut v, mut workspace) = on_document();
    let (doc, img) = illustrated(&mut workspace.ws, "spec", 120);

    assert!(v.open_node(&workspace.ws, img));
    assert_eq!(v.document_root(), Some(doc), "the document, not the image");
    assert_eq!(v.tabs().active_tab().label, "spec");
    assert!(
        active_scroll(&v) > 0.0,
        "an image 120 lines down should have scrolled to"
    );
}

/// Ordinary image navigation selects the picture without opening rename mode.
#[test]
fn opening_an_image_outcome_does_not_open_rename() {
    let (mut v, mut workspace) = on_document();
    let (doc, image) = illustrated(&mut workspace.ws, "spec", 2);

    assert!(v.apply(&workspace.ws, Outcome::OpenNode(image)));
    assert_eq!(v.document_root(), Some(doc));
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("document leaf");
    };
    assert_eq!(leaf.body().focused_id(), Some(image));
    assert!(leaf.view().overlay.trigger().is_none());
    assert!(leaf.overlay_panel_rect().is_none());
}

/// A paragraph is a place in a document too — the same rule, not a second one.
#[test]
fn clicking_a_paragraph_opens_its_document_and_scrolls_to_it() {
    let (mut v, mut workspace) = on_document();
    let (doc, _) = illustrated(&mut workspace.ws, "spec", 120);
    let last_line = *workspace
        .ws
        .children(Parent::Node(doc))
        .unwrap()
        .iter()
        .rev()
        .nth(1)
        .expect("a paragraph before the image");

    assert!(v.open_node(&workspace.ws, last_line));
    assert_eq!(v.document_root(), Some(doc));
    assert!(active_scroll(&v) > 0.0);
}

/// Something near the top is already on screen, so nothing should jump.
#[test]
fn clicking_something_already_visible_does_not_scroll() {
    let (mut v, mut workspace) = on_document();
    let (doc, _) = illustrated(&mut workspace.ws, "spec", 80);
    let first = workspace.ws.children(Parent::Node(doc)).unwrap()[0];

    v.open_node(&workspace.ws, first);
    assert_eq!(active_scroll(&v), 0.0);
}

/// A contact has no document above it. Opening a tab rooted at one gave a blank
/// page; now it opens nothing at all.
#[test]
fn clicking_a_contact_opens_no_tab() {
    let (mut v, mut workspace) = on_document();
    let kim = koru_workspace::contact(&mut workspace.ws, "Kim").expect("contact");
    let before = v.tabs().len();

    assert!(!v.open_node(&workspace.ws, kim), "nothing to open");
    assert_eq!(v.tabs().len(), before, "and no tab appeared");
    assert!(v.recents.is_empty(), "nor was it recorded as reopenable");
}

/// Recents are recorded by the shell (`show_active_document`), not by a node
/// open — a node open does not know the store id.
#[test]
fn opening_a_node_does_not_record_a_recent() {
    let (mut v, mut workspace) = on_document();
    let (_doc, img) = illustrated(&mut workspace.ws, "spec", 4);

    v.open_node(&workspace.ws, img);
    assert!(v.recents.is_empty(), "a node open records nothing");
}

/// Clicking a second thing in a document already open reuses its tab and just
/// moves within it.
#[test]
fn a_second_click_inside_one_document_reuses_its_tab() {
    let (mut v, mut workspace) = on_document();
    let (doc, img) = illustrated(&mut workspace.ws, "spec", 120);
    let first = workspace.ws.children(Parent::Node(doc)).unwrap()[0];

    v.open_node(&workspace.ws, img);
    let tabs_after_first = v.tabs().len();
    assert!(active_scroll(&v) > 0.0);

    v.open_node(&workspace.ws, first);
    assert_eq!(v.tabs().len(), tabs_after_first, "no second tab");
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("a leaf");
    };
    assert_eq!(l.body().focused_id(), Some(first), "the caret moved to it");
    // Not zero: the document's name is row 0, so the first paragraph starts
    // below it and scrolling *to* it stops there rather than at the top.
    assert!(
        active_scroll(&v) <= l.body().rows[1].y,
        "and it scrolled back up to the line"
    );
}

// ------------------------------------------------------------- context menu
//
// Right-clicking a tab opens a floating menu — Close, Close Others, Close to
// the Left, Close to the Right. The model operations are pinned in `tabset`;
// what is left to prove here is the container's part: the right click opens a
// menu (and does *not* select the tab), a choice runs the right op, and the
// menu dismisses cleanly. The fixture is `four_tabs`: [Welcome, Workspace, a, b]
// — `Workspace` is the permanent tab, the rest closable.

/// Whether `action` is offered on tab `tab`. Opens the menu for it, since
/// enablement is now asked of an open menu rather than of a bare index.
fn tab_offers(v: &mut MainView, ws: &mut WorkspaceDoc, tab: usize, action: MenuAction) -> bool {
    right_click_tab(v, ws, tab);
    let menu = v.menu.clone().expect("the menu opened");
    v.enabled_items(&menu)[item_of(action)]
}

/// The index in [`TAB_ITEMS`] of `action`.
fn item_of(action: MenuAction) -> usize {
    context_menu::TAB_ITEMS
        .iter()
        .position(|&a| a == action)
        .unwrap()
}

/// Right-click the middle of tab `tab_index`, opening its menu.
fn right_click_tab(v: &mut MainView, ws: &mut WorkspaceDoc, tab_index: usize) {
    let rects = tab_rects(v);
    let (x, y) = rects[tab_index].body.centre();
    v.context_menu(ws, x, y);
}

/// Left-click menu item `action` on an already-open menu. The menu is `Copy`, so
/// reading it out for the click point does not dismiss it.
fn choose(v: &mut MainView, ws: &mut WorkspaceDoc, action: MenuAction) {
    let menu = v.menu.clone().expect("the menu is open");
    let (x, y) = menu.item_rect(v.viewport, item_of(action)).centre();
    v.mouse_down(ws, x, y, false);
}

#[test]
fn right_clicking_a_tab_opens_a_menu_without_selecting_it() {
    let (mut v, mut workspace) = four_tabs();
    let active = v.tabs().active();
    right_click_tab(&mut v, &mut workspace.ws, 2); // "a"
    let menu = v.menu.as_ref().expect("the menu opened");
    assert_eq!(
        menu.subject,
        MenuSubject::Tab(2),
        "the menu is for the right-clicked tab"
    );
    assert_eq!(
        v.tabs().active(),
        active,
        "a right-click does not switch tabs"
    );
}

#[test]
fn a_store_document_tab_offers_rename_and_move_to_trash() {
    let (mut v, mut workspace) = on_document();
    let document = koru_core::DocumentId::new();
    v.tabs_mut().active_mut().document = Some(document);
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: document,
            parent: None,
            ordinal: 0,
            title: "Notes".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        Some(document),
    );
    let tab = v.tabs().active();

    right_click_tab(&mut v, &mut workspace.ws, tab);
    let menu = v.menu.as_ref().expect("the document tab menu opened");
    assert_eq!(
        menu.items(),
        context_menu::tab_items(true, false, false, false)
    );
    assert!(v.item_is_enabled(menu, MenuAction::Rename));
    assert!(v.item_is_enabled(menu, MenuAction::MoveToTrash));

    assert!(v.run_menu_action(MenuSubject::Tab(tab), MenuAction::Rename));
    // Renaming a tab opens that document first, and opening one is a navigation
    // now rather than its own request: every document target goes through the
    // one Store-resolved route (ADR-127). `SourceWindow`, because the prompt is
    // owed to this window — see
    // `a_second_window_does_not_take_a_gesture_raised_in_the_first`.
    assert_eq!(
        v.take_pane_request(),
        Some(PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::SourceWindow,
        })
    );
    assert!(v.run_menu_action(MenuSubject::Tab(tab), MenuAction::MoveToTrash,));
    assert_eq!(
        v.take_pane_request(),
        Some(PaneRequest::TrashDocuments(vec![document]))
    );

    right_click_tab(&mut v, &mut workspace.ws, 0);
    assert_eq!(
        v.menu.as_ref().unwrap().items(),
        context_menu::tab_items(false, false, false, false),
        "Welcome does not show document-only actions"
    );
}

#[test]
fn right_clicking_document_content_forwards_to_the_leaf_menu() {
    let (mut v, mut workspace) = four_tabs();
    // A point well inside the content rect, not on any tab.
    assert!(v.context_menu(&mut workspace.ws, v.content().centre().0, 400.0));
    assert!(
        v.menu.is_none(),
        "the document owns this menu rather than the container"
    );
}

#[test]
fn right_clicking_again_repositions_the_menu() {
    let (mut v, mut workspace) = four_tabs();
    right_click_tab(&mut v, &mut workspace.ws, 2);
    right_click_tab(&mut v, &mut workspace.ws, 3); // "b"
    assert_eq!(
        v.menu.clone().unwrap().subject,
        MenuSubject::Tab(3),
        "the second right-click replaced the first menu"
    );
}

#[test]
fn close_from_the_menu_closes_the_tab() {
    let (mut v, mut workspace) = four_tabs();
    right_click_tab(&mut v, &mut workspace.ws, 2); // "a"
    choose(&mut v, &mut workspace.ws, MenuAction::Close);
    assert_eq!(labels(&v), ["Welcome", "Notes", "b"]);
    assert!(v.menu.is_none(), "choosing dismisses the menu");
}

#[test]
fn close_others_keeps_only_the_right_clicked_tab() {
    let (mut v, mut workspace) = four_tabs();
    right_click_tab(&mut v, &mut workspace.ws, 2); // "a"
    choose(&mut v, &mut workspace.ws, MenuAction::CloseOthers);
    // Nothing is spared any more: there is no permanent tab to keep.
    assert_eq!(labels(&v), ["a"]);
}

#[test]
fn close_left_from_the_menu() {
    let (mut v, mut workspace) = four_tabs(); // [Welcome, Notes, a, b]
    right_click_tab(&mut v, &mut workspace.ws, 3); // "b"
    choose(&mut v, &mut workspace.ws, MenuAction::CloseLeft);
    // Everything to the left goes, and only "b" is left.
    assert_eq!(labels(&v), ["b"]);
}

#[test]
fn close_right_from_the_menu() {
    let (mut v, mut workspace) = four_tabs(); // [Welcome, Notes, a, b]
    right_click_tab(&mut v, &mut workspace.ws, 2); // "a"
    choose(&mut v, &mut workspace.ws, MenuAction::CloseRight);
    // Only "b" is to the right of "a".
    assert_eq!(labels(&v), ["Welcome", "Notes", "a"]);
}

#[test]
fn clicking_outside_the_menu_dismisses_it_without_acting() {
    let (mut v, mut workspace) = four_tabs();
    right_click_tab(&mut v, &mut workspace.ws, 2);
    let before = labels(&v);
    // A click well away from the menu and off the tabs.
    v.mouse_down(&mut workspace.ws, v.content().centre().0, 400.0, false);
    assert!(v.menu.is_none(), "the click dismissed the menu");
    assert_eq!(labels(&v), before, "and nothing was closed");
}

#[test]
fn escape_dismisses_the_menu() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, mut workspace) = four_tabs();
    right_click_tab(&mut v, &mut workspace.ws, 2);
    let before = labels(&v);
    assert!(v.key(&mut workspace.ws, &Key::Named(NamedKey::Escape), None, NONE));
    assert!(v.menu.is_none());
    assert_eq!(labels(&v), before);
}

/// Close is offered on every tab now, including the first and the last: closing
/// the only one leaves a welcome tab rather than an empty window, so there is
/// nothing left to dim it for.
#[test]
fn every_tabs_close_item_is_offered() {
    let (mut v, mut workspace) = four_tabs();
    for i in 0..v.tabs().len() {
        right_click_tab(&mut v, &mut workspace.ws, i);
        assert!(
            tab_offers(&mut v, &mut workspace.ws, i, MenuAction::Close),
            "Close is dimmed on tab {i}"
        );
    }
    // The bulk items still depend on there being something to bulk-close.
    right_click_tab(&mut v, &mut workspace.ws, 0);
    let enabled = v.enabled_items(&v.menu.clone().unwrap());
    assert!(enabled[item_of(MenuAction::CloseOthers)]);
    assert!(
        !enabled[item_of(MenuAction::CloseLeft)],
        "nothing to the left"
    );
    assert!(enabled[item_of(MenuAction::CloseRight)]);
    // Pop Out still leads and a welcome tab is not poppable, so the menu opens
    // on the first *enabled* item — which is Close, no longer dimmed.
    right_click_tab(&mut v, &mut workspace.ws, 0);
    assert_eq!(
        v.menu.as_ref().unwrap().hover,
        Some(item_of(MenuAction::Close))
    );

    // And choosing it does close the tab, rather than being inert.
    let before = labels(&v);
    choose(&mut v, &mut workspace.ws, MenuAction::Close);
    assert!(v.menu.is_none(), "the click dismissed the menu");
    assert_eq!(labels(&v).len(), before.len() - 1, "and it closed");
}

#[test]
fn arrow_keys_drive_the_menu_and_enter_chooses() {
    use winit::keyboard::{Key, NamedKey};
    let (mut v, mut workspace) = four_tabs();
    right_click_tab(&mut v, &mut workspace.ws, 2); // "a": every item enabled
    assert_eq!(
        v.menu.as_ref().unwrap().hover,
        Some(0),
        "opens on the first item"
    );

    let down = |v: &mut MainView, ws: &mut WorkspaceDoc| {
        v.key(ws, &Key::Named(NamedKey::ArrowDown), None, NONE);
    };
    // Down steps through every *selectable* item and wraps — it steps over the
    // divider. Counted off `TAB_ITEMS` rather than a literal, so adding a row
    // does not make this a lie.
    let selectable: Vec<usize> = context_menu::TAB_ITEMS
        .iter()
        .enumerate()
        .filter(|(_, a)| a.selectable())
        .map(|(i, _)| i)
        .collect();
    down(&mut v, &mut workspace.ws);
    assert_eq!(v.menu.as_ref().unwrap().hover, Some(selectable[1]));
    for _ in 2..selectable.len() {
        down(&mut v, &mut workspace.ws);
    }
    assert_eq!(
        v.menu.as_ref().unwrap().hover,
        selectable.last().copied(),
        "Down reaches the bottom, over the divider"
    );
    down(&mut v, &mut workspace.ws);
    assert_eq!(
        v.menu.as_ref().unwrap().hover,
        Some(0),
        "Down wraps back to the top"
    );
    // Step down to Close Others, and Enter runs it.
    let target = item_of(MenuAction::CloseOthers);
    while v.menu.as_ref().unwrap().hover != Some(target) {
        down(&mut v, &mut workspace.ws);
    }
    assert!(v.key(&mut workspace.ws, &Key::Named(NamedKey::Enter), None, NONE));
    assert_eq!(
        labels(&v),
        ["a"],
        "close others keeps only the one it was on"
    );
}

// ------------------------------------------------------------- popping a tab out

/// Only a real document tab can be popped out: the welcome screen and the
/// permanent workspace tab have nowhere to go. The menu reflects that.
#[test]
fn pop_out_is_only_offered_for_a_document_tab() {
    let (mut v, mut workspace) = four_tabs();
    assert!(
        !tab_offers(&mut v, &mut workspace.ws, 0, MenuAction::PopOut),
        "the welcome tab is not poppable"
    );
    assert!(
        tab_offers(&mut v, &mut workspace.ws, 1, MenuAction::PopOut),
        "a document tab is poppable"
    );
}

/// Choosing Pop Out from the menu closes the tab and asks the shell for a
/// window over the same document — the two halves of a pop-out.
#[test]
fn popping_out_a_tab_closes_it_and_asks_for_a_window() {
    let (mut v, mut workspace) = four_tabs();
    let doc = v.tabs().get(2).unwrap().view.root().unwrap();
    let before = labels(&v);
    assert!(before.contains(&"a".to_string()));

    right_click_tab(&mut v, &mut workspace.ws, 2);
    choose(&mut v, &mut workspace.ws, MenuAction::PopOut);

    assert_eq!(v.take_wants_popout(), Some(doc), "the pop-out is requested");
    assert!(
        !labels(&v).contains(&"a".to_string()),
        "the popped tab left the strip"
    );
    // The request is drained once.
    assert!(v.take_wants_popout().is_none());
}

/// A vertical drag past the threshold tears the tab off the strip and switches
/// to cross-pane drag mode. Releasing inside the content drops the tab back
/// (or splits, if it is over an edge zone); releasing outside the content pops
/// it out into its own window.
#[test]
fn dragging_a_tab_down_tears_it_off_and_pops_it_out() {
    let (mut v, mut workspace) = four_tabs();
    let rects = tab_rects(&mut v);
    let i = 2; // "a"
    let (x, y) = rects[i].body.centre();

    v.mouse_down(&mut workspace.ws, x, y, false);
    assert!(v.group_drag().is_some(), "the press grabbed the tab");

    // A small wobble is still a reorder, not a tear.
    v.mouse_move(x, y + 5.0);
    assert!(!v.drag_tearing(), "below the threshold is still a reorder");

    // Past the threshold the tab tears loose into a cross-pane drag.
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    assert!(v.drag_tearing(), "past the threshold the tab has torn off");

    // Moving back within the band stays torn: it is now a cross-pane drag.
    v.mouse_move(x, y + 5.0);
    assert!(v.drag_tearing(), "back within the band it stays torn");

    // Releasing outside the content pops the tab out.
    v.mouse_move(x, -10.0);
    v.mouse_up(&mut workspace.ws);
    assert!(
        v.take_wants_popout().is_some(),
        "releasing outside the content pops the tab out"
    );
    assert!(
        !labels(&v).contains(&"a".to_string()),
        "the torn tab left the strip"
    );
}

/// A drag that never crosses the threshold is a reorder, not a pop-out: the tab
/// drops back into the strip and nothing is asked of the shell.
#[test]
fn a_short_drag_drops_the_tab_back_without_popping_out() {
    let (mut v, mut workspace) = four_tabs();
    let rects = tab_rects(&mut v);
    let (x, y) = rects[2].body.centre();

    v.mouse_down(&mut workspace.ws, x, y, false);
    v.mouse_move(x, y + TEAR_OFF_PX * 0.5);
    v.mouse_up(&mut workspace.ws);

    assert!(v.take_wants_popout().is_none(), "no pop-out was requested");
    assert!(
        labels(&v).contains(&"a".to_string()),
        "the tab stayed on the strip"
    );
}

// ---------------------------------------------------------- split-view drops

fn two_document_tabs() -> (MainView, Workspace) {
    let (mut v, mut workspace) = on_document();
    let doc = a_document(&mut workspace.ws, "Second");
    v.open_node(&workspace.ws, doc);
    (v, workspace)
}

fn press_tab(v: &mut MainView, ws: &mut WorkspaceDoc, label: &str) -> (f32, f32) {
    let rects = tab_rects(v);
    let i = v
        .tabs()
        .iter()
        .position(|t| t.label == label)
        .unwrap_or_else(|| panic!("tab {label:?} not found"));
    let (x, y) = rects[i].body.centre();
    v.mouse_down(ws, x, y, false);
    (x, y)
}

/// Drag a tab to the right edge of the view to create a vertical split.
#[test]
fn dragging_a_tab_to_the_right_edge_splits_the_pane() {
    let (mut v, mut workspace) = two_document_tabs();
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");

    // Tear off the strip first.
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    assert!(v.drag_tearing());

    let content = v.content();
    let drop_x = content.x + content.w - 5.0;
    let drop_y = content.y + content.h * 0.5;
    v.mouse_move(drop_x, drop_y);
    v.mouse_up(&mut workspace.ws);

    assert_eq!(
        v.panes.iter_groups().count(),
        2,
        "the pane split into two groups"
    );
    let all_labels: Vec<String> = v
        .panes
        .iter_groups()
        .flat_map(|(_, g)| g.iter_tabs().map(|t| t.label.clone()))
        .collect();
    assert!(all_labels.contains(&"Second".to_string()));
    assert!(all_labels.contains(&"Notes".to_string()));
}

/// Drag a tab to the left edge of the view to create a vertical split with the
/// dragged tab on the left.
#[test]
fn dragging_a_tab_to_the_left_edge_splits_left() {
    let (mut v, mut workspace) = two_document_tabs();
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");

    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    let drop_x = content.x + 5.0;
    let drop_y = content.y + content.h * 0.5;
    v.mouse_move(drop_x, drop_y);
    v.mouse_up(&mut workspace.ws);

    assert_eq!(v.panes.iter_groups().count(), 2);
}

/// Releasing a torn tab in the centre of the same pane does nothing — there is
/// no split intent and the source and target are the same group.
#[test]
fn releasing_a_torn_tab_in_the_centre_does_not_change_layout() {
    let (mut v, mut workspace) = two_document_tabs();
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");

    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    v.mouse_move(content.x + content.w * 0.5, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    assert_eq!(v.panes.iter_groups().count(), 1);
    assert!(v.take_wants_popout().is_none());
}

/// A drop onto a target pane's **strip** lands in the slot the marker showed —
/// before the first tab when the pointer is over the strip's left end, at the
/// end when it is past the last tab — and the dropped tab takes over.
#[test]
fn dropping_on_a_strip_lands_in_the_slot_the_marker_showed() {
    use crate::mainview::panes::PanePath;

    let (mut v, mut workspace) = two_document_tabs();
    // Second goes to a pane on the right; Notes and Welcome stay left.
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);
    assert_eq!(v.panes.iter_groups().count(), 2, "the split happened");

    // Tear Notes off the left pane and hover the right pane's strip at its
    // left end: the marker offers slot 0. The right pane is a top-row pane, so
    // its strip is its share of the title bar.
    let right = PanePath::root().child(1);
    let right_lane = v.lane_rect_for(&right).expect("the right pane's lane");
    v.panes.set_focus(PanePath::root().child(0));
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Notes");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    assert!(v.drag_tearing());
    v.mouse_move(right_lane.x + 2.0, right_lane.y + right_lane.h * 0.5);
    assert_eq!(
        v.drop_slot,
        Some((right.clone(), 0)),
        "the marker offered the first slot"
    );

    v.mouse_up(&mut workspace.ws);
    let right_labels: Vec<String> = v
        .panes
        .group(&right)
        .unwrap()
        .iter_tabs()
        .map(|t| t.label.clone())
        .collect();
    assert_eq!(
        right_labels,
        ["Notes", "Second"],
        "the drop landed in slot 0"
    );
    assert_eq!(
        v.panes.group(&right).unwrap().active_tab().label,
        "Notes",
        "the dropped tab took over"
    );
}

/// The same drop past the last tab lands at the end of the strip.
#[test]
fn dropping_past_the_last_tab_lands_at_the_end() {
    use crate::mainview::panes::PanePath;

    let (mut v, mut workspace) = two_document_tabs();
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    let right = PanePath::root().child(1);
    let right_lane = v.lane_rect_for(&right).expect("the right pane's lane");
    v.panes.set_focus(PanePath::root().child(0));
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Notes");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    v.mouse_move(
        right_lane.x + right_lane.w - 2.0,
        right_lane.y + right_lane.h * 0.5,
    );
    v.mouse_up(&mut workspace.ws);

    let right_labels: Vec<String> = v
        .panes
        .group(&right)
        .unwrap()
        .iter_tabs()
        .map(|t| t.label.clone())
        .collect();
    assert_eq!(
        right_labels,
        ["Second", "Notes"],
        "the drop landed at the end"
    );
}

/// **Moving a pane's only tab into another pane must lose nothing.** The take
/// emptied the source — and collapsing there, before the landing, renumbered
/// the paths the landing was aimed at (the tab vanished) and asked the pane to
/// close the very document being moved (the crash this pins).
#[test]
fn moving_a_panes_last_tab_into_another_pane_loses_nothing() {
    use crate::mainview::panes::PanePath;

    let (mut v, mut workspace) = two_document_tabs();
    // Split Second out to the right: it is that pane's only tab.
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);
    assert_eq!(v.panes.iter_groups().count(), 2, "the split happened");

    // Drag it back onto the left pane's strip — which, as a top-row pane, is
    // its share of the title bar.
    let left = PanePath::root().child(0);
    let left_lane = v.lane_rect_for(&left).expect("the left pane's lane");
    v.panes.set_focus(PanePath::root().child(1));
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    v.mouse_move(left_lane.x + 2.0, left_lane.y + left_lane.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    assert_eq!(
        v.panes.iter_groups().count(),
        1,
        "the emptied pane went away"
    );
    let labels: Vec<String> = v.tabs().iter().map(|t| t.label.clone()).collect();
    assert!(
        labels.contains(&"Second".to_string()),
        "the moved tab survived: {labels:?}"
    );
    assert_eq!(
        v.tabs().active_tab().label,
        "Second",
        "the dropped tab took over"
    );
}

/// The same move aimed at the dead centre, not the strip: the landing is a
/// plain drop into the group, and the emptied source still collapses without
/// taking the tab with it.
#[test]
fn dropping_a_panes_last_tab_in_the_centre_loses_nothing() {
    use crate::mainview::panes::PanePath;

    let (mut v, mut workspace) = two_document_tabs();
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    let left_rect = v
        .panes
        .leaf_rects(v.content())
        .into_iter()
        .find(|(p, _)| *p == PanePath::root().child(0))
        .map(|(_, r)| r)
        .expect("the left pane");
    v.panes.set_focus(PanePath::root().child(1));
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    // Dead centre of the left pane's view: no split, no strip — a plain drop.
    v.mouse_move(
        left_rect.x + left_rect.w * 0.5,
        left_rect.y + left_rect.h * 0.5,
    );
    v.mouse_up(&mut workspace.ws);

    assert_eq!(
        v.panes.iter_groups().count(),
        1,
        "the emptied pane went away"
    );
    let labels: Vec<String> = v.tabs().iter().map(|t| t.label.clone()).collect();
    assert!(
        labels.contains(&"Second".to_string()),
        "the moved tab survived: {labels:?}"
    );
}

/// And the same move aimed at the *edge*: the split is created from a path
/// taken before the emptied source collapsed.
#[test]
fn splitting_with_a_panes_last_tab_loses_nothing() {
    use crate::mainview::panes::PanePath;

    let (mut v, mut workspace) = two_document_tabs();
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    let left_rect = v
        .panes
        .leaf_rects(v.content())
        .into_iter()
        .find(|(p, _)| *p == PanePath::root().child(0))
        .map(|(_, r)| r)
        .expect("the left pane");
    v.panes.set_focus(PanePath::root().child(1));
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    // The left pane's bottom edge: split it horizontally.
    v.mouse_move(
        left_rect.x + left_rect.w * 0.5,
        left_rect.y + left_rect.h - 3.0,
    );
    v.mouse_up(&mut workspace.ws);

    assert_eq!(v.panes.iter_groups().count(), 2, "a new split happened");
    let all: Vec<String> = v
        .panes
        .iter_groups()
        .flat_map(|(_, g)| g.iter_tabs().map(|t| t.label.clone()))
        .collect();
    assert!(
        all.contains(&"Second".to_string()),
        "the moved tab survived: {all:?}"
    );
    assert!(all.contains(&"Welcome".to_string()));
}

/// Escape cancels a cross-pane drag and leaves the layout untouched.
#[test]
fn escape_cancels_a_cross_pane_drag() {
    let (mut v, mut workspace) = two_document_tabs();
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");

    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    assert!(v.drag_tearing());

    v.key(
        &mut workspace.ws,
        &winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        winit::keyboard::ModifiersState::empty(),
    );

    assert!(!v.drag_tearing());
    assert_eq!(v.panes.iter_groups().count(), 1);
}

// ------------------------------------------- shell and agent tabs travel too

/// A shell tab tears off its strip and splits the pane like any document tab.
#[test]
fn tearing_a_shell_tab_off_splits_the_pane() {
    let (mut v, mut workspace) = two_document_tabs();
    assert!(
        v.open_session_tab_for_profile(0, None),
        "a shell tab opened"
    );
    let label = v.tabs().active_tab().label.clone();

    let (x, y) = press_tab(&mut v, &mut workspace.ws, &label);
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    assert!(v.drag_tearing(), "a shell tab tears off like a document");

    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    assert_eq!(v.panes.iter_groups().count(), 2, "the pane split");
    let right = crate::mainview::panes::PanePath::root().child(1);
    let right_group = v.panes.group(&right).expect("the new pane");
    assert!(
        matches!(
            right_group.active_tab().view,
            crate::mainview::tab::TabView::Session(_)
        ),
        "the shell tab owns the new pane"
    );
}

/// An agent tab drops into another pane's strip, in the slot shown.
#[test]
fn an_agent_tab_drops_into_another_panes_strip() {
    use crate::mainview::panes::PanePath;

    let (mut v, mut workspace) = two_document_tabs();
    assert!(v.open_ai_chat_tab(), "an agent tab opened");
    let agent = v.tabs().active_tab().label.clone();

    // Split Second out to the right first, so there is somewhere to go.
    let (x, y) = press_tab(&mut v, &mut workspace.ws, "Second");
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);
    assert_eq!(v.panes.iter_groups().count(), 2);

    // Now carry the agent tab from the left pane onto the right pane's lane.
    let right = PanePath::root().child(1);
    let lane = v.lane_rect_for(&right).expect("the right pane's lane");
    v.panes.set_focus(PanePath::root().child(0));
    let (x, y) = press_tab(&mut v, &mut workspace.ws, &agent);
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    assert!(v.drag_tearing(), "an agent tab tears off like a document");
    v.mouse_move(lane.x + lane.w - 2.0, lane.y + lane.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    let right_labels: Vec<String> = v
        .panes
        .group(&right)
        .unwrap()
        .iter_tabs()
        .map(|t| t.label.clone())
        .collect();
    assert_eq!(
        right_labels,
        ["Second".to_string(), agent],
        "the agent tab moved over"
    );
}

/// A shell or agent tab released outside the window goes home — it has
/// nowhere to pop out *to*, and it must not vanish.
#[test]
fn a_shell_tab_released_outside_the_window_goes_home() {
    let (mut v, mut workspace) = two_document_tabs();
    assert!(
        v.open_session_tab_for_profile(0, None),
        "a shell tab opened"
    );
    let label = v.tabs().active_tab().label.clone();
    let before = labels(&v);

    let (x, y) = press_tab(&mut v, &mut workspace.ws, &label);
    v.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    assert!(v.drag_tearing());
    v.mouse_move(x, -10.0);
    v.mouse_up(&mut workspace.ws);

    assert!(
        v.take_wants_popout().is_none(),
        "a shell tab cannot pop out"
    );
    assert_eq!(labels(&v), before, "the tab stayed in its strip");
    assert_eq!(v.panes.iter_groups().count(), 1, "no split happened");
}

// ------------------------------------------- dragging a document out of the column

/// The sidebar open with one unopened document ("Brief") and its row located.
fn sidebar_with_a_document() -> (MainView, Workspace, Rect, koru_core::NodeId) {
    let (mut v, mut workspace) = container();
    let doc = a_document(&mut workspace.ws, "Brief");
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);
    let sb = v.regions.sidebar;
    let (_, row) = v
        .explore
        .row_rects(sb)
        .into_iter()
        .find(|(i, _)| v.explore.row_node(*i) == Some(doc))
        .expect("Brief has a row");
    (v, workspace, row, doc)
}

/// Press a row, arm the drag inside the column, and carry it out.
fn drag_row_out(v: &mut MainView, ws: &mut WorkspaceDoc, row: Rect) -> (f32, f32) {
    let (x, y) = row.centre();
    v.mouse_down(ws, x, y, false);
    v.mouse_move(x, y + 5.0);
    assert!(
        v.explore.drag_armed(),
        "past the threshold the drag is armed"
    );
    (x, y)
}

/// A document dragged into a pane's dead centre opens as a tab there. The tree
/// is not a drop target outside the column — the node stays where it was.
#[test]
fn dragging_a_document_into_a_pane_opens_it_there() {
    let (mut v, mut workspace, row, doc) = sidebar_with_a_document();
    drag_row_out(&mut v, &mut workspace.ws, row);

    let content = v.content();
    v.mouse_move(content.x + content.w * 0.5, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    assert!(
        labels(&v).contains(&"Brief".to_string()),
        "the document opened as a tab: {:?}",
        labels(&v)
    );
    assert_eq!(v.document_root(), Some(doc), "and it is showing");
    // No tree move: the row is still listed.
    v.refresh_explore(&workspace.ws, true);
    assert!(
        v.explore
            .row_rects(v.regions.sidebar)
            .iter()
            .any(|(i, _)| v.explore.row_node(*i) == Some(doc)),
        "the node stayed in the tree"
    );
}

/// The same drag aimed at the pane's right edge splits it: the document owns
/// the new pane.
#[test]
fn dragging_a_document_to_a_panes_edge_splits_it() {
    let (mut v, mut workspace, row, doc) = sidebar_with_a_document();
    drag_row_out(&mut v, &mut workspace.ws, row);

    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    assert!(
        v.split_intent.is_some(),
        "the edge zone previewed the split"
    );
    v.mouse_up(&mut workspace.ws);

    assert_eq!(v.panes.iter_groups().count(), 2, "the pane split");
    let right = crate::mainview::panes::PanePath::root().child(1);
    let group = v.panes.group(&right).expect("the new pane");
    assert_eq!(
        group.active_tab().view.root(),
        Some(doc),
        "the dragged document owns the new pane"
    );
}

/// The same drag onto a lane lands the new tab in the slot the marker showed.
#[test]
fn dragging_a_document_onto_a_lane_lands_in_the_slot() {
    let (mut v, mut workspace, row, _doc) = sidebar_with_a_document();
    // A second open document gives the strip two tabs to land between.
    let notes = a_document(&mut workspace.ws, "Notes");
    v.open_node(&workspace.ws, notes);
    drag_row_out(&mut v, &mut workspace.ws, row);

    let lane = v
        .lane_rect_for(&crate::mainview::panes::PanePath::root())
        .expect("the lane");
    v.mouse_move(lane.x + lane.w - 2.0, lane.y + lane.h * 0.5);
    assert!(v.drop_slot.is_some(), "the lane offered a slot");
    v.mouse_up(&mut workspace.ws);

    assert_eq!(
        labels(&v).last().map(String::as_str),
        Some("Brief"),
        "the document landed at the end slot: {:?}",
        labels(&v)
    );
}

// ----------------------- dragging a document the workspace has not loaded

/// The sidebar open with one manifest document the workspace has **not**
/// loaded — the row names a store id, so opening it is a store round trip and
/// the drop's placement has to survive until the landing.
fn sidebar_with_a_manifest_document() -> (MainView, Workspace, Rect, koru_core::DocumentId) {
    let (mut v, workspace) = container();
    let document = koru_core::DocumentId::new();
    View::set_entries(&mut v, vec![document_entry(document, "Brief")], None);
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);
    let node = koru_core::NodeId::from_uuid(document.as_uuid());
    let (_, row) = v
        .explore
        .row_rects(v.regions.sidebar)
        .into_iter()
        .find(|(i, _)| v.explore.row_node(*i) == Some(node))
        .expect("Brief has a row");
    (v, workspace, row, document)
}

/// The store's answer to the drop's ask: the document materialises in the
/// workspace and the shell announces it active.
fn the_document_arrives(
    v: &mut MainView,
    workspace: &mut Workspace,
    document: koru_core::DocumentId,
) {
    a_document(&mut workspace.ws, "Brief");
    View::show_active_document(v, &workspace.ws, Some(document));
}

/// A manifest row dragged to a pane's edge cannot split on the spot — the tab
/// does not exist yet — but the split must land when the document does. This
/// is the common case in a real window: explore lists the manifest, and most
/// of it is not loaded.
#[test]
fn dragging_a_manifest_document_to_a_panes_edge_splits_on_arrival() {
    let (mut v, mut workspace, row, document) = sidebar_with_a_manifest_document();
    drag_row_out(&mut v, &mut workspace.ws, row);

    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    assert!(
        v.split_intent.is_some(),
        "the edge zone previewed the split"
    );
    v.mouse_up(&mut workspace.ws);

    assert_eq!(
        v.panes.iter_groups().count(),
        1,
        "no split before the document arrives"
    );
    assert!(
        matches!(
            v.take_pane_request(),
            Some(koru_workspace_host::PaneRequest::Navigate {
                disposition: koru_workspace_host::SearchTargetDisposition::SourceWindow,
                ..
            })
        ),
        "the document was asked for, pinned to this window"
    );
    assert_eq!(v.pending_pane_drops.len(), 1, "the placement waits");

    the_document_arrives(&mut v, &mut workspace, document);

    assert_eq!(
        v.panes.iter_groups().count(),
        2,
        "the pane split on arrival"
    );
    let right = crate::mainview::panes::PanePath::root().child(1);
    let group = v.panes.group(&right).expect("the new pane");
    assert_eq!(
        group.active_tab().document,
        Some(document),
        "the dragged document owns the new pane"
    );
    assert!(
        v.pending_pane_drops.is_empty(),
        "the arrival consumed the placement"
    );
}

/// The same drag onto a lane lands in the slot the marker showed — again on
/// arrival, not on the drop.
#[test]
fn dragging_a_manifest_document_onto_a_lane_lands_in_its_slot_on_arrival() {
    let (mut v, mut workspace, row, document) = sidebar_with_a_manifest_document();
    drag_row_out(&mut v, &mut workspace.ws, row);

    // Ahead of the one tab the strip has. Nothing draws in a test, so the
    // strip's widths are ensured the way a frame would have.
    v.ensure_tab_widths();
    let lane = v
        .lane_rect_for(&crate::mainview::panes::PanePath::root())
        .expect("the lane");
    v.mouse_move(lane.x + 2.0, lane.y + lane.h * 0.5);
    let slot = v.drop_slot.as_ref().expect("the lane offered a slot").1;
    v.mouse_up(&mut workspace.ws);

    assert!(
        !labels(&v).iter().any(|l| l == "Brief"),
        "nothing opens before the store answers: {:?}",
        labels(&v)
    );

    the_document_arrives(&mut v, &mut workspace, document);

    assert_eq!(
        labels(&v).get(slot).map(String::as_str),
        Some("Brief"),
        "the document landed in the marked slot: {:?}",
        labels(&v)
    );
    assert!(v.pending_pane_drops.is_empty());
}

/// A dead-centre drop of a manifest row opens the document as the pane's new
/// tab once it arrives — no split, no slot.
#[test]
fn dragging_a_manifest_document_into_a_pane_opens_it_there_on_arrival() {
    let (mut v, mut workspace, row, document) = sidebar_with_a_manifest_document();
    drag_row_out(&mut v, &mut workspace.ws, row);

    let content = v.content();
    v.mouse_move(content.x + content.w * 0.5, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);

    assert!(
        !labels(&v).iter().any(|l| l == "Brief"),
        "nothing opens before the store answers: {:?}",
        labels(&v)
    );
    assert_eq!(v.pending_pane_drops.len(), 1, "the placement waits");

    the_document_arrives(&mut v, &mut workspace, document);

    assert!(
        labels(&v).contains(&"Brief".to_string()),
        "the document opened as a tab: {:?}",
        labels(&v)
    );
    assert_eq!(
        v.panes.iter_groups().count(),
        1,
        "no split for a dead-centre drop"
    );
    assert!(v.pending_pane_drops.is_empty());
}

/// A parked placement belongs to the arrival it was asked for: another
/// document landing first must not spend it — the same contract
/// `pending_lands` keeps.
#[test]
fn a_parked_pane_drop_is_not_spent_on_a_strangers_arrival() {
    let (mut v, mut workspace, row, document) = sidebar_with_a_manifest_document();
    drag_row_out(&mut v, &mut workspace.ws, row);

    let content = v.content();
    v.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    v.mouse_up(&mut workspace.ws);
    assert_eq!(v.pending_pane_drops.len(), 1);

    // Something else arrives first.
    let other = a_document(&mut workspace.ws, "Other");
    let other_document = koru_core::DocumentId::from_uuid(other.as_uuid());
    View::show_active_document(&mut v, &workspace.ws, Some(other_document));

    assert_eq!(
        v.panes.iter_groups().count(),
        1,
        "a stranger's arrival splits nothing"
    );
    assert_eq!(
        v.pending_pane_drops.len(),
        1,
        "the placement is still waiting for its own document"
    );

    // Its own arrival still lands the split. The workspace's first root is
    // the stranger's now, so land Brief by its root the way the shell's
    // per-document workspace would.
    let brief = a_document(&mut workspace.ws, "Brief");
    v.active_document = Some(document);
    assert!(v.land_on(&workspace.ws, brief, false, None));
    v.settle_pending_pane_drop();
    assert_eq!(v.panes.iter_groups().count(), 2, "its own arrival splits");
}

// ------------------------------------------- the cross-window tab drag

/// Over a pane the tree's carried ghost goes away: the drop there opens the
/// document, and a row following the pointer would be promising a tree move
/// that will not happen. Back over the tree, the ghost returns.
#[test]
fn dragging_a_document_over_a_pane_hides_the_tree_ghost() {
    let (mut v, mut workspace, row, _doc) = sidebar_with_a_document();
    drag_row_out(&mut v, &mut workspace.ws, row);

    let content = v.content();
    v.mouse_move(content.x + content.w * 0.5, content.y + content.h * 0.5);
    assert!(
        v.explore.carried_hidden(),
        "over a pane the ghost is hidden"
    );

    let sb = v.regions.sidebar;
    v.mouse_move(sb.x + sb.w * 0.5, row.y + row.h * 0.5);
    assert!(
        !v.explore.carried_hidden(),
        "over the tree the ghost is back"
    );
}

/// The shell's gate for the pop-out marker: set while another window accepts
/// the drag, reset by the drop that lands there.
#[test]
fn foreign_targeting_round_trips_and_resets_with_the_drop() {
    let (mut a, mut workspace) = two_document_tabs();
    let (x, y) = press_tab(&mut a, &mut workspace.ws, "Second");
    a.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = a.content();
    a.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    assert!(a.pane_drag_document().is_some(), "the drag is in flight");

    assert!(a.set_foreign_targeted(true), "a change redraws");
    assert!(!a.set_foreign_targeted(true), "a repeat does not");

    a.complete_foreign_drop();
    assert!(
        !a.set_foreign_targeted(false),
        "the drop already reset the gate"
    );
}

/// The receiving window's preview is the same two states a local drag drives,
/// with no source strip to exclude — and it clears when the drag moves on.
#[test]
fn a_foreign_hover_previews_the_split_and_leaves() {
    let (mut v, workspace) = on_document();
    let doc = v.document_root().expect("a document");

    let content = v.content();
    let (ex, ey) = (content.x + content.w - 5.0, content.y + content.h * 0.5);
    assert!(v.foreign_tab_hover(&workspace.ws, doc, ex, ey));
    assert!(
        v.split_intent.is_some(),
        "the edge zone previewed the split for the incoming tab"
    );

    assert!(v.foreign_tab_leave(), "a preview was showing");
    assert!(v.split_intent.is_none() && v.drop_slot.is_none());
    assert_eq!(v.panes.iter_groups().count(), 1, "a preview splits nothing");
}

/// The full handoff: the dragged tab lands in the receiving window as the
/// split the point says, and leaves the source's strip — with no pop-out.
#[test]
fn a_foreign_drop_lands_the_split_and_leaves_the_source() {
    let (mut a, mut workspace) = two_document_tabs();
    let (mut b, _bw) = container();
    let second = a
        .tabs()
        .iter()
        .find(|t| t.label == "Second")
        .and_then(|t| t.view.root())
        .expect("Second's root");

    // The drag tears off in the source — as far as the source ever gets.
    let (x, y) = press_tab(&mut a, &mut workspace.ws, "Second");
    a.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = a.content();
    a.mouse_move(content.x + content.w - 5.0, content.y + content.h * 0.5);
    assert_eq!(
        a.pane_drag_document(),
        Some(second),
        "the drag carries the document"
    );

    // The drop point, in the receiving window: its right edge.
    let content = b.content();
    let (dx, dy) = (content.x + content.w - 5.0, content.y + content.h * 0.5);
    assert!(
        b.foreign_tab_drop(&workspace.ws, second, dx, dy),
        "the receiving window accepts"
    );
    a.complete_foreign_drop();

    let right = crate::mainview::panes::PanePath::root().child(1);
    let group = b.panes.group(&right).expect("the new pane");
    assert_eq!(
        group.active_tab().view.root(),
        Some(second),
        "the dragged document owns the receiving window's new pane"
    );
    assert!(
        !labels(&a).contains(&"Second".to_string()),
        "the source's strip no longer holds the tab: {:?}",
        labels(&a)
    );
    assert!(
        a.take_wants_popout().is_none(),
        "a landed drop is not also a pop-out"
    );
    assert!(a.pane_drag_document().is_none(), "the drag is over");
}

/// A foreign drop onto the lane lands in the slot the marker showed.
#[test]
fn a_foreign_drop_onto_a_lane_lands_in_the_slot() {
    let (a, workspace) = two_document_tabs();
    let (mut b, _bw) = container();
    let second = a
        .tabs()
        .iter()
        .find(|t| t.label == "Second")
        .and_then(|t| t.view.root())
        .expect("Second's root");
    // Nothing draws in a test, so the strip's widths are ensured the way a
    // frame would have.
    b.ensure_tab_widths();
    let lane = b
        .lane_rect_for(&crate::mainview::panes::PanePath::root())
        .expect("the lane");
    assert!(b.foreign_tab_drop(&workspace.ws, second, lane.x + 2.0, lane.y + lane.h * 0.5));
    assert_eq!(
        labels(&b).first().map(String::as_str),
        Some("Second"),
        "the tab landed ahead of the strip: {:?}",
        labels(&b)
    );
}

/// A release over no pane of the other window is refused, and the source
/// keeps its pop-out answer.
#[test]
fn a_foreign_drop_over_no_pane_is_refused() {
    let (mut b, _bw) = container();
    let (mut a, mut workspace) = two_document_tabs();
    let second = a
        .tabs()
        .iter()
        .find(|t| t.label == "Second")
        .and_then(|t| t.view.root())
        .expect("Second");

    assert!(
        !b.foreign_tab_drop(&workspace.ws, second, -10.0, -10.0),
        "over nothing, the drop is refused"
    );
    assert_eq!(b.panes.iter_groups().count(), 1, "nothing changed");

    // The fallback is the source's own release: the pop-out it always was.
    let (x, y) = press_tab(&mut a, &mut workspace.ws, "Second");
    a.mouse_move(x, y + TEAR_OFF_PX + 1.0);
    let content = a.content();
    a.mouse_move(content.x - 30.0, content.y + 20.0);
    a.mouse_up(&mut workspace.ws);
    assert!(
        a.take_wants_popout().is_some(),
        "refused means the pop-out still happens"
    );
}

// ------------------------------------------- the popover overhangs its column

/// A pane with two profiles and the profiles popover already showing.
fn with_profiles_open() -> (MainView, Workspace, Vec<koru_core::ProfileSummary>) {
    let (v, w) = container();
    let profiles: Vec<koru_core::ProfileSummary> = ["mine", "theirs"]
        .iter()
        .map(|n| koru_core::ProfileSummary {
            id: koru_core::ProfileId::new(),
            name: (*n).into(),
            email: format!("{n}@example.com"),
            avatar: None,
            icon: None,
            color: None,
            account: None,
            folder: format!("C:/dev/{n}"),
        })
        .collect();
    let list = vec![koru_core::WorkspaceEntry::new(
        WorkspaceId::new(),
        "Example",
        0,
    )];
    let (mut v, mut w) = (v, w);
    // The pane starts shut; its layout width is final the instant it is
    // toggled, so `regions.sidebar` is right without waiting for the slide.
    v.toggle_sidebar();
    v.set_pane_context(
        profiles.clone(),
        profiles[0].id,
        list.clone(),
        Some(list[0].id),
    );
    // The avatar opens the profiles.
    let h = crate::panes::explore::ExploreView::header_rects(v.regions.sidebar);
    let (x, y) = h.avatar.centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert!(v.explore.popover_open(), "the avatar opens the profiles");
    (v, w, profiles)
}

/// **The popover is wider than the column it hangs off**, so its right-hand
/// half sits over the active view. `regions::hit` answers `Content` there, and
/// routing on that alone made the container clear the pane's hover — which took
/// the highlight off a row's right-hand end (a Profile row's caret) as the
/// pointer moved onto it.
#[test]
fn moving_onto_the_popovers_overhang_keeps_the_row_it_is_over_hovered() {
    let (mut v, _w, _) = with_profiles_open();
    let sb = v.regions.sidebar;
    let vp = v.viewport;
    let (row, r) = v
        .explore
        .popover_rects(sb, vp)
        .into_iter()
        .find(|(row, _)| *row == crate::panes::explore::PopoverRow::Profile(1))
        .expect("the other profile's row");
    let (x, y) = koru_ui::profile_picker::row_parts(r).trailing.centre();
    assert!(
        x > sb.x + sb.w,
        "the point this is about is outside the column: {x} vs {}",
        sb.x + sb.w
    );

    v.mouse_move(x, y);
    let i = v
        .explore
        .popover_rects(sb, vp)
        .iter()
        .position(|(candidate, _)| *candidate == row)
        .expect("the row's index");
    assert_eq!(
        v.explore.popover_hover(),
        Some(i),
        "the row under the pointer stays lit"
    );
}

/// ...and the press that follows reaches it: the Profile row drills into its
/// workspaces, asking the host for nothing and leaving the switcher open.
#[test]
fn a_press_on_the_overhang_reaches_the_profile_row() {
    let (mut v, mut w, _) = with_profiles_open();
    let sb = v.regions.sidebar;
    let vp = v.viewport;
    let (_, r) = v
        .explore
        .popover_rects(sb, vp)
        .into_iter()
        .find(|(row, _)| *row == crate::panes::explore::PopoverRow::Profile(1))
        .expect("the other profile's row");
    let (x, y) = koru_ui::profile_picker::row_parts(r).trailing.centre();
    assert!(x > sb.x + sb.w, "the point is outside the column");
    v.mouse_move(x, y);
    v.mouse_down(&mut w.ws, x, y, false);
    assert_eq!(
        v.take_pane_request(),
        None,
        "choosing a Profile does not switch"
    );
    assert!(v.explore.popover_open(), "the switcher stays open");
    assert_eq!(
        v.explore.popover_rects(sb, vp).first().map(|(row, _)| *row),
        Some(crate::panes::explore::PopoverRow::Back),
        "on that Profile's workspaces"
    );
}

// ------------------------------------------------------------------- drafts

/// How many things are in the library. A draft must not add to it.
fn library(ws: &WorkspaceDoc) -> usize {
    ws.children(Parent::Root).map(|c| c.len()).unwrap_or(0)
}

fn enter() -> Key {
    Key::Named(winit::keyboard::NamedKey::Enter)
}

/// `+` used to open the welcome screen. It opens a page you can type on — and
/// **creates nothing**, which is the half that is easy to lose.
#[test]
fn plus_opens_a_blank_page_and_writes_nothing_to_the_workspace() {
    let (mut v, workspace) = container();
    let before = library(&workspace.ws);
    let tabs = v.tabs().len();

    assert!(v.new_tab());

    assert_eq!(v.tabs().len(), tabs + 1);
    assert!(v.tabs().active_tab().draft.is_some(), "it is a draft");
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Leaf(_)),
        "and a real editor, not a picture of one"
    );
    assert_eq!(v.tabs().active_tab().label, tab::DRAFT_NAME);
    assert_eq!(
        library(&workspace.ws),
        before,
        "nothing was added to the library"
    );
}

/// The first character makes it real — and **lands in it**. Promoting after the
/// leaf had already taken the key would drop the character into the throwaway
/// document, where nobody would ever see it again. The caret opens in the
/// title, so the first character is the document's name.
#[test]
fn the_first_character_creates_the_document_and_is_kept() {
    let (mut v, mut workspace) = container();
    let before = library(&workspace.ws);
    v.new_tab();

    assert!(v.key(&mut workspace.ws, &ch("H"), Some("H"), NONE));

    // Still a draft: it has asked the shell and is holding the character until
    // the answer arrives, which is the guarantee that matters.
    assert!(
        v.tabs().active_tab().draft.is_some(),
        "the draft waits for the shell rather than creating the document itself"
    );
    let document = super::harness::shell_answers_the_draft(&mut v, &mut workspace.ws);
    let _ = before;

    assert!(v.tabs().active_tab().draft.is_none(), "no longer a draft");
    assert_eq!(koru_workspace::name_of(&workspace.ws, document), "H");
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("a leaf");
    };
    // Row 0 is the document's name; that is where the typing landed.
    assert_eq!(l.body().rows[0].text, "H", "the character was not eaten");
}

/// **A failed creation hands the latch back, and the next keystroke asks
/// again.** `promote_draft` asks the shell exactly once per latch; before
/// `create_document_failed` existed, a store that hiccuped on that one ask
/// left the draft typing forever with no retry and no way out but a restart —
/// found live, against a gateway that went away between the app opening and
/// the first keystroke.
#[test]
fn a_failed_creation_lets_the_draft_ask_again() {
    let (mut v, mut workspace) = container();
    v.new_tab();

    // First keystroke: the one ask.
    v.key(&mut workspace.ws, &ch("H"), Some("H"), NONE);
    assert!(
        matches!(
            View::take_pane_request(&mut v),
            Some(koru_workspace_host::PaneRequest::CreateDocument(_))
        ),
        "the first keystroke asks the shell"
    );

    // While the latch is held, more typing does not ask again.
    v.key(&mut workspace.ws, &ch("i"), Some("i"), NONE);
    assert!(
        View::take_pane_request(&mut v).is_none(),
        "one ask per latch"
    );

    // The shell reports the create failed; the draft survives, and the next
    // keystroke re-asks rather than typing into a tab that can never be real.
    View::create_document_failed(&mut v);
    assert!(v.tabs().active_tab().draft.is_some(), "still a draft");
    v.key(&mut workspace.ws, &ch("!"), Some("!"), NONE);
    assert!(
        matches!(
            View::take_pane_request(&mut v),
            Some(koru_workspace_host::PaneRequest::CreateDocument(_))
        ),
        "a failed create must not latch the draft shut"
    );
}

/// **Switching tabs opens the document that tab is over — without Navigate.**
///
/// ActivateDocument parks the room in place so Explore stays on the current
/// workspace. Navigate would switch whenever locate_document differed.
#[test]
fn selecting_a_tab_opens_the_document_it_is_over() {
    let (mut v, mut workspace) = container();
    let other = koru_core::DocumentId::new();
    let home = koru_core::WorkspaceId::new();

    // Two tabs: the welcome screen the container opens on, and one over a
    // document the window is *not* currently in.
    let doc = a_document(&mut workspace.ws, "Elsewhere");
    v.tabs_mut().push(tab::Tab::document_at(
        Leaf::document(&workspace.ws, doc),
        "Elsewhere",
        Some(other),
        Some(home),
    ));
    // `push` makes it active, so step away first and come back — which is the
    // gesture under test anyway.
    let elsewhere = v.tabs().len() - 1;
    v.select(0);
    let _ = View::take_pane_request(&mut v);

    assert!(v.select(elsewhere), "the tab did not become active");
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::ActivateDocument {
            document: other,
            workspace: Some(home),
        }),
        "switching to the tab did not open its document in place"
    );
}

/// ...and a tab over the document already open asks for nothing. A request per
/// click would be noise in a log somebody will one day read.
#[test]
fn selecting_the_tab_you_are_already_in_asks_for_nothing() {
    use koru_core::explore::ExploreEntry;
    let (mut v, mut workspace) = container();
    let open = koru_core::DocumentId::new();
    View::set_entries(
        &mut v,
        vec![ExploreEntry {
            id: open,
            parent: None,
            ordinal: 0,
            title: "Here".to_owned(),
            kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        Some(open),
    );

    let doc = a_document(&mut workspace.ws, "Here");
    v.tabs_mut().push(tab::Tab::document_of(
        Leaf::document(&workspace.ws, doc),
        "Here",
        Some(open),
    ));
    let here = v.tabs().len() - 1;
    v.select(0);
    let _ = View::take_pane_request(&mut v);

    assert!(v.select(here));
    assert_eq!(View::take_pane_request(&mut v), None);
}

/// **Selecting Manage arms its Workspaces poll and asks the shell for
/// nothing.** The controller answers on its own thread, so what selection
/// arms is the pass that takes what arrived. The tick's re-arming is
/// `workspace_menu/tests.rs`'s.
#[test]
fn selecting_manage_arms_its_poll_and_asks_the_shell_for_nothing() {
    let (mut v, _workspace) = container();
    v.tabs_mut().push(tab::Tab::manage());
    let manage = v.tabs().len() - 1;
    assert!(v.select(0));
    let _ = View::take_pane_request(&mut v);
    assert_eq!(v.workspaces_poll_due, None, "a document ticks for nothing");

    assert!(v.select(manage));
    assert_eq!(View::take_pane_request(&mut v), None);
    assert!(v.workspaces_poll_due.is_some(), "the Manage tick is armed");
}

#[test]
fn confirmation_scrim_owns_pointer_and_cursor() {
    let (mut v, _workspace) = on_document();
    let node = v.document_root().expect("a document");
    v.confirm = Some((
        koru_ui::Confirm::new(
            "Move to Trash?",
            "This can be restored later.",
            "Move to Trash",
        ),
        crate::mainview::trash::ConfirmSubject::Trash(node),
    ));

    assert!(v.mouse_move_rest(300.0, 300.0), "the scrim claims hover");
    assert_eq!(v.cursor_icon(), CursorIcon::Default);
}

/// **The first click on a document opens it**, rather than reselecting the tab
/// you were already in.
///
/// Reported from a running window: the first click sometimes did nothing and the
/// second worked. `open_node_with_centering` asks "is this already in a tab?"
/// through `self.active_document`, which arrived *after* this call — through
/// `set_entries`, one line later in the shell — so it matched the tab of the
/// **previous** document, selected that, and opened nothing. The push landing in
/// between is what made the second click work, and what made it intermittent:
/// with no tab open on the previous document there was nothing to match.
#[test]
fn the_first_click_on_a_document_opens_it() {
    let (mut v, mut workspace) = container();
    let was = koru_core::DocumentId::new();
    let now = koru_core::DocumentId::new();

    // A container test cannot hold two `LoroDoc`s, so both documents' roots live
    // in one: `show_active_document` finds the **first** `Kind::Document` child,
    // which stands in for the newly opened document, and the tab already showing
    // is over the other — as it would be over a root in a document of its own.
    let clicked = a_document(&mut workspace.ws, "Clicked");
    let open = a_document(&mut workspace.ws, "Already open");
    v.tabs_mut().push(tab::Tab::document_of(
        Leaf::document(&workspace.ws, open),
        "Already open",
        Some(was),
    ));
    View::set_entries(&mut v, Vec::new(), Some(was));
    let had = v.tabs().len();
    let _ = clicked;

    // The shell swapped the document and says which in the same breath.
    assert!(View::show_active_document(&mut v, &workspace.ws, Some(now)));

    assert_eq!(
        v.tabs().len(),
        had + 1,
        "no tab opened for the clicked document"
    );
    assert_eq!(
        v.tabs().active_tab().document,
        Some(now),
        "the click landed on the tab that was already showing"
    );
}

/// **Opening a second document does not close the first one's tab.**
///
/// Reported from a running window: clicking documents sometimes reset the strip
/// to one tab, and then a restart reopened only that one. Both are `adopt`
/// culling tabs against the *active* document — a `NodeId` is meaningful only
/// inside the document that minted it, so every tab over another one looked
/// deleted.
#[test]
fn opening_another_document_leaves_the_other_tabs_alone() {
    let (mut v, mut workspace) = container();
    let here = koru_core::DocumentId::new();
    let elsewhere = koru_core::DocumentId::new();

    let doc = a_document(&mut workspace.ws, "Here");
    v.tabs_mut().push(tab::Tab::document_of(
        Leaf::document(&workspace.ws, doc),
        "Here",
        Some(here),
    ));
    // A tab over a document this window is not in. A container test cannot open
    // two `LoroDoc`s, so the second document's root is **made and then removed**
    // — which is precisely what the active document says about a node in another
    // one: `read` fails. What must stop the cull is the id the tab carries.
    let far = a_document(&mut workspace.ws, "Elsewhere");
    v.tabs_mut().push(tab::Tab::document_of(
        Leaf::document(&workspace.ws, far),
        "Elsewhere",
        Some(elsewhere),
    ));
    workspace.ws.delete(far).expect("delete");
    let had = v.tabs().len();

    // The window is in `here`, and something changed.
    View::set_entries(&mut v, Vec::new(), Some(here));
    View::adopt(&mut v, &workspace.ws);

    assert_eq!(
        v.tabs().len(),
        had,
        "a tab over another document was closed"
    );
    assert!(v.tabs().iter().any(|t| t.document == Some(elsewhere)));
}

/// ...and it keeps its name, which comes from the manifest rather than from a
/// document that has never heard of it.
///
/// `refresh_labels` reads the active document, where that tab's root resolves to
/// nothing — so every background document tab was renamed *Untitled* the moment
/// anything changed.
#[test]
fn a_background_tab_is_named_by_the_manifest() {
    use koru_core::explore::ExploreEntry;
    let (mut v, mut workspace) = container();
    let here = koru_core::DocumentId::new();
    let elsewhere = koru_core::DocumentId::new();

    let doc = a_document(&mut workspace.ws, "Here");
    v.tabs_mut().push(tab::Tab::document_of(
        Leaf::document(&workspace.ws, doc),
        "Elsewhere",
        Some(elsewhere),
    ));

    View::set_entries(
        &mut v,
        vec![ExploreEntry {
            id: elsewhere,
            parent: None,
            ordinal: 0,
            title: "Renamed by a peer".to_owned(),
            kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        Some(here),
    );
    View::adopt(&mut v, &workspace.ws);

    let label = v
        .tabs()
        .iter()
        .find(|t| t.document == Some(elsewhere))
        .map(|t| t.label.clone())
        .expect("the tab");
    assert_eq!(label, "Renamed by a peer");
}

/// **A tab is what keeps a document open**, so closing the last one over it has
/// to say so.
///
/// An open document is a `LoroDoc`, a room and a socket thread. The workspace
/// cannot see a tab close, so a close that says nothing leaks all three for as
/// long as the window is up — and restoring a session opens one per remembered
/// tab, which is what made this worth paying for rather than noting.
///
/// The *last* one: a document in two tabs is still being shown after one of them
/// goes.
#[test]
fn closing_the_last_tab_over_a_document_lets_it_go() {
    let (mut v, mut workspace) = container();
    let open = koru_core::DocumentId::new();
    let doc = a_document(&mut workspace.ws, "Shared");
    for _ in 0..2 {
        v.tabs_mut().push(tab::Tab::document_of(
            Leaf::document(&workspace.ws, doc),
            "Shared",
            Some(open),
        ));
    }
    while View::take_pane_request(&mut v).is_some() {}

    assert!(v.close(v.tabs().len() - 1));
    assert_eq!(
        View::take_pane_request(&mut v),
        None,
        "released while a tab was still showing it"
    );

    assert!(v.close(v.tabs().len() - 1));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::CloseDocument(open)),
        "the last tab closed and nothing asked the shell to let the document go"
    );
}

/// *Close all tabs* is one action rather than a loop, so it needs its own
/// release — the same trap `close_others` / `close_left` / `close_right` have.
#[test]
fn closing_every_tab_lets_go_of_every_document() {
    let (mut v, mut workspace) = container();
    let (one, two) = (koru_core::DocumentId::new(), koru_core::DocumentId::new());
    for (id, name) in [(one, "One"), (two, "Two")] {
        let doc = a_document(&mut workspace.ws, name);
        v.tabs_mut().push(tab::Tab::document_of(
            Leaf::document(&workspace.ws, doc),
            name,
            Some(id),
        ));
    }
    while View::take_pane_request(&mut v).is_some() {}

    assert!(v.close_all());
    let mut released = Vec::new();
    while let Some(r) = View::take_pane_request(&mut v) {
        if let koru_workspace_host::PaneRequest::CloseDocument(id) = r {
            released.push(id);
        }
    }
    released.sort_by_key(|id| id.to_storage());
    let mut expected = vec![one, two];
    expected.sort_by_key(|id| id.to_storage());
    assert_eq!(released, expected);
}

/// **Move to trash on an Explore row asks the shell**, rather than looking for a
/// node that is not there.
///
/// The reported bug was that it did nothing at all. An Explore row's `NodeId` is
/// a `DocumentId`'s bytes, so `ws.trash(node)` looked for a node in the *open
/// document*, found none, and failed in silence. The list is what tells the two
/// apart, and it is exact: a colliding id would have to be a v4 uuid minted for
/// a document in this very workspace.
#[test]
fn trashing_an_explore_row_asks_the_shell_to_trash_the_document() {
    use koru_core::explore::ExploreEntry;
    let (mut v, mut workspace) = container();

    let doc = koru_core::DocumentId::new();
    View::set_entries(
        &mut v,
        vec![ExploreEntry {
            id: doc,
            parent: None,
            ordinal: 0,
            title: "Binned".to_owned(),
            kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        Some(doc),
    );

    // The row, in the coordinate system Explore rows actually use.
    let row = NodeId::from_uuid(doc.as_uuid());
    assert!(v.queue_trash(vec![row]), "the gesture was not acted on");
    // **Documents go as a set, at once.** They are not queued one at a time the
    // way nodes are: a gesture is one ask, one toast and one Undo, because an
    // Undo that took back the last of nine is not an undo.
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::TrashDocuments(vec![doc])),
    );
    let (_, payload) = v.toast.as_ref().expect("no undo was offered");
    assert!(
        matches!(
            payload,
            crate::mainview::ToastPayload::Restore(nodes) if nodes == &vec![row]
        ),
        "Undo would not take it back: {payload:?}"
    );

    // A node that is *not* one of the entries keeps the in-document path, which
    // is the half that already worked and must not be routed away.
    let node = NodeId::new();
    assert!(v.queue_trash(vec![node]));
    v.drain_trash(&mut workspace.ws);
    assert_eq!(
        View::take_pane_request(&mut v),
        None,
        "an ordinary node was sent to the shell"
    );
}

/// **Above three, it asks first** — and it counts the whole subtree, because
/// trashing a section takes its pages with it by inheritance. A count of direct
/// children would quote a smaller number than the confirm is about to act on.
#[test]
fn trashing_a_document_with_several_beneath_it_asks_first() {
    use koru_core::explore::ExploreEntry;
    let (mut v, mut workspace) = container();

    let root = koru_core::DocumentId::new();
    let entry = |id, parent, title: &str| ExploreEntry {
        id,
        parent,
        ordinal: 0,
        title: title.to_owned(),
        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    };
    // A chain, so a direct-children count would say one and the real answer is
    // four.
    let a = koru_core::DocumentId::new();
    let b = koru_core::DocumentId::new();
    let c = koru_core::DocumentId::new();
    let d = koru_core::DocumentId::new();
    View::set_entries(
        &mut v,
        vec![
            entry(root, None, "Section"),
            entry(a, Some(root), "One"),
            entry(b, Some(a), "Two"),
            entry(c, Some(b), "Three"),
            entry(d, Some(c), "Four"),
        ],
        Some(root),
    );

    assert!(v.queue_trash(vec![NodeId::from_uuid(root.as_uuid())]));
    v.drain_trash(&mut workspace.ws);
    assert!(v.confirm.is_some(), "four nested items went without asking");
    assert_eq!(
        View::take_pane_request(&mut v),
        None,
        "it binned the document before the answer"
    );
}

/// **A peer who binned your open document tells you so.**
///
/// The notice asked `ws.in_trash(root)` — the open document's own hidden trash
/// root — which a *document* binned in the manifest never enters. So a window
/// went on editing a page that had left the Library, with nothing said.
#[test]
fn a_document_binned_elsewhere_says_so_in_the_window_reading_it() {
    use koru_core::explore::ExploreEntry;
    let (mut v, mut workspace) = container();

    let doc = koru_core::DocumentId::new();
    let binned = |by: Option<&str>| ExploreEntry {
        id: doc,
        parent: None,
        ordinal: 0,
        title: "Shared".to_owned(),
        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
        mime: None,
        note: None,
        trashed_at: Some(1),
        trashed_by: by.map(str::to_owned),
    };

    // The window is in it, and it is not in the library any more. The tab
    // carries the document as well as the window: the notice belongs to what
    // the active tab shows, not to the last document the Store announced, or
    // it leaks onto Welcome.
    v.tabs_mut().active_mut().document = Some(doc);
    View::set_entries(&mut v, Vec::new(), Some(doc));
    View::set_trashed(&mut v, vec![binned(Some("Ada"))]);

    let notice = v
        .in_trash_notice(&workspace.ws)
        .expect("no notice for a document binned elsewhere");
    assert!(
        notice.title.contains("Ada"),
        "it did not say who: {:?}",
        notice.title
    );

    // **Not in the window that did the binning.** ADR-70: the notice is for the
    // peer who was reading it, and telling somebody about their own action is
    // how a warning becomes noise.
    v.trashed_here.insert(NodeId::from_uuid(doc.as_uuid()));
    assert!(v.in_trash_notice(&workspace.ws).is_none());
    v.trashed_here.clear();

    // **Both ways out are offered.** Restoring argues with the peer who binned
    // it; copying does not. For a while this was Restore alone, because forking
    // went through a subtree copy *inside one document* — right while a document
    // was a node in the library tree, wrong once it became a document of its own.
    let offered: Vec<_> = v
        .notice_targets(&workspace.ws)
        .into_iter()
        .map(|(hit, _)| hit)
        .collect();
    assert_eq!(
        offered,
        vec![
            crate::mainview::trash::NoticeHit::Restore,
            crate::mainview::trash::NoticeHit::SaveCopy,
        ]
    );

    // And pressing it asks the shell to put the document back.
    v.pending_restore = Some(vec![NodeId::from_uuid(doc.as_uuid())]);
    v.drain_restore(&mut workspace.ws);
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::RestoreDocuments(vec![
            doc
        ])),
    );

    // **Save a copy asks the shell too**, and by document rather than by node:
    // the copy is registered in the manifest, which a view does not hold. The
    // name is the view's, because what a copy is called is this interface's
    // language rather than the store's.
    v.pending_save_copy = Some(NodeId::from_uuid(doc.as_uuid()));
    assert!(v.drain_save_copy(&mut workspace.ws));
    assert_eq!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::CopyDocument {
            source: doc,
            title: "Shared (copy)".to_owned(),
        }),
    );
}

/// **Undo takes back everything the toast was about.**
///
/// The toast carried one `NodeId`, and each document set its own — so nine
/// binned at once left an Undo that restored the last and abandoned eight. The
/// toast is the gesture's, not the last document's.
#[test]
fn undo_takes_back_every_document_the_toast_was_about() {
    use koru_core::explore::ExploreEntry;
    let (mut v, mut workspace) = container();

    let ids: Vec<koru_core::DocumentId> = (0..9).map(|_| koru_core::DocumentId::new()).collect();
    View::set_entries(
        &mut v,
        ids.iter()
            .map(|id| ExploreEntry {
                id: *id,
                parent: None,
                ordinal: 0,
                title: "Page".to_owned(),
                kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
                mime: None,
                note: None,
                trashed_at: None,
                trashed_by: None,
            })
            .collect(),
        Some(ids[0]),
    );

    let rows: Vec<NodeId> = ids
        .iter()
        .map(|id| NodeId::from_uuid(id.as_uuid()))
        .collect();
    assert!(v.queue_trash(rows.clone()));
    assert!(v.confirm.is_some(), "nine went without asking");
    v.settle_confirm(true);

    let (toast, payload) = v.toast.as_ref().expect("no undo was offered");
    let crate::mainview::ToastPayload::Restore(undoes) = payload else {
        panic!("expected Restore payload, got {payload:?}");
    };
    assert_eq!(undoes.len(), 9, "Undo would take back only some of them");
    assert!(
        toast.message().contains('9'),
        "the toast named one document out of nine: {:?}",
        toast.message()
    );

    // ...and it is the nine that were binned, not nine of anything.
    let mut undoes = undoes.clone();
    let mut expected = rows;
    undoes.sort_by_key(|n| n.as_uuid());
    expected.sort_by_key(|n| n.as_uuid());
    assert_eq!(undoes, expected);
    let _ = &mut workspace;
}

/// **Six selected, six binned.** A gesture may ask the shell for several things,
/// and every one of them has to arrive.
///
/// `ask_pane` kept the *last* request and dropped the rest without a word, so
/// six documents dragged to the Trash produced six asks and one binning — the
/// label said six, the confirm said six, and one went. Nothing reported a
/// failure, because nothing had failed: the asks were simply overwritten.
#[test]
fn a_gesture_that_asks_several_times_loses_none_of_them() {
    use koru_core::explore::ExploreEntry;
    let (mut v, mut workspace) = container();

    let ids: Vec<koru_core::DocumentId> = (0..6).map(|_| koru_core::DocumentId::new()).collect();
    View::set_entries(
        &mut v,
        ids.iter()
            .map(|id| ExploreEntry {
                id: *id,
                parent: None,
                ordinal: 0,
                title: "Page".to_owned(),
                kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
                mime: None,
                note: None,
                trashed_at: None,
                trashed_by: None,
            })
            .collect(),
        Some(ids[0]),
    );

    let rows: Vec<NodeId> = ids
        .iter()
        .map(|id| NodeId::from_uuid(id.as_uuid()))
        .collect();
    assert!(v.queue_trash(rows));
    // Six is above the threshold, so it asks first — and answering is what puts
    // the whole set in the queue.
    assert!(v.confirm.is_some());
    v.settle_confirm(true);
    v.drain_trash_all(&mut workspace.ws);

    let mut asked: Vec<koru_core::DocumentId> = Vec::new();
    while let Some(request) = View::take_pane_request(&mut v) {
        if let koru_workspace_host::PaneRequest::TrashDocuments(mut batch) = request {
            asked.append(&mut batch);
        }
    }
    asked.sort_by_key(|id| id.as_uuid());
    let mut expected = ids.clone();
    expected.sort_by_key(|id| id.as_uuid());
    assert_eq!(asked, expected, "the shell was not asked about all six");
}

/// **Four pages side by side ask too**, and that is the case the old shape
/// missed entirely: it confirmed on how many documents sat *beneath* the one
/// being binned, so a selection of four top-level pages — each carrying nothing
/// — went straight to the bin without a word.
#[test]
fn trashing_several_documents_at_once_asks_first() {
    use koru_core::explore::ExploreEntry;
    let (mut v, _workspace) = container();

    let entry = |id, title: &str| ExploreEntry {
        id,
        parent: None,
        ordinal: 0,
        title: title.to_owned(),
        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    };
    let ids: Vec<koru_core::DocumentId> = (0..4).map(|_| koru_core::DocumentId::new()).collect();
    View::set_entries(
        &mut v,
        ids.iter().map(|id| entry(*id, "Page")).collect(),
        Some(ids[0]),
    );

    let rows: Vec<NodeId> = ids
        .iter()
        .map(|id| NodeId::from_uuid(id.as_uuid()))
        .collect();
    assert!(v.queue_trash(rows));
    assert!(
        v.confirm.is_some(),
        "four documents went to the bin without asking"
    );

    // ...and three do not, which is what makes the threshold a threshold.
    v.confirm = None;
    let three: Vec<NodeId> = ids[..3]
        .iter()
        .map(|id| NodeId::from_uuid(id.as_uuid()))
        .collect();
    assert!(v.queue_trash(three));
    assert!(v.confirm.is_none(), "three asked");
}

/// **A selected child does not count twice**, and a section counts everything
/// under it. One function answers both the drag label and the confirm, so they
/// cannot quote different numbers at each other — which they did: the label said
/// "Move 4 items" while binning one.
#[test]
fn what_a_gesture_moves_is_counted_once_and_in_full() {
    use koru_core::explore::{ExploreEntry, documents_moved};

    let entry = |id, parent| ExploreEntry {
        id,
        parent,
        ordinal: 0,
        title: "Page".to_owned(),
        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    };
    let root = koru_core::DocumentId::new();
    let child = koru_core::DocumentId::new();
    let grandchild = koru_core::DocumentId::new();
    let other = koru_core::DocumentId::new();
    let entries = vec![
        entry(root, None),
        entry(child, Some(root)),
        entry(grandchild, Some(child)),
        entry(other, None),
    ];

    // The whole subtree, not just the children.
    assert_eq!(documents_moved(&[root], &entries), 3);
    // A selected child of a selected parent is already travelling; counting it
    // as a root too would report its subtree twice.
    assert_eq!(documents_moved(&[root, child], &entries), 3);
    assert_eq!(documents_moved(&[root, grandchild], &entries), 3);
    // Independent selections add up.
    assert_eq!(documents_moved(&[root, other], &entries), 4);
    assert_eq!(documents_moved(&[other], &entries), 1);
    assert_eq!(documents_moved(&[], &entries), 0);
}

/// **The offline chip takes its room from the tabs rather than covering them.**
///
/// It is drawn over the caption, so nothing stops it sitting on top of a tab
/// except the strip being narrower while it is up — and the strip is what both
/// the drawing and the hit-testing ask for, so if the two ever disagreed a tab
/// would be clickable where the chip is.
#[test]
fn the_offline_chip_narrows_the_tab_strip_rather_than_covering_it() {
    let (mut v, _workspace) = container();
    let wide = v.caption_strip();
    assert!(v.offline_chip().is_none(), "nothing to say while connected");

    assert!(View::set_store_trouble(
        &mut v,
        Some(koru_workspace::StoreTrouble {
            message: "the store went away".to_owned(),
            owed: 3,
            since: Some(std::time::Instant::now()),
            fatal: false,
            unconfirmed: koru_workspace::Unconfirmed::default(),
        })
    ));

    let chip = v.offline_chip().expect("a chip while offline");
    let narrow = v.caption_strip();
    assert!(
        narrow.w < wide.w,
        "the strip did not make room: {} -> {}",
        wide.w,
        narrow.w
    );
    assert!(
        narrow.x + narrow.w <= chip.x + 0.5,
        "the tabs run under the chip"
    );

    // ...and it goes away again, rather than leaving a gap forever.
    assert!(View::set_store_trouble(&mut v, None));
    assert!(v.offline_chip().is_none());
    assert_eq!(v.caption_strip().w, wide.w);
}

/// **Clicking the chip opens the detail; clicking anywhere else puts it away.**
///
/// The chip says only *Offline* — how much is at stake is a question, not a
/// standing announcement — so the click is the whole of how anybody finds out.
/// It is checked before the shared hit test, because the chip sits inside the
/// caption band and would otherwise start dragging the window.
#[test]
fn the_offline_chip_opens_a_popover_and_the_next_click_closes_it() {
    let (mut v, mut workspace) = container();
    assert!(View::set_store_trouble(
        &mut v,
        Some(koru_workspace::StoreTrouble {
            message: "the store went away".to_owned(),
            owed: 2,
            since: Some(std::time::Instant::now()),
            fatal: false,
            unconfirmed: koru_workspace::Unconfirmed::default(),
        })
    ));
    // **Recomputed before every press, never captured once.** `offline_chip`
    // reads `theme::CAPTION_H` live, and the theme and density are process
    // globals other tests flip under the parallel runner — so a centre taken at
    // the top of this test can be outside the chip by the time it is clicked.
    // That is the race AGENTS.md records about the theme, met in a new place.
    let press_chip = |v: &mut MainView, ws: &mut WorkspaceDoc| {
        let (cx, cy) = v.offline_chip().expect("a chip while offline").centre();
        v.mouse_down(ws, cx, cy, false);
    };

    assert!(!v.offline_open);
    press_chip(&mut v, &mut workspace.ws);
    assert!(v.offline_open, "the chip did not open its popover");

    // **Into the content, not along the caption.** A press has to dismiss it
    // from anywhere at all, and the content is the case that proves it: such a
    // click never reaches the caption's hit dispatch, which is where the
    // dismissal used to live and why it only answered part of the window.
    let c = v.content();
    v.mouse_down(&mut workspace.ws, c.x + c.w * 0.5, c.y + c.h * 0.5, false);
    assert!(
        !v.offline_open,
        "the popover outlived a click in the content"
    );

    // ...and from the sidebar, the other half the old placement never heard
    // about.
    press_chip(&mut v, &mut workspace.ws);
    assert!(v.offline_open);
    let sb = v.sidebar_rect();
    v.mouse_down(
        &mut workspace.ws,
        sb.x + sb.w * 0.5,
        sb.y + sb.h * 0.5,
        false,
    );
    assert!(!v.offline_open, "a click in the sidebar left it open");
}

/// The chip is pushed every tick, and `since` moves on its own. Comparing the
/// whole value would report a change forever and redraw the window forever.
#[test]
fn an_unchanged_offline_state_asks_for_no_frame() {
    let (mut v, _workspace) = container();
    let trouble = |owed| {
        Some(koru_workspace::StoreTrouble {
            message: "gone".to_owned(),
            owed,
            since: Some(std::time::Instant::now()),
            fatal: false,
            unconfirmed: koru_workspace::Unconfirmed::default(),
        })
    };
    assert!(View::set_store_trouble(&mut v, trouble(1)));
    assert!(
        !View::set_store_trouble(&mut v, trouble(1)),
        "a later instant counted as a change"
    );
    assert!(
        View::set_store_trouble(&mut v, trouble(2)),
        "a changed count is a change"
    );
}

/// **The document you are in is drawn as the one you are in.**
///
/// Explore rows are keyed by `DocumentId` — `rebuild_from_entries` spells each
/// as `NodeId::from_uuid(entry.id.as_uuid())` — while a tab's root is a `NodeId`
/// *inside* a document. Two different uuids, so marking the current row from
/// `document_root()` matched nothing and every row drew the same. The active
/// document now arrives beside the list, from the only side that holds both.
#[test]
fn the_open_document_is_the_row_marked_in_explore() {
    use koru_core::explore::ExploreEntry;
    let (mut v, workspace) = container();

    let entry = |title: &str| ExploreEntry {
        id: koru_core::DocumentId::new(),
        parent: None,
        ordinal: 0,
        title: title.to_owned(),
        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    };
    let first = entry("First");
    let second = entry("Second");
    let open = second.id;

    assert!(View::set_entries(&mut v, vec![first, second], Some(open)));
    v.refresh_explore(&workspace.ws, true);

    assert_eq!(
        v.explore.selected(),
        Some(NodeId::from_uuid(open.as_uuid())),
        "the open document is not the marked row"
    );
}

/// **`First` comes out as `First`, not as `rstFi`.**
///
/// Creating a document is a round trip to the store, so the first few
/// characters land in the draft and are carried across at the swap. The swap
/// builds a whole new `Leaf` over a different `LoroDoc`, so every row is new and
/// every row's caret starts at 0 — and the characters already written to the
/// title are sitting in front of it. The next keystroke went in at 0.
///
/// The number of characters that arrive before the answer is the round trip
/// measured in keystrokes, which is why the symptom was reported as "after two
/// characters" and why it would come and go with how busy the store was. Two
/// here, because two is what was seen.
#[test]
fn typing_while_the_document_is_being_created_keeps_the_letters_in_order() {
    let (mut v, mut workspace) = container();
    v.new_tab();

    // Before the shell answers.
    for c in ["F", "i"] {
        v.key(&mut workspace.ws, &ch(c), Some(c), NONE);
    }
    let document = super::harness::shell_answers_the_draft(&mut v, &mut workspace.ws);

    // After it.
    for c in ["r", "s", "t"] {
        v.key(&mut workspace.ws, &ch(c), Some(c), NONE);
    }

    assert_eq!(
        koru_workspace::name_of(&workspace.ws, document),
        "First",
        "the caret was not carried across the swap"
    );
}

/// The same guarantee one row down: a caret in the *body* of a draft is a caret
/// somewhere, and the swap has to put it back there too. It was lost outright
/// before, because what crossed the swap was a `bool` meaning "in the title".
#[test]
fn a_draft_promoted_with_the_caret_in_its_body_keeps_it_there() {
    use winit::keyboard::NamedKey;
    let (mut v, mut workspace) = container();
    v.new_tab();

    v.key(&mut workspace.ws, &ch("N"), Some("N"), NONE);
    // Out of the title and into the first body row, then type there.
    v.key(&mut workspace.ws, &Key::Named(NamedKey::Enter), None, NONE);
    for c in ["a", "b"] {
        v.key(&mut workspace.ws, &ch(c), Some(c), NONE);
    }
    let document = super::harness::shell_answers_the_draft(&mut v, &mut workspace.ws);
    for c in ["c", "d"] {
        v.key(&mut workspace.ws, &ch(c), Some(c), NONE);
    }

    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("a leaf");
    };
    assert_eq!(l.body().rows[0].text, "N", "the title took body keystrokes");
    assert_eq!(
        l.body().rows[1].text,
        "abcd",
        "the body caret was not carried across the swap"
    );
    let _ = document;
}

/// Enter counts as writing — an empty line is content somebody typed.
#[test]
fn enter_also_makes_a_draft_real() {
    let (mut v, mut workspace) = container();
    let before = library(&workspace.ws);
    v.new_tab();
    v.key(&mut workspace.ws, &enter(), None, NONE);
    assert!(
        v.tabs().active_tab().draft.is_some(),
        "asked, still waiting"
    );
    super::harness::shell_answers_the_draft(&mut v, &mut workspace.ws);
    assert!(v.tabs().active_tab().draft.is_none());
    let _ = before;
}

/// **Everything somebody does while deciding whether to write** leaves the
/// draft a draft. Without this, any chord at all — since Ctrl+letter arrives
/// as a control *character* on Windows — would litter the library with empty
/// documents. Escape is the one exception with its own meaning: it closes the
/// draft, which the next two tests pin.
#[test]
fn looking_around_a_draft_creates_nothing() {
    use winit::keyboard::NamedKey;
    let (mut v, mut workspace) = container();
    let before = library(&workspace.ws);
    v.new_tab();

    let harmless: Vec<(Key, Option<&str>, ModifiersState)> = vec![
        (Key::Named(NamedKey::ArrowDown), None, NONE),
        (Key::Named(NamedKey::ArrowRight), None, NONE),
        (Key::Named(NamedKey::Home), None, NONE),
        (Key::Named(NamedKey::Backspace), None, NONE),
        // Ctrl+B as Windows actually delivers it: an ASCII control code, not a
        // `Character("b")`. `is_control` is what catches this one.
        (ch("\u{2}"), Some("\u{2}"), PRIMARY),
        (ch("b"), Some("b"), PRIMARY),
    ];
    for (key, text, mods) in harmless {
        v.key(&mut workspace.ws, &key, text, mods);
        assert!(
            v.tabs().active_tab().draft.is_some(),
            "{key:?} is not somebody starting to write"
        );
        assert_eq!(library(&workspace.ws), before, "{key:?} created something");
    }
}

/// Escape on a page that was never written is "nothing happened": the tab
/// closes and you are back where you were, exactly as Alt+Left or the mouse's
/// back button would leave you — and the library never hears about any of it.
#[test]
fn escape_closes_a_draft_as_if_it_never_happened() {
    use winit::keyboard::NamedKey;
    let (mut v, mut workspace) = container();
    let tabs = v.tabs().len();
    let before = library(&workspace.ws);
    v.new_tab();
    assert!(v.tabs().active_tab().draft.is_some());

    assert!(v.key(&mut workspace.ws, &Key::Named(NamedKey::Escape), None, NONE));

    assert_eq!(v.tabs().len(), tabs, "the draft is gone");
    assert!(
        v.tabs().active_tab().draft.is_none(),
        "back on the tab it was opened over"
    );
    assert_eq!(library(&workspace.ws), before, "and nothing was created");
}

/// Typing straight into the title is the first thing a new page is for, so the
/// caret **opens there**. The promoting keystroke is routed to a **fresh**
/// leaf, which promotion has to point at the title again — otherwise the name
/// would land as a paragraph.
#[test]
fn typing_in_a_drafts_title_names_the_document() {
    let (mut v, mut workspace) = container();
    v.new_tab();
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("a leaf");
    };
    assert!(l.body().focus_is_title(), "the caret opens in the title");

    assert!(v.key(&mut workspace.ws, &ch("H"), Some("H"), NONE));
    let document = super::harness::shell_answers_the_draft(&mut v, &mut workspace.ws);

    assert!(v.tabs().active_tab().draft.is_none(), "no longer a draft");
    assert_eq!(koru_workspace::name_of(&workspace.ws, document), "H");
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("a leaf");
    };
    assert_eq!(
        l.body().rows[0].text,
        "H",
        "the character named the document"
    );
    assert_eq!(l.body().rows[1].text, "", "and did not land in the body");
}

/// A tab whose document is in a *different* document must not be swept up by
/// the "this node has gone" pass — which asks the window's workspace about it
/// and is told, correctly, that it has never heard of it.
#[test]
fn a_draft_survives_the_pass_that_closes_deleted_tabs() {
    let (mut v, workspace) = container();
    v.new_tab();
    let tabs = v.tabs().len();
    v.adopt(&workspace.ws);
    assert_eq!(v.tabs().len(), tabs, "the draft is still open");
    assert!(v.tabs().active_tab().draft.is_some());
}

/// Reopening gives back the blank page — still blank, and still creating
/// nothing, so a window reopened ten times does not leave ten *Untitled*s.
#[test]
fn a_draft_comes_back_as_a_draft() {
    let (mut v, workspace) = container();
    v.new_tab();
    let mut settings = Settings::default();
    v.capture_session(&mut settings);
    assert_eq!(
        settings.tabs.last().map(|(t, _)| t.clone()),
        Some(koru_workspace_host::TabRef::Draft)
    );

    let before = library(&workspace.ws);
    let (mut back, _) = container();
    back.restore_session(&workspace, &settings);
    assert!(back.tabs().active_tab().draft.is_some());
    assert_eq!(library(&workspace.ws), before);
}

/// **The bug this exists to stop.** `koru_workspace::document` is
/// find-or-create *by title*, which is right for a `/document` pill and wrong
/// here: a second blank page resolved to the first one, showed its contents,
/// and took the keystroke meant for a new one — so typing on what looked like
/// an empty page appended to something you had already written.
#[test]
fn a_second_blank_page_is_a_second_document_not_the_first_one_again() {
    let (mut v, mut workspace) = container();
    v.new_tab();
    v.key(&mut workspace.ws, &ch("a"), Some("a"), NONE);
    let first = super::harness::shell_answers_the_draft(&mut v, &mut workspace.ws);

    v.new_tab();
    assert!(v.tabs().active_tab().draft.is_some());
    v.key(&mut workspace.ws, &ch("b"), Some("b"), NONE);
    // A second answer, and a second document: the store mints a fresh id per
    // call, so "find-or-create by title" cannot resurface the first page here.
    let second = super::harness::shell_answers_the_draft(&mut v, &mut workspace.ws);

    assert_ne!(first, second, "two blank pages are two documents");
    assert_eq!(
        koru_workspace::name_of(&workspace.ws, second),
        "b",
        "and the second holds its own name, not the first page's"
    );
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("a leaf");
    };
    assert_eq!(
        l.body().rows[0].text,
        "b",
        "the second page holds what was typed into it, not the first page's text"
    );
}

/// The lookup that a pill needs is unchanged: one name, one node. Breaking this
/// to fix the above would have made two tasks linking one document point at two.
#[test]
fn a_pill_still_resolves_one_title_to_one_node() {
    let (_v, mut workspace) = container();
    let a = koru_workspace::document(&mut workspace.ws, "Brief").expect("first");
    let b = koru_workspace::document(&mut workspace.ws, "Brief").expect("again");
    assert_eq!(
        a, b,
        "a pill is a reference, so the name has to resolve back"
    );

    let c = koru_workspace::create_document(&mut workspace.ws, "Brief").expect("a new one");
    assert_ne!(c, a, "...but asking for a new one gives a new one");
}

// ------------------------------------------------------------------- F2

/// `F2` over a file row renames that file in place. Files have no document to
/// open, so the field sits on the Explore row the way a workspace rename does.
#[test]
fn f2_over_a_file_row_renames_that_file() {
    let (mut v, mut workspace) = on_document();
    v.toggle_sidebar();
    let file = koru_core::DocumentId::new();
    let node = koru_core::NodeId::from_uuid(file.as_uuid());
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: file,
            parent: None,
            ordinal: 0,
            title: "Shared link.url".into(),
            kind: koru_core::explore::KIND_FILE.into(),
            mime: Some("text/uri-list".into()),
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );
    v.explore.show_files();
    v.refresh_explore(&workspace.ws, true);

    let sb = v.regions.sidebar;
    let (_, r) = v
        .explore
        .row_rects(sb)
        .into_iter()
        .find(|(i, _)| v.explore.row_node(*i) == Some(node))
        .expect("the file has a row");
    let (x, y) = r.centre();
    v.mouse_move(x, y);

    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::F2),
        None,
        NONE
    ));
    assert!(
        v.workspace_rename.is_some(),
        "F2 names the file under the pointer"
    );
}

/// `F2` over a tree row renames **that** row, opening its document if it is not
/// the one showing. The pointer says which — the tree's own highlight follows
/// the active tab, so "the selected row" would be an obscure way of spelling
/// "the tab you are looking at".
#[test]
fn f2_over_a_tree_row_renames_that_document() {
    let (mut v, mut workspace) = container();
    v.toggle_sidebar();
    let brief = a_document(&mut workspace.ws, "Brief");
    let notes = a_document(&mut workspace.ws, "Notes");
    v.open_node(&workspace.ws, notes);
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);

    let sb = v.regions.sidebar;
    let (i, r) = v
        .explore
        .row_rects(sb)
        .into_iter()
        .find(|(i, _)| v.explore.row_node(*i) == Some(brief))
        .expect("Brief has a row");
    let _ = i;
    let (x, y) = r.centre();
    v.mouse_move(x, y);

    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::F2),
        None,
        NONE
    ));
    assert_eq!(
        v.document_root(),
        Some(brief),
        "the row pointed at came forward"
    );
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("a leaf");
    };
    assert!(l.view().overlay.is_open(), "and its name is being edited");
}

/// Away from the tree, `F2` renames the document showing — which is what it
/// means in a window with no list in front of it.
#[test]
fn f2_over_the_document_renames_the_document() {
    let (mut v, mut workspace) = on_document();
    let c = v.content();
    v.mouse_move(c.x + c.w * 0.5, c.y + c.h * 0.5);

    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::F2),
        None,
        NONE
    ));
    let TabView::Leaf(l) = &v.tabs().active_tab().view else {
        panic!("a leaf");
    };
    assert!(l.view().overlay.is_open());
}

/// A welcome tab has no name to rename, and must not panic being asked.
#[test]
fn f2_on_a_welcome_tab_does_nothing() {
    let (mut v, mut workspace) = container();
    assert!(matches!(v.tabs().active_tab().view, TabView::Welcome(_)));
    assert!(!v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::F2),
        None,
        NONE
    ));
}

/// **Close All is offered even on a lone welcome tab**, because it means *start
/// again* rather than *close things* — and it lands where closing the last tab
/// by hand lands, on one fresh welcome tab rather than an empty window.
#[test]
fn close_all_leaves_one_welcome_tab() {
    let (mut v, mut workspace) = on_document();
    for t in ["a", "b"] {
        let d = a_document(&mut workspace.ws, t);
        v.open_node(&workspace.ws, d);
    }
    assert!(v.tabs().len() > 1);

    right_click_tab(&mut v, &mut workspace.ws, 0);
    assert!(v.run_menu_action(MenuSubject::Tab(0), MenuAction::CloseAll));
    assert_eq!(v.tabs().len(), 1);
    assert_eq!(v.tabs().get(0).map(|t| t.label.as_str()), Some("Welcome"));
}

/// **Dimmed on a lone tab.** *Close All Tabs* means *start again*, and with one
/// tab open it starts you again where you already are — an item that does
/// nothing visible reads as broken. Getting rid of that last tab is ✕'s job,
/// and ✕ now closes the window.
#[test]
fn close_all_is_dimmed_when_there_is_only_one_tab() {
    let (mut v, mut w) = container();
    assert_eq!(v.tabs().len(), 1, "one welcome tab, nothing else to close");
    assert!(!tab_offers(&mut v, &mut w.ws, 0, MenuAction::CloseAll));
}

/// ...and offered as soon as there is more than one.
#[test]
fn close_all_is_offered_once_there_is_more_than_one_tab() {
    let (mut v, mut w) = on_document();
    assert!(v.tabs().len() > 1);
    assert!(tab_offers(&mut v, &mut w.ws, 0, MenuAction::CloseAll));
}

// ------------------------------------------------------------ Ctrl and the ✕

/// One of the caption's window buttons, by which one it is.
fn button_rect(v: &MainView, want: CaptionAction) -> koru_draw::geometry::Rect {
    koru_ui::caption::controls(v.regions.caption)
        .into_iter()
        .find(|control| control.action == want)
        .map(|control| control.rect)
        .expect("the caption draws its own window buttons")
}

/// **Ctrl and the ✕ asks for the tray; the plain ✕ still quits.** The ordinary
/// gesture keeps its ordinary meaning, which is what makes the tray something
/// you have to mean rather than something you fall into.
#[test]
fn ctrl_clicking_the_close_button_asks_for_the_tray() {
    crate::tests::with_menu(false, || {
        let (mut v, mut workspace) = container();
        let close = button_rect(&v, CaptionAction::Close);
        let (x, y) = close.centre();

        v.mouse_down(&mut workspace.ws, x, y, true);
        assert_eq!(v.take_window_request(), Some(WindowRequest::HideToTray));

        v.mouse_down(&mut workspace.ws, x, y, false);
        assert_eq!(v.take_window_request(), Some(WindowRequest::Close));
    });
}

/// The overflow remains an overflow while open; Ctrl does not turn it into a
/// hidden close-to-tray gesture.
#[test]
fn ctrl_clicking_the_open_menu_button_only_dismisses_it() {
    crate::tests::with_menu(true, || {
        let (mut v, mut workspace) = container();
        let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();

        v.mouse_down(&mut workspace.ws, x, y, false);
        assert!(v.window_menu_open(), "the first press opens the menu");
        assert_eq!(v.take_window_request(), None, "and asks for nothing");

        v.mouse_down(&mut workspace.ws, x, y, true);
        assert_eq!(v.take_window_request(), None);
        assert!(!v.window_menu_open());
    });
}

/// Ctrl on any *other* window button is still that button. Only the ✕ has a
/// second meaning, and a modifier that quietly changed minimise would be worse
/// than no modifier at all.
#[test]
fn ctrl_does_not_change_what_the_other_window_buttons_do() {
    crate::tests::with_menu(false, || {
        let (mut v, mut workspace) = container();
        for (hit, expected) in [
            (CaptionAction::Minimise, WindowRequest::Minimise),
            (CaptionAction::ToggleMaximise, WindowRequest::ToggleMaximise),
        ] {
            let rect = button_rect(&v, hit);
            let (x, y) = rect.centre();
            v.mouse_down(&mut workspace.ws, x, y, true);
            assert_eq!(v.take_window_request(), Some(expected));
        }
    });
}

// ------------------------------------------------- the experimental menu button

/// The same overflow control opens and dismisses its menu.
#[test]
fn pressing_the_menu_button_again_dismisses_the_menu() {
    crate::tests::with_menu(true, || {
        let (mut v, mut workspace) = container();
        let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();

        v.mouse_down(&mut workspace.ws, x, y, false);
        assert!(v.window_menu_open());

        v.mouse_down(&mut workspace.ws, x, y, false);
        assert!(!v.window_menu_open(), "and the menu went with it");
        assert_eq!(v.take_window_request(), None);
    });
}

/// A press anywhere *else* while the menu is open keeps the ordinary modal
/// behaviour: it dismisses, and asks for nothing. This is the other half of the
/// exception above — without it, the exception would have been "clicks close
/// the window", which is not what anybody wants.
#[test]
fn a_press_away_from_the_menu_button_only_dismisses_its_menu() {
    crate::tests::with_menu(true, || {
        let (mut v, mut workspace) = container();
        let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
        v.mouse_down(&mut workspace.ws, x, y, false);
        assert!(v.window_menu_open());

        // The middle of the content, well clear of the menu and the caption.
        let (cx, cy) = (v.viewport.0 * 0.5, v.viewport.1 * 0.75);
        v.mouse_down(&mut workspace.ws, cx, cy, false);
        assert!(!v.window_menu_open());
        assert_eq!(v.take_window_request(), None, "no window was harmed");
    });
}

#[test]
fn enabled_overflow_rows_run_their_distinct_actions() {
    crate::tests::with_menu(true, || {
        let mut actions = vec![MenuAction::WindowAlwaysOnTop, MenuAction::HeaderSettings];
        // Updates is one of the rows hidden for now.
        if context_menu::OVERFLOW_HIDDEN_ROWS_SHOWN {
            actions.push(MenuAction::HeaderUpdates);
        }
        for action in actions {
            let (mut v, mut workspace) = container();
            let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
            v.mouse_down(&mut workspace.ws, x, y, false);
            let menu = v.menu.clone().expect("open");
            let i = menu
                .items()
                .iter()
                .position(|item| *item == action)
                .expect("the row is present");
            let (rx, ry) = menu.item_rect(v.viewport, i).centre();
            v.mouse_down(&mut workspace.ws, rx, ry, false);
            match action {
                MenuAction::WindowAlwaysOnTop => {
                    assert_eq!(v.take_window_request(), Some(WindowRequest::TogglePin));
                }
                MenuAction::HeaderSettings => {
                    assert_eq!(v.tabs().active_tab().label, "Settings");
                }
                MenuAction::HeaderUpdates => assert!(v.updates_modal),
                _ => unreachable!(),
            }
        }
    });
}

/// **The zoom stepper works and the menu stays up.** It was a label with
/// spaces in it and no hit targets at all; going from 100% to 70% is three
/// presses of one button, so a row that dismissed on the first would be a
/// worse Ctrl+-.
#[test]
fn the_zoom_steppers_controls_zoom_and_leave_the_menu_open() {
    crate::tests::with_menu(true, || {
        // `↺` is deliberately dim at 100% — see the test below — so the
        // pointer path is checked on the two that are live there. What all
        // three ask for is `each_zoom_control_names_the_action_it_takes`, and
        // the verb each one runs is the arm below.
        for (control, expected) in [(0, ZoomIntent::Out), (2, ZoomIntent::In)] {
            let (mut v, mut workspace) = container();
            let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
            v.mouse_down(&mut workspace.ws, x, y, false);
            let menu = v.menu.clone().expect("open");
            let row = menu
                .items()
                .iter()
                .position(|item| *item == MenuAction::HeaderZoom)
                .expect("the zoom row is present");
            let (cx, cy) = menu
                .control_rect(v.viewport, row, control)
                .expect("the control")
                .centre();

            v.mouse_down(&mut workspace.ws, cx, cy, false);

            assert_eq!(
                v.take_pane_request(),
                Some(PaneRequest::Zoom(expected)),
                "control {control}"
            );
            assert!(
                v.window_menu_open(),
                "control {control} took the menu down with it"
            );
        }
    });
}

/// Each of the three verbs asks the shell for its own move. Taken without the
/// pointer, because `↺` is dim at 100% and this is about what the action does
/// rather than about when it can be reached.
#[test]
fn the_three_zoom_actions_each_ask_the_shell_for_their_own_move() {
    for (action, expected) in [
        (MenuAction::ZoomOut, ZoomIntent::Out),
        (MenuAction::ZoomIn, ZoomIntent::In),
        (MenuAction::ZoomActualSize, ZoomIntent::ActualSize),
    ] {
        let (mut v, _workspace) = container();
        assert!(v.run_menu_action_for_test(MenuSubject::Window, action));
        assert_eq!(v.take_pane_request(), Some(PaneRequest::Zoom(expected)));
    }
}

/// The readout is not a button, and neither is a control at the end of its
/// range: pressing either must neither zoom nor dismiss. The press is still
/// swallowed — falling through would read as a click on the panel and take the
/// menu down for doing nothing.
#[test]
fn a_readout_or_a_dimmed_zoom_control_does_nothing_and_keeps_the_menu() {
    crate::tests::with_menu(true, || {
        // The readout, and `↺` — dim because a fresh window is at 100%.
        for control in [1, 3] {
            let (mut v, mut workspace) = container();
            let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
            v.mouse_down(&mut workspace.ws, x, y, false);
            let menu = v.menu.clone().expect("open");
            let row = menu
                .items()
                .iter()
                .position(|item| *item == MenuAction::HeaderZoom)
                .expect("the zoom row");
            let (cx, cy) = menu
                .control_rect(v.viewport, row, control)
                .expect("the control")
                .centre();

            v.mouse_down(&mut workspace.ws, cx, cy, false);

            assert_eq!(v.take_pane_request(), None, "control {control}");
            assert!(v.window_menu_open(), "control {control}");
        }
    });
}

/// **Resting on a stepper control raises its chip**, carrying the chord.
///
/// The chord is the half worth pinning: `−` cannot say `Ctrl+-` by being a
/// minus sign, and teaching the wrong key would be worse than teaching none.
#[test]
fn hovering_a_zoom_control_teaches_what_it_does_and_its_chord() {
    crate::tests::with_menu(true, || {
        use koru_keys::ShortcutId;
        for (control, id) in [
            (0, ShortcutId::ZoomOut),
            (2, ShortcutId::ZoomIn),
            (3, ShortcutId::ZoomReset),
        ] {
            let (mut v, mut workspace) = container();
            let (bx, by) = button_rect(&v, CaptionAction::WindowMenu).centre();
            v.mouse_down(&mut workspace.ws, bx, by, false);
            let menu = v.menu.clone().expect("open");
            let row = menu
                .items()
                .iter()
                .position(|item| *item == MenuAction::HeaderZoom)
                .expect("the zoom row");
            let (cx, cy) = menu
                .control_rect(v.viewport, row, control)
                .expect("the control")
                .centre();

            v.mouse_move(cx, cy);

            assert_eq!(
                v.tooltip.label(),
                Some(koru_keys::registry::get(id).label),
                "control {control}"
            );
            assert_eq!(
                v.tooltip.shortcut(),
                Some(koru_keys::label_of(id).as_str()),
                "control {control}"
            );
        }
    });
}

/// ...and moving off the controls takes it down. A chip that outlived the
/// thing it was about would sit over the rows explaining nothing.
#[test]
fn moving_off_a_zoom_control_takes_its_chip_down() {
    crate::tests::with_menu(true, || {
        let (mut v, mut workspace) = container();
        let (bx, by) = button_rect(&v, CaptionAction::WindowMenu).centre();
        v.mouse_down(&mut workspace.ws, bx, by, false);
        let menu = v.menu.clone().expect("open");
        let row = menu
            .items()
            .iter()
            .position(|item| *item == MenuAction::HeaderZoom)
            .expect("the zoom row");
        let (cx, cy) = menu
            .control_rect(v.viewport, row, 0)
            .expect("the − control")
            .centre();
        v.mouse_move(cx, cy);
        assert!(v.tooltip.label().is_some(), "the chip is up");

        // The readout, which is not a control — one row, one step sideways.
        let (rx, ry) = menu
            .control_rect(v.viewport, row, 1)
            .expect("the readout")
            .centre();
        v.mouse_move(rx, ry);
        assert_eq!(v.tooltip.label(), None, "a readout explains nothing");

        v.mouse_move(cx, cy);
        assert!(v.tooltip.label().is_some());
        // A plain row of the same menu.
        let settings = menu
            .items()
            .iter()
            .position(|item| *item == MenuAction::HeaderSettings)
            .expect("the Settings row");
        let (sx, sy) = menu.item_rect(v.viewport, settings).centre();
        v.mouse_move(sx, sy);
        assert_eq!(v.tooltip.label(), None, "off the controls, off the chip");
    });
}

/// **Hovering the ⋮ button opens its menu**, with no click at all.
#[test]
fn hovering_the_menu_button_opens_its_menu() {
    crate::tests::with_menu(true, || {
        let (mut v, _workspace) = container();
        assert!(!v.window_menu_open());
        let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
        assert!(v.mouse_move(x, y), "the move earns a frame");
        assert!(v.window_menu_open());
    });
}

/// ...and moving away closes it. Without this the menu would follow the pointer
/// out of the caption and never let go.
#[test]
fn moving_off_the_menu_button_closes_its_menu() {
    crate::tests::with_menu(true, || {
        let (mut v, _workspace) = container();
        let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
        v.mouse_move(x, y);
        assert!(v.window_menu_open());

        assert!(v.mouse_move(v.viewport.0 * 0.5, v.viewport.1 * 0.75));
        assert!(!v.window_menu_open());
    });
}

/// **The panel is not "outside".** It hangs under the button, so the walk from
/// one to the other has to keep the menu up — otherwise there is no way to
/// reach a row and the whole hover gesture is useless.
#[test]
fn moving_from_the_menu_button_into_its_panel_keeps_it_open() {
    crate::tests::with_menu(true, || {
        let (mut v, _workspace) = container();
        let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
        v.mouse_move(x, y);
        let menu = v.menu.clone().expect("open");

        for i in 0..4 {
            let (rx, ry) = menu.item_rect(v.viewport, i).centre();
            v.mouse_move(rx, ry);
            assert!(v.window_menu_open(), "row {i} closed the menu under itself");
        }
    });
}

/// A hover must not open a menu over a dialog the user has been asked to
/// answer: a press cannot reach the button then, and hovering must not become
/// the way round that.
#[test]
fn hovering_does_not_open_the_menu_over_a_dialog() {
    crate::tests::with_menu(true, || {
        let (mut v, mut workspace) = container();
        v.prompt_new_document();
        let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
        v.mouse_move(x, y);
        assert!(!v.window_menu_open());
        let _ = &mut workspace;
    });
}

/// With the flag off there is no ⋮ button, so nothing opens on hover — the
/// four buttons behave exactly as they did.
#[test]
fn hovering_the_close_button_opens_nothing() {
    crate::tests::with_menu(false, || {
        let (mut v, _workspace) = container();
        let (x, y) = button_rect(&v, CaptionAction::Close).centre();
        v.mouse_move(x, y);
        assert!(v.menu.is_none());
    });
}

/// `Esc` closes the menu without touching the window — the same key that closes
/// every other context menu here.
#[test]
fn escape_closes_the_window_menu_and_nothing_else() {
    crate::tests::with_menu(true, || {
        let (mut v, mut workspace) = container();
        let (x, y) = button_rect(&v, CaptionAction::WindowMenu).centre();
        v.mouse_down(&mut workspace.ws, x, y, false);
        assert!(v.window_menu_open());

        v.key(
            &mut workspace.ws,
            &Key::Named(winit::keyboard::NamedKey::Escape),
            None,
            NONE,
        );
        assert!(!v.window_menu_open());
        assert_eq!(v.take_window_request(), None);
    });
}

/// **A local rename updates the tab and the tree at once.** Neither is rebuilt
/// from the document per frame — both are caches — and a local edit does not
/// come back round through `adopt` the way a peer's does, so the container has
/// to notice the commit as it routes the key. This was done for an image rename
/// and not for a document's title, which is why one updated immediately and the
/// other did not update until something unrelated invalidated the cache.
#[test]
fn renaming_a_document_retitles_its_tab_immediately() {
    let (mut v, mut workspace) = on_document();
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);
    assert_eq!(v.tabs().active_tab().label, "Notes");

    // F2 opens the prompt over the document showing.
    let c = v.content();
    v.mouse_move(c.x + c.w * 0.5, c.y + c.h * 0.5);
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::F2),
        None,
        NONE
    ));
    assert!(v.key(&mut workspace.ws, &ch("Brief"), Some("Brief"), NONE));
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE
    ));

    assert_eq!(
        v.tabs().active_tab().label,
        "Brief",
        "the tab retitled without waiting for anything else to happen"
    );
    assert!(v.explore_stale, "and the tree will be rebuilt");
}

/// **A prompt owns Enter, whoever else has the arrows.** The tree may hold the
/// keyboard, but not over a panel that is asking a question — without this the
/// Enter that should commit a rename opened the focused row instead and the
/// rename was silently dropped.
#[test]
fn a_prompt_takes_enter_back_from_the_tree() {
    let (mut v, mut workspace) = on_document();
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);

    let c = v.content();
    v.mouse_move(c.x + c.w * 0.5, c.y + c.h * 0.5);
    v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::F2),
        None,
        NONE,
    );
    assert!(v.tabs().active_tab().view.is_prompting());

    // Hand the tree the keyboard back *while the prompt is up*, which is the
    // state the guard is for.
    v.explore.focus_tree(v.regions.sidebar);
    v.key(&mut workspace.ws, &ch("Brief"), Some("Brief"), NONE);
    v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );
    assert_eq!(
        v.tabs().active_tab().label,
        "Brief",
        "Enter committed the name rather than opening the focused row"
    );
}

// ------------------------------------------------------- Ctrl+E, three states

/// **The state that was missing.** Typing in a document with the pane already
/// open and reaching for `Ctrl+E` means *put me back in the tree*, not *hide the
/// tree* — and shutting it was the only thing that could happen before, which
/// made the chord useless from exactly the place you would reach for it.
#[test]
fn ctrl_e_from_the_document_takes_the_keyboard_rather_than_shutting_the_pane() {
    let (mut v, mut workspace) = on_document();
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);
    assert!(
        v.sidebar.is_open() && !v.explore.has_focus(),
        "in the document"
    );

    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(v.sidebar.is_open(), "the pane stayed open");
    assert!(v.explore.has_focus(), "and the tree took the keyboard");

    // Pressed again, it is still a toggle — the muscle memory survives.
    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(!v.sidebar.is_open());
    assert!(!v.explore.has_focus(), "a hidden list keeps nothing");
}

// -------------------------------------------------- Ctrl+Alt+W, the switcher

/// The switcher is a floating surface, not a request to change the window's
/// layout. A virtual copy of the header geometry anchors it while Explore is
/// shut, leaving the document at exactly the width the user chose.
#[test]
fn ctrl_alt_w_shows_the_workspaces_without_opening_the_pane() {
    let (mut v, mut workspace) = on_document();
    assert!(!v.sidebar.is_open(), "shut to begin with");
    let content = v.content();

    assert!(v.key(&mut workspace.ws, &ch("w"), Some("w"), PRIMARY_ALT));
    assert!(!v.sidebar.is_open(), "the pane stayed shut");
    assert_eq!(v.content(), content, "the document kept all of its width");
    assert!(v.explore.popover_open(), "the switcher is showing");
    assert!(
        v.explore
            .popover_panel(v.explore_popover_area(), v.viewport)
            .is_some(),
        "and has visible geometry without the pane"
    );
    assert!(!v.explore.has_focus());
}

/// Pressed with the pane already open it just shows the list — and pressed
/// again it stays. **Not a toggle**: Escape is how a menu shuts, and a shortcut
/// that closed it would make the second press of a chord you hold down undo the
/// first.
#[test]
fn ctrl_alt_w_leaves_the_switcher_open() {
    let (mut v, mut workspace) = on_document();
    v.toggle_sidebar();
    v.adopt(&workspace.ws);

    v.key(&mut workspace.ws, &ch("w"), Some("w"), PRIMARY_ALT);
    assert!(v.explore.popover_open());
    v.key(&mut workspace.ws, &ch("w"), Some("w"), PRIMARY_ALT);
    assert!(v.explore.popover_open(), "still showing");

    // Escape is what shuts it, through the popover's own key handling.
    v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        NONE,
    );
    assert!(!v.explore.popover_open());
}

/// And it must not be confused with `Ctrl+W`, which closes a tab.
#[test]
fn ctrl_alt_w_does_not_close_a_tab() {
    let (mut v, mut workspace) = on_document();
    let before = v.tabs().len();
    v.key(&mut workspace.ws, &ch("w"), Some("w"), PRIMARY_ALT);
    assert_eq!(v.tabs().len(), before, "Alt is what tells the two apart");
}

/// *Import content…* is a switcher row now: it shuts the switcher and opens
/// the import formats as a menu, whose rows import through the picker.
#[test]
fn import_content_closes_the_switcher_and_offers_the_formats() {
    let (mut v, mut workspace) = on_document();
    v.key(&mut workspace.ws, &ch("w"), Some("w"), PRIMARY_ALT);
    let row = v
        .explore
        .popover_rects(v.explore_popover_area(), v.viewport)
        .into_iter()
        .find(|(row, _)| *row == crate::panes::explore::PopoverRow::ImportContent)
        .map(|(_, rect)| rect)
        .expect("the Import content row");
    let (x, y) = row.centre();
    v.mouse_down(&mut workspace.ws, x, y, false);
    assert!(
        !v.explore.popover_open(),
        "the switcher closes after choosing"
    );

    let markdown = v
        .menu
        .as_ref()
        .map(|menu| {
            assert_eq!(menu.items(), context_menu::import_items().as_slice());
            menu.item_rect(v.viewport, 0)
        })
        .expect("the import format menu");
    let (x, y) = markdown.centre();
    v.mouse_down(&mut workspace.ws, x, y, false);
    assert_eq!(
        v.take_pane_request(),
        Some(PaneRequest::ImportContent {
            format: "markdown",
            source: koru_workspace_host::ImportSourceSelection::Picker,
        })
    );
}

#[test]
fn the_workspace_popover_owns_the_resize_grip_under_its_rows() {
    let (mut v, mut workspace) = on_document();
    let workspace_id = workspace.ws.id().expect("workspace id");
    let profile_id = koru_core::ProfileId::new();
    v.set_pane_context(
        Vec::new(),
        profile_id,
        vec![koru_core::WorkspaceEntry::new(workspace_id, "Example", 0)],
        Some(workspace_id),
    );
    v.refresh_explore(&workspace.ws, true);
    v.toggle_sidebar();
    v.sidebar
        .advance(std::time::Instant::now() + std::time::Duration::from_secs(1));
    v.key(&mut workspace.ws, &ch("w"), Some("w"), PRIMARY_ALT);

    let (_, rect) = v
        .explore
        .popover_rects(v.explore_popover_area(), v.viewport)
        .into_iter()
        .find(|(row, _)| matches!(row, crate::panes::explore::PopoverRow::Workspace(0)))
        .expect("the workspace row");
    let grip = v.sidebar_grip();
    let left = rect.x.max(grip.x);
    let right = (rect.x + rect.w).min(grip.x + grip.w);
    assert!(
        left < right,
        "this test point is where the row and the grip overlap"
    );
    let x = (left + right) * 0.5;
    let y = rect.centre().1;

    v.mouse_move(x, y);
    assert_eq!(View::cursor_icon(&v), koru_ui::cursor::PRESS);
    v.mouse_down(&mut workspace.ws, x, y, false);
    assert!(
        v.sidebar_drag.is_none(),
        "the underlying grip did not start"
    );
    // The press is the row's: it switches to that workspace.
    assert_eq!(
        v.take_pane_request(),
        Some(PaneRequest::Switch(workspace_id))
    );
}

// ------------------------------------------------- Ctrl+Alt+P, the profiles

#[test]
fn ctrl_alt_p_shows_profiles_without_opening_explore() {
    let (mut v, mut workspace) = on_document();
    assert!(!v.sidebar.is_open(), "shut to begin with");
    let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view else {
        panic!("a document");
    };
    leaf.view_mut().view_mode = koru_draw::theme::ViewMode::Full;
    let content = v.content();

    assert!(v.key(&mut workspace.ws, &ch("p"), Some("p"), PRIMARY_ALT,));
    assert!(!v.sidebar.is_open(), "Explore stayed shut");
    assert_eq!(v.content(), content, "the document kept all of its width");
    assert!(v.explore.popover_open(), "the profiles list is showing");
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("a document");
    };
    assert_eq!(
        leaf.view().view_mode,
        koru_draw::theme::ViewMode::Full,
        "the document did not take the chord as Page width"
    );
    v.explore
        .advance_popover(std::time::Instant::now() + std::time::Duration::from_secs(1));
    assert!(
        v.explore
            .popover_progress(crate::panes::explore::PopoverKind::Profiles)
            > 0.0,
        "the open popover is Profiles, not Workspaces"
    );
    assert!(
        v.explore
            .popover_panel(v.explore_popover_area(), v.viewport)
            .is_some(),
        "and has visible geometry without Explore"
    );
}

/// It lands on the document you are in, not at the top of a list you would have
/// to walk back down.
#[test]
fn ctrl_e_lands_the_keyboard_on_the_document_showing() {
    let (mut v, mut workspace) = on_document();
    let brief = a_document(&mut workspace.ws, "Brief");
    v.open_node(&workspace.ws, brief);
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);
    v.explore.clear_focus();

    v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT);
    assert_eq!(v.explore.focused(), Some(brief));
}

/// **An empty library must not give you a pane that opens and never shuts.**
/// With nothing to focus, the chord falls through to being a plain toggle.
#[test]
fn ctrl_e_still_toggles_when_there_is_nothing_to_focus() {
    let (mut v, mut workspace) = container();
    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(v.sidebar.is_open());
    assert!(v.key(&mut workspace.ws, &ch("e"), Some("e"), PRIMARY_SHIFT));
    assert!(
        !v.sidebar.is_open(),
        "an empty tree cannot trap the pane open"
    );
}

/// Clicking the logo is unambiguously *hide the pane* — there is no "put me
/// back" reading of a click on the thing that shuts it.
#[test]
fn the_toggle_shuts_the_pane_even_when_the_keyboard_is_in_the_document() {
    let (mut v, workspace) = on_document();
    v.toggle_sidebar();
    v.adopt(&workspace.ws);
    v.refresh_explore(&workspace.ws, true);
    assert!(v.sidebar.is_open() && !v.explore.has_focus());

    assert!(v.toggle_sidebar());
    assert!(!v.sidebar.is_open(), "the logo shut it");
}

// ------------------------------------------------------- the taskbar's recents

/// A Recent entry names a document by id. It outlives the document — deleted,
/// or in a workspace this window is not on — and a **stale shortcut must do
/// nothing quietly** rather than put a dialog in front of somebody who clicked
/// a taskbar row.
#[test]
fn a_taskbar_entry_for_a_document_that_is_gone_does_nothing() {
    let (mut v, workspace) = container();
    let tabs = v.tabs().len();
    assert!(!v.open_document_by_id(&workspace.ws, NodeId::new()));
    assert_eq!(v.tabs().len(), tabs, "and opened no tab");
}

#[test]
fn a_taskbar_entry_opens_the_document_it_names() {
    let (mut v, mut workspace) = container();
    let brief = a_document(&mut workspace.ws, "Brief");
    assert!(v.open_document_by_id(&workspace.ws, brief));
    assert_eq!(v.document_root(), Some(brief));
}

/// Cold jump-list / pinned click: shell runs `initial` then `restore`. Opening
/// before restore used to be wiped by the previous session's active tab — the
/// user clicked "Bugs" and saw last session's "Roadmap" instead.
#[test]
fn a_cold_taskbar_open_survives_session_restore() {
    use koru_workspace_host::{Settings, TabRef, View};

    let (mut v, mut workspace) = container();
    let session_doc = a_document(&mut workspace.ws, "Last session");
    let pinned = a_document(&mut workspace.ws, "Pinned pick");
    v.open_node(&workspace.ws, session_doc);

    let mut settings = Settings::default();
    v.capture_session(&mut settings);
    assert_eq!(
        settings.tabs.last().map(|(r, _)| r),
        Some(&TabRef::Node(session_doc))
    );
    assert_eq!(
        settings.tabs.get(settings.tab).map(|(r, _)| r),
        Some(&TabRef::Node(session_doc)),
        "precondition: session would reopen on Last session"
    );

    // Fresh window: defer the jump-list id the way `Manager::initial` does, then
    // restore the way `App::new` does.
    let mut cold = MainView::new();
    cold.defer_search_target(None, Some(pinned), None, None);
    View::restore(&mut cold, &workspace, &settings);

    assert_eq!(
        cold.document_root(),
        Some(pinned),
        "pinned / --open document must win over the restored session tab"
    );
}

/// The shared resolver: most-recent first, and anything not in the live listing
/// (deleted, or in another workspace) is dropped — this is what both the Jump
/// List and the welcome tab draw from.
#[test]
fn the_recent_documents_are_resolved_most_recent_first() {
    use koru_core::explore::ExploreEntry;

    fn entry(id: koru_core::DocumentId, title: &str) -> ExploreEntry {
        ExploreEntry {
            id,
            parent: None,
            ordinal: 0,
            title: title.into(),
            kind: "page".into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }
    }

    let (mut v, _workspace) = container();
    let a = koru_core::DocumentId::new();
    let b = koru_core::DocumentId::new();
    View::set_entries(&mut v, vec![entry(a, "A"), entry(b, "B")], None);
    v.recents = vec![b, a, koru_core::DocumentId::new()];
    v.recents_stale = true;

    let offered = v.recent_documents();
    assert_eq!(
        offered,
        vec![("B".to_string(), b), ("A".to_string(), a)],
        "most recent first, the id absent from the listing dropped"
    );
}

/// **Clear recents lives in + → Documents now**, under the recents it clears,
/// and only while there are some. The Welcome page no longer lists them.
#[test]
fn clear_recents_is_the_last_row_of_the_documents_flyout_and_clears_them() {
    use koru_core::explore::ExploreEntry;

    let (mut v, _workspace) = container();
    let id = koru_core::DocumentId::new();
    v.recents = vec![id];
    View::set_entries(
        &mut v,
        vec![ExploreEntry {
            id,
            parent: None,
            ordinal: 0,
            title: "Recent page".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );
    v.snapshot_plus_menu();
    let items = context_menu::document_plus_items();
    assert_eq!(items.last(), Some(&MenuAction::ClearRecents));
    assert!(items.contains(&MenuAction::OpenRecentDocument(id)));

    v.recents_stale = false;
    assert!(v.run_menu_action(MenuSubject::NewTab, MenuAction::ClearRecents));
    assert!(v.recents.is_empty());
    assert!(v.recents_stale, "the Jump List follows");
    v.snapshot_plus_menu();
    assert!(
        !context_menu::document_plus_items().contains(&MenuAction::ClearRecents),
        "nothing left to clear"
    );
}

/// A row ticked on one Welcome tab is ticked on every Welcome tab in the
/// window, and the action behind it ran: the tick is machine-wide state the
/// container owns, not the view's.
#[test]
fn a_welcome_tick_reaches_the_container_and_runs_the_existing_command() {
    let (mut v, mut workspace) = container();
    v.sync_welcome_lists();
    let TabView::Welcome(welcome) = &mut v.tabs_mut().active_mut().view else {
        panic!("a fresh container opens Welcome")
    };
    // Tab past Customize to the first row, and press Enter.
    let ws = &mut workspace.ws;
    assert!(welcome.key(
        ws,
        &Key::Named(winit::keyboard::NamedKey::Tab),
        None,
        ModifiersState::empty()
    ));
    assert!(welcome.key(
        ws,
        &Key::Named(winit::keyboard::NamedKey::Tab),
        None,
        ModifiersState::empty()
    ));
    assert!(welcome.key(
        ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        ModifiersState::empty()
    ));
    v.drain_welcome();
    assert!(
        v.new_document.is_some(),
        "Make your first document opened the name prompt"
    );
    assert_eq!(
        v.welcome_progress.done().collect::<Vec<_>>(),
        ["first-document"]
    );
    let TabView::Welcome(welcome) = &v.tabs().active_tab().view else {
        panic!("still Welcome")
    };
    assert!(welcome.is_done(crate::welcome::RowId::FirstDocument));
}

/// Welcome's *Run an agent in a terminal* opens the `+` menu's list of
/// agents, so a pick there starts the same session tab it does from `+`.
#[test]
fn welcome_opens_the_plus_menus_list_of_agents() {
    let (mut v, _w) = container();
    let agents = context_menu::shown(context_menu::agent_plus_items());
    assert!(!agents.is_empty(), "agents are on unless set off");
    assert!(v.open_agents_menu());
    let menu = v.menu.as_ref().expect("the list is open");
    assert_eq!(menu.subject, MenuSubject::NewTab);
    assert_eq!(menu.items(), agents.as_slice());
}

/// Clicking away closes the updates modal — on the *release*, so both halves
/// of the click are consumed and neither reaches the document beneath
/// (ADR-150). A press inside the panel that misses every button keeps it open,
/// and Escape still closes it.
#[test]
fn updates_modal_closes_on_the_release_of_an_outside_click() {
    let (mut v, mut workspace) = container();
    v.updates_modal = true;
    let outside = (4.0, v.viewport.1 - 4.0);
    let inside = (v.viewport.0 * 0.5, v.viewport.1 * 0.5);

    // Inside, off every button: consumed, still open.
    assert!(v.mouse_down(&mut workspace.ws, inside.0, inside.1, false));
    assert!(v.mouse_up(&mut workspace.ws));
    assert!(v.updates_modal, "a press inside the panel does not dismiss");

    // Outside: the press keeps the modal (the leak ADR-150 names), the
    // release closes it, and both were consumed.
    assert!(v.mouse_down(&mut workspace.ws, outside.0, outside.1, false));
    assert!(v.updates_modal, "the press alone does not dismiss");
    assert!(v.mouse_up(&mut workspace.ws));
    assert!(
        !v.updates_modal,
        "the release of an outside press dismisses"
    );

    v.updates_modal = true;
    assert!(v.key(
        &mut workspace.ws,
        &Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        NONE,
    ));
    assert!(!v.updates_modal);
}

// -------------------------------------------------------- naming a document

/// The tab strip and the explore tree wait for the caret to leave the title.
///
/// A document's name is its first row now, so it is typed rather than
/// committed. Following it keystroke by keystroke means a tab whose width
/// changes under the pointer and a tree row that re-sorts while you are looking
/// at it, for a name nobody has finished writing.
#[test]
fn the_tab_label_waits_until_the_caret_leaves_the_title() {
    let (mut v, mut workspace) = on_document();
    let doc = v.document_root().expect("a document tab");
    let before = v.tabs().active_tab().label.clone();
    assert_eq!(before, "Notes");

    // Into the title, and type.
    let TabView::Leaf(l) = &mut v.tabs_mut().active_mut().view else {
        panic!("a leaf");
    };
    l.body_mut().focus = 0;
    l.view_mut().active = true;
    assert!(v.tabs().active_tab().view.editing_title() == Some(doc));
    v.key(&mut workspace.ws, &ch("!"), Some("!"), NONE);

    // The document itself has the new name straight away — it is the model.
    assert_eq!(koru_workspace::name_of(&workspace.ws, doc), "!Notes");
    v.settle_naming(&workspace.ws);
    assert_eq!(
        v.tabs().active_tab().label,
        before,
        "but the tab still shows the name it had"
    );
    assert!(
        !v.explore_stale || v.naming.is_some(),
        "and the tree is held with it"
    );

    // Leave the title: both catch up, in one go.
    let TabView::Leaf(l) = &mut v.tabs_mut().active_mut().view else {
        panic!("a leaf");
    };
    l.body_mut().focus = 1;
    assert!(
        v.settle_naming(&workspace.ws),
        "leaving is what commits the caches"
    );
    assert_eq!(v.tabs().active_tab().label, "!Notes");
    assert!(v.explore_stale, "and the tree is asked to rebuild");
}

/// Entering a title is not a commit — only leaving one is. Without this the
/// first frame after clicking into the name would refresh for nothing.
#[test]
fn entering_a_title_refreshes_nothing() {
    let (mut v, workspace) = on_document();
    v.explore_stale = false;
    let TabView::Leaf(l) = &mut v.tabs_mut().active_mut().view else {
        panic!("a leaf");
    };
    l.body_mut().focus = 0;
    l.view_mut().active = true;

    assert!(!v.settle_naming(&workspace.ws));
    assert!(!v.explore_stale);
    assert!(v.naming.is_some(), "but it is remembered");
}

/// The unclaimed layout chords reach the document through the container.
///
/// They were `Ctrl+Alt+P/N/W` and never fired: on Windows `Ctrl+Alt` is AltGr,
/// so `AltGr+letter` is a different logical key on most layouts — which the
/// key map had already written down as the reason `SetHeading` lives on
/// `Ctrl+Alt+`*digits*. Narrow and Full remain on `Ctrl+Shift`; Page is now the
/// profile switcher in a container, while a standalone document retains its
/// direct Page-width chord. This drives the container's whole key path, which
/// is where a chord claimed higher up would show.
#[test]
fn the_layout_chords_reach_the_document_through_the_container() {
    use koru_draw::theme::ViewMode;
    let (mut v, mut workspace) = on_document();
    let ctrl_shift = koru_keys::PRIMARY.union(ModifiersState::SHIFT);

    // `W` closes every tab and `F` opens global search, so Full width is `G`.
    for (letter, mode) in [("n", ViewMode::Narrow), ("g", ViewMode::Full)] {
        assert!(
            v.key(&mut workspace.ws, &ch(letter), Some(letter), ctrl_shift),
            "Ctrl+Shift+{letter} was handled"
        );
        let TabView::Leaf(l) = &v.tabs().active_tab().view else {
            panic!("a leaf");
        };
        assert_eq!(l.view().view_mode, mode, "Ctrl+Shift+{letter}");
    }
    assert_eq!(v.tabs().len(), 2, "and no tab or window was opened");
}

// -------------------------------------------------- the workspace manager
//
// The Workspaces tab's tests moved with its entry points: Ctrl+Shift+O, the
// restart, the right-click menu and the menu's Delete now open and act on the
// Manage tab's Profile pages — see `workspace_menu/tests.rs`.

// -------------------------------------------------------- back and forward

/// Alt+Left / the mouse's Back button, once you have been somewhere.
#[test]
fn back_returns_to_the_document_you_came_from() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    v.open_node(&w.ws, b);
    assert_eq!(v.document_root(), Some(b));

    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(v.document_root(), Some(a));

    assert!(v.navigate_history(&w.ws, 1));
    assert_eq!(v.document_root(), Some(b), "and forward comes back");
}

/// Store-backed workspaces load one document at a time. A history destination
/// in another document must be reopened by its stable id rather than rejected
/// because its root cannot resolve in the document currently under the caret.
#[test]
fn back_reopens_an_unloaded_store_document_before_restoring_its_place() {
    use koru_core::explore::ExploreEntry;

    let (mut v, w) = on_document();
    let current = v.document_root().expect("a current document");
    let mut target_ws = WorkspaceDoc::create(w.ws.id().unwrap(), "target").unwrap();
    let target = a_document(&mut target_ws, "Target");
    // Store document ids and root-node ids are deliberately unrelated. The
    // production bug was hidden by a test that minted one from the other.
    let current_document = koru_core::DocumentId::new();
    let target_document = koru_core::DocumentId::new();
    let entry = |id, title: &str| ExploreEntry {
        id,
        parent: None,
        ordinal: 0,
        title: title.into(),
        kind: koru_core::explore::KIND_DOCUMENT.into(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    };
    View::set_entries(
        &mut v,
        vec![
            entry(current_document, "Current"),
            entry(target_document, "Target"),
        ],
        Some(current_document),
    );
    v.tabs_mut().active_mut().document = Some(current_document);

    // Follow a Store manifest row through the same asynchronous request/swap
    // boundary as an Explore click or a document pill.
    assert!(v.open_node(&w.ws, NodeId::from_uuid(target_document.as_uuid())));
    assert_eq!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: target_document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        })
    );
    assert!(View::show_active_document(
        &mut v,
        &target_ws,
        Some(target_document)
    ));
    v.tabs_mut().active_view().set_scroll(24.0);
    assert_eq!(v.document_root(), Some(target));

    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: current_document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        })
    );
    assert_eq!(
        v.history_pending.and_then(|spot| spot.document),
        Some(current_document)
    );

    assert!(View::show_active_document(
        &mut v,
        &w.ws,
        Some(current_document)
    ));
    assert_eq!(v.document_root(), Some(current));
    assert!(v.history_pending.is_none());
    assert!(!v.restoring);

    assert!(v.navigate_history(&w.ws, 1));
    assert_eq!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: target_document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        }),
        "Forward reopens the document reached before Back"
    );
    assert!(View::show_active_document(
        &mut v,
        &target_ws,
        Some(target_document)
    ));
    assert_eq!(v.tabs_mut().active_view().scroll(), 24.0);
}

#[test]
fn creating_a_document_records_the_real_page_not_the_private_draft() {
    let (mut v, w) = on_document();
    let came_from = v.document_root().expect("a current document");

    assert!(v.new_tab());
    assert!(v.tabs().active_tab().draft.is_some());
    assert_eq!(v.history.len(), 1);
    assert_eq!(v.history[0].doc, came_from);
    assert!(
        v.here().is_none(),
        "a private draft is not a history location"
    );

    // Back from a draft has no current location to append, but the real page
    // captured before Ctrl+T remains the destination.
    v.history_at = v.history.len();
    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(v.document_root(), Some(came_from));
}

#[test]
fn entity_create_uses_the_query_as_the_document_title_without_a_prompt() {
    let (mut v, _w) = on_document();

    assert!(v.new_page_from_entity("  someone  ".into()));
    assert!(v.new_document.is_none(), "no naming prompt was opened");
    assert_eq!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::CreateLinkedDocument(
            "someone".into()
        ))
    );
}

/// The browser rule: going back and then somewhere *new* drops the branch you
/// came from. Keeping it would make this a graph, and Forward would have to ask
/// which way.
#[test]
fn opening_something_new_after_a_back_truncates_the_forward_branch() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    let c = a_document(&mut w.ws, "C");
    v.open_node(&w.ws, a);
    v.open_node(&w.ws, b);
    v.navigate_history(&w.ws, -1);
    assert_eq!(v.document_root(), Some(a));

    v.open_node(&w.ws, c);

    assert!(!v.navigate_history(&w.ws, 1), "B is no longer ahead of us");
    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(v.document_root(), Some(a), "back still goes where it did");
}

/// Nothing to go back to is a press that does nothing, not a press that lands
/// somewhere arbitrary.
#[test]
fn back_from_the_first_place_does_nothing() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    v.open_node(&w.ws, a);

    assert!(!v.navigate_history(&w.ws, -1));
    assert!(!v.navigate_history(&w.ws, 1));
    assert_eq!(v.document_root(), Some(a));
}

/// A deleted document must not become a wall you cannot press past: the walk
/// skips it and carries on in the same direction.
#[test]
fn a_deleted_document_is_skipped_rather_than_stopping_the_walk() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    let c = a_document(&mut w.ws, "C");
    v.open_node(&w.ws, a);
    v.open_node(&w.ws, b);
    v.open_node(&w.ws, c);
    // Close B's tab as well, or `open_node` would find it and reuse it.
    let i = v.tabs().showing(b).expect("B has a tab");
    v.close(i);
    w.ws.delete(b).unwrap();

    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(v.document_root(), Some(a), "B was skipped, not stopped on");
}

/// Restoring a place must not itself count as a jump, or the cursor would walk
/// forward again the moment it moved back and Back would toggle between two
/// entries forever.
#[test]
fn a_restore_records_no_history_of_its_own() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    let c = a_document(&mut w.ws, "C");
    v.open_node(&w.ws, a);
    v.open_node(&w.ws, b);
    v.open_node(&w.ws, c);

    v.navigate_history(&w.ws, -1);
    v.navigate_history(&w.ws, -1);

    assert_eq!(
        v.document_root(),
        Some(a),
        "two backs reach two places back"
    );
}

/// A workspace swap keeps routed history, so Back can ask the shell to reopen
/// the prior workspace and its document rather than treating their node ids as
/// though they belonged to the document currently loaded.
#[test]
fn back_across_a_workspace_swap_routes_to_the_prior_document() {
    let (mut v, mut w) = container();
    let source_root = a_document(&mut w.ws, "Source");
    v.open_node(&w.ws, source_root);
    let source_document = koru_core::DocumentId::new();
    v.tabs_mut().active_mut().document = Some(source_document);
    v.history.clear();
    v.history_at = 0;
    v.record_jump();

    let (_, profile, _, source_workspace) = v.pane_context.clone().unwrap();
    let mut other = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "other").unwrap();
    let other_workspace = other.id().unwrap();
    let other_root = a_document(&mut other, "Bugs");
    let other_document = koru_core::DocumentId::new();
    v.reseat(&other);
    View::set_pane_context(
        &mut v,
        Vec::new(),
        profile,
        Vec::new(),
        Some(other_workspace),
    );
    v.open_node(&other, other_root);
    v.tabs_mut().active_mut().document = Some(other_document);

    assert_eq!(v.history.len(), 1, "the source survived the reseat");
    assert!(v.navigate_history(&other, -1));
    assert_eq!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: source_document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        })
    );

    // Simulate the shell's workspace swap and document arrival. The deferred
    // source position survives reseat, then Forward routes back to Bugs.
    v.reseat(&w.ws);
    View::set_pane_context(&mut v, Vec::new(), profile, Vec::new(), source_workspace);
    View::set_entries(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: source_document,
            parent: None,
            ordinal: 0,
            title: "Source".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        Some(source_document),
    );
    assert!(View::show_active_document(
        &mut v,
        &w.ws,
        Some(source_document)
    ));
    assert_eq!(v.document_root(), Some(source_root));
    assert!(v.history_pending.is_none());
    assert!(
        v.restoring,
        "the routed request still has one duplicate open phase to complete"
    );

    // `PaneRequest::Navigate` resumes after `switch_workspace` and
    // applies its target to the panes. This second phase used to run after
    // `show_active_document` cleared `restoring`, so it looked like a new jump
    // and truncated Bugs from the forward branch.
    assert!(View::open_search_target(
        &mut v,
        &w.ws,
        Some(NodeId::from_uuid(source_document.as_uuid())),
        None,
        None,
        None,
    ));
    assert!(!v.restoring);

    assert!(v.navigate_history(&w.ws, 1));
    assert!(matches!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document { document, .. },
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
            ..
        }) if document == other_document
    ));
}

#[test]
fn back_across_a_workspace_keeps_the_saved_scroll_through_the_default_document() {
    let (mut v, mut w) = container();
    let source_root = a_document(&mut w.ws, "Source");
    v.open_node(&w.ws, source_root);
    let source_document = koru_core::DocumentId::new();
    let default_document = koru_core::DocumentId::new();
    v.tabs_mut().active_mut().document = Some(source_document);
    v.tabs_mut().active_view().set_scroll(48.0);
    v.history.clear();
    v.history_at = 0;
    v.record_jump();
    assert_eq!(v.history.last().map(|s| s.scroll), Some(48.0));

    let (_, profile, _, source_workspace) = v.pane_context.clone().unwrap();
    let mut other = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "other").unwrap();
    let other_workspace = other.id().unwrap();
    let other_root = a_document(&mut other, "Bugs");
    v.reseat(&other);
    View::set_pane_context(
        &mut v,
        Vec::new(),
        profile,
        Vec::new(),
        Some(other_workspace),
    );
    v.open_node(&other, other_root);

    assert!(v.navigate_history(&other, -1));
    let _ = v.take_pane_request();

    v.reseat(&w.ws);
    View::set_pane_context(&mut v, Vec::new(), profile, Vec::new(), source_workspace);
    assert!(
        !View::show_active_document(&mut v, &w.ws, Some(default_document)),
        "the workspace default must not consume the pending spot"
    );
    assert!(v.history_pending.is_some(), "scroll is still waiting");

    assert!(View::show_active_document(
        &mut v,
        &w.ws,
        Some(source_document)
    ));
    assert_eq!(v.tabs_mut().active_view().scroll(), 48.0);

    assert!(View::open_search_target(
        &mut v,
        &w.ws,
        Some(NodeId::from_uuid(source_document.as_uuid())),
        None,
        None,
        None,
    ));
    assert_eq!(
        v.tabs_mut().active_view().scroll(),
        48.0,
        "the duplicate open phase must not reset the caret"
    );
}

#[test]
fn following_a_routed_pill_records_the_source_for_back() {
    let (mut v, _w) = on_document();
    let (_, _, _, current_workspace) = v.pane_context.clone().unwrap();
    let target = koru_editor::trigger::DocumentTarget {
        document: koru_core::DocumentId::new(),
        node: Some(koru_core::NodeId::new()),
    };
    v.entity_trashed.insert(target.document.to_storage());
    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf")
    };
    leaf.view_mut().wants_entity_open = Some((target, OpenWhere::Reuse));

    assert!(v.sync_entity_picker());
    assert_eq!(v.history.len(), 1, "the source document was recorded");
    assert_eq!(v.history[0].workspace, current_workspace);
    assert!(matches!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document { document, node },
            match_text: Some(ref match_text),
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        }) if document == target.document
            && node == target.node
            && match_text.is_empty()
    ));
}

#[test]
fn a_document_level_routed_pill_uses_the_title_landing_route() {
    let (mut v, _w) = on_document();
    let document = koru_core::DocumentId::new();
    let target = koru_editor::trigger::DocumentTarget {
        document,
        node: None,
    };
    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf")
    };
    leaf.view_mut().wants_entity_open = Some((target, OpenWhere::Reuse));

    assert!(v.sync_entity_picker());
    assert!(matches!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: routed_document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        }) if routed_document.as_uuid() == document.as_uuid()
    ));
}

#[test]
fn a_routed_background_pill_asks_to_open_in_the_background() {
    let (mut v, _w) = on_document();
    let target = koru_editor::trigger::DocumentTarget {
        document: koru_core::DocumentId::new(),
        node: None,
    };
    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf")
    };
    leaf.view_mut().wants_entity_open = Some((target, OpenWhere::BackgroundTab));

    assert!(v.sync_entity_picker());
    assert!(matches!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::OpenBackground {
            document,
            node: None,
        }) if document == target.document
    ));
    assert!(v.history.is_empty(), "staying here is not a jump");
}

/// **A disposition belongs to the arrival it was asked for.**
///
/// `land_on` runs for every arrival — a workspace switch, a reopened session,
/// the shell naming the active document — and it returns early when the node
/// will not resolve, before the pending disposition is taken. So a store-backed
/// open that never landed used to sit there and be spent by whatever arrived
/// next, replacing a tab nobody asked to replace. Pairing it with the document
/// is what makes the stale one harmless.
#[test]
fn a_disposition_for_a_document_that_never_arrived_is_not_spent_on_another() {
    let (mut v, workspace) = on_document();
    let target = koru_editor::trigger::DocumentTarget {
        document: koru_core::DocumentId::new(),
        node: None,
    };
    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf")
    };
    leaf.view_mut().wants_entity_open = Some((target, OpenWhere::SameTab));
    assert!(v.sync_entity_picker());
    assert!(!v.pending_lands.is_empty());
    let tabs_before = v.tabs().len();
    let waiting = v.pending_lands.front().copied();

    // Something else arrives: the queued disposition is for another document,
    // so it must not be spent — multi-select NewTab relies on that.
    let other = koru_core::DocumentId::new();
    View::show_active_document(&mut v, &workspace.ws, Some(other));

    assert_eq!(
        v.pending_lands.front().copied(),
        waiting,
        "a stranger arrival must not claim another document's disposition"
    );
    assert_eq!(
        v.tabs().len(),
        tabs_before,
        "an arrival that was not asked for must not replace this tab"
    );
}

#[test]
fn a_routed_same_tab_pill_remembers_to_replace_this_tab() {
    let (mut v, _w) = on_document();
    let target = koru_editor::trigger::DocumentTarget {
        document: koru_core::DocumentId::new(),
        node: None,
    };
    let TabView::Leaf(leaf) = v.active_view() else {
        panic!("document leaf")
    };
    leaf.view_mut().wants_entity_open = Some((target, OpenWhere::SameTab));

    assert!(v.sync_entity_picker());
    // Paired with the document it was asked for: `land_on` runs for every
    // arrival, so an unpaired disposition would be spent by the next one.
    assert_eq!(
        v.pending_lands.front().copied(),
        Some((target.document, OpenWhere::SameTab))
    );
    assert!(matches!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
            ..
        })
    ));
}

/// A cross-workspace pill must not leave the destination workspace's default
/// document on the Back stack. The shell lands that document while switching,
/// then opens the real target — only the place the person left is a stop.
#[test]
fn back_after_a_cross_workspace_pill_skips_the_workspace_default_document() {
    use koru_core::explore::ExploreEntry;

    let (mut v, _w) = on_document();
    let source_root = v.document_root().expect("source document");
    let source_document = koru_core::DocumentId::new();
    v.tabs_mut().active_mut().document = Some(source_document);
    let (_, profile, _, _source_workspace) = v.pane_context.clone().unwrap();
    // Pill click records the source, then asks the shell for the target.
    v.record_jump();
    assert_eq!(v.history.len(), 1);
    assert_eq!(v.history[0].doc, source_root);

    // Under the Store, one LoroDoc is one document. The workspace default and
    // the pill target are separate docs the shell opens in turn.
    let other_workspace = koru_core::WorkspaceId::new();
    let mut default_ws = WorkspaceDoc::create(other_workspace, "default").unwrap();
    let default_root = a_document(&mut default_ws, "Workspace default");
    let mut target_ws = WorkspaceDoc::create(other_workspace, "target").unwrap();
    let target_root = a_document(&mut target_ws, "Pill target");
    let default_document = koru_core::DocumentId::new();
    let target_document = koru_core::DocumentId::new();
    let entry = |id, title: &str| ExploreEntry {
        id,
        parent: None,
        ordinal: 0,
        title: title.into(),
        kind: koru_core::explore::KIND_DOCUMENT.into(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    };
    let entries = vec![
        entry(default_document, "Workspace default"),
        entry(target_document, "Pill target"),
    ];

    // Switch: reseat, announce the workspace default the way `switch_workspace`
    // does, then open the pill's target the way `open_search_target` does.
    v.reseat(&default_ws);
    View::set_pane_context(
        &mut v,
        Vec::new(),
        profile,
        Vec::new(),
        Some(other_workspace),
    );
    View::set_entries(&mut v, entries.clone(), Some(default_document));
    assert!(View::show_active_document(
        &mut v,
        &default_ws,
        Some(default_document)
    ));
    assert_eq!(v.document_root(), Some(default_root));
    assert_eq!(
        v.history.len(),
        1,
        "landing the workspace default must not become a Back stop"
    );

    assert!(View::open_search_target(
        &mut v,
        &default_ws,
        Some(NodeId::from_uuid(target_document.as_uuid())),
        None,
        None,
        None,
    ));
    assert_eq!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document {
                document: target_document,
                node: None,
            },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        })
    );
    View::set_entries(&mut v, entries, Some(target_document));
    assert!(View::show_active_document(
        &mut v,
        &target_ws,
        Some(target_document)
    ));
    assert_eq!(v.document_root(), Some(target_root));
    assert_eq!(
        v.history.len(),
        1,
        "only the source of the pill jump is on the stack"
    );

    assert!(v.navigate_history(&target_ws, -1));
    assert!(matches!(
        v.take_pane_request(),
        Some(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document { document, .. },
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
            ..
        }) if document == source_document
    ));
}

#[test]
fn a_document_route_which_arrives_in_trash_opens_the_trash_tab() {
    let (mut v, w) = on_document();
    let document = koru_core::DocumentId::new();
    View::set_trashed(
        &mut v,
        vec![koru_core::explore::ExploreEntry {
            id: document,
            parent: None,
            ordinal: 0,
            title: "Deleted plan".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: Some(1),
            trashed_by: None,
        }],
    );

    assert!(View::open_search_target(
        &mut v,
        &w.ws,
        Some(NodeId::from_uuid(document.as_uuid())),
        None,
        None,
        None,
    ));
    assert!(matches!(v.tabs().active_tab().view, TabView::Trash(_)));
}

// ------------------------------------------------- where a pill opens things

/// Ctrl+Enter on a document pill. The difference from a plain open is the whole
/// point: `open_node` reuses a tab already on the document, and a modifier that
/// says "somewhere else" then moving you to a tab you already had looks broken.
#[test]
fn a_new_tab_request_opens_a_second_tab_on_the_same_document() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    v.open_node(&w.ws, a);
    let before = v.tabs().len();

    assert!(v.open_node_in_new_tab(&w.ws, a));

    assert_eq!(v.tabs().len(), before + 1);
    assert_eq!(v.document_root(), Some(a));
}

/// Shift+Enter asks the shell for a window, on the channel a popped-out tab
/// uses — but unlike a pop-out it closes nothing, because this document was
/// never a tab here.
#[test]
fn a_new_window_request_closes_no_tab() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    let before = v.tabs().len();

    assert!(v.open_node_in_new_window(&w.ws, b));

    assert_eq!(v.take_wants_popout(), Some(b));
    assert_eq!(v.tabs().len(), before, "and nothing here was closed");
    assert_eq!(v.document_root(), Some(a), "or switched away from");
}

/// Enter on a pill follows the reference **in this tab**. `open_node`'s reuse
/// rule opens a second tab whenever nothing already shows the target, which is
/// right for a click and is not what "the same tab" means.
#[test]
fn a_same_tab_request_replaces_this_tab_rather_than_adding_one() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    let before = v.tabs().len();

    assert!(v.open_node_here(&w.ws, b));

    assert_eq!(v.tabs().len(), before, "no tab was added");
    assert_eq!(v.document_root(), Some(b), "and this tab followed the link");
}

/// ...but a tab already showing the target is still preferred: following a
/// reference to the document open beside you should take you there rather than
/// leave you with two of it.
#[test]
fn a_same_tab_request_prefers_a_tab_that_already_shows_it() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    v.open_node_in_new_tab(&w.ws, b);
    let before = v.tabs().len();
    let a_tab = v.tabs().showing(a).expect("A still has a tab");

    assert!(v.open_node_here(&w.ws, a));

    assert_eq!(
        v.tabs().len(),
        before,
        "nothing opened and nothing was replaced"
    );
    assert_eq!(v.tabs().active(), a_tab);
    assert_eq!(v.document_root(), Some(a));
}

#[test]
fn a_background_tab_request_adds_a_tab_without_switching() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    let before = v.tabs().len();
    let active = v.tabs().active();

    assert!(v.open_node_in_background_tab(&w.ws, b));

    assert_eq!(v.tabs().len(), before + 1);
    assert_eq!(v.tabs().active(), active);
    assert_eq!(v.document_root(), Some(a));
}

#[test]
fn a_background_tab_request_reuses_a_tab_already_on_the_document() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    v.open_node_in_new_tab(&w.ws, b);
    v.select(v.tabs().showing(a).expect("A"));
    let before = v.tabs().len();
    let active = v.tabs().active();

    assert!(v.open_node_in_background_tab(&w.ws, b));

    assert_eq!(v.tabs().len(), before);
    assert_eq!(v.tabs().active(), active);
    assert_eq!(v.document_root(), Some(a));
}

#[test]
fn restoring_a_replaced_tab_puts_the_document_back_in_this_tab() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    let before = v.tabs().len();
    let slot = v.tabs().active();
    v.replace_this_tab(&w.ws, b, None);
    v.restoring = true;

    assert!(v.land_on(&w.ws, a, false, None));

    v.restoring = false;
    assert_eq!(v.tabs().len(), before, "back must not add a tab");
    assert_eq!(v.tabs().active(), slot);
    assert_eq!(v.document_root(), Some(a));
}

#[test]
fn back_after_switching_duplicate_document_tabs_returns_to_the_other_tab() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    let original = v.tabs().active_tab().id;
    assert!(v.open_node_in_background_tab(&w.ws, b));
    let background = v
        .tabs()
        .iter()
        .find(|t| t.id != original)
        .expect("background tab")
        .id;
    v.replace_this_tab(&w.ws, b, None);
    assert_eq!(v.tabs().active_tab().id, original);
    let background_index = v
        .tabs()
        .iter()
        .position(|t| t.id == background)
        .expect("background still open");
    assert!(v.select(background_index));
    assert_eq!(v.tabs().active_tab().id, background);

    assert!(v.navigate_history(&w.ws, -1));

    assert_eq!(
        v.tabs().active_tab().id,
        original,
        "Back returns to the tab that followed the link, not the background copy"
    );
    assert_eq!(v.document_root(), Some(b));
}

#[test]
fn a_click_follow_replaces_this_tab_even_if_a_sibling_already_shows_it() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    v.open_node_in_new_tab(&w.ws, b);
    v.select(v.tabs().showing(a).expect("A"));
    let before = v.tabs().len();
    let a_tab = v.tabs().active();

    assert!(v.replace_this_tab(&w.ws, b, None));

    assert_eq!(v.tabs().len(), before);
    assert_eq!(v.tabs().active(), a_tab);
    assert_eq!(v.document_root(), Some(b));
}

#[test]
fn back_and_forward_restore_caret_offset_and_selection() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    let a_line =
        w.ws.insert(Parent::Node(a), Node::paragraph("hello world").unwrap())
            .unwrap();
    let b_line =
        w.ws.insert(Parent::Node(b), Node::paragraph("tracked text").unwrap())
            .unwrap();
    v.open_node(&w.ws, a);
    v.adopt(&w.ws);
    {
        let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view else {
            panic!("leaf");
        };
        assert!(Editing::reveal(leaf, a_line));
        Editing::place_caret(leaf, 6);
        Editing::reflow(leaf);
    }
    v.open_node_here(&w.ws, b);
    v.adopt(&w.ws);
    {
        let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view else {
            panic!("leaf");
        };
        assert!(Editing::reveal(leaf, b_line));
        Editing::place_selection_pair(leaf, (0, 7));
        Editing::reflow(leaf);
    }

    assert!(v.navigate_history(&w.ws, -1));
    {
        let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
            panic!("leaf");
        };
        let row = leaf.body().focused().expect("focused");
        assert_eq!(row.id, a_line);
        assert_eq!(row.caret(), 6);
        assert!(row.range().is_empty());
    }

    assert!(v.navigate_history(&w.ws, 1));
    {
        let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
            panic!("leaf");
        };
        let row = leaf.body().focused().expect("focused");
        assert_eq!(row.id, b_line);
        assert_eq!(row.range(), 0..7);
    }
}

/// ...and it is a place you can come back from, like every other jump.
#[test]
fn following_a_link_in_place_is_recorded_in_the_history() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "A");
    let b = a_document(&mut w.ws, "B");
    v.open_node(&w.ws, a);
    v.open_node_here(&w.ws, b);

    assert!(v.navigate_history(&w.ws, -1));
    assert_eq!(v.document_root(), Some(a));
}

#[test]
fn search_modal_is_620_by_400_and_centered() {
    let bounds = search_modal_bounds((1200.0, 700.0));
    assert_eq!(bounds, Rect::new(290.0, 150.0, 620.0, 400.0));
}

#[test]
fn search_modal_keeps_a_margin_in_a_small_window() {
    let bounds = search_modal_bounds((500.0, 300.0));
    assert_eq!(bounds, Rect::new(16.0, 16.0, 468.0, 268.0));
}

#[test]
fn clicking_the_scrim_closes_search_without_reaching_the_page() {
    let (mut view, mut workspace) = container();
    assert!(view.open_search());

    assert!(View::mouse_down(
        &mut view,
        &mut workspace.ws,
        10.0,
        10.0,
        false,
    ));
    assert!(view.search.is_none());
}

#[test]
fn typing_in_internal_search_does_not_mutate_the_collaborative_workspace() {
    let (mut view, mut workspace) = container();
    assert!(view.open_search());
    let before = workspace.ws.version();

    assert!(View::key(
        &mut view,
        &mut workspace.ws,
        &Key::Character("s".into()),
        Some("s"),
        ModifiersState::empty(),
    ));

    assert_eq!(workspace.ws.version(), before);
}

#[test]
fn internal_search_is_scoped_to_the_window_workspace() {
    let (mut view, workspace) = container();
    let profile = ProfileId::new();
    let workspace_id = workspace.ws.id().expect("workspace identity");
    let profiles = vec![ProfileSummary {
        id: profile,
        name: "Testing".into(),
        email: String::new(),
        avatar: None,
        icon: None,
        color: None,
        account: None,
        folder: "C:/testing".into(),
    }];
    let workspaces = vec![WorkspaceEntry::new(workspace_id, "Roadmap", 0)];
    View::set_pane_context(&mut view, profiles, profile, workspaces, Some(workspace_id));

    assert!(view.open_search());
    assert_eq!(
        view.search
            .as_ref()
            .and_then(|search| search.workspace_scope()),
        Some((profile, workspace_id))
    );
}

#[test]
fn opening_move_to_does_not_prefetch_every_workspace_listing() {
    use koru_core::explore::ExploreEntry;

    let (mut view, workspace) = container();
    let profile = ProfileId::new();
    let current = workspace.ws.id().expect("workspace identity");
    let other = WorkspaceId::new();
    let document = koru_core::DocumentId::new();
    View::set_pane_context(
        &mut view,
        Vec::new(),
        profile,
        vec![
            WorkspaceEntry::new(current, "Current", 0),
            WorkspaceEntry::new(other, "Other", 1),
        ],
        Some(current),
    );
    View::set_entries(
        &mut view,
        vec![ExploreEntry {
            id: document,
            parent: None,
            ordinal: 0,
            title: "Move me".into(),
            kind: koru_core::explore::KIND_DOCUMENT.into(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        Some(document),
    );

    assert!(view.open_move_to(vec![document]));
    assert!(view.move_to.is_some());
    assert_eq!(
        View::take_pane_request(&mut view),
        None,
        "opening Ctrl+M must not issue Store requests for unopened workspaces"
    );
}

#[test]
fn ctrl_shift_f_opens_global_search_inside_the_app() {
    let (mut view, mut workspace) = container();
    // PRIMARY_SHIFT, not a literal CONTROL: the chord is ⌘⇧F on a Mac, and a
    // spelled-out modifier is how a test goes green on one platform only.
    assert!(view.key(&mut workspace.ws, &ch("f"), Some("f"), PRIMARY_SHIFT,));
    assert!(view.search.is_some());
}

#[test]
fn successful_search_navigation_retains_the_surface_for_the_next_ctrl_p() {
    let (mut view, mut workspace) = container();
    let document = a_document(&mut workspace.ws, "Search target");
    view.explore.show_files();
    assert!(view.open_search());

    assert!(View::open_search_target(
        &mut view,
        &workspace.ws,
        // The node is in the document this window already has open, so no
        // document has to be asked for.
        None,
        Some(document),
        None,
        None,
    ));
    assert_eq!(
        view.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Documents,
        "opening a page result must leave the Files projection"
    );
    assert!(view.search.is_none());
    assert!(view.retained_search.is_some());

    assert!(view.open_search());
    assert!(view.search.is_some());
    assert!(view.retained_search.is_none());
}

/// **Opening a result opens the tree the document is in.**
///
/// Reported from a running window: a hit two levels down opened its tab and
/// highlighted a row that was still behind a shut caret, so the tree looked as
/// though it had ignored the result entirely. The container's half of this is
/// the wiring — that the reveal reaches the pane at all, and that the rows are
/// walked again afterwards, since a fold that changes makes them wrong.
#[test]
fn opening_a_search_result_unfolds_the_tree_to_its_document() {
    use koru_core::explore::ExploreEntry;
    let (mut v, workspace) = container();
    let parent = koru_core::DocumentId::new();
    let child = koru_core::DocumentId::new();
    let entry = |id, parent, ordinal, title: &str| ExploreEntry {
        id,
        parent,
        ordinal,
        title: title.to_owned(),
        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    };
    View::set_entries(
        &mut v,
        vec![
            entry(parent, None, 0, "2nd workspace, here we go"),
            entry(child, Some(parent), 1, "Let me create another one"),
        ],
        None,
    );
    v.toggle_sidebar();
    v.explore
        .set_collapsed(vec![NodeId::from_uuid(parent.as_uuid())]);
    v.refresh_explore(&workspace.ws, true);
    let area = v.regions.sidebar;
    assert_eq!(
        v.explore.row_rects(area).len(),
        1,
        "the child starts behind a shut caret"
    );

    // A node inside that document, which this window's `ws` cannot resolve —
    // exactly what a hit in an unopened document looks like.
    let node = NodeId::new();
    assert!(View::open_search_target(
        &mut v,
        &workspace.ws,
        Some(NodeId::from_uuid(child.as_uuid())),
        Some(node),
        None,
        None,
    ));
    v.refresh_explore(&workspace.ws, true);

    let rects = v.explore.row_rects(area);
    assert_eq!(rects.len(), 2, "the fold above the result did not open");
    let (_, r) = rects[1];
    assert_eq!(
        v.explore.row_node_at(area, r.x + r.w * 0.5, r.centre().1),
        Some(NodeId::from_uuid(child.as_uuid()))
    );
}

/// **A result whose document has not arrived survives the adopts in between.**
///
/// Reported from a running window: a hit in another workspace opened nothing at
/// all, twice in a row. Opening the document is a round trip through the shell,
/// and the drain used to `take()` the deferred target and only *then* ask
/// whether it resolved — so the first `adopt` threw it away, and a workspace
/// switch fires several before the document is anywhere near open.
#[test]
fn a_deferred_search_target_waits_for_its_document() {
    let (mut v, mut workspace) = container();
    let elsewhere = NodeId::new();

    assert!(
        View::open_search_target(
            &mut v,
            &workspace.ws,
            Some(NodeId::new()),
            Some(elsewhere),
            None,
            None,
        ),
        "a target the window has to fetch is accepted, not refused"
    );
    assert!(v.deferred_search_target.is_some());

    // The adopts a workspace switch fires, before the document is here.
    for _ in 0..3 {
        v.adopt(&workspace.ws);
        assert!(
            v.deferred_search_target.is_some(),
            "the target was dropped before its document could arrive"
        );
    }

    // The document lands, carrying the node the hit named.
    let document = a_document(&mut workspace.ws, "Arrived");
    let node = workspace
        .ws
        .insert(
            Parent::Node(document),
            Node::paragraph("the matched row").unwrap(),
        )
        .unwrap();
    v.deferred_search_target = Some(super::DeferredTarget {
        document: None,
        node: Some(node),
        trash_root: None,
        match_text: None,
        asked: true,
        until: Some(std::time::Instant::now() + super::DEFERRED_TARGET_WAIT),
    });
    v.adopt(&workspace.ws);
    assert!(v.deferred_search_target.is_none(), "it was spent");
    assert_eq!(v.document_root(), Some(document), "the result opened");
}

/// **A hit on a document's own title names no node**, and that is not the same
/// thing as a workspace hit.
///
/// Reported from a running window with the distinguishing detail that made it
/// findable: searching a page by a word in its *body* opened it, and searching
/// the same page by its *name* did not. The index stores a title as a fragment
/// with a null `node_id` — it comes from the manifest's listing rather than
/// from a node in anybody's tree — so a title match arrived indistinguishable
/// from a workspace-level one and was answered "nothing more to do".
#[test]
fn a_title_hit_opens_its_document_and_lands_on_the_title_row() {
    use koru_core::explore::ExploreEntry;
    let (mut v, mut workspace) = container();
    let id = koru_core::DocumentId::new();
    View::set_entries(
        &mut v,
        vec![ExploreEntry {
            id,
            parent: None,
            ordinal: 0,
            title: "Hidden second workspace".to_owned(),
            kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
            mime: None,
            note: None,
            trashed_at: None,
            trashed_by: None,
        }],
        None,
    );
    let row = NodeId::from_uuid(id.as_uuid());

    // No node — which is exactly what the store returns for a title.
    assert!(View::open_search_target(
        &mut v,
        &workspace.ws,
        Some(row),
        None,
        None,
        Some("Hidden second"),
    ));
    let target = v
        .deferred_search_target
        .as_ref()
        .expect("a title hit waits for its document rather than doing nothing");
    assert_eq!(target.document, Some(row));
    assert!(target.node.is_none(), "there is no node to aim at yet");

    // The document arrives and becomes the active one.
    let document = a_document(&mut workspace.ws, "Hidden second workspace");
    v.open_node(&workspace.ws, document);
    let entries = v.entries.clone().unwrap_or_default();
    View::set_entries(&mut v, entries, Some(id));
    v.adopt(&workspace.ws);

    assert!(v.deferred_search_target.is_none(), "the target was spent");
    assert_eq!(v.document_root(), Some(document));
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("a document tab")
    };
    assert!(
        leaf.view().search_flash.is_some(),
        "the title row was not flashed"
    );
}

/// ...but not forever. A target whose document never arrives is one nobody is
/// waiting for, and firing it minutes later — on a document somebody opened for
/// their own reasons — is worse than dropping it.
#[test]
fn a_deferred_target_that_never_resolves_expires() {
    let (mut v, workspace) = container();
    v.deferred_search_target = Some(super::DeferredTarget {
        document: None,
        node: Some(NodeId::new()),
        trash_root: None,
        match_text: None,
        asked: true,
        until: Some(std::time::Instant::now() - std::time::Duration::from_secs(1)),
    });

    v.adopt(&workspace.ws);
    assert!(v.deferred_search_target.is_none());
}

/// A binned result does not touch the library's folds: its row is in the Trash
/// tab, and the tree does not list it at all.
#[test]
fn opening_a_binned_result_leaves_the_tree_folded() {
    use koru_core::explore::ExploreEntry;
    let (mut v, workspace) = container();
    let parent = koru_core::DocumentId::new();
    let child = koru_core::DocumentId::new();
    let entry = |id, parent, ordinal, title: &str| ExploreEntry {
        id,
        parent,
        ordinal,
        title: title.to_owned(),
        kind: koru_core::explore::KIND_DOCUMENT.to_owned(),
        mime: None,
        note: None,
        trashed_at: None,
        trashed_by: None,
    };
    View::set_entries(
        &mut v,
        vec![
            entry(parent, None, 0, "Library"),
            entry(child, Some(parent), 1, "Binned"),
        ],
        None,
    );
    v.toggle_sidebar();
    v.explore
        .set_collapsed(vec![NodeId::from_uuid(parent.as_uuid())]);
    v.refresh_explore(&workspace.ws, true);

    let binned = NodeId::from_uuid(child.as_uuid());
    assert!(View::open_search_target(
        &mut v,
        &workspace.ws,
        Some(binned),
        Some(NodeId::new()),
        Some(binned),
        None,
    ));
    v.refresh_explore(&workspace.ws, true);
    assert_eq!(v.explore.row_rects(v.regions.sidebar).len(), 1);
}

#[test]
fn search_navigation_lands_before_the_matched_text() {
    let (mut view, mut workspace) = container();
    let document = a_document(&mut workspace.ws, "Testing Notes");
    let paragraph = workspace
        .ws
        .insert(
            Parent::Node(document),
            Node::paragraph("accented text, CJK, and emoji.").unwrap(),
        )
        .unwrap();

    assert!(View::open_search_target(
        &mut view,
        &workspace.ws,
        None,
        Some(paragraph),
        None,
        Some("emoji"),
    ));
    let TabView::Leaf(leaf) = &view.tabs().active_tab().view else {
        panic!("search opened a document tab")
    };
    let row = leaf.body().focused().expect("matched paragraph");
    assert_eq!(row.id, paragraph);
    assert_eq!(row.caret(), "accented text, CJK, and ".len());
    assert!(row.selection.is_collapsed());
    assert!(leaf.view().search_flash.is_some());
}

// -------------------------------------------------------- the pane's toggle

/// **The hint goes quiet once you act on it.** It explains what the button will
/// do; left up after the press it re-explains a thing you have just watched
/// happen, with its verb flipped, under a pointer that has not moved.
#[test]
fn pressing_the_pane_toggle_mutes_its_tooltip() {
    let (mut v, mut workspace) = container();
    let (x, y) = koru_shell_chrome::caption::sidebar_toggle(v.regions.caption).centre();

    v.mouse_move(x, y);
    assert!(!v.toggle_tip_muted, "pointing at it asks the question");

    v.mouse_down(&mut workspace.ws, x, y, false);
    assert!(v.sidebar.is_open(), "the press did its job");
    assert!(v.toggle_tip_muted, "and the hint stopped answering");
}

/// ...and moving away re-arms it, because pointing at it again is a fresh
/// question and deserves a fresh answer.
#[test]
fn leaving_the_pane_toggle_re_arms_its_tooltip() {
    let (mut v, mut workspace) = container();
    let (x, y) = koru_shell_chrome::caption::sidebar_toggle(v.regions.caption).centre();
    v.mouse_down(&mut workspace.ws, x, y, false);
    assert!(v.toggle_tip_muted);

    // Off into the content, then back.
    v.mouse_move(v.viewport.0 * 0.5, v.viewport.1 * 0.75);
    assert!(!v.toggle_tip_muted);

    let (x2, y2) = koru_shell_chrome::caption::sidebar_toggle(v.regions.caption).centre();
    v.mouse_move(x2, y2);
    assert!(!v.toggle_tip_muted, "still armed while merely hovering");
}

#[test]
fn caption_search_explains_both_the_action_and_its_chord() {
    let (v, _workspace) = container();
    let tip = v
        .caption_tooltip(Hit::Search)
        .expect("search has a tooltip");
    assert_eq!(tip.label, "Search");
    // **Against the registry, not against `"Ctrl+P"`.** The literal was here
    // and in the tooltip both, so the pair agreed with each other and with
    // nothing else — on macOS the chord is ⌘P and this assertion was the reason
    // nobody found out.
    assert_eq!(
        tip.shortcut,
        Some(koru_keys::label_of(koru_keys::ShortcutId::Search))
    );
    assert_eq!(
        tip.anchor,
        koru_shell_chrome::caption::search_button_with_host_action(
            v.drawn_caption(),
            WINDOW_BUTTONS,
            CONTEXT_TOGGLE,
            reminder_bell(),
        )
    );
}

#[test]
fn caption_host_action_is_the_localized_reminder_bell() {
    let (v, _workspace) = container();
    let button = koru_shell_chrome::caption::host_action_with_context(
        v.drawn_caption(),
        WINDOW_BUTTONS,
        CONTEXT_TOGGLE,
        reminder_bell(),
    );
    assert!(button.w > 0.0, "the reminder bell owns the host slot");
    let tip = v
        .caption_tooltip(Hit::HostAction)
        .expect("the reminder bell has a tooltip");
    assert_eq!(tip.anchor, button);
    assert_eq!(
        tip.label,
        koru_i18n::t(crate::format::lang(), koru_i18n::Key::ReminderAttention)
    );
}

#[test]
fn pressing_caption_bell_opens_attention_popover() {
    let (mut v, mut workspace) = container();
    let (x, y) = v.reminder_bell_rect().centre();
    assert!(v.mouse_down(&mut workspace.ws, x, y, false));
    assert!(v.reminder_shell.popover_live());
}

#[test]
fn outside_context_press_dismisses_attention_without_opening_document_menu() {
    let (mut v, mut workspace) = on_document();
    let (x, y) = v.reminder_bell_rect().centre();
    assert!(v.mouse_down(&mut workspace.ws, x, y, false));
    assert!(v.reminder_shell.popover_live());

    assert!(View::context_menu(&mut v, &mut workspace.ws, 400.0, 300.0));
    assert!(!v.reminder_shell.popover_live());
    assert!(
        v.menu.is_none(),
        "the underlying document menu stayed closed"
    );
}

#[test]
fn reminder_hover_change_is_part_of_the_mouse_move_repaint_result() {
    let (mut v, _workspace) = on_document();
    let point = (400.0, 300.0);
    let _ = v.mouse_move_rest(point.0, point.1);
    let bell = v.reminder_bell_rect();
    let (bell_x, bell_y) = bell.centre();
    assert!(
        v.reminder_shell
            .pointer_move(v.viewport, bell, bell_x, bell_y),
        "the isolated reminder hover changed"
    );
    assert!(
        v.mouse_move_rest(point.0, point.1),
        "clearing reminder hover requests a repaint even when the underlying hover is stable"
    );
}

/// **Every caption chip names the chord the registry binds.**
///
/// The property rather than four assertions: a fifth button added with a
/// hand-written `"Ctrl+…"` fails here, which is what the four literals this
/// replaced never could.
#[test]
fn every_caption_tooltip_takes_its_chord_from_the_registry() {
    use koru_keys::ShortcutId;
    let (v, _workspace) = container();
    for (hit, id) in [
        (Hit::ToggleSidebar, ShortcutId::ToggleExplore),
        (Hit::Search, ShortcutId::Search),
        // The historical hit name is the caption `+`; it now opens Add rather
        // than directly creating the Ctrl+T document.
        (Hit::NewTab, ShortcutId::AddContent),
    ] {
        let Some(tip) = v.caption_tooltip(hit) else {
            continue;
        };
        assert_eq!(
            tip.shortcut,
            Some(koru_keys::label_of(id)),
            "{hit:?} teaches a chord the registry does not bind"
        );
    }
}

/// **A list dragged on the rail writes the whole section's order.** The rows
/// are already where the drag left them, so what the container owes the table
/// is every id in that order — one rank per list, dense from zero, rather
/// than one row's new number with the rest left to be guessed at.
#[test]
fn dragging_a_list_on_the_rail_writes_the_sections_order() {
    use crate::panes::explore::rail::{RailList, TasksRailRequest};
    let (mut v, _w) = on_tasks();
    let ids: Vec<koru_core::ids::ListId> = (0..3).map(|_| koru_core::ids::ListId::new()).collect();
    v.explore.rail_mut().set_lists(
        ids.iter()
            .enumerate()
            .map(|(i, id)| RailList {
                id: *id,
                name: format!("list {i}"),
                color: None,
                count: 0,
                overdue: 0,
            })
            .collect(),
    );
    assert!(v.apply_tasks_rail(TasksRailRequest::ReorderList { id: ids[0], to: 2 }));
    let edits = View::take_wants_list_edits(&mut v);
    assert_eq!(
        edits,
        vec![koru_workspace_host::tasks::ListEdit::Reorder(ids.clone())],
        "the rail's own order, every id in it"
    );
}

/// **`Alt+1..3` show the column's three views**, opening the column when it
/// is closed, and each icon's tooltip names its view and teaches that chord
/// from the registry.
#[test]
fn alt_digits_show_the_views_and_the_icons_teach_them() {
    use crate::panes::explore::NavigatorMode;
    let (mut v, mut w) = on_document();
    assert!(!v.sidebar.is_open());
    // The view modifier: Alt, or ⌃⌘ on a Mac (M42).
    let alt = koru_keys::VIEW;
    assert!(v.key(&mut w.ws, &Key::Character("3".into()), Some("3"), alt));
    assert!(v.sidebar.is_open(), "a view asked for by name is opened");
    assert!(v.explore.showing_tasks());
    assert!(v.key(&mut w.ws, &Key::Character("2".into()), Some("2"), alt));
    assert_eq!(v.explore.navigator_mode(), NavigatorMode::Files);
    assert!(v.key(&mut w.ws, &Key::Character("1".into()), Some("1"), alt));
    assert_eq!(v.explore.navigator_mode(), NavigatorMode::Documents);

    // The tooltips wait for the column's slide to end, as the caption's do.
    v.sidebar.advance(Instant::now() + Duration::from_secs(5));
    v.relayout();
    for mode in NavigatorMode::ALL {
        let (x, y) = mode_icon(&v, mode).centre();
        let tip = v
            .navigator_mode_tooltip(x, y)
            .unwrap_or_else(|| panic!("{mode:?} has a tooltip"));
        assert_eq!(tip.label, mode.label());
        assert_eq!(
            tip.shortcut,
            Some(koru_keys::label_of(mode.shortcut())),
            "{mode:?} teaches the chord the registry binds"
        );
        assert_eq!(tip.anchor, mode_icon(&v, mode), "anchored on its own icon");
    }
    assert_eq!(
        koru_keys::label_of(NavigatorMode::Tasks.shortcut()),
        // The registry's label, in the platform's own spelling.
        // ⌃⌘ on a Mac, where ⌥digit types a character (M42).
        if cfg!(target_os = "macos") {
            "⌃⌘3"
        } else {
            "Alt+3"
        }
    );
    // Hovering the icon raises it through the one interaction clock.
    let (x, y) = mode_icon(&v, NavigatorMode::Tasks).centre();
    View::mouse_move(&mut v, x, y);
    assert_eq!(v.tooltip.label(), Some("Tasks"));
}

#[test]
fn the_plus_tooltip_names_add_and_its_own_shortcut() {
    let (v, _workspace) = container();
    let tip = v
        .caption_tooltip(Hit::NewTab)
        .expect("the visible plus has a tooltip");
    assert_eq!(tip.label, "Add…");
    assert_eq!(
        tip.shortcut,
        Some(koru_keys::label_of(koru_keys::ShortcutId::AddContent))
    );
}

#[test]
fn the_document_context_icon_explains_itself() {
    let (mut v, _workspace) = on_document();
    v.size_visible_groups();
    let button = v
        .document_context_button()
        .expect("a document tab offers the page icon");
    let (x, y) = button.rect().centre();
    v.pointer = (x, y);
    let tip = v
        .document_context_tooltip()
        .expect("hovering the icon teaches the chord");
    assert_eq!(tip.label, "Document context");
    assert_eq!(
        tip.shortcut,
        Some(koru_keys::label_of(koru_keys::ShortcutId::ToggleContext))
    );
    assert_eq!(tip.anchor, button.rect());
}

#[test]
fn pressing_a_caption_control_clears_its_tooltip() {
    // Drives the sidebar toggle: the search control left the caption for the
    // Explore header, so this is the caption's remaining hoverable control.
    let (mut v, mut workspace) = container();
    let toggle = koru_shell_chrome::caption::sidebar_toggle(v.drawn_caption());
    let (x, y) = toggle.centre();
    v.mouse_move(x, y);
    assert!(v.tooltip.is_present());

    v.mouse_down(&mut workspace.ws, x, y, false);
    assert!(!v.tooltip.is_present());
}

// ------------------------------------------------- closing the last tab

/// **✕ on the only tab closes the window**, rather than replacing it with a
/// welcome tab you did not ask for. All three ways of saying "close this tab"
/// agree: the box on the tab, `Ctrl+W`, and *Close* in its menu.
#[test]
fn closing_the_last_tab_by_hand_closes_the_window() {
    for gesture in ["box", "chord", "menu"] {
        let (mut v, mut workspace) = container();
        assert_eq!(v.tabs().len(), 1, "a fresh container has one tab");

        match gesture {
            "box" => {
                // Through `tab_rects`, which measures the widths first — the
                // strip scrolls now, so a tab's rect is not a function of the
                // strip and the count alone.
                let close = tab_rects(&mut v)[0].close;
                let (x, y) = close.centre();
                v.mouse_down(&mut workspace.ws, x, y, false);
            }
            "chord" => {
                v.key(&mut workspace.ws, &ch("w"), Some("w"), PRIMARY);
            }
            _ => {
                v.run_menu_action(MenuSubject::Tab(0), MenuAction::Close);
            }
        }
        assert_eq!(
            v.take_window_request(),
            Some(WindowRequest::Close),
            "{gesture} on the last tab"
        );
    }
}

/// ...and with more than one open it is still just a tab closing. The window
/// only goes when the thing you closed was the last of them.
#[test]
fn closing_one_of_several_tabs_leaves_the_window_alone() {
    let (mut v, _workspace) = on_document();
    assert!(v.tabs().len() > 1);
    let before = v.tabs().len();

    v.run_menu_action(MenuSubject::Tab(0), MenuAction::Close);
    assert_eq!(v.tabs().len(), before - 1);
    assert_eq!(v.take_window_request(), None, "the window stays");
}

/// In a split, ✕ on a pane's last tab closes **the pane**, not the window —
/// the window only goes when the pane that went was the last one standing.
#[test]
fn closing_the_last_tab_of_a_pane_closes_the_pane_not_the_window() {
    use crate::mainview::panes::{PanePath, SplitDir};

    let (mut v, _workspace) = on_document();
    // Move the document into a second pane on the right, which takes the focus.
    let notes = v
        .panes
        .focused()
        .tabs()
        .iter()
        .find(|t| !t.is_pinned() && t.label == "Notes")
        .map(|t| t.id)
        .expect("the document tab");
    let notes = v
        .panes
        .take_tab_by_id(&PanePath::root(), notes)
        .expect("took the tab");
    v.panes.split(&PanePath::root(), SplitDir::Right, notes);
    assert_eq!(v.panes.iter_groups().count(), 2, "the split happened");
    assert_eq!(v.tabs().len(), 1, "the new pane holds just the moved tab");

    // ✕ on the right pane's only tab: the pane collapses, the window stays.
    assert!(v.close_requested(v.tabs().active()));
    assert_eq!(
        v.panes.iter_groups().count(),
        1,
        "the pane went with its tab"
    );
    assert_eq!(v.take_window_request(), None, "the window stays");
    assert!(
        v.tabs().iter().any(|t| t.label == "Welcome"),
        "the surviving pane kept its own tabs"
    );

    // And now that one pane is all that stands, the same gesture is the
    // window's again.
    assert!(v.close_requested(v.tabs().active()));
    assert_eq!(v.take_window_request(), Some(WindowRequest::Close));
}

/// *Close All Tabs* means **start again**, not *go away* — so it lands on a
/// fresh welcome tab even though it closed the last tab to get there. This is
/// the case the gesture above must not swallow.
#[test]
fn close_all_still_leaves_a_welcome_tab_rather_than_closing_the_window() {
    let (mut v, _workspace) = on_document();
    v.run_menu_action(MenuSubject::Tab(0), MenuAction::CloseAll);
    assert_eq!(v.tabs().len(), 1);
    assert_eq!(v.tabs().get(0).map(|t| t.label.as_str()), Some("Welcome"));
    assert_eq!(v.take_window_request(), None, "the window stays");
}

// ------------------------------------------------------------ the update banner

/// A report the agent would push on finding a build, with release notes.
fn a_build_is_available() -> koru_agent_wire::UpdateReport {
    koru_agent_wire::UpdateReport {
        stage: koru_agent_wire::UpdateStage::Available,
        version: "0.9.1".into(),
        build_number: 412,
        title: "Faster documents, quieter windows".into(),
        summary: vec!["Opening a large workspace is nine times faster.".into()],
        notes_url: Some("https://koru.do/notes/412".into()),
        received: 0,
        total: 0,
        can_install: true,
        download_url: Some("https://koru.do/download".into()),
        message: None,
        detail: None,
        channel: "nightly".into(),
        automatic_checks: true,
        active_session_count: 0,
        session_generation: 1,
    }
}

/// **The two-step consent, asserted from the button.**
///
/// The banner's primary button names its own step now — *Download update*
/// over a build that has not been downloaded, *Restart and install* over one
/// that has — but the label is only the drawing, so the proof that they are
/// two consents is still what each one sends. On `Available` it must be
/// `Download`. Sending `Install` here would skip the first step, and while the
/// agent refuses that (`wrong_stage`), a banner relying on the far end to
/// catch it is a banner whose own behaviour nobody checked.
///
/// The notes' title rides along, because it is the thing a person is consenting
/// *about* and it has to be on screen before the first press, not at install
/// time.
#[test]
fn the_first_consent_asks_to_download_and_shows_what_it_is_offering() {
    let (mut v, mut workspace) = container();
    let report = a_build_is_available();
    let (feed, sent) = crate::agent::UpdateFeed::for_test(report.clone());
    v.updates = feed;

    let (title, lines) = super::update_ui::banner_copy(&report);
    assert_eq!(title, "Koru 0.9.1 · build 412 is available");
    assert_eq!(
        lines.first().map(String::as_str),
        Some("Faster documents, quieter windows"),
        "the release notes' title leads the banner"
    );

    let (_, _, rect) = v
        .actions(&report)
        .into_iter()
        .find(|(_, label, _)| *label == "Download update")
        .expect("a writable install offers the download button");
    assert!(
        v.mouse_down(&mut workspace.ws, rect.x + 4.0, rect.y + 4.0, false),
        "the banner claims the press"
    );

    let koru_agent_wire::FromShell::Request { body, .. } =
        sent.try_recv().expect("the press sent something");
    assert_eq!(
        body,
        koru_agent_wire::Request::UpdateChoice {
            choice: koru_agent_wire::UpdateChoice::Download
        },
        "the first press downloads; installing is the second consent"
    );
    assert!(sent.try_recv().is_err(), "one press, one message");
}

/// The banner names the build the way the agent's About row does — version,
/// then `· build n` — so the two surfaces agree about what is being offered. A
/// report with no allocated number (a dev build) says only its version.
#[test]
fn the_banner_names_the_offered_build_number() {
    let mut report = a_build_is_available();
    report.stage = koru_agent_wire::UpdateStage::Ready;
    let (title, _) = super::update_ui::banner_copy(&report);
    assert_eq!(title, "Koru 0.9.1 · build 412 is ready");

    report.build_number = 0;
    let (title, _) = super::update_ui::banner_copy(&report);
    assert_eq!(title, "Koru 0.9.1 is ready");
}

/// A click whose consent cannot even be queued is admitted immediately: the
/// banner says the agent is not connected, instead of spending the ten-second
/// timeout pretending the click is in flight. This is the click that used to
/// vanish without a word — the window stayed open, nothing said anything was
/// running, and the button simply turned blue again.
#[test]
fn a_consent_with_no_agent_is_admitted_immediately() {
    let (mut v, mut workspace) = container();
    let mut report = a_build_is_available();
    report.stage = koru_agent_wire::UpdateStage::Ready;
    let (feed, sent) = crate::agent::UpdateFeed::for_test(report.clone());
    // The far end of the queue is gone — the send must report failure.
    drop(sent);
    v.updates = feed;

    let (_, _, rect) = v
        .actions(&report)
        .into_iter()
        .find(|(_, label, _)| *label == "Restart and install")
        .expect("a ready build offers the install button");
    assert!(v.mouse_down(&mut workspace.ws, rect.x + 4.0, rect.y + 4.0, false));

    let (_, lines) = v.banner_text(&report);
    assert_eq!(
        lines.last().map(String::as_str),
        Some(super::update_ui::AGENT_GONE),
        "a dropped click is admitted, not pretended"
    );
    assert!(
        v.update_action_pending.is_none(),
        "nothing is in flight, so nothing waits out the timeout"
    );
}

/// While a consent is in flight the banner says so, and when the agent never
/// answers the re-armed button comes back beside an admission — a button that
/// silently turns blue again reads as a reset, not a failure.
#[test]
fn an_unanswered_consent_is_admitted_when_the_buttons_rearm() {
    let (mut v, mut workspace) = container();
    let mut report = a_build_is_available();
    report.stage = koru_agent_wire::UpdateStage::Ready;
    let (feed, _sent) = crate::agent::UpdateFeed::for_test(report.clone());
    v.updates = feed;

    let (_, _, rect) = v
        .actions(&report)
        .into_iter()
        .find(|(_, label, _)| *label == "Restart and install")
        .expect("a ready build offers the install button");
    assert!(v.mouse_down(&mut workspace.ws, rect.x + 4.0, rect.y + 4.0, false));
    let (_, lines) = v.banner_text(&report);
    assert_eq!(
        lines.last().map(String::as_str),
        Some(super::update_ui::ASKING_AGENT),
        "an in-flight consent is said out loud"
    );

    // The round-trip deadline passes with no answer.
    let expired = std::time::Instant::now() - std::time::Duration::from_secs(11);
    v.update_action_pending = Some((report.stage, expired));
    assert!(v.mouse_move(rect.centre().0, rect.centre().1));

    let (_, lines) = v.banner_text(&report);
    assert_eq!(
        lines.last().map(String::as_str),
        Some(super::update_ui::AGENT_SILENT),
        "the silence is admitted where the button re-arms"
    );
    assert!(v.update_action_pending.is_none(), "the buttons re-arm");
}

/// The primary button over a build that is on the disk and verified is the
/// *second* consent — and this is the half that would silently still work if
/// somebody collapsed the two stages into one.
#[test]
fn the_second_consent_asks_to_install() {
    let (mut v, mut workspace) = container();
    let mut report = a_build_is_available();
    report.stage = koru_agent_wire::UpdateStage::Ready;
    let (feed, sent) = crate::agent::UpdateFeed::for_test(report.clone());
    v.updates = feed;

    let (_, _, rect) = v
        .actions(&report)
        .into_iter()
        .find(|(_, label, _)| *label == "Restart and install")
        .expect("a ready build offers the install button");
    v.mouse_down(&mut workspace.ws, rect.x + 4.0, rect.y + 4.0, false);

    let koru_agent_wire::FromShell::Request { body, .. } =
        sent.try_recv().expect("the press sent something");
    assert_eq!(
        body,
        koru_agent_wire::Request::UpdateChoice {
            choice: koru_agent_wire::UpdateChoice::Install {
                session_generation: 1
            }
        }
    );
    // The agent updates on another thread. Until its `Installing` report
    // reaches this window, the banner must not allow a second click to queue a
    // second install request.
    assert!(v.mouse_down(&mut workspace.ws, rect.x + 4.0, rect.y + 4.0, false));
    assert!(
        sent.try_recv().is_err(),
        "the local pending state swallows repeat install clicks"
    );
}

/// The banner floats over the active view, so it must own the pointer just as
/// its press handler does. Otherwise an editor below it keeps its I-beam and
/// reacts to hover/scroll gestures that are visibly aimed at the update UI.
#[test]
fn update_banner_owns_pointer_and_wheel_input() {
    let (mut v, _workspace) = on_document();
    let mut report = a_build_is_available();
    report.stage = koru_agent_wire::UpdateStage::Ready;
    let (feed, _sent) = crate::agent::UpdateFeed::for_test(report.clone());
    v.updates = feed;
    let (_, _, action) = v.actions(&report)[0];
    // The gap left of the first action is safely inside the banner, but is not
    // itself a button; it must still block the editor underneath.
    let point = (action.x - 4.0, action.centre().1);

    assert!(View::mouse_move(&mut v, point.0, point.1));
    assert_eq!(View::cursor_icon(&v), winit::window::CursorIcon::Default);

    // Scrolling a document under the panel would move it even though the
    // pointer never left the banner. The call is intentionally just a safety
    // assertion: the visible surface claims the wheel and no content handler
    // is reached.
    View::scroll_by(&mut v, winit::event::MouseScrollDelta::LineDelta(0.0, -1.0));
}

#[test]
fn reminder_management_line_wheel_is_scaled_once_by_main_view() {
    let (mut v, _workspace) = on_document();
    let node = NodeId::new();
    v.viewport = (900.0, 320.0);
    v.reminder_creation
        .open_management(node, Rect::new(20.0, 20.0, 1.0, 1.0));
    let mut management = koru_editor::reminders::Management::loading("target".to_owned());
    management.replace(
        (0..24)
            .map(|index| koru_editor::reminders::Item {
                id: format!("id-{index}"),
                revision: format!("revision-{index}"),
                occurrence: None,
                draft: koru_editor::reminders::Draft::new(
                    koru_editor::utils::when::ReminderDraft::Floating(
                        koru_core::time::FloatingDateTime::new(
                            chrono::NaiveDate::from_ymd_opt(2026, 9, 3)
                                .unwrap()
                                .and_hms_opt(9, 0, 0)
                                .unwrap(),
                        ),
                    ),
                ),
                state: koru_editor::reminders::ItemState::Active,
            })
            .collect(),
        Vec::new(),
    );
    v.reminders.management = Some(management);

    View::scroll_by(&mut v, MouseScrollDelta::LineDelta(0.0, -1.0));
    assert_eq!(
        v.reminder_creation.management_scroll_for_test(),
        theme::NAV_ROW_H
    );
}

/// **No agent is not an error.** A window that could not reach one draws no
/// banner at all rather than an empty one or a failure — the same degradation
/// the module note describes for the global shortcut.
#[test]
fn a_window_with_no_agent_draws_no_update_banner() {
    let (v, _workspace) = container();
    assert_eq!(v.update_banner_top(), None);
    assert!(!v.update_newer_modal_open());
}

// ------------------------------------------------------- what's new

/// It opens, and it is a real page rather than an empty one.
#[test]
fn the_release_notes_open_as_a_page() {
    let (mut v, _w) = on_document();
    assert!(v.open_whats_new("0.0.4"));

    let tab = v.tabs().active_tab();
    assert!(tab.whats_new.is_some(), "it owns a private workspace");
    assert!(
        tab.draft.is_none(),
        "and is not a draft — nothing promotes it"
    );
    assert!(
        tab.document.is_none(),
        "it is not a document in this workspace"
    );
    assert!(tab.label.contains("0.0.4"), "the tab names the build");

    // Title, then the confirmation, then whatever the build shipped. Row 0 is
    // the document's own name — `Body::title_row` prepends the root node — and
    // `paste_rich` appends the blocks under it in order, so the greeting is the
    // first thing read. Pinned because "an update happened" is the part that
    // must not end up below a screen of release notes.
    let TabView::Leaf(leaf) = &tab.view else {
        panic!("the notes are a leaf")
    };
    let rows: Vec<&str> = leaf.body().rows.iter().map(|r| r.text.as_str()).collect();
    assert!(rows[0].contains("What's new in Koru 0.0.4"), "{rows:?}");
    assert!(rows[1].starts_with("Updated to Koru 0.0.4"), "{rows:?}");
}

/// **The assertion this whole change rests on.**
///
/// A notes tab is a real editor over a real Loro document, so "read-only" is
/// something the code has to mean rather than something the situation provides.
/// Typing must change nothing, and must not quietly create a page in the
/// person's library on the way — see `MainView::promote_draft`, which is why
/// this is a separate field from `draft`.
#[test]
fn typing_into_the_release_notes_changes_nothing() {
    let (mut v, mut w) = on_document();
    let before = w.ws.walk_all().expect("walk").len();
    assert!(v.open_whats_new("0.0.4"));

    let rows_before = match &v.tabs().active_tab().view {
        TabView::Leaf(l) => l
            .body()
            .rows
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>(),
        _ => panic!("the notes are a leaf"),
    };
    assert!(rows_before.len() > 1, "there is something there to protect");

    for (key, text) in [
        (ch("x"), Some("x")),
        (Key::Named(winit::keyboard::NamedKey::Enter), None),
        (Key::Named(winit::keyboard::NamedKey::Backspace), None),
        (Key::Named(winit::keyboard::NamedKey::Delete), None),
    ] {
        v.key(&mut w.ws, &key, text, ModifiersState::empty());
    }

    let rows_after = match &v.tabs().active_tab().view {
        TabView::Leaf(l) => l
            .body()
            .rows
            .iter()
            .map(|r| r.text.clone())
            .collect::<Vec<_>>(),
        _ => panic!("the notes are a leaf"),
    };
    assert_eq!(rows_before, rows_after, "not one keystroke landed");
    assert_eq!(
        w.ws.walk_all().expect("walk").len(),
        before,
        "and nothing was created in the person's workspace"
    );
}

#[test]
fn editable_document_keeps_tab_and_f6_reaches_the_reminder_bell() {
    let (mut v, mut w) = on_document();
    assert!(v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Tab),
        None,
        ModifiersState::empty(),
    ));
    assert!(!v.reminder_shell.bell_is_focused());

    assert!(v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::F6),
        None,
        ModifiersState::empty(),
    ));
    assert!(v.reminder_shell.bell_is_focused());
    assert!(v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        ModifiersState::empty(),
    ));
    assert!(v.reminder_shell.popover_live());
}

/// One build, one page. The automatic path cannot ask twice — the agent forgets
/// the notice once a window has been told — but a menu row added later could.
#[test]
fn asking_twice_brings_the_same_page_forward() {
    let (mut v, _w) = on_document();
    assert!(v.open_whats_new("0.0.4"));
    let tabs = v.tabs().len();
    assert!(v.open_whats_new("0.0.4"));
    assert_eq!(v.tabs().len(), tabs, "the second ask reuses the first tab");
    assert!(v.tabs().active_tab().whats_new.is_some(), "and selects it");
}

/// The private workspace is the structural half of read-only: the container
/// physically cannot hand this view the window's document, so nothing typed
/// here could reach the person's content even if a key did get through.
#[test]
fn the_release_notes_are_talked_to_through_their_own_workspace() {
    let (mut v, mut w) = on_document();
    assert!(v.open_whats_new("0.0.4"));

    let window_doc = std::ptr::from_ref(&w.ws);
    let tab = v.tabs_mut().active_mut();
    let (_, doc) = tab.doc(&mut w.ws);
    assert!(
        !std::ptr::eq(std::ptr::from_ref(doc), window_doc),
        "a notes tab is never handed the window's workspace"
    );
}

/// Moving a selection to a new page is a jump like the @-picker's "Add new
/// page": the shell lands the window on the created document, and a
/// shell-announced landing records nothing — so the step Back returns to must
/// be recorded at the gesture. Moving to an *existing* document never leaves
/// the source, so that arm records nothing.
#[test]
fn moving_a_selection_to_a_new_page_records_the_source_in_history() {
    let (mut v, mut w) = container();
    let a = a_document(&mut w.ws, "Source");
    v.open_node(&w.ws, a);
    let before = v.history.len();

    v.request_selection_move(
        koru_editor::view_state::MoveSelectionDestination::NewPage {
            title: "Fresh".into(),
            follow: true,
            child: false,
        },
        koru_editor::view_state::MoveSelectionPayload::Rows(vec![a]),
    );
    assert_eq!(
        v.history.len(),
        before + 1,
        "the source spot is the step Back returns to"
    );
    assert_eq!(v.history.last().map(|spot| spot.doc), Some(a));

    v.request_selection_move(
        koru_editor::view_state::MoveSelectionDestination::Existing {
            target: koru_editor::trigger::DocumentTarget {
                document: DocumentId::new(),
                node: None,
            },
            title: "Elsewhere".into(),
        },
        koru_editor::view_state::MoveSelectionPayload::Rows(vec![a]),
    );
    assert_eq!(
        v.history.len(),
        before + 1,
        "a move that stays on the source is not a jump"
    );
}

/// The move picker's empty query lists the window's own recents — most recent
/// first, without the document the picker is open over.
#[test]
fn recent_document_choices_skip_the_open_document() {
    let (mut v, _w) = container();
    let here = DocumentId::new();
    let elsewhere = DocumentId::new();
    v.recent_documents.push((here, "Here".into()));
    v.recent_documents.push((elsewhere, "Elsewhere".into()));
    v.active_document = Some(here);

    let choices = v.recent_document_choices();
    assert_eq!(choices.len(), 1);
    assert_eq!(choices[0].title, "Elsewhere");
    assert_eq!(choices[0].target.document, elsewhere);
}

/// **The branch pill takes its room from the tabs, the way the offline chip
/// does.** Same hazard, same fix: it is drawn over the caption, so the only
/// thing keeping a tab from being clickable underneath it is the strip being
/// narrower while it is up.
///
/// Driven through `branch_pill_for` rather than `branch_pill`, because the real
/// one reads a stamp fixed when this binary compiled — on `dev` there is no
/// pill, and a test that only ran on somebody's feature branch would be a test
/// that never ran.
#[test]
fn the_branch_pill_narrows_the_tab_strip_rather_than_covering_it() {
    let (v, _workspace) = container();
    let full = v.branch_pill_for(None, 0.0);
    assert!(full.is_none(), "no pill on dev, main, or a released build");

    let pill = v
        .branch_pill_for(Some("import-google-docs"), 120.0)
        .expect("a pill on a branch worth naming");
    assert_eq!(pill.w, 120.0);
    // The strip itself is measured against the *real* stamp, so this asserts
    // the arithmetic rather than the current branch: the pill is placed inside
    // the same band `caption_strip` subtracts from.
    let band = v.caption_strip();
    assert!(
        band.x + band.w <= pill.x + pill.w + 0.5,
        "the pill escaped the caption band"
    );
    let bell = v.reminder_bell_rect();
    assert!(
        pill.x + pill.w <= bell.x,
        "the branch pill overlapped the reminder bell"
    );
}

/// **An unmeasured pill is not reserved.** `branch_pill_w` is `0.0` until the
/// tab widths are first measured, and a pill of no width parked at the right of
/// the strip would take the tabs' room without drawing anything.
#[test]
fn a_pill_that_has_not_been_measured_yet_takes_no_room() {
    let (v, _workspace) = container();
    assert!(v.branch_pill_for(Some("kimi"), 0.0).is_none());
    assert!(v.branch_pill_for(Some("kimi"), -1.0).is_none());
    assert!(v.branch_pill_for(Some("kimi"), 90.0).is_some());
}

/// **The pill sits left of the offline chip, and the tabs stop before both.**
///
/// The chip is the one that says something is wrong, so it keeps the end of the
/// band; a pill that changed position depending on whether the store happened
/// to be reachable would be worse than either order.
#[test]
fn the_offline_chip_keeps_the_end_of_the_band_and_the_pill_sits_inside_it() {
    let (mut v, _workspace) = container();
    let alone = v
        .branch_pill_for(Some("kimi"), 90.0)
        .expect("a pill with the store up");

    assert!(View::set_store_trouble(
        &mut v,
        Some(koru_workspace::StoreTrouble {
            message: "the store went away".to_owned(),
            owed: 1,
            since: Some(std::time::Instant::now()),
            fatal: false,
            unconfirmed: koru_workspace::Unconfirmed::default(),
        })
    ));
    let chip = v.offline_chip().expect("a chip while offline");
    let beside = v
        .branch_pill_for(Some("kimi"), 90.0)
        .expect("a pill while offline too");

    assert!(
        beside.x + beside.w <= chip.x,
        "the pill runs under the offline chip"
    );
    assert!(
        beside.x < alone.x,
        "the pill did not move aside for the chip: {} -> {}",
        alone.x,
        beside.x
    );
}

// ------------------------------------------------------------ task activity

/// **A checkbox click reaches the desktop wrapper, through the whole
/// container** ([ADR-286], B-Gate).
#[test]
fn reminder_numeric_date_order_uses_the_raw_locale_region() {
    use koru_editor::utils::when::{DateOrder, Language};

    for (tag, language, order) in [
        ("en-US", Language::English, DateOrder::MonthFirst),
        ("en-PH", Language::English, DateOrder::MonthFirst),
        ("en-GB", Language::English, DateOrder::DayFirst),
        ("en-AU", Language::English, DateOrder::DayFirst),
        ("en-CA", Language::English, DateOrder::YearFirst),
        ("es-ES", Language::Spanish, DateOrder::DayFirst),
        ("pt-BR", Language::PortugueseBrazil, DateOrder::DayFirst),
        ("pt-PT", Language::PortugueseBrazil, DateOrder::DayFirst),
        ("de-DE", Language::German, DateOrder::DayFirst),
        ("fr-FR", Language::French, DateOrder::DayFirst),
        ("fr-CA", Language::French, DateOrder::YearFirst),
        ("ar-EG", Language::Arabic, DateOrder::DayFirst),
        ("zh-CN", Language::ChineseSimplified, DateOrder::YearFirst),
        ("ja-JP", Language::Japanese, DateOrder::YearFirst),
    ] {
        let locale = koru_i18n::time::LocaleFormat::for_system(Some(tag), None);
        assert_eq!(reminder_language(locale.lang()), language, "{tag}");
        assert_eq!(reminder_date_order(&locale), order, "{tag}");
    }
    let fallback = koru_i18n::time::LocaleFormat::for_system(None, None);
    assert_eq!(fallback.lang(), koru_i18n::Lang::En);
    assert_eq!(reminder_date_order(&fallback), DateOrder::MonthFirst);
}

#[test]
fn finishing_onboarding_selects_welcome_over_manage() {
    let mut view = MainView::new();
    assert!(view.open_manage(crate::manage::Page::NewProfile));
    assert!(matches!(
        view.tabs().get(view.tabs().active()).expect("active").view,
        TabView::Manage(_)
    ));

    assert!(view.show_first_minutes("Ana", "Personal", "Research"));
    let active = view.tabs().get(view.tabs().active()).expect("active");
    assert!(matches!(active.view, TabView::Welcome(_)));
    assert_eq!(active.label, "Welcome");
    assert!(
        view.tabs()
            .iter()
            .all(|tab| !matches!(tab.view, TabView::Manage(_))),
        "the creation-only utility tab remained open"
    );
}

#[test]
fn signed_in_lifecycle_reconciliation_is_requested_once_without_manage_profiles() {
    let mut view = MainView::new();
    let state = koru_agent_wire::AccountState {
        stage: koru_agent_wire::AccountStage::SignedIn,
        generation: 9,
        active_scope: 41,
        ..Default::default()
    };

    assert!(View::set_account_state(&mut view, state.clone()));
    assert!(matches!(
        View::take_pane_request(&mut view),
        Some(PaneRequest::AccountOwned {
            owner_scope: 41,
            command: koru_agent_wire::AccountCommand::ReconcileProfileLifecycles
        })
    ));
    View::set_account_state(&mut view, state);
    assert!(View::take_pane_request(&mut view).is_none());
}

#[test]
fn a_deleted_local_profile_opens_the_blocking_lifecycle_decision() {
    let mut view = MainView::new();
    let profile = ProfileId::new();
    View::set_pane_context(
        &mut view,
        vec![ProfileSummary {
            id: profile,
            name: "Work".into(),
            email: String::new(),
            avatar: None,
            icon: None,
            color: None,
            account: Some("koru:00000000-0000-4000-8000-000000000041".into()),
            folder: String::new(),
        }],
        profile,
        Vec::new(),
        None,
    );

    assert!(View::account_result(
        &mut view,
        9,
        koru_agent_wire::AccountOperationResult::ProfileLifecycleReconciliation {
            profiles: vec![koru_agent_wire::AccountProfileLifecycleRow {
                profile_id: profile.to_storage(),
                lifecycle_state: koru_agent_wire::AccountProfileLifecycleState::Deleted,
                lifecycle_revision: 1,
                operation_id: Some("00000000-0000-4000-8000-000000000042".into()),
                cleanup_stage: Some(koru_agent_wire::ProfileCleanupStage::HostedCommitted),
                failure_code: None,
            }],
        },
    ));
    // The Manage tab, on that Profile's page, where the choice is.
    match &view.tabs().active_tab().view {
        TabView::Manage(manage) => {
            assert_eq!(manage.page(), Some(crate::manage::Page::Profile(profile)));
            assert_eq!(
                manage.controller().discovered_deletion().map(|(id, _)| id),
                Some(profile)
            );
        }
        _ => panic!("a deletion found elsewhere opens Manage"),
    }
}

#[test]
fn canadian_reminder_fields_round_trip_through_year_first_resolution() {
    use koru_core::time::{FloatingDateTime, TimeZoneName};
    use koru_editor::utils::when::{ReminderDraft, Resolution, ResolutionContext, parse_reminder};

    let expected = ReminderDraft::Floating(FloatingDateTime::new(
        chrono::NaiveDate::from_ymd_opt(2026, 9, 4)
            .unwrap()
            .and_hms_opt(9, 0, 0)
            .unwrap(),
    ));
    for tag in ["en-CA", "fr-CA"] {
        let locale = koru_i18n::time::LocaleFormat::for_system(Some(tag), None);
        let context = ResolutionContext {
            now_ms: chrono::NaiveDate::from_ymd_opt(2026, 8, 26)
                .unwrap()
                .and_hms_opt(10, 15, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis(),
            language: reminder_language(locale.lang()),
            date_order: reminder_date_order(&locale),
            current_zone: TimeZoneName::utc(),
            selection: None,
            end_of_day: None,
            due_anchor: None,
            system_week_start: koru_editor::utils::when::WeekStart::Monday,
            week_start_override: None,
        };
        assert_eq!(
            parse_reminder("2026-09-04", &context),
            Resolution::Resolved(expected.clone()),
            "{tag}"
        );
        assert_eq!(
            parse_reminder("09/04", &context),
            Resolution::Needs(koru_editor::utils::when::ResolutionIssue::Unsupported),
            "{tag} must not guess a US reading for an incomplete numeric date"
        );
    }
}

#[test]
fn reminder_submission_is_applied_by_maintenance_without_more_input() {
    let (mut v, mut workspace) = on_document();
    let (identity, node) = {
        let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view else {
            panic!("a document leaf");
        };
        let node = leaf.body().focused().unwrap().id;
        workspace.ws.set_text(node, "/remind tomorrow").unwrap();
        let identity = koru_editor::reminders::RequestIdentity {
            token: koru_editor::reminders::RequestToken::new(),
            node,
        };
        leaf.view_mut().pending_reminder_text.insert(
            identity.token,
            koru_editor::reminders::PendingText {
                identity,
                text: "/remind tomorrow".into(),
                range: workspace
                    .ws
                    .capture_text_range(node, 0, "/remind tomorrow".len())
                    .unwrap(),
                origin: koru_editor::reminders::RequestOrigin::PersonAuthored,
            },
        );
        (identity, node)
    };
    let document = koru_core::DocumentId::new();
    v.active_document = Some(document);
    let (_, profile, _, workspace_id) = v.pane_context.clone().unwrap();
    let target = reminders::Target {
        profile: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
        entity: koru_store_core::EntityRef::new(
            "block",
            koru_store_core::WorkspaceId::from_uuid(
                workspace_id.expect("fixture workspace").as_uuid(),
            ),
            koru_store_core::DocumentId::from_uuid(document.as_uuid()),
            node.as_uuid(),
        )
        .unwrap(),
    };
    v.pending_reminder_submissions.push_back((target, identity));

    assert!(View::maintain(
        &mut v,
        &mut workspace.ws,
        std::time::Instant::now(),
    ));
    assert_eq!(workspace.ws.read(node).unwrap().content.as_text(), Some(""));
    assert!(v.pending_reminder_submissions.is_empty());
}

#[test]
fn reminders_require_a_durable_workspace_in_the_pane_context() {
    let (mut view, _) = on_document();
    // The editor fixture has a document node; the shell separately supplies
    // its durable document identity when the Store open completes.
    let document = koru_core::DocumentId::new();
    view.active_document = Some(document);
    let node = view.document_root().expect("document fixture");
    let target = view.reminder_target(node).expect("durable reminder target");
    assert_eq!(target.entity.document().as_uuid(), document.as_uuid());
    view.pane_context.as_mut().expect("pane fixture").3 = None;
    assert!(view.reminder_target(node).is_none());
    assert!(!view.ensure_reminder_profile());
}

#[test]
fn reminder_submission_publishes_its_inline_chip_before_projection_catches_up() {
    let (mut v, workspace) = on_document();
    let node = match &v.tabs().active_tab().view {
        TabView::Leaf(leaf) => leaf.body().focused().unwrap().id,
        _ => panic!("a document leaf"),
    };
    let document = koru_core::DocumentId::new();
    v.active_document = Some(document);
    let target = v.reminder_target(node).expect("active reminder target");
    let context = v.reminder_context(&workspace.ws, node);
    let identity = koru_editor::reminders::RequestIdentity {
        token: koru_editor::reminders::RequestToken::new(),
        node,
    };
    v.reminders.open(
        target,
        koru_editor::reminders::Request {
            identity,
            phrase: "in 1 min".into(),
            due_anchor: None,
        },
        context,
    );
    assert!(
        v.settle_reminder_creation(
            &workspace.ws,
            reminder_creation::Outcome {
                handled: true,
                action: Some(reminder_creation::Action::Confirm(
                    koru_editor::reminders::Draft::new(
                        koru_editor::utils::when::ReminderDraft::Floating(
                            koru_core::time::FloatingDateTime::new(
                                chrono::NaiveDate::from_ymd_opt(2026, 9, 6)
                                    .unwrap()
                                    .and_hms_opt(0, 29, 0)
                                    .unwrap(),
                            ),
                        ),
                    ),
                )),
            },
        )
    );
    let reminder_id = v
        .reminders
        .optimistic_creations()
        .next()
        .expect("optimistic creation")
        .reminder_id
        .clone();

    assert!(
        v.reminder_anchor(node, Some(&reminder_id)).is_some(),
        "the Set interaction must publish the normal inline chip without waiting for the Store projection"
    );
}

#[test]
fn reminder_management_closes_when_the_active_document_changes() {
    let (mut v, mut workspace) = on_document();
    v.active_document = Some(DocumentId::new());
    let node = match &v.tabs().active_tab().view {
        TabView::Leaf(leaf) => leaf.body().focused().unwrap().id,
        _ => panic!("a document leaf"),
    };
    let target = v.reminder_target(node).expect("active reminder target");
    let context = v.reminder_context(&workspace.ws, node);
    v.reminders.open_management(target, context);
    v.reminder_creation
        .open_management(node, Rect::new(20.0, 20.0, 1.0, 1.0));
    assert!(v.reminder_creation.is_management());

    v.active_document = Some(DocumentId::new());
    View::maintain(&mut v, &mut workspace.ws, Instant::now());
    assert!(!v.reminder_creation.is_open());
    assert!(v.reminders.management.is_none());
}

#[test]
fn right_clicking_content_replaces_an_open_reminder_surface() {
    let (mut v, mut workspace) = on_document();
    v.active_document = Some(DocumentId::new());
    let node = match &v.tabs().active_tab().view {
        TabView::Leaf(leaf) => leaf.body().focused().unwrap().id,
        _ => unreachable!(),
    };
    let reminder_id = String::default();
    let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view else {
        unreachable!()
    };
    assert!(leaf.set_reminder_chips(std::collections::HashMap::from([(
        node,
        vec![koru_editor::reminders::InlineChip {
            reminder_id: reminder_id.clone(),
            label: String::default(),
            paused: false,
            snoozed: false,
            attention: false,
            handled: false,
        }],
    )])));
    let target = v.reminder_target(node).unwrap();
    let context = v.reminder_context(&workspace.ws, node);
    v.reminders.open_management(target, context);
    v.reminder_creation
        .open_management(node, Rect::new(20.0, 20.0, 1.0, 1.0));
    let point = v
        .reminder_anchor(node, Some(&reminder_id))
        .unwrap()
        .centre();

    assert!(View::context_menu(
        &mut v,
        &mut workspace.ws,
        point.0,
        point.1,
    ));
    assert!(!v.reminder_creation.is_open());
    assert!(v.reminders.management.is_none());
    assert!(v.menu.is_none());
}

#[test]
fn global_reminder_handoff_requires_exact_target_and_published_anchor() {
    let (mut v, _workspace) = on_document();
    let document = DocumentId::new();
    v.active_document = Some(document);
    let node = match &v.tabs().active_tab().view {
        TabView::Leaf(leaf) => leaf.body().focused().unwrap().id,
        _ => panic!("a document leaf"),
    };
    let exact = v.reminder_target(node).expect("active reminder target");
    assert!(v.global_reminder_landing(&exact).is_some());

    let wrong_kind = reminders::Target {
        profile: exact.profile,
        entity: koru_store_core::EntityRef::new(
            "document",
            exact.entity.workspace(),
            exact.entity.document(),
            exact.entity.node(),
        )
        .unwrap(),
    };
    assert!(v.global_reminder_landing(&wrong_kind).is_none());

    let missing_node = NodeId::new();
    let no_anchor = reminders::Target {
        profile: exact.profile,
        entity: koru_store_core::EntityRef::new(
            "block",
            exact.entity.workspace(),
            exact.entity.document(),
            missing_node.as_uuid(),
        )
        .unwrap(),
    };
    assert!(v.global_reminder_landing(&no_anchor).is_none());
}

/// **A Settings tab opened an hour in shows what is stored, not the defaults.**
///
/// The editor never emits: a command answers typed intents and leaves them on
/// the body, and the host drains them *after* dispatch. Everything between
/// those two halves is wiring, and this is the test of the wiring — the click
/// goes in at the pane's own view (`ManagerView`, which is what the shell
/// actually holds), and what comes out is a wrapper counter that moved.
///
/// It is driven through `ManagerView` rather than `MainView` on purpose: the
/// drain hangs off the outer enum so a popped-out document is covered by the
/// same line, and a test that went in one level down would pass with that line
/// deleted.
///
/// The pixel→`set_done` half is `koru-editor`'s and is pinned there; what is
/// new here is that the intents leave the view at all.
///
/// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
#[test]
fn a_checkbox_completion_reaches_the_task_wrapper_through_the_container() {
    use koru_core::time::{DateOnly, Temporal};
    use koru_draw::{geometry, theme};

    let (mut v, mut workspace) = on_document();
    let doc = v.document_root().expect("a document tab is showing");
    let item = workspace
        .ws
        .insert(Parent::Node(doc), Node::task("ship it").unwrap())
        .expect("insert");
    // A due date is one of the two ways a task is created, and the one a test
    // can write without a marker constant.
    koru_core::tasks::set_due(
        &mut workspace.ws,
        item,
        Temporal::Date(DateOnly::parse("2026-09-01").unwrap()),
    )
    .expect("due");
    v.adopt(&workspace.ws);

    // The row's own geometry, read back rather than guessed: the checkbox sits
    // in the affordance lane at the row's depth, and the box is centred on the
    // first line.
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("a document tab holds a leaf")
    };
    let row = leaf
        .body()
        .rows
        .iter()
        .find(|r| r.id == item)
        .expect("the checklist item has a row");
    assert!(row.is_task(), "and it draws a checkbox");
    let view = v.focused_view_rect();
    let x = view.x + geometry::checkbox_x(row.depth) + theme::CHECKBOX_SIZE / 2.0;
    let y = view.y + row.y + theme::ROW_H / 2.0;

    // Nothing is owed before the click. `set_due` above ran outside a view, so
    // its own intent never reached a body.
    assert!(v.take_task_activity().is_empty());
    let before = koru_workspace_host::tasks::counters();

    let mut pane = crate::manager_view::ManagerView::Container(v);
    assert!(
        View::mouse_down(&mut pane, &mut workspace.ws, x, y, false),
        "the click landed"
    );

    assert_eq!(
        koru_core::tasks::TaskFields::of(&workspace.ws.read(item).unwrap()).lifecycle,
        koru_core::attributes::Lifecycle::Completed,
        "the box ticked, through the canonical command"
    );
    let after = koru_workspace_host::tasks::counters();
    assert!(
        after.attempted > before.attempted,
        "the wrapper attempted the completion's activity: {before:?} -> {after:?}"
    );
    assert_eq!(
        after.dropped, before.dropped,
        "and named it rather than dropping it"
    );

    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    assert!(
        v.take_task_activity().is_empty(),
        "the drain left nothing behind for a second host to attempt twice"
    );
}

#[test]
fn missing_end_of_day_action_opens_settings_at_the_focused_control() {
    let (mut view, workspace) = on_document();
    let profile = koru_store_core::ids::ProfileId::new();
    view.reminder_preferences.select_profile(profile);
    let generation = view.reminder_preferences.model.generation();
    view.reminder_preferences.model.settings_loaded(
        profile,
        generation,
        Ok(koru_store_core::reminders::ReminderSettings::default()),
    );

    assert!(view.settle_reminder_creation(
        &workspace.ws,
        reminder_creation::Outcome {
            handled: true,
            action: Some(reminder_creation::Action::OpenEndOfDaySettings),
        },
    ));

    let TabView::Settings(settings) = &view.tabs().active_tab().view else {
        panic!();
    };
    assert!(settings.reminder_end_of_day_focused());
    assert!(settings.end_of_day_on_screen());
}

/// **A task changed in a document refreshes the Tasks page on that gesture.**
///
/// Three surfaces read the same facts: the rail's numbers, the context column's
/// section, and the Tasks page itself. Only the first two were told when an
/// editor ran a canonical command, and the page was left on its own half-minute
/// deadline — so typing `/task` in a document moved the number beside a queue
/// while the queue itself went on saying it was empty. Two readings of one
/// workspace, disagreeing on screen, for up to thirty seconds.
///
/// The click goes in at `ManagerView`, the same door as
/// `a_checkbox_completion_reaches_the_task_wrapper_through_the_container`,
/// because the drain hangs off the outer enum: a test one level down would pass
/// with the wiring deleted. What is asserted is the *page*, which is the
/// surface that was being missed.
#[test]
fn a_task_changed_in_a_document_refreshes_the_tasks_page_at_once() {
    use koru_core::time::{DateOnly, Temporal};
    use koru_draw::{geometry, theme};

    // A window with both surfaces open: a document tab, and a Tasks tab whose
    // page has already been caught up.
    let (mut v, mut workspace, _, _) = on_a_task_row(Vec::new());
    let leaf = v
        .tabs()
        .position(|tab| matches!(tab.view, TabView::Leaf(_)))
        .expect("the document this window opened on");
    let tasks = v
        .tabs()
        .position(|tab| matches!(tab.view, TabView::Tasks(_)))
        .expect("and the Tasks tab the rail opened");
    assert!(
        v.tabs_mut().select(leaf),
        "the document comes forward to be typed in"
    );

    let doc = v.document_root().expect("a document tab is showing");
    let item = workspace
        .ws
        .insert(Parent::Node(doc), Node::task("ship it").unwrap())
        .expect("insert");
    koru_core::tasks::set_due(
        &mut workspace.ws,
        item,
        Temporal::Date(DateOnly::parse("2026-09-01").unwrap()),
    )
    .expect("due");
    v.adopt(&workspace.ws);

    // The page is current before the gesture, so a stale flag afterwards can
    // only have come from the gesture.
    let Some(Tab {
        view: TabView::Tasks(page),
        ..
    }) = v.tabs_mut().get_mut(tasks)
    else {
        panic!("the Tasks tab is still there")
    };
    page.catch_up(&workspace.ws);
    assert!(
        !page.catch_up(&workspace.ws),
        "a second catch-up reads nothing: the page is current"
    );
    v.context_tasks_stale = false;

    let TabView::Leaf(leaf_view) = &v.tabs().active_tab().view else {
        panic!("a document tab holds a leaf")
    };
    let row = leaf_view
        .body()
        .rows
        .iter()
        .find(|r| r.id == item)
        .expect("the task has a row");
    let view = v.focused_view_rect();
    let x = view.x + geometry::checkbox_x(row.depth) + theme::CHECKBOX_SIZE / 2.0;
    let y = view.y + row.y + theme::ROW_H / 2.0;

    let mut pane = crate::manager_view::ManagerView::Container(v);
    assert!(
        koru_workspace_host::View::mouse_down(&mut pane, &mut workspace.ws, x, y, false),
        "the click landed on the checkbox"
    );
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };

    assert!(
        v.context_tasks_stale,
        "the column's section is owed a rebuild, as it always was"
    );
    let Some(Tab {
        view: TabView::Tasks(page),
        ..
    }) = v.tabs_mut().get_mut(tasks)
    else {
        panic!("the Tasks tab is still there")
    };
    assert!(
        page.catch_up(&workspace.ws),
        "and so is the page — which is the surface this was missing"
    );
}

/// **The tab is named for the page it is showing, not for the feature.**
///
/// One Tasks tab serves five queues and every list in turn — that is the rail's
/// one-selection rule, applied to the strip — so the word *Tasks* on it said
/// nothing about where you were. The strip is also the only place a window in
/// the background answers *where was I*, which is what makes this the label
/// rather than a subtitle somewhere on the page.
#[test]
fn the_tasks_tab_is_named_for_its_target() {
    use crate::panes::explore::rail::{Bucket, RailRow};
    let (mut v, mut w) = on_tasks();
    for (row, want) in [
        (RailRow::Bucket(Bucket::Today), "Today"),
        (RailRow::Bucket(Bucket::Next), "Next"),
        (RailRow::Bucket(Bucket::Upcoming), "Upcoming"),
        (RailRow::Bucket(Bucket::Later), "Later"),
        (RailRow::Bucket(Bucket::Completed), "Completed"),
    ] {
        let (x, y) = rail_row(&v, row).centre();
        v.mouse_down(&mut w.ws, x, y, false);
        assert_eq!(
            v.tabs().active_tab().label,
            want,
            "the strip says which queue, not which feature"
        );
    }
}

/// **A list's tab wears the list's name, and follows a rename.**
///
/// The label is a cache and the view is the authority, which is the contract a
/// document tab's label already states. It matters more here: nobody presses
/// anything when a peer renames a list, so writing the label where the queue is
/// pressed would leave the strip naming a list that no longer exists. The
/// rename arrives the way the app delivers it — the list table, pushed.
#[test]
fn a_list_tab_follows_the_lists_name() {
    use crate::panes::explore::rail::RailTarget;
    let (mut v, w) = on_tasks();
    let id = koru_core::ids::ListId::new();
    let entry = |name: &str, color: &str| koru_core::system::ListEntry {
        id,
        name: name.to_owned(),
        color: Some(color.to_owned()),
        order: None,
        created_ms: Some(1),
        deleted_ms: None,
    };
    View::set_list_table(&mut v, &[entry("Wedding", "pink")]);
    // The tab first: the push is a push, and it has nowhere to land until a
    // Tasks tab exists — which is also why the label it opens with is the
    // fallback, before the table has told it the list's name.
    v.open_tasks_tab(RailTarget::List(id), false);
    assert_eq!(
        v.tabs().active_tab().label,
        "List",
        "before the table lands"
    );
    assert!(v.refresh_tasks_context(&w));
    assert_eq!(v.tabs().active_tab().label, "Wedding");

    // Renamed and recoloured underneath it, with nothing pressed.
    View::set_list_table(&mut v, &[entry("Honeymoon", "teal")]);
    assert!(v.refresh_tasks_context(&w));
    assert_eq!(
        v.tabs().active_tab().label,
        "Honeymoon",
        "the strip follows the table"
    );

    // And the swatch is the list's colour — the same square the rail and the
    // page's own heading draw for this list, from the same token.
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("a Tasks tab")
    };
    assert_eq!(
        page.title_swatch(),
        Some(Some("teal".to_owned())),
        "a token, not a colour: the square has to read in both themes"
    );

    // A queue is not a list, so it brings no swatch and keeps the glyph.
    v.open_tasks_tab(
        RailTarget::Bucket(crate::panes::explore::rail::Bucket::Today),
        false,
    );
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("a Tasks tab")
    };
    assert_eq!(page.title_swatch(), None);
}

/// **`Ctrl+Shift+E` reveals a Tasks tab too**, and what it reveals is the rail.
///
/// The shortcut means *show me where this tab lives in the column*, and it
/// refused the one tab whose place there is a selection rather than a file:
/// `reveal_tab_in_explore` returned on the missing document before it reached
/// anything else.
#[test]
fn revealing_a_tasks_tab_opens_the_rail_on_its_queue() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Later)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    // Shut the column, so what the shortcut does is visible rather than assumed.
    v.toggle_sidebar();
    assert!(!v.sidebar.is_open());

    assert!(v.focus_or_toggle_sidebar(&w.ws), "the shortcut is answered");
    assert!(v.sidebar.is_open(), "the column opens");
    assert!(
        v.explore.showing_tasks(),
        "on the tasks rail, not the document tree — that is what this tab is in"
    );
    assert_eq!(
        v.explore.rail().selected(),
        Some(RailTarget::Bucket(Bucket::Later)),
        "with the page's own queue selected"
    );
}

/// **Every task the Tasks page queues leaves through `ManagerView`.**
///
/// This is the test that was missing, and its absence is acceptance findings 1,
/// 3 and 4 in one line. `View::take_wants_task_commands` has a default that
/// answers `Vec::new()` — right for the twenty views that never queue one —
/// and `ManagerView` is what the shell actually holds. It did not forward this
/// drain, so the sweep asked the wrapper, the wrapper answered "nothing", and
/// every tick, removal and capture sat in `MainView::task_commands` for ever.
/// Nothing failed, so nothing was reported: no toast, and no line under
/// `KORU_TASKS_DEBUG`, because the queue was never read.
///
/// **Driven through `ManagerView` on purpose**, exactly as
/// `a_checkbox_completion_reaches_the_task_wrapper_through_the_container` says
/// of itself: the suite already had four tests of this queue and every one of
/// them called `MainView` directly, one level below the seam, so all four passed
/// throughout. A test that goes in where the shell goes in is the only kind that
/// could have caught this.
///
/// All four verbs, because the forward is one line and the four are one queue:
/// a fix that carried `Complete` and dropped `Create` would look identical from
/// the page.
#[test]
fn every_task_command_leaves_through_the_pane_the_shell_holds() {
    use crate::mainview::context_menu::{MenuAction, MenuSubject};
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, mut w, document, node) = on_a_task_row(Vec::new());

    // A real gesture for the verb the round-1 report opened with: *Complete*
    // from the row's own menu, which is the same queue the tick uses.
    assert!(v.run_menu_action_for_test(
        MenuSubject::Task { document, node },
        MenuAction::TaskComplete
    ));

    // The other three, put in beside it: the four verbs reach this queue by four
    // gestures and leave by one step, and that step is what is under test. A fix
    // that carried `Complete` and dropped `Create` would look identical from the
    // page.
    v.queue_task_commands_for_test(vec![
        TaskCommand::Create {
            title: "Chase it".to_owned(),
            due: None,
            list: None,
            priority: None,
        },
        TaskCommand::Remove { document, node },
        TaskCommand::SetList {
            document,
            node,
            list: None,
        },
    ]);

    // And the whole question: does the view the shell holds hand them over?
    let mut pane = crate::manager_view::ManagerView::Container(v);
    let queued = View::take_wants_task_commands(&mut pane);
    assert_eq!(
        queued.len(),
        4,
        "the shell's own view answers with the commands, not with silence: {queued:?}"
    );
    assert_eq!(
        queued[0],
        TaskCommand::Complete {
            document,
            node,
            completed: true
        },
        "in the order they were asked for"
    );
    assert!(
        View::take_wants_task_commands(&mut pane).is_empty(),
        "and taken, not copied — a second sweep must not run them twice"
    );

    // The return channel is forwarded too, for the reason it exists: a refused
    // `Create` has taken somebody's typing with it, and the page can only put
    // the words back if the wrapper passes the refusal down.
    assert!(
        View::task_command_failed(
            &mut pane,
            &TaskCommand::Create {
                title: "Chase it".to_owned(),
                due: None,
                list: None,
                priority: None,
            },
            "the store would not take it"
        ),
        "the capture row is given its words back through the same wrapper"
    );
    let _ = &mut w;
}

/// **A Tasks page is a place, so Back and Forward both reach it.**
///
/// It was not one. `here_for_tab` asked a tab for its document root and a Tasks
/// tab has none, so leaving one wrote nothing down: following a task opened its
/// document and the queue you came from was simply gone from the history. Back
/// walked past it to whatever document you had been in before, and Forward had
/// nothing to come forward *to* — the journey was one-way in both directions.
///
/// The queue is the identity, not the tab and not a row: the tab re-points, and
/// the page refetches its rows every half minute, so a row index would name a
/// different task by the time you pressed anything.
#[test]
fn back_and_forward_both_reach_the_queue_you_came_from() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    let (mut v, mut w) = on_tasks();

    // Two stops: a queue, then the document behind the window's first tab.
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Later)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert!(matches!(v.tabs().active_tab().view, TabView::Tasks(_)));
    let leaf = v
        .tabs()
        .position(|t| matches!(t.view, TabView::Leaf(_)))
        .expect("the document this window opened on");
    v.select(leaf);
    assert!(matches!(v.tabs().active_tab().view, TabView::Leaf(_)));

    // Back returns to the queue, by name.
    assert!(
        v.step_history(&w.ws, -1),
        "there is somewhere to go back to"
    );
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("Back landed on the Tasks page")
    };
    assert_eq!(
        page.target(),
        RailTarget::Bucket(Bucket::Later),
        "and on the queue that was showing, not merely on the tab"
    );

    // ...and Forward comes back out of it, which is the half that was reported.
    assert!(v.step_history(&w.ws, 1), "and forward again");
    assert!(matches!(v.tabs().active_tab().view, TabView::Leaf(_)));
}

/// **The return leg: a task in its document asks to be shown on the page.**
///
/// Following a task to where it is written has been one gesture since the Tasks
/// page shipped, and coming back the other way was not offered at all — so a
/// task you were reading in its document could only be found again by
/// remembering which queue it fell in.
///
/// **Which queue is asked of `koru_core::tasks::bucket`**, the domain function
/// SPEC §7 defines and the store's own placement is held equal to. This test
/// gives the task a date thirty-odd days out precisely because that is the edge
/// a hand-written comparison gets wrong: the copy that used to live in
/// `taskcounts.rs` had no thirty-day horizon, so everything past next week
/// counted as Upcoming.
#[test]
fn a_task_in_its_document_can_ask_to_be_shown_on_the_tasks_page() {
    use crate::panes::explore::rail::{Bucket, RailTarget};
    let (mut v, mut w) = on_document();
    let doc = v.document_root().expect("a document tab is showing");
    let task =
        w.ws.insert(Parent::Node(doc), Node::task("ship it").unwrap())
            .expect("insert");
    // Sixty days out is *Later*, not Upcoming — the thirty-day edge.
    let due = chrono::Local::now().date_naive() + chrono::Duration::days(60);
    koru_core::tasks::set_due(
        &mut w.ws,
        task,
        koru_core::time::Temporal::Date(
            koru_core::time::DateOnly::parse(&due.format("%Y-%m-%d").to_string()).unwrap(),
        ),
    )
    .expect("due");
    v.adopt(&w.ws);

    assert!(
        v.reveal_task_in_tasks_view(&w.ws, task),
        "the ask is answered"
    );
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks page came forward")
    };
    assert_eq!(
        page.target(),
        RailTarget::Bucket(Bucket::Later),
        "on the queue the domain function places it in, horizon included"
    );
    assert!(
        v.explore.showing_tasks(),
        "and the column is on the rail, so the card agrees with the page"
    );
}

/// **A task due today opens Today**, which is the ordinary case and the one
/// that would still pass if the bucket were guessed. It is here so the pair
/// says *the rule is asked*, not *one answer happens to match*.
#[test]
fn revealing_a_task_due_today_opens_today() {
    use crate::panes::explore::rail::{Bucket, RailTarget};
    let (mut v, mut w) = on_document();
    let doc = v.document_root().expect("a document tab is showing");
    let task =
        w.ws.insert(Parent::Node(doc), Node::task("today").unwrap())
            .expect("insert");
    let today = chrono::Local::now().date_naive();
    koru_core::tasks::set_due(
        &mut w.ws,
        task,
        koru_core::time::Temporal::Date(
            koru_core::time::DateOnly::parse(&today.format("%Y-%m-%d").to_string()).unwrap(),
        ),
    )
    .expect("due");
    v.adopt(&w.ws);

    assert!(
        v.reveal_task_in_tasks_view(&w.ws, task),
        "active_document={:?} read_ok={}",
        v.active_document,
        w.ws.read(task).is_ok()
    );
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks page came forward")
    };
    assert_eq!(page.target(), RailTarget::Bucket(Bucket::Today));
}

// ------------------------------------------------------- the tasks rail (V-1)

/// Where the rail's rows are, given a container with its sidebar open.
fn rail_row(v: &MainView, want: crate::panes::explore::rail::RailRow) -> koru_draw::geometry::Rect {
    let sb = v.regions.sidebar;
    let top = v.explore.rail_top(sb);
    v.explore
        .rail()
        .rows(sb, top)
        .into_iter()
        .find(|(row, _)| *row == want)
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("the rail has no {want:?}"))
}

/// A container showing the tasks rail, reached the way a person reaches it:
/// by pressing the third icon at the column's head.
fn on_tasks() -> (MainView, Workspace) {
    let (mut v, mut w) = on_document();
    v.toggle_sidebar();
    let (x, y) = mode_icon(&v, crate::panes::explore::NavigatorMode::Tasks).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    (v, w)
}

/// One mode button, from the same arithmetic that draws and hit-tests it.
fn mode_icon(
    v: &MainView,
    want: crate::panes::explore::NavigatorMode,
) -> koru_draw::geometry::Rect {
    v.explore
        .navigator_mode_rects(v.regions.sidebar)
        .into_iter()
        .find(|(mode, _)| *mode == want)
        .map(|(_, r)| r)
        .unwrap_or_else(|| panic!("no rect for {want:?}"))
}

/// **One sidebar, three modes.** The third icon swaps the column's content for
/// the rail and tells the container nothing; going back brings the library
/// rows with it.
#[test]
fn the_tasks_icon_swaps_the_column_and_documents_comes_back() {
    let (mut v, mut w) = on_document();
    v.toggle_sidebar();
    v.refresh_explore(&w.ws, true);
    let before = v.explore.navigator_mode();
    assert_eq!(before, crate::panes::explore::NavigatorMode::Documents);

    let (x, y) = mode_icon(&v, crate::panes::explore::NavigatorMode::Tasks).centre();
    assert!(v.mouse_down(&mut w.ws, x, y, false), "the press landed");
    assert!(v.explore.showing_tasks());

    let (x, y) = mode_icon(&v, crate::panes::explore::NavigatorMode::Documents).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert_eq!(
        v.explore.navigator_mode(),
        crate::panes::explore::NavigatorMode::Documents
    );
    assert!(!v.explore.showing_tasks());
}

/// The projection is remembered per session, as a token — the third mode is a
/// name rather than a second flag.
#[test]
fn the_tasks_mode_survives_a_capture_and_a_restore() {
    let (v, w) = on_tasks();
    let mut settings = koru_workspace_host::Settings::default();
    View::capture(&v, &mut settings);
    assert_eq!(settings.navigator_mode, "tasks");

    let (mut fresh, _) = container();
    View::restore(&mut fresh, &w, &settings);
    assert!(fresh.explore.showing_tasks());
}

/// The rail's selection is the seam the Tasks tab (V-2) reads: a press records
/// which bucket you asked for, and the row is carded on the frame it was
/// clicked rather than a round trip later.
#[test]
fn pressing_a_bucket_records_the_selection_for_the_tasks_tab() {
    let (mut v, mut w) = on_tasks();
    assert_eq!(v.tasks_selection(), None);
    let r = rail_row(
        &v,
        crate::panes::explore::rail::RailRow::Bucket(crate::panes::explore::rail::Bucket::Upcoming),
    );
    let (x, y) = r.centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert_eq!(
        v.tasks_selection(),
        Some(crate::panes::explore::rail::RailTarget::Bucket(
            crate::panes::explore::rail::Bucket::Upcoming
        ))
    );
}

/// The list table arrives from the workspace's system document, through the
/// same push the tag table uses — names and swatches from there, counts from
/// the store, and a list nothing matches counts zero rather than unknown.
#[test]
fn the_list_table_reaches_the_rail_with_its_names_and_swatches() {
    let (mut v, _w) = on_tasks();
    let id = koru_core::ids::ListId::new();
    let entry = koru_core::system::ListEntry {
        id,
        name: "Bike Project".into(),
        color: Some("blue".into()),
        order: None,
        created_ms: Some(1),
        deleted_ms: None,
    };
    assert!(View::set_list_table(&mut v, std::slice::from_ref(&entry)));
    let lists = v.explore.rail().lists();
    assert_eq!(lists.len(), 1);
    assert_eq!(lists[0].id, id);
    assert_eq!(lists[0].name, "Bike Project");
    assert_eq!(lists[0].color.as_deref(), Some("blue"));
    assert_eq!(
        (lists[0].count, lists[0].overdue),
        (0, 0),
        "no group row is a zero, not an unknown"
    );
    assert!(
        !View::set_list_table(&mut v, std::slice::from_ref(&entry)),
        "and the same table again changes nothing"
    );
}

/// **New list — inline.** The `+` opens one field in the sidebar, Enter keeps
/// it, and what leaves the container is a name and a swatch for the shell to
/// write into the system document. No dialog anywhere in it.
#[test]
fn a_list_is_made_inline_and_handed_to_the_shell() {
    let (mut v, mut w) = on_tasks();
    let plus = {
        let sb = v.regions.sidebar;
        let heading = rail_row(&v, crate::panes::explore::rail::RailRow::ListsHeading);
        let _ = sb;
        v.explore
            .rail()
            .row_parts(crate::panes::explore::rail::RailRow::ListsHeading, heading)
            .trailing
    };
    let (x, y) = plus.centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert!(v.explore.rail().composing(), "the row is reserved");

    for ch in "Wedding".chars() {
        let text = ch.to_string();
        v.key(
            &mut w.ws,
            &Key::Character(text.clone().into()),
            Some(&text),
            NONE,
        );
    }
    assert_eq!(
        View::take_wants_create_list(&mut v),
        None,
        "nothing leaves until Enter"
    );
    v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );
    let (name, color) = View::take_wants_create_list(&mut v).expect("the list");
    assert_eq!(name, "Wedding");
    assert!(color.is_some(), "with a swatch off the shared set");
    assert!(!v.explore.rail().composing(), "and the field is gone");
}

/// Escape takes the field away and makes nothing.
#[test]
fn escape_cancels_the_inline_list_field() {
    let (mut v, mut w) = on_tasks();
    let row = rail_row(&v, crate::panes::explore::rail::RailRow::NewList);
    let (x, y) = row.centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert!(v.explore.rail().composing());
    for ch in "Reading".chars() {
        let text = ch.to_string();
        v.key(
            &mut w.ws,
            &Key::Character(text.clone().into()),
            Some(&text),
            NONE,
        );
    }
    v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        NONE,
    );
    assert!(!v.explore.rail().composing());
    assert_eq!(View::take_wants_create_list(&mut v), None);
}

// ------------------------------------------------------- content bursts (T-0b)
//
// The container's half of ADR-273's `task.content.changed`: the editors note
// what they edited, and the window's one caret decides when a burst ends. The
// coalescing itself belongs to `koru_workspace_host::tasks::ContentBursts` and
// is pinned there; what these prove is that the two are actually joined up.

/// A document with a task as its first body row, and that row's node.
fn on_a_task() -> (MainView, koru_workspace::Workspace, NodeId) {
    let (mut v, mut workspace) = on_document();
    let doc = fill(&v, &mut workspace.ws, 3);
    let ws = &mut workspace.ws;
    let row = ws.children(Parent::Node(doc)).expect("children")[0];
    koru_core::tasks::ensure_task(ws, row, koru_core::attributes::TaskReason::Explicit)
        .expect("marked a task");
    View::adopt(&mut v, ws);
    (v, workspace, row)
}

/// **One burst, ended by the caret and not before.** Ten keystrokes in a task's
/// row are one event, and it is the click on the row below that produces it —
/// no clock, no document walk, and nothing per press.
#[test]
fn typing_in_a_task_is_one_burst_that_ends_when_the_caret_leaves() {
    let (mut v, mut workspace, row) = on_a_task();
    let ws = &mut workspace.ws;
    let c = v.focused_view_rect();
    v.mouse_down(ws, c.x + 60.0, c.y + row_y(0), false);
    v.drain_content_bursts(ws);

    for _ in 0..10 {
        v.key(ws, &ch("x"), Some("x"), NONE);
        assert!(
            v.drain_content_bursts(ws).is_empty(),
            "the caret is still in the row it is typing in"
        );
    }
    assert!(v.task_caret.open().is_some(), "a burst is open on it");

    v.mouse_down(ws, c.x + 60.0, c.y + row_y(1), false);
    let ended = v.drain_content_bursts(ws);
    assert_eq!(ended.len(), 1, "ten keystrokes, one event");
    assert_eq!(ended[0].node_id(), row);
    assert_eq!(ended[0].kind(), "task");
    assert!(
        v.drain_content_bursts(ws).is_empty(),
        "and the burst ended once"
    );
}

/// **A tab closing ends what it held.** There is no row left for a caret to
/// leave and no maintenance tick will ever see it again, so the burst that was
/// open when the tab went is flushed by the close itself.
#[test]
fn closing_a_tab_ends_the_burst_it_held() {
    let (mut v, mut workspace, _row) = on_a_task();
    let ws = &mut workspace.ws;
    let c = v.content();
    v.mouse_down(ws, c.x + 60.0, c.y + row_y(0), false);
    v.key(ws, &ch("x"), Some("x"), NONE);
    v.drain_content_bursts(ws);
    let open = v.task_caret.open().expect("a burst is open on the task");

    let showing = v.tabs().active();
    assert!(v.close(showing));

    assert!(
        v.task_caret.open().is_none(),
        "the window is not holding a burst on a row it no longer shows"
    );
    assert!(
        koru_workspace_host::tasks::content_closed(&[open]).is_empty(),
        "the close drained it — a second boundary finds nothing"
    );
}

/// A row that is not a task is not activity, all the way through the container:
/// nothing is noted, nothing is opened, and the caret leaving owes nothing.
#[test]
fn typing_in_a_plain_row_opens_no_burst() {
    let (mut v, mut workspace, _row) = on_a_task();
    let ws = &mut workspace.ws;
    let c = v.focused_view_rect();
    v.mouse_down(ws, c.x + 60.0, c.y + row_y(1), false);
    v.key(ws, &ch("x"), Some("x"), NONE);

    assert!(v.drain_content_bursts(ws).is_empty());
    assert!(v.task_caret.open().is_none());
}

// ---------------------------------------------------- the tasks tab (V-2)

/// Pressing a bucket opens the Tasks tab on it — the seam V-1 left live, now
/// wired to the tab that reads it.
#[test]
fn pressing_a_bucket_opens_the_tasks_tab_on_it() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    let (mut v, mut w) = on_tasks();
    let r = rail_row(&v, RailRow::Bucket(Bucket::Upcoming));
    let (x, y) = r.centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let TabView::Tasks(tab) = &v.tabs().active_tab().view else {
        panic!("the bucket opened a Tasks tab");
    };
    assert_eq!(tab.target(), RailTarget::Bucket(Bucket::Upcoming));
}

/// **One Tasks tab per window.** The rail holds one selection, so a second tab
/// would be a second answer to *which bucket am I looking at* — pressing
/// another bucket re-points the tab that is already there.
#[test]
fn a_second_bucket_re_points_the_one_tasks_tab() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    let (mut v, mut w) = on_tasks();
    let before = v.tabs().len();
    for bucket in [Bucket::Upcoming, Bucket::Later] {
        let (x, y) = rail_row(&v, RailRow::Bucket(bucket)).centre();
        v.mouse_down(&mut w.ws, x, y, false);
    }
    assert_eq!(v.tabs().len(), before + 1, "one tab, not two");
    let TabView::Tasks(tab) = &v.tabs().active_tab().view else {
        panic!("still a Tasks tab");
    };
    assert_eq!(tab.target(), RailTarget::Bucket(Bucket::Later));
}

/// The rail and the tab hold one answer, and the rail is the half put back:
/// a tab restored from a session cards its own queue on the next refresh.
#[test]
fn a_restored_tasks_tab_cards_its_queue_in_the_rail() {
    use crate::panes::explore::rail::{Bucket, RailTarget};
    let (mut v, w) = on_tasks();
    let mut settings = koru_workspace_host::Settings::default();
    settings.tabs = vec![(
        koru_workspace_host::TabRef::Tasks(koru_workspace_host::TasksTabRef::Bucket("later")),
        0.0,
    )];
    View::restore(&mut v, &w, &settings);
    let TabView::Tasks(tab) = &v.tabs().active_tab().view else {
        panic!("the session put a Tasks tab back");
    };
    assert_eq!(tab.target(), RailTarget::Bucket(Bucket::Later));
    // …and the column follows it, the way the footer follows a Trash tab.
    v.refresh_explore(&w.ws, true);
    assert_eq!(v.tasks_selection(), Some(RailTarget::Bucket(Bucket::Later)));
}

// ------------------------------------------- the reminders page (the rail's row)

/// The rail and tab use the same localized title, and a tab that already
/// exists takes a later locale rather than keeping the language it opened in.
/// Arabic stays in logical order; direction is a rendering decision.
#[test]
fn reminders_rail_and_existing_tab_follow_every_locale() {
    let mut tab = Tab::reminders();
    for lang in koru_i18n::Lang::ALL {
        let expected = koru_i18n::t(lang, koru_i18n::Key::ReminderTitle);
        assert_eq!(
            crate::panes::explore::rail::reminders_label(lang),
            expected,
            "{} rail label",
            lang.tag()
        );
        assert!(tab.refresh_tasks_label_for(lang) || tab.label == expected);
        assert_eq!(tab.label, expected, "{} existing tab label", lang.tag());
    }
    assert_eq!(
        crate::panes::explore::rail::reminders_label(koru_i18n::Lang::Ar),
        "التذكيرات"
    );
    assert!(koru_i18n::Lang::Ar.is_rtl());
}

/// The frame settle borrows the controller's projection into the disjoint tab
/// field. A clone here is O(all reminders) even when the page immediately says
/// the projection is unchanged, so pin the hot path's ownership shape.
#[test]
fn unchanged_reminders_page_settles_do_not_clone_the_global_projection() {
    let source = include_str!("mod.rs");
    let start = source
        .find("let reminders_page_changed =")
        .expect("the Reminders page feeding block");
    let end = source[start..]
        .find("while let Some(request)")
        .map(|offset| start + offset)
        .expect("the end of the Reminders page feeding block");
    let settle = &source[start..end];

    assert!(settle.contains("let global = self.reminders.global();"));
    assert!(settle.contains("view.set_global(global, environment)"));
    assert!(
        !settle.contains("global().clone()"),
        "an idle settle must not copy every reminder"
    );
}

/// Pressing the rail's Reminders row re-points the Tasks tab to the All
/// Reminders page and cards the row — the queue rows' behavior, verbatim:
/// one tab, its content swapped.
#[test]
fn pressing_reminders_re_points_the_tasks_tab_to_the_page() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Upcoming)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let before = v.tabs().len();

    let (x, y) = rail_row(&v, RailRow::Reminders).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert_eq!(v.tabs().len(), before, "one tab, its content swapped");
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Reminders(_)),
        "the front tab shows the reminders page"
    );
    assert_eq!(v.tabs().active_tab().label, "Reminders");
    assert_eq!(v.tasks_selection(), Some(RailTarget::Reminders));
}

/// **Ctrl+click carries the queue rows' disposition**: the page opens in a
/// second tab *behind* the one you are reading, and the card does not move.
#[test]
fn ctrl_clicking_reminders_opens_a_second_tab_and_leaves_the_first_in_front() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Upcoming)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let before = v.tabs().len();
    let active = v.tabs().active();

    let (x, y) = rail_row(&v, RailRow::Reminders).centre();
    v.mouse_down(&mut w.ws, x, y, true);
    assert_eq!(v.tabs().len(), before + 1, "a second tab");
    assert_eq!(v.tabs().active(), active, "the first stays in front");
    assert!(matches!(v.tabs().active_tab().view, TabView::Tasks(_)));
    assert!(
        v.tabs()
            .iter()
            .any(|t| matches!(t.view, TabView::Reminders(_))),
        "the page opened behind it"
    );
    assert_eq!(
        v.tasks_selection(),
        Some(RailTarget::Bucket(Bucket::Upcoming)),
        "the card stays on the queue still in front"
    );
}

/// The swap is reversible: a bucket press on a tab showing the Reminders
/// page trades the view back to the tasks page, same tab.
#[test]
fn a_bucket_press_on_the_reminders_page_swaps_back_to_tasks() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Reminders).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert!(matches!(v.tabs().active_tab().view, TabView::Reminders(_)));
    let before = v.tabs().len();

    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Later)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert_eq!(v.tabs().len(), before, "still one tab");
    let TabView::Tasks(tab) = &v.tabs().active_tab().view else {
        panic!("the bucket swapped the view back to Tasks");
    };
    assert_eq!(tab.target(), RailTarget::Bucket(Bucket::Later));
    assert_eq!(v.tasks_selection(), Some(RailTarget::Bucket(Bucket::Later)));
}

/// The page is a place: a session writes it as the rail's Reminders target,
/// and the restore puts the same page back with the row carded.
#[test]
fn the_reminders_page_survives_the_session_round_trip() {
    use crate::panes::explore::rail::{RailRow, RailTarget};
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Reminders).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert!(matches!(v.tabs().active_tab().view, TabView::Reminders(_)));

    let mut settings = koru_workspace_host::Settings::default();
    v.capture_session(&mut settings);
    assert!(
        settings.tabs.iter().any(|(which, _)| *which
            == koru_workspace_host::TabRef::Tasks(koru_workspace_host::TasksTabRef::Reminders)),
        "captured as the rail's Reminders target"
    );

    let (mut v, w) = on_tasks();
    let mut settings = koru_workspace_host::Settings::default();
    settings.tabs = vec![(
        koru_workspace_host::TabRef::Tasks(koru_workspace_host::TasksTabRef::Reminders),
        0.0,
    )];
    View::restore(&mut v, &w, &settings);
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Reminders(_)),
        "the session put the reminders page back"
    );
    v.refresh_explore(&w.ws, true);
    assert_eq!(v.tasks_selection(), Some(RailTarget::Reminders));
}

/// **A swapped view is laid out before it is drawn.** Re-pointing a Tasks
/// tab to the Reminders page trades the tab's *view* for a new one, and the
/// tab's cached size has to go with it — the cache belongs to the view it
/// replaced, so without invalidating it the page keeps a zero viewport and
/// draws a blank sheet. This drives the real path: rail press, swap, the
/// loop's own maintain, then the same geometry draw and hit-testing read.
#[test]
fn re_pointing_to_the_reminders_page_sizes_it_before_the_draw() {
    use crate::mainview::reminder_management::{Filter, Hit};
    use crate::panes::explore::rail::{Bucket, RailRow};
    let (mut v, mut w) = on_tasks();
    // A Tasks tab, sized by the ordinary layout — the swap is the path under
    // test, not the fresh-tab push.
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Upcoming)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let (x, y) = rail_row(&v, RailRow::Reminders).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    // The loop's own pass: the feeding block, then the tab's maintain.
    v.maintain(&mut w.ws, std::time::Instant::now());

    let c = v.focused_view_rect();
    let TabView::Reminders(view) = &v.tabs().active_tab().view else {
        panic!("the row re-pointed the tab");
    };
    assert_eq!(
        view.content_rect_for_test(),
        Some(koru_draw::geometry::Rect::new(0.0, 0.0, c.w, c.h)),
        "the page fills the content area it was laid out at"
    );
    // …and the surface is really there: the first filter chip answers a hit
    // where geometry put it — the same geometry `draw` reads.
    let first_chip = (16.0 + 10.0, 52.0 + koru_ui::Chip::H / 2.0);
    assert_eq!(
        view.hit_for_test(first_chip.0, first_chip.1),
        Some(Hit::Filter(Filter::All)),
        "the filter chips are on the page"
    );
}

/// **The page is not modal.** Arrows and Enter are the page's own — they walk
/// its focus and activate what they land on — and a key it does not own falls
/// through, where the bell's modal overlay would have swallowed it.
#[test]
fn keys_reach_the_reminders_page_and_everything_else_falls_through() {
    use crate::panes::explore::rail::RailRow;
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Reminders).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    assert!(matches!(v.tabs().active_tab().view, TabView::Reminders(_)));
    // The maintain pass feeds the page and builds its geometry — the loop's
    // own pass, run by hand the way every test here runs it.
    v.maintain(&mut w.ws, std::time::Instant::now());
    // A press inside the page puts the keyboard on the tab rather than the
    // column, the way a click into a document does.
    let c = v.focused_view_rect();
    v.mouse_down(&mut w.ws, c.x + c.w / 2.0, c.y + c.h / 2.0, false);

    let down = winit::keyboard::Key::Named(winit::keyboard::NamedKey::ArrowDown);
    assert!(
        v.key(&mut w.ws, &down, None, NONE),
        "the page owns the arrows"
    );
    assert!(v.key(&mut w.ws, &down, None, NONE));
    let enter = winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter);
    assert!(v.key(&mut w.ws, &enter, None, NONE));
    assert_eq!(
        v.reminders.global().filter,
        koru_editor::reminders::GlobalFilter::NeedsAttention,
        "two steps landed on the second filter chip and Enter chose it"
    );

    // **F13, not F9**: F9 is the internal session recorder's chord now
    // (SESSIONRECORD), claimed by the container long before the tab is asked —
    // which is what this assertion is for, so the probe moved.
    let f13 = winit::keyboard::Key::Named(winit::keyboard::NamedKey::F13);
    assert!(
        !v.key(&mut w.ws, &f13, None, NONE),
        "a key the page does not own is not swallowed"
    );
}

/// **A tick leaves as a command, not as a write.** The view holds one
/// `&mut WorkspaceDoc` and the task lives wherever it was typed, so the verb
/// goes out to the shell — which is the side that can park that document.
#[test]
fn ticking_a_row_leaves_as_a_task_command() {
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, _w) = on_tasks();
    let document = koru_core::DocumentId::new();
    let node = NodeId::new();
    v.run_menu_action_for_test(
        MenuSubject::Task { document, node },
        MenuAction::TaskComplete,
    );
    assert_eq!(
        View::take_wants_task_commands(&mut v),
        vec![TaskCommand::Complete {
            document,
            node,
            completed: true,
        }]
    );
}

/// **Remove from Tasks cannot reach the store without its confirm.** The menu
/// row queues nothing; the dialog's *Remove* is what does.
#[test]
fn remove_from_tasks_goes_through_the_confirm() {
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, _w) = on_tasks();
    let document = koru_core::DocumentId::new();
    let node = NodeId::new();
    v.run_menu_action_for_test(MenuSubject::Task { document, node }, MenuAction::TaskRemove);
    assert!(
        View::take_wants_task_commands(&mut v).is_empty(),
        "the row asks a question rather than doing anything"
    );
    assert!(v.confirm.is_some(), "…and the question is on screen");

    // Cancelling leaves nothing behind to go off on the next turn.
    v.settle_confirm(false);
    assert!(View::take_wants_task_commands(&mut v).is_empty());

    v.run_menu_action_for_test(MenuSubject::Task { document, node }, MenuAction::TaskRemove);
    v.settle_confirm(true);
    assert_eq!(
        View::take_wants_task_commands(&mut v),
        vec![TaskCommand::Remove { document, node }]
    );
}

/// A failed capture gives the typing back rather than only naming it.
#[test]
fn a_failed_capture_puts_the_words_back_in_the_row() {
    use crate::panes::explore::rail::{Bucket, RailRow};
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Today)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let command = TaskCommand::Create {
        title: "Chase it tomorrow".into(),
        due: None,
        list: None,
        priority: None,
    };
    assert!(View::task_command_failed(
        &mut v,
        &command,
        "this workspace has no store to keep tasks in"
    ));
    assert!(v.toast.is_some(), "and it says so");
}

// ------------------------------------------------- lists and membership (V-4)

/// One answered page with a single task row in it, put into the open Tasks tab
/// the way the worker's answer would arrive. Answers the row's ids.
fn seed_one_task_row(
    v: &mut MainView,
    ws: &koru_core::schema::WorkspaceDoc,
    lists: Vec<crate::tasks_view::ListFace>,
) -> (koru_core::DocumentId, NodeId) {
    seed_one_task_row_in(v, ws, lists, None)
}

/// The same, with the row in `list` when one is given — the membership
/// travels as the store sends it, an attribute value.
fn seed_one_task_row_in(
    v: &mut MainView,
    ws: &koru_core::schema::WorkspaceDoc,
    lists: Vec<crate::tasks_view::ListFace>,
    list: Option<koru_core::ids::ListId>,
) -> (koru_core::DocumentId, NodeId) {
    let workspace = koru_store_core::ids::WorkspaceId::new();
    let document = koru_store_core::ids::DocumentId::new();
    let node = NodeId::new();
    let row = koru_store_wire::tasks::TaskRow {
        workspace,
        document,
        node: node.to_storage(),
        task: format!(
            "task:{}:{}:{}",
            workspace.to_storage(),
            document.to_storage(),
            node.to_storage()
        ),
        node_kind: "paragraph".to_owned(),
        lifecycle: "open".to_owned(),
        document_trashed: false,
        in_trash: false,
        breadcrumb: "Test".to_owned(),
        title: "Order the groupset".to_owned(),
        attributes: list
            .map(|list| {
                vec![koru_store_wire::attributes::AttributeRowValue {
                    id: "list-1".to_owned(),
                    role: koru_core::attributes::KEY_LIST.to_owned(),
                    kind: "text".to_owned(),
                    value: list.to_storage(),
                    zone: None,
                    label: None,
                }]
            })
            .unwrap_or_default(),
    };
    let answered = koru_store_wire::tasks::TaskQueryResponse {
        rows: vec![row],
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: Vec::new(),
        projected: true,
        updating_workspaces: Vec::new(),
        limit: 100,
        next_after: None,
        restarted: false,
        covered: None,
    };
    let today = chrono::Local::now().date_naive();
    let now_ms = chrono::Utc::now().timestamp_millis();
    let TabView::Tasks(tab) = &mut v.tabs_mut().active_mut().view else {
        panic!("a Tasks tab is open");
    };
    tab.panel_mut().accept_for_test(&answered, today, now_ms);
    tab.set_lists(lists);
    tab.mark_stale();
    tab.catch_up(ws);
    (koru_core::DocumentId::from_uuid(document.as_uuid()), node)
}

/// Open the Tasks tab on Today, with one row on it.
fn on_a_task_row(
    lists: Vec<crate::tasks_view::ListFace>,
) -> (MainView, Workspace, koru_core::DocumentId, NodeId) {
    use crate::panes::explore::rail::{Bucket, RailRow};
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Today)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let (document, node) = seed_one_task_row(&mut v, &w.ws, lists);
    (v, w, document, node)
}

fn a_list(name: &str, color: &str) -> (koru_core::ids::ListId, crate::tasks_view::ListFace) {
    let id = koru_core::ids::ListId::new();
    (
        id,
        crate::tasks_view::ListFace {
            id,
            name: name.to_owned(),
            color: Some(color.to_owned()),
        },
    )
}

/// **A row in a list leads to the list.** *Go to list* on the row's menu moves
/// the Tasks tab in front to the list's page and the rail's selection with it
/// — the same door a press on the rail's row is — and the list chip on the row
/// asks for the same by the view's own request. Only while the table still has
/// the list: a membership the table cannot name is refused, as the list's own
/// menu refuses a list deleted next door.
#[test]
fn go_to_list_moves_the_page_to_the_list() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    let (birthday, face) = a_list("Birthday Tasks", "pink");
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Today)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let (document, node) = seed_one_task_row_in(&mut v, &w.ws, vec![face], Some(birthday));

    // The table does not know the list yet: refused, and the page stays.
    assert!(!v.run_menu_action_for_test(
        MenuSubject::Task { document, node },
        MenuAction::TaskGoToList
    ));
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks tab")
    };
    assert_eq!(page.target(), RailTarget::Bucket(Bucket::Today));

    v.set_list_table(&[koru_core::system::ListEntry {
        id: birthday,
        name: "Birthday Tasks".to_owned(),
        color: Some("pink".to_owned()),
        order: None,
        created_ms: Some(1),
        deleted_ms: None,
    }]);
    assert!(v.run_menu_action_for_test(
        MenuSubject::Task { document, node },
        MenuAction::TaskGoToList
    ));
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks tab, still")
    };
    assert_eq!(page.target(), RailTarget::List(birthday));
    assert_eq!(
        v.explore.rail().selected(),
        Some(RailTarget::List(birthday)),
        "and the rail follows"
    );

    // **And it is a place in history**: Back is the queue it was pressed on,
    // Forward the list again.
    assert!(v.step_history(&w.ws, -1), "somewhere to go back to");
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks tab, still")
    };
    assert_eq!(page.target(), RailTarget::Bucket(Bucket::Today));
    assert!(v.step_history(&w.ws, 1), "and forward again");
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks tab, still")
    };
    assert_eq!(page.target(), RailTarget::List(birthday));

    // The chip's request takes the same door.
    v.open_tasks_tab(RailTarget::Bucket(Bucket::Today), false);
    if let TabView::Tasks(page) = &mut v.tabs_mut().active_mut().view {
        page.request_for_test(crate::tasks_view::Request::OpenList { list: birthday });
    }
    assert!(v.drain_tasks_request());
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks tab, still")
    };
    assert_eq!(page.target(), RailTarget::List(birthday));
}

/// **A capture that lands somewhere else says so.** Typed into Today with a
/// due three weeks out, the task is Upcoming's; when its receipt lands the
/// toast says *Added to Upcoming* with *View task* and *Undo*. View task
/// opens Upcoming with the row selected; Undo takes the task back off Tasks
/// by the one door. A capture that lands on the page it was typed on raises
/// nothing, and one filed in a list names the list and goes there.
#[test]
fn a_capture_that_lands_elsewhere_raises_a_toast_with_a_way_there_and_back() {
    use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
    use koru_core::time::{DateOnly, Temporal};
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Today)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let today = v.tasks_today();
    let far = Temporal::Date(DateOnly::new(today + chrono::Duration::days(20)));

    // On the page: nothing to say.
    v.queue_task_command(
        TaskCommand::Create {
            title: "Now".to_owned(),
            due: Some(Temporal::Date(DateOnly::new(today))),
            list: None,
            priority: None,
        },
        false,
    );
    assert_eq!(v.pending_landings.back(), Some(&Landing::Plain));

    // Elsewhere: the toast, on the receipt.
    v.queue_task_command(
        TaskCommand::Create {
            title: "Later on".to_owned(),
            due: Some(far.clone()),
            list: None,
            priority: None,
        },
        false,
    );
    assert!(matches!(
        v.pending_landings.back(),
        Some(Landing::Announce { message, target })
            if message == "Added to Upcoming" && *target == RailTarget::Bucket(Bucket::Upcoming)
    ));
    let (document, node) = (koru_core::DocumentId::new(), NodeId::new());
    let mut pane = crate::manager_view::ManagerView::Container(v);
    let _ = View::take_wants_task_commands(&mut pane);
    View::task_command_landed(&mut pane, &a_receipt(&w, document, true));
    {
        let crate::manager_view::ManagerView::Container(v) = &pane else {
            panic!("container")
        };
        assert!(v.toast.is_none(), "the first landed on the page: no toast");
    }
    let receipt = koru_workspace_host::tasks::Landed {
        node: Some(node),
        ..a_receipt(&w, document, true)
    };
    View::task_command_landed(&mut pane, &receipt);
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    let (toast, payload) = v.toast.as_ref().expect("the toast");
    assert_eq!(toast.message(), "Added to Upcoming");
    assert_eq!(toast.action(), Some("View task"));
    assert_eq!(toast.secondary(), Some("Undo"));
    assert_eq!(
        payload,
        &ToastPayload::TaskAdded {
            document,
            node,
            target: RailTarget::Bucket(Bucket::Upcoming),
        }
    );

    // View task: the queue it went to, the row asked for.
    assert!(v.view_added_task(document, node, RailTarget::Bucket(Bucket::Upcoming)));
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks tab")
    };
    assert_eq!(page.target(), RailTarget::Bucket(Bucket::Upcoming));
    assert!(v.step_history(&w.ws, -1), "and Today is one Back away");

    // Undo: the task comes back off Tasks by the one door.
    v.undo_added_task(document, node);
    assert_eq!(
        View::take_wants_task_commands(&mut pane),
        vec![TaskCommand::Remove { document, node }]
    );

    // Filed in a list from a bucket page: the list is named and is the place.
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    let (birthday, _) = a_list("Birthday Tasks", "pink");
    v.set_list_table(&[koru_core::system::ListEntry {
        id: birthday,
        name: "Birthday Tasks".to_owned(),
        color: Some("pink".to_owned()),
        order: None,
        created_ms: Some(1),
        deleted_ms: None,
    }]);
    v.open_tasks_tab(RailTarget::Bucket(Bucket::Today), false);
    v.queue_task_command(
        TaskCommand::Create {
            title: "Balloons".to_owned(),
            due: Some(far),
            list: Some(birthday),
            priority: None,
        },
        false,
    );
    assert!(matches!(
        v.pending_landings.back(),
        Some(Landing::Announce { message, target })
            if message == "Added to Birthday Tasks" && *target == RailTarget::List(birthday)
    ));
}

/// **A page-made task is offered a document, and the receipt opens it.**
/// *Create document* queues the one command, the row says *Notes …* under its
/// title at once, and when the receipt lands naming the new page and the
/// planted paragraph the container asks for that paragraph in a tab of its
/// own, by the path *View in origin* takes — which is what puts the list one
/// Back, and one tab-close, away.
#[test]
fn create_document_on_a_page_made_task_opens_the_home_when_it_lands() {
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, w, document, node) = on_a_task_row(Vec::new());
    if let TabView::Tasks(page) = &mut v.tabs_mut().active_mut().view {
        page.set_task_documents(vec![document]);
    }
    assert!(v.run_menu_action_for_test(
        MenuSubject::Task { document, node },
        MenuAction::TaskCreateDocument
    ));
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks tab")
    };
    let row = page.row_of(document, node).expect("the row is still there");
    assert_eq!(
        row.title, "Notes Order the groupset",
        "the page is the task"
    );

    let mut pane = crate::manager_view::ManagerView::Container(v);
    let commands = View::take_wants_task_commands(&mut pane);
    assert_eq!(commands, vec![TaskCommand::CreateHome { document, node }]);

    let (home, first) = (koru_core::DocumentId::new(), NodeId::new());
    let receipt = koru_workspace_host::tasks::Landed {
        node: Some(first),
        ..a_receipt(&w, home, true)
    };
    View::task_command_landed(&mut pane, &receipt);
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    let asked = v.take_pane_request();
    assert!(
        matches!(
            asked,
            Some(koru_workspace_host::PaneRequest::Navigate {
                target: koru_workspace_host::NavigationTarget::Document {
                    document: d,
                    node: Some(n),
                },
                ..
            }) if d == home && n == first
        ),
        "the new page is asked for, on its first paragraph: {asked:?}"
    );
    assert!(
        v.pending_lands
            .iter()
            .any(|(d, w)| *d == home && matches!(w, OpenWhere::NewTab)),
        "in a tab of its own"
    );
    let TabView::Tasks(page) = &v.tabs().active_tab().view else {
        panic!("the Tasks tab, until the page arrives")
    };
    let row = page
        .row_of(home, first)
        .expect("the row took the page's ids");
    assert_eq!(row.title, "Notes Order the groupset");
}

/// **The whole hop, as one gesture.** *List…* opens the Tasks tab's own picker
/// over the row, a press on a row of it leaves as a request, and the container
/// turns that into the ordinary task command — so membership is written into
/// the task's own document by the door a tick already goes through.
///
/// It is deliberately not the *go there and act* shape `Due…` and `Priority…`
/// have: there is no editor picker for `koru.list` to go to.
#[test]
fn the_list_row_picks_a_membership_and_leaves_as_a_command() {
    use koru_workspace_host::tasks::TaskCommand;
    let (wedding, face) = a_list("Wedding", "pink");
    let (mut v, mut w, document, node) = on_a_task_row(vec![face]);

    assert!(
        v.run_menu_action_for_test(MenuSubject::Task { document, node }, MenuAction::TaskList),
        "the row is on this page, so the picker opens over it"
    );
    assert!(
        View::take_wants_task_commands(&mut v).is_empty(),
        "opening a picker changes nothing"
    );

    let r = {
        let TabView::Tasks(tab) = &v.tabs().active_tab().view else {
            panic!("still a Tasks tab");
        };
        tab.picker_row_rect_for_test(0).expect("the first list")
    };
    let c = v.focused_view_rect();
    v.mouse_down(
        &mut w.ws,
        c.x + r.x + r.w * 0.5,
        c.y + r.y + r.h * 0.5,
        false,
    );
    assert_eq!(
        View::take_wants_task_commands(&mut v),
        vec![TaskCommand::SetList {
            document,
            node,
            list: Some(wedding),
        }]
    );
}

/// *List…* over a row this page is not showing does nothing at all — the page
/// it was asked from can be half a minute old.
#[test]
fn the_list_row_refuses_a_task_the_page_does_not_hold() {
    let (_, face) = a_list("Wedding", "pink");
    let (mut v, _w, _document, _node) = on_a_task_row(vec![face]);
    assert!(!v.run_menu_action_for_test(
        MenuSubject::Task {
            document: koru_core::DocumentId::new(),
            node: NodeId::new(),
        },
        MenuAction::TaskList,
    ));
}

/// **The list table reaches the Tasks tab**, which is what turns the id the
/// wire carries into the name on the chip. Pushed on the draw path, so a tab
/// opened since the last change is still told.
#[test]
fn the_list_table_reaches_the_tasks_tab() {
    use crate::panes::explore::rail::{Bucket, RailRow};
    let (mut v, mut w) = on_tasks();
    let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Today)).centre();
    v.mouse_down(&mut w.ws, x, y, false);
    let id = koru_core::ids::ListId::new();
    View::set_list_table(
        &mut v,
        &[koru_core::system::ListEntry {
            id,
            name: "Bike Project".to_owned(),
            color: Some("blue".to_owned()),
            order: None,
            created_ms: Some(1),
            deleted_ms: None,
        }],
    );
    assert!(v.refresh_tasks_context(&w));
    let TabView::Tasks(tab) = &v.tabs().active_tab().view else {
        panic!("a Tasks tab is open");
    };
    assert_eq!(
        tab.list_face_for_test(id).map(|face| face.name.clone()),
        Some("Bike Project".to_owned()),
        "the tab resolves the id the answer carries"
    );
}

// ------------------------------------------- the context column's Tasks section (V-5)

/// Mark `node` a task by giving it a due, which is half of ADR-272's whole
/// derivation — and the half a person types.
fn due_on(ws: &mut WorkspaceDoc, node: NodeId, day: &str) {
    let due =
        koru_core::time::Temporal::Date(koru_core::time::DateOnly::parse(day).expect("a real day"));
    koru_core::tasks::set_due(ws, node, due).expect("the due is written");
}

/// A container on a document holding two paragraphs, one of them a task, with
/// the context column open on **Tasks**.
fn on_a_context_column() -> (MainView, Workspace, NodeId) {
    let (mut v, mut w) = on_document();
    let doc = fill(&v, &mut w.ws, 2);
    // **By its line, not by its index.** A document opens holding an empty row
    // of its own, so position 0 is not the first paragraph a test wrote — and a
    // title assertion against the wrong node reads as a bug in the reader.
    let rows =
        w.ws.children(koru_core::schema::Parent::Node(doc))
            .expect("the document's rows");
    let task = rows
        .into_iter()
        .find(|id| {
            w.ws.read(*id)
                .ok()
                .and_then(|n| n.content.as_text().map(str::to_owned))
                .as_deref()
                == Some("row 0")
        })
        .expect("the paragraph the fixture wrote");
    due_on(&mut w.ws, task, "2099-01-02");
    v.toggle_context();
    v.context_view.select(crate::panes::context::Section::Tasks);
    (v, w, task)
}

/// **The section fills from the open document**, which is the source R-1's
/// mapping gives it: the column already holds the document, and a question the
/// document can answer exactly is not one to ask the store's read model.
#[test]
fn the_context_column_lists_this_documents_tasks() {
    let (mut v, mut w, task) = on_a_context_column();
    assert!(
        View::maintain(&mut v, &mut w.ws, std::time::Instant::now()),
        "the first tick with the section showing fills it"
    );
    let rows = v.context_view.tasks();
    assert_eq!(rows.len(), 1, "one task, not two paragraphs");
    assert_eq!(rows[0].node, task);
    assert_eq!(rows[0].title, "row 0");
    assert!(rows[0].due.is_some(), "and it carries the due as a word");
    assert!(!rows[0].completed);
    assert!(!rows[0].overdue, "a date in 2099 is not late");
}

/// **Nothing walks the document on a timer.** With the section on screen and
/// nothing changed, a second tick reads nothing; a task command is what puts
/// the walk back on the table.
#[test]
fn the_section_is_not_rebuilt_on_a_tick() {
    let (mut v, mut w, _) = on_a_context_column();
    View::maintain(&mut v, &mut w.ws, std::time::Instant::now());
    assert!(!v.context_tasks_stale, "the flag was spent by the rebuild");
    v.tasks_changed();
    assert!(
        v.context_tasks_stale,
        "and a task command is what sets it again"
    );
}

/// A shut column pays nothing, and **keeps its flag** — so opening it rebuilds
/// rather than showing what the document held before.
#[test]
fn a_shut_column_costs_nothing_and_keeps_its_flag() {
    let (mut v, mut w) = on_document();
    View::maintain(&mut v, &mut w.ws, std::time::Instant::now());
    assert!(v.context_view.tasks().is_empty());
    assert!(v.context_tasks_stale, "still owed when it opens");
}

/// **A row reveals the task where it lives** — the ordinary navigation to a
/// node in the document already in front of you, the same request the Tasks
/// tab's own row makes. It is read-only this release: nothing is written.
#[test]
fn a_context_task_row_reveals_it_in_the_document() {
    let (mut v, mut w, task) = on_a_context_column();
    View::maintain(&mut v, &mut w.ws, std::time::Instant::now());
    let document = koru_core::DocumentId::new();
    v.active_document = Some(document);
    assert!(v.apply(&w.ws, crate::panes::Outcome::RevealTask(task)));
    let asked = View::take_pane_request(&mut v).expect("a navigation was asked for");
    match asked {
        koru_workspace_host::PaneRequest::Navigate { target, .. } => assert_eq!(
            target,
            koru_workspace_host::NavigationTarget::Document {
                document,
                node: Some(task),
            }
        ),
        other => panic!("expected a navigation, got {other:?}"),
    }
}

// ----------------------------------------------- the resolver at the open (D-2c)
//
// SPEC §3: a `task:` reference that no longer names a task answers ordinary Not
// Found, **with no fallback navigation to the underlying content**. A Tasks page
// is up to half a minute old, so the row that was pressed is exactly where that
// rule bites — and until D-2c `resolve_task_ref` had no production caller at
// all.

/// **A row whose task is still a task opens the way it always did.** The check
/// is a gate on one line of the landing, not a second navigation, so the
/// ordinary case has to be provably unchanged.
#[test]
fn a_live_task_row_still_opens_its_node() {
    let (mut v, w, task) = on_a_context_column();
    let document = koru_core::DocumentId::new();
    v.active_document = Some(document);
    assert!(v.apply(&w.ws, crate::panes::Outcome::RevealTask(task)));
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::Navigate { .. })
    ));

    assert!(
        View::open_search_target(&mut v, &w.ws, None, Some(task), None, None),
        "it still resolves, so the landing is made"
    );
    assert!(v.toast.is_none(), "and nothing was refused");
}

/// **A row unmarked since the page was fetched opens nothing.** The reference
/// resolves to `NotFound`, and the one thing that must not happen is the
/// paragraph underneath it opening as though it were the task.
///
/// The refusal is a toast and a refresh: the page's notice state is its *empty*
/// state and would throw away rows that are perfectly good, while the toast is
/// the surface that already says a gesture could not be done. The refresh is
/// what takes the stale row off the page.
#[test]
fn a_stale_task_row_does_not_open_the_paragraph_underneath_it() {
    let (mut v, mut w, task) = on_a_context_column();
    let document = koru_core::DocumentId::new();
    v.active_document = Some(document);
    assert!(v.apply(&w.ws, crate::panes::Outcome::RevealTask(task)));
    assert!(matches!(
        View::take_pane_request(&mut v),
        Some(koru_workspace_host::PaneRequest::Navigate { .. })
    ));

    // Somebody took it out of Tasks between the page and the landing. The node
    // is still there and still reads — which is precisely why `ws.read` was
    // never the question.
    koru_core::tasks::remove_from_tasks(&mut w.ws, task).expect("it is removed from tasks");
    assert!(w.ws.read(task).is_ok(), "the paragraph is still there");

    assert!(
        !View::open_search_target(&mut v, &w.ws, None, Some(task), None, None),
        "the landing is refused rather than falling back to the content"
    );
    assert!(
        v.toast.is_some(),
        "and it says so, rather than doing nothing visible"
    );
    assert!(v.context_tasks_stale, "the surfaces are asked again");
    assert!(
        v.pending_task_open.is_none(),
        "the latch is spent either way"
    );
}

/// **The latch names one node.** A search hit, a pill or Back landing on some
/// other row is not a task open and is never asked about — otherwise one
/// pending gesture would gate every navigation behind it.
#[test]
fn a_landing_this_window_did_not_ask_for_as_a_task_is_never_refused() {
    let (mut v, mut w, task) = on_a_context_column();
    let document = koru_core::DocumentId::new();
    v.active_document = Some(document);
    assert!(v.apply(&w.ws, crate::panes::Outcome::RevealTask(task)));
    let _ = View::take_pane_request(&mut v);

    let other =
        w.ws.children(koru_core::schema::Parent::Node(
            v.active_view().root().expect("a rooted tab"),
        ))
        .expect("the rows")
        .into_iter()
        .find(|id| *id != task)
        .expect("a second row");
    koru_core::tasks::remove_from_tasks(&mut w.ws, task).expect("it is removed from tasks");

    assert!(
        View::open_search_target(&mut v, &w.ws, None, Some(other), None, None),
        "a different row lands normally"
    );
    assert!(v.toast.is_none());
}

// ------------------------------------------------------------- the rollover (V-5)

/// **Midnight asks every task surface again**, through the one door a mutation
/// already uses — and it is a function of the day it is handed, so the tick can
/// be driven rather than waited for.
#[test]
fn a_rollover_asks_every_task_surface_again() {
    let (mut v, mut w, _) = on_a_context_column();
    View::maintain(&mut v, &mut w.ws, std::time::Instant::now());
    let today = chrono::Local::now().date_naive();
    assert!(
        !v.day_rolled(today),
        "the same day is not a rollover, however many ticks it takes"
    );
    assert!(!v.context_tasks_stale, "so nothing was invalidated");
    let tomorrow = today.succ_opt().expect("a real day");
    assert!(v.day_rolled(tomorrow));
    assert!(
        v.context_tasks_stale,
        "every bucket edge moved, so every reading of them is owed again"
    );
    assert!(
        !v.day_rolled(tomorrow),
        "once, not on every tick for the rest of the day"
    );
}

// ------------------------------------------------------ managing a list (V-6)
//
// Rename, Colour and Delete on a rail list row. The rail reports the
// right-click and hands over its geometry; everything a person touches after
// that — the menu, the field, the confirmation — is the container's, so this is
// where all four gestures are pinned.

/// A container on the tasks rail with one list in its table, and that list's id.
fn on_a_list() -> (MainView, Workspace, koru_core::ids::ListId) {
    let (mut v, w) = on_tasks();
    let id = koru_core::ids::ListId::new();
    View::set_list_table(
        &mut v,
        &[koru_core::system::ListEntry {
            id,
            name: "Wedding".into(),
            color: Some("blue".into()),
            order: None,
            created_ms: Some(1),
            deleted_ms: None,
        }],
    );
    (v, w, id)
}

/// Where the list's row is, from the same arithmetic that draws it.
fn list_row(v: &MainView) -> koru_draw::geometry::Rect {
    rail_row(v, crate::panes::explore::rail::RailRow::List(0))
}

/// **Right-click a list, get its verbs.** The menu is about the row under the
/// pointer: the three things SPEC §8 says a list has, and above them the one
/// thing you can do to any rail row — open it in a tab of its own.
#[test]
fn right_clicking_a_list_opens_its_menu() {
    let (mut v, mut w, id) = on_a_list();
    let (x, y) = list_row(&v).centre();
    assert!(View::context_menu(&mut v, &mut w.ws, x, y));
    let menu = v.menu.as_ref().expect("a menu");
    assert_eq!(menu.subject, MenuSubject::List(id));
    assert_eq!(
        menu.items(),
        &[
            MenuAction::RailOpenInNewTab,
            MenuAction::Divider,
            MenuAction::ListRename,
            MenuAction::ListColor,
            MenuAction::Divider,
            MenuAction::ListDelete,
        ]
    );
    assert!(
        MenuAction::ListDelete.destructive(),
        "and the one that does not come back is the red one"
    );
    // Every row is live while the list is, and dead the moment it is not — the
    // table is a cache of a document another window writes.
    assert!(v.enabled_items(menu).iter().any(|on| *on));
    let stale = ContextMenu::new(
        &mut v.engine,
        MenuSubject::List(koru_core::ids::ListId::new()),
        (0.0, 0.0),
        context_menu::list_items(),
    );
    assert!(
        v.enabled_items(&stale).iter().all(|on| !on),
        "a list that has gone offers nothing"
    );
}

/// **A right-click on a task row opens its menu on that press.**
///
/// The Tasks tab answers a context menu by *asking* for one — the row is the
/// view's, the menu is the container's — and that ask sits in `pending` until a
/// drain runs. `mouse_down`, `mouse_up` and `key` all end in `drain_trash_all`
/// and `context_menu` did not, so the menu appeared on whatever input came
/// next rather than on the click that asked for it.
#[test]
fn right_clicking_a_task_row_opens_its_menu_on_that_press() {
    let (mut v, mut w, document, node) = on_a_task_row(Vec::new());
    let content = v.focused_view_rect();
    let local = {
        let TabView::Tasks(tab) = &v.tabs().active_tab().view else {
            panic!("a Tasks tab is open");
        };
        tab.task_row_rect_for_test(0)
            .expect("the seeded row is drawn")
            .centre()
    };
    assert!(View::context_menu(
        &mut v,
        &mut w.ws,
        content.x + local.0,
        content.y + local.1
    ));
    let menu = v.menu.as_ref().expect("the row's menu, on this press");
    assert_eq!(menu.subject, MenuSubject::Task { document, node });
}

/// **A right-click on a queue opens one row.** It used to open nothing, on the
/// grounds that a computed queue has nothing to rename, recolour or delete
/// — true, and one step too broad: *Open in new tab* is a thing you can do to
/// a queue, and withholding the menu withheld that too.
#[test]
fn right_clicking_a_bucket_offers_the_one_thing_you_can_do_to_it() {
    {
        use crate::panes::explore::rail::{Bucket, RailRow, RailTarget};
        let (mut v, mut w, _id) = on_a_list();
        let (x, y) = rail_row(&v, RailRow::Bucket(Bucket::Later)).centre();
        assert!(View::context_menu(&mut v, &mut w.ws, x, y));
        let menu = v.menu.as_ref().expect("a menu");
        assert_eq!(
            menu.subject,
            MenuSubject::RailTarget(RailTarget::Bucket(Bucket::Later))
        );
        assert_eq!(menu.items(), &[MenuAction::RailOpenInNewTab]);

        // And the furniture still opens nothing, rather than falling through to the
        // library menu the other two modes raise.
        let (x, y) = rail_row(&v, RailRow::NewList).centre();
        v.menu = None;
        View::context_menu(&mut v, &mut w.ws, x, y);
        assert!(v.menu.is_none());
    }
}

/// **Opening a second page leaves the one in front where it is**, which is the
/// whole meaning of *in a new tab* — and it is the rule the rail's
/// one-selection invariant used to forbid outright. A plain press still
/// re-points, so the card and the page in front still agree.
#[test]
fn a_new_tab_from_the_rail_parks_a_page_without_leaving_this_one() {
    {
        use crate::panes::explore::rail::{Bucket, RailTarget};
        let (mut v, mut w) = on_tasks();
        v.open_tasks_tab(RailTarget::Bucket(Bucket::Today), false);
        let front = v.tabs().active();
        let before = v.tabs().len();

        assert!(v.run_menu_action_for_test(
            MenuSubject::RailTarget(RailTarget::Bucket(Bucket::Later)),
            MenuAction::RailOpenInNewTab
        ));
        assert_eq!(
            v.tabs().len(),
            before + 1,
            "a second Tasks tab, not a re-point"
        );
        assert_eq!(v.tabs().active(), front, "and you are still on the first");
        assert_eq!(v.tabs().active_tab().label, "Today");

        // A plain press re-points the tab in front rather than reaching past it to
        // whichever Tasks tab was opened first.
        v.open_tasks_tab(RailTarget::Bucket(Bucket::Upcoming), false);
        assert_eq!(v.tabs().len(), before + 1, "nothing new");
        assert_eq!(v.tabs().active(), front);
        assert_eq!(v.tabs().active_tab().label, "Upcoming");
        let _ = &mut w;
    }
}

/// **Rename round-trips**: the field opens over the row filled and selected, and
/// what leaves the container is the id and the new name.
#[test]
fn renaming_a_list_happens_on_its_own_row() {
    let (mut v, mut w, id) = on_a_list();
    v.run_menu_action_for_test(MenuSubject::List(id), MenuAction::ListRename);
    assert_eq!(
        v.explore.rail().renaming(),
        Some(id),
        "the rail reserves the row"
    );
    let field = v.new_list_field_rect().expect("a field over the row");
    assert_eq!(
        field.y,
        list_row(&v).y,
        "and it is that row's own rectangle"
    );

    // Selected on arrival, so typing replaces rather than appends.
    for ch in "Bike".chars() {
        let text = ch.to_string();
        v.key(
            &mut w.ws,
            &Key::Character(text.clone().into()),
            Some(&text),
            NONE,
        );
    }
    assert!(
        View::take_wants_list_edits(&mut v).is_empty(),
        "nothing leaves until Enter"
    );
    v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );
    assert_eq!(
        View::take_wants_list_edits(&mut v),
        vec![koru_workspace_host::tasks::ListEdit::Rename {
            id,
            name: "Bike".into()
        }]
    );
    assert_eq!(v.explore.rail().renaming(), None, "and the field is gone");
}

/// Escape leaves the name alone, and puts the row back.
#[test]
fn escape_cancels_a_rename() {
    let (mut v, mut w, id) = on_a_list();
    v.run_menu_action_for_test(MenuSubject::List(id), MenuAction::ListRename);
    for ch in "Bike".chars() {
        let text = ch.to_string();
        v.key(
            &mut w.ws,
            &Key::Character(text.clone().into()),
            Some(&text),
            NONE,
        );
    }
    v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Escape),
        None,
        NONE,
    );
    assert_eq!(v.explore.rail().renaming(), None);
    assert!(View::take_wants_list_edits(&mut v).is_empty());
}

/// Renaming a list to the name it already has is not a duplicate of itself.
#[test]
fn a_rename_does_not_clash_with_its_own_name() {
    let (mut v, mut w, id) = on_a_list();
    v.run_menu_action_for_test(MenuSubject::List(id), MenuAction::ListRename);
    v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );
    assert_eq!(
        View::take_wants_list_edits(&mut v),
        vec![koru_workspace_host::tasks::ListEdit::Rename {
            id,
            name: "Wedding".into()
        }]
    );
}

/// **A swatch is chosen from the shared set and nothing else.** The row's colour
/// and the membership chip's both read the list table, so one push moves both.
#[test]
fn recoloring_a_list_moves_the_row_and_the_chip() {
    let (mut v, _w, id) = on_a_list();
    assert_eq!(v.explore.rail().lists()[0].color.as_deref(), Some("blue"));
    assert_eq!(
        context_menu::list_color_items().len(),
        koru_draw::theme::ACCENT_TOKENS.len(),
        "one row per token in the accent set, and no tenth colour"
    );
    v.run_menu_action_for_test(MenuSubject::List(id), MenuAction::ListSwatch("teal"));
    assert_eq!(
        View::take_wants_list_edits(&mut v),
        vec![koru_workspace_host::tasks::ListEdit::Recolor {
            id,
            color: Some("teal".into())
        }]
    );

    // What comes back from the shell is the table, and the rail and the Tasks
    // tab's `ListFace` both read it.
    View::set_list_table(
        &mut v,
        &[koru_core::system::ListEntry {
            id,
            name: "Wedding".into(),
            color: Some("teal".into()),
            order: None,
            created_ms: Some(1),
            deleted_ms: None,
        }],
    );
    assert_eq!(v.explore.rail().lists()[0].color.as_deref(), Some("teal"));
    assert_eq!(
        v.list_table[0].color.as_deref(),
        Some("teal"),
        "and the chip's own source with it"
    );
}

/// **Deleting cannot reach the shell without its confirmation**, and the
/// question says the thing a person is actually afraid of: the tasks stay.
#[test]
fn deleting_a_list_goes_through_the_confirm() {
    let (mut v, mut w, id) = on_a_list();
    v.run_menu_action_for_test(MenuSubject::List(id), MenuAction::ListDelete);
    assert!(
        View::take_wants_list_edits(&mut v).is_empty(),
        "the row asks a question rather than doing anything"
    );
    assert!(v.confirm.is_some(), "…and the question is on screen");

    // **Enter is Cancel** — the destructive answer has no default, which is the
    // whole reason `Confirm` exists rather than a third `ErrorDialog`.
    v.key(
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Enter),
        None,
        NONE,
    );
    assert!(v.confirm.is_none(), "the dialog went");
    assert!(
        View::take_wants_list_edits(&mut v).is_empty(),
        "and took the deletion with it"
    );

    v.run_menu_action_for_test(MenuSubject::List(id), MenuAction::ListDelete);
    v.settle_confirm(true);
    assert_eq!(
        View::take_wants_list_edits(&mut v),
        vec![koru_workspace_host::tasks::ListEdit::Delete(id)]
    );
}

/// **A Settings tab opened an hour in shows what is stored, not the defaults.**
///
/// Almost everything that page draws reads a process global, so a change made
/// elsewhere repaints it for free. The handful it keeps a *field* of do not —
/// and before the container held them, a tab minted later showed the defaults
/// and then wrote them straight back over the real values on its next capture.
#[test]
fn a_settings_tab_opened_later_starts_from_what_is_stored() {
    let mut view = MainView::new();
    View::adopt_preferences(
        &mut view,
        &Settings {
            close_behavior: koru_window::CloseBehavior::Quit,
            pinned_show_title: true,
            ..Settings::default()
        },
    );

    assert!(view.open_settings_tab());
    let opened = view
        .tabs()
        .iter()
        .find_map(|tab| match &tab.view {
            TabView::Settings(settings) if tab.label == "Settings" => Some(settings),
            _ => None,
        })
        .expect("the Settings tab was opened");
    assert_eq!(opened.close_behavior(), koru_window::CloseBehavior::Quit);
    assert!(opened.pinned_show_title());
}

/// `Ctrl+,` is a keyboard's way into Settings, so it lands the keyboard in
/// the search box: the next thing typed is what to find. Pressed again over
/// an open Settings tab it returns there and focuses the box again, and
/// opens no second tab.
#[test]
fn the_settings_chord_opens_the_tab_with_its_search_box_focused() {
    let (mut view, mut workspace) = container();
    assert!(view.key(&mut workspace.ws, &ch(","), Some(","), PRIMARY));
    let focused = |view: &MainView| match &view.tabs().active_tab().view {
        TabView::Settings(settings) => settings.search_focused(),
        _ => panic!("the Settings tab is not active"),
    };
    assert!(focused(&view), "the search box has the keyboard");
    assert!(view.key(&mut workspace.ws, &ch(","), Some(","), PRIMARY));
    assert!(focused(&view));
    assert_eq!(
        view.tabs()
            .iter()
            .filter(|tab| matches!(tab.view, TabView::Settings(_)))
            .count(),
        1,
        "one Settings tab"
    );
}

/// A document's diagram viewer is modal over the whole window: the chrome
/// around its pane is dimmed from the container, and a press on that chrome
/// goes to the viewer as a press on its scrim — so it closes, and the strip
/// under the press does nothing. The sidebar and the context column used to
/// show through, bright and clickable, beside the dimmed page.
#[test]
fn a_documents_diagram_viewer_takes_the_whole_window() {
    let (mut view, mut workspace) = container();
    let (leaf, node) = crate::leaf::tests::leaf_with_a_drawing(&mut workspace.ws, (400.0, 300.0));
    view.tabs_mut().push(Tab::document(leaf, "Diagrams"));
    assert!(
        view.diagram_viewer_pane().is_none(),
        "nothing to dim while the viewer is down"
    );
    let TabView::Leaf(Leaf::Document(document)) = &mut view.tabs_mut().active_mut().view else {
        panic!("the document is active");
    };
    assert!(document.open_diagram_viewer(node));
    let pane = view.diagram_viewer_pane().expect("the viewer's pane");
    assert!(
        pane.y > 0.0,
        "there is chrome above the pane to dim: {pane:?}"
    );
    // Lifted: the viewer lays itself out over the whole window.
    view.sync_viewer_lift();
    let TabView::Leaf(Leaf::Document(document)) = &view.tabs().active_tab().view else {
        panic!("the document is active");
    };
    assert_eq!(document.diagram_viewer().unwrap().viewport(), view.viewport);
    // The title row's Context and AI buttons are painted after the pane's
    // content, which put them over the viewer's scrim; while the viewer is
    // up there are none to paint or to press.
    assert!(view.document_context_button().is_none());
    assert!(view.document_ai_button().is_none());
    // A press on that chrome reaches the viewer, not the strip, and closes
    // it on release — the way a press on the dimmed page does.
    let tabs_before = view.tabs().len();
    assert!(view.mouse_down(&mut workspace.ws, pane.x + 10.0, pane.y - 1.0, false));
    view.mouse_up(&mut workspace.ws);
    assert_eq!(view.tabs().len(), tabs_before, "the strip was not pressed");
    assert!(view.diagram_viewer_pane().is_none(), "the viewer closed");
}

/// The comment on `adopt_preferences` says it reaches every Settings tab. When
/// Settings split out of Welcome, the fan-out still only named Welcome, so an
/// already-open Settings tab kept showing the values it was minted with.
#[test]
fn an_open_settings_tab_adopts_a_preference_changed_elsewhere() {
    let mut view = MainView::new();
    assert!(view.open_settings_tab());
    View::adopt_preferences(
        &mut view,
        &Settings {
            close_behavior: koru_window::CloseBehavior::Quit,
            pinned_show_title: true,
            ..Settings::default()
        },
    );
    let opened = view
        .tabs()
        .iter()
        .find_map(|tab| match &tab.view {
            TabView::Settings(settings) if tab.label == "Settings" => Some(settings),
            _ => None,
        })
        .expect("the Settings tab is still open");
    assert_eq!(opened.close_behavior(), koru_window::CloseBehavior::Quit);
    assert!(opened.pinned_show_title());
}

#[test]
fn feedback_opens_as_a_singleton_tab() {
    crate::feedback::draft::reset();
    let mut view = MainView::new();
    let labels: Vec<String> = view.tabs().iter().map(|t| t.label.clone()).collect();
    assert!(
        view.open_send_feedback_tab(),
        "open failed with tabs {labels:?}"
    );
    let first = view
        .tabs()
        .iter()
        .filter(|tab| matches!(tab.view, TabView::Feedback(_)) && tab.label == "Feedback")
        .count();
    assert_eq!(first, 1);
    assert!(view.open_send_feedback_tab());
    let second = view
        .tabs()
        .iter()
        .filter(|tab| matches!(tab.view, TabView::Feedback(_)))
        .count();
    assert_eq!(second, 1, "a second click selects the existing tab");
}

#[test]
fn a_feedback_tab_is_wide_enough_for_its_title_and_icon() {
    crate::feedback::draft::reset();
    let mut view = MainView::new();
    assert!(view.open_send_feedback_tab());
    view.ensure_tab_widths();
    let i = view
        .tabs()
        .iter()
        .position(|tab| matches!(tab.view, TabView::Feedback(_)))
        .expect("the Feedback tab");
    let tab = view.tabs().iter().nth(i).expect("the Feedback tab");
    assert!(tab.view.icon().is_some());
    assert!(tab.icon.is_none(), "a place glyph, not a title emoji");
    let mut engine = koru_draw::text::TextEngine::new();
    let with_icon = koru_shell_chrome::tabs::preferred_width(&mut engine, "Feedback", true, true);
    let without_icon =
        koru_shell_chrome::tabs::preferred_width(&mut engine, "Feedback", false, true);
    assert_eq!(view.tab_widths()[i], with_icon);
    assert!(
        with_icon > without_icon,
        "the user-sound slot is in the measured width"
    );
}

// ------------------------------------------------------- the receipt (FRESHNESS D)

/// The Tasks tab's panel, for reading what it is waiting on.
fn tasks_panel(v: &mut MainView) -> &mut crate::tasks_view::query::TaskListPanel {
    let tasks = v
        .tabs()
        .position(|tab| matches!(tab.view, TabView::Tasks(_)))
        .expect("a Tasks tab");
    let Some(Tab {
        view: TabView::Tasks(page),
        ..
    }) = v.tabs_mut().get_mut(tasks)
    else {
        panic!("the Tasks tab is still there")
    };
    page.panel_mut()
}

fn a_receipt(
    w: &Workspace,
    document: koru_core::DocumentId,
    published: bool,
) -> koru_workspace_host::tasks::Landed {
    koru_workspace_host::tasks::Landed {
        workspace: w.ws.id(),
        document,
        node: None,
        caret: None,
        // The real session and its real progress, so a test asserting the
        // receipt is asserting the thing the shell would actually send.
        receipt: Some(koru_workspace::tasks::WriteReceipt {
            peer: w.ws.peer_id(),
            counter: i64::from(w.ws.version().get(&w.ws.peer_id()).copied().unwrap_or(0)),
        }),
        published,
    }
}

/// **Taking the commands marks nothing; the receipt does.** The mark used to
/// be made when the queue was taken — before the shell had run anything — so
/// the page's next question was caused by the intention to write and answered
/// from a projection that had not seen the write. Now the take is a take, and
/// the surfaces learn of the write from its receipt, which names the document
/// and its version so they can wait for the read model to include it.
#[test]
fn a_landed_command_is_what_the_surfaces_wait_for() {
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, w, document, node) = on_a_task_row(Vec::new());
    v.queue_task_commands_for_test(vec![TaskCommand::Complete {
        document,
        node,
        completed: true,
    }]);

    let mut pane = crate::manager_view::ManagerView::Container(v);
    assert_eq!(View::take_wants_task_commands(&mut pane).len(), 1);
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    assert!(
        v.task_counts.wanted_for_test().is_empty() && tasks_panel(v).wanted_for_test().is_empty(),
        "taking the queue is not a write, so nothing waits on one yet"
    );

    let landed = a_receipt(&w, document, true);
    View::task_command_landed(&mut pane, &landed);
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    let expected = koru_workspace::explore::store_document(document);
    let rail = v.task_counts.wanted_for_test();
    assert_eq!(rail.len(), 1, "the rail waits on the write");
    assert_eq!(rail[0].document, expected);
    let peer = w.ws.peer_id();
    assert_eq!(rail[0].peer, peer, "the session that wrote");
    assert_eq!(
        rail[0].counter,
        i64::from(w.ws.version().get(&peer).copied().unwrap_or(0)),
        "at the operation it landed on"
    );
    let page = tasks_panel(v).wanted_for_test();
    assert_eq!(page.len(), 1, "and so does the page");
    assert_eq!(page[0].document, expected);
    assert!(
        v.context_tasks_stale,
        "the column's section is owed a rebuild too"
    );
}

/// A receipt the store was not told about — a stopped session — invalidates
/// and waits for nothing: a wait on a store that has not heard of the write
/// would burn its whole budget every time. Local-first is an ordinary state.
#[test]
fn an_unpublished_receipt_invalidates_without_a_requirement() {
    let (mut v, w, document, _) = on_a_task_row(Vec::new());
    v.context_tasks_stale = false;
    let mut pane = crate::manager_view::ManagerView::Container(v);
    View::task_command_landed(&mut pane, &a_receipt(&w, document, false));
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    assert!(v.task_counts.wanted_for_test().is_empty());
    assert!(tasks_panel(v).wanted_for_test().is_empty());
    assert!(
        v.context_tasks_stale,
        "still asked again — once, on the ordinary schedule"
    );
}

/// The rollover and a stale navigation are not writes, and fabricating a
/// vector for them would be inventing a wait nothing can satisfy.
#[test]
fn the_rollover_waits_for_nothing() {
    let (mut v, _w, _, _) = on_a_task_row(Vec::new());
    let today = chrono::Local::now().date_naive();
    v.day_rolled(today);
    v.day_rolled(today + chrono::Duration::days(1));
    v.tasks_changed();
    assert!(v.task_counts.wanted_for_test().is_empty());
    assert!(tasks_panel(&mut v).wanted_for_test().is_empty());
}

/// **The editor's own gesture is the second producer**, and the one finding
/// 27 was reported through: a checkbox in an open document. The sweep holds
/// the written document, so the requirement is that document's version — at
/// least the write, read where it is borrowed — not a vector invented from an
/// intent.
#[test]
fn a_checkbox_in_a_document_becomes_a_requirement_at_least_the_write() {
    use koru_core::time::{DateOnly, Temporal};
    use koru_draw::{geometry, theme};

    let (mut v, mut workspace, _, _) = on_a_task_row(Vec::new());
    let leaf = v
        .tabs()
        .position(|tab| matches!(tab.view, TabView::Leaf(_)))
        .expect("the document this window opened on");
    assert!(v.tabs_mut().select(leaf));
    let doc = v.document_root().expect("a document tab is showing");
    let item = workspace
        .ws
        .insert(Parent::Node(doc), Node::task("ship it").unwrap())
        .expect("insert");
    koru_core::tasks::set_due(
        &mut workspace.ws,
        item,
        Temporal::Date(DateOnly::parse("2026-09-01").unwrap()),
    )
    .expect("due");
    v.adopt(&workspace.ws);
    let before = workspace.ws.version();

    let TabView::Leaf(leaf_view) = &v.tabs().active_tab().view else {
        panic!("a document tab holds a leaf")
    };
    let row = leaf_view
        .body()
        .rows
        .iter()
        .find(|r| r.id == item)
        .expect("the task has a row");
    let view = v.focused_view_rect();
    let x = view.x + geometry::checkbox_x(row.depth) + theme::CHECKBOX_SIZE / 2.0;
    let y = view.y + row.y + theme::ROW_H / 2.0;

    let mut pane = crate::manager_view::ManagerView::Container(v);
    assert!(
        koru_workspace_host::View::mouse_down(&mut pane, &mut workspace.ws, x, y, false),
        "the click landed on the checkbox"
    );
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };

    let expected = koru_workspace::explore::store_document(
        workspace
            .ws
            .own_document_id()
            .expect("the document knows its id"),
    );
    let rail = v.task_counts.wanted_for_test();
    assert_eq!(rail.len(), 1, "the rail waits on the checkbox's write");
    assert_eq!(rail[0].document, expected);
    let peer = workspace.ws.peer_id();
    let before_own = i64::from(before.get(&peer).copied().unwrap_or(0));
    assert_eq!(rail[0].peer, peer);
    assert!(
        rail[0].counter > before_own,
        "at least the write: this session's own operations moved and the          requirement moved with them"
    );
    assert_eq!(
        rail[0].counter,
        i64::from(workspace.ws.version().get(&peer).copied().unwrap_or(0))
    );
    assert_eq!(
        tasks_panel(v).wanted_for_test().len(),
        1,
        "and so does the page"
    );
}

// ------------------------------------------ Due… and Priority… act in place (INPLACE)

/// The Tasks tab, for what its pickers are showing.
fn tasks_tab(v: &mut MainView) -> &mut crate::tasks_view::TasksView {
    let tasks = v
        .tabs()
        .position(|tab| matches!(tab.view, TabView::Tasks(_)))
        .expect("a Tasks tab");
    let Some(Tab {
        view: TabView::Tasks(page),
        ..
    }) = v.tabs_mut().get_mut(tasks)
    else {
        panic!("the Tasks tab is still there")
    };
    page
}

/// **The whole hop, as one gesture, and nowhere else.** *Due…* opens the
/// calendar over the row on this page, a day pressed on it leaves as a
/// request, the container turns that into the ordinary task command — and the
/// active tab is still the Tasks tab, because setting a date is not a journey
/// (finding 11).
#[test]
fn the_due_row_opens_the_calendar_on_the_page_and_leaves_as_a_command() {
    use koru_core::time::{DateOnly, Temporal};
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, mut w, document, node) = on_a_task_row(Vec::new());

    assert!(v.run_menu_action_for_test(MenuSubject::Task { document, node }, MenuAction::TaskDue));
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Tasks(_)),
        "still on the Tasks page: nothing was opened"
    );
    assert!(
        View::take_wants_task_commands(&mut v).is_empty(),
        "opening a picker changes nothing"
    );
    let (selected, _, panel) = tasks_tab(&mut v)
        .due_picker_for_test()
        .expect("the calendar is up over the row");
    let cell = koru_ui::date_grid::cell_rect(selected, 10);
    let c = v.focused_view_rect();
    v.mouse_down(
        &mut w.ws,
        c.x + panel.x + cell.x + cell.w * 0.5,
        c.y + panel.y + cell.y + koru_ui::date_grid::field_band() + cell.h * 0.5,
        false,
    );
    let day = chrono::NaiveDate::from_ymd_opt(
        chrono::Datelike::year(&selected),
        chrono::Datelike::month(&selected),
        10,
    )
    .unwrap();
    assert_eq!(
        View::take_wants_task_commands(&mut v),
        vec![TaskCommand::SetDue {
            document,
            node,
            due: Some(Temporal::Date(DateOnly::new(day))),
        }]
    );
    assert!(matches!(v.tabs().active_tab().view, TabView::Tasks(_)));
}

#[test]
fn the_priority_row_opens_the_picker_on_the_page_and_leaves_as_a_command() {
    use koru_workspace_host::tasks::TaskCommand;
    let (mut v, mut w, document, node) = on_a_task_row(Vec::new());
    assert!(v.run_menu_action_for_test(
        MenuSubject::Task { document, node },
        MenuAction::TaskPriority
    ));
    let r = tasks_tab(&mut v)
        .picker_row_rect_for_test(0)
        .expect("the first level");
    let c = v.focused_view_rect();
    v.mouse_down(
        &mut w.ws,
        c.x + r.x + r.w * 0.5,
        c.y + r.y + r.h * 0.5,
        false,
    );
    assert_eq!(
        View::take_wants_task_commands(&mut v),
        vec![TaskCommand::SetPriority {
            document,
            node,
            priority: Some(koru_core::attributes::Priority::Urgent),
        }]
    );
    assert!(matches!(v.tabs().active_tab().view, TabView::Tasks(_)));
}

/// *Due…* over a row this page is not showing does nothing at all.
#[test]
fn the_due_row_refuses_a_task_the_page_does_not_hold() {
    let (mut v, _w, _document, _node) = on_a_task_row(Vec::new());
    assert!(!v.run_menu_action_for_test(
        MenuSubject::Task {
            document: koru_core::DocumentId::new(),
            node: NodeId::new(),
        },
        MenuAction::TaskDue,
    ));
    assert!(!v.run_menu_action_for_test(
        MenuSubject::Task {
            document: koru_core::DocumentId::new(),
            node: NodeId::new(),
        },
        MenuAction::TaskPriority,
    ));
}

// ------------------------------------------------ View in Tasks, from a document (32)

/// A task in the open document, with the caret on it.
fn a_focused_document_task() -> (MainView, Workspace, NodeId) {
    let (mut v, mut w) = on_document();
    let doc = v.document_root().expect("a document tab is showing");
    let task =
        w.ws.insert(Parent::Node(doc), Node::task("ship it").unwrap())
            .expect("insert");
    koru_core::tasks::set_due(
        &mut w.ws,
        task,
        koru_core::time::Temporal::Date(koru_core::time::DateOnly::parse("2026-09-01").unwrap()),
    )
    .expect("due");
    v.adopt(&w.ws);
    if let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view {
        Editing::body_mut(leaf).focus_on(task);
    }
    (v, w, task)
}

/// **A task followed to its document is flashed where it lands**, the way a
/// global-search hit is: the Tasks page's open — Enter, a double click, *View
/// in origin* — asks for the paragraph with the empty match that means *the
/// whole row*, and both landings, in place and in a tab of its own, take the
/// search result's reveal.
#[test]
fn a_task_followed_to_its_document_is_flashed_where_it_lands() {
    let (mut v, w, task) = a_focused_document_task();
    let document = koru_core::DocumentId::from_uuid(v.document_root().unwrap().as_uuid());
    assert!(v.open_task_node(document, task, true));
    assert!(
        matches!(
            View::take_pane_request(&mut v),
            Some(koru_workspace_host::PaneRequest::Navigate {
                match_text: Some(ref cue),
                ..
            }) if cue.is_empty()
        ),
        "the whole-row cue travels with the ask"
    );
    // In place.
    assert!(v.land_on(&w.ws, task, true, Some("")));
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("a document tab")
    };
    assert!(leaf.view().search_flash.is_some(), "flashed in place");

    // In a tab of its own — *View in origin*'s disposition.
    let (mut v, w, task) = a_focused_document_task();
    let root = v.document_root().unwrap();
    let document = koru_core::DocumentId::from_uuid(root.as_uuid());
    v.active_document = Some(document);
    v.pending_lands.push_back((document, OpenWhere::NewTab));
    let tabs = v.tabs().len();
    assert!(v.land_on(&w.ws, task, true, Some("")));
    assert_eq!(v.tabs().len(), tabs + 1, "a tab of its own");
    let TabView::Leaf(leaf) = &v.tabs().active_tab().view else {
        panic!("a document tab")
    };
    assert!(leaf.view().search_flash.is_some(), "flashed there too");
}

/// **The chip's *View in Tasks* is drained after the dispatch that chose it**,
/// not only when a document arrival happened to run the explore gestures.
/// The ask goes in through the pane the shell holds, and the next dispatch —
/// any key — is what lands it.
#[test]
fn view_in_tasks_from_an_open_document_lands_on_its_own_dispatch() {
    let (mut v, mut w, task) = a_focused_document_task();
    if let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view {
        Editing::parts(leaf).1.wants_task_view = Some(task);
    }
    let mut pane = crate::manager_view::ManagerView::Container(v);
    koru_workspace_host::View::key(
        &mut pane,
        &mut w.ws,
        &Key::Named(winit::keyboard::NamedKey::Shift),
        None,
        ModifiersState::empty(),
    );
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Tasks(_)),
        "the Tasks page came forward without any other gesture pending"
    );
    assert!(v.explore.showing_tasks(), "and the column is on the rail");
}

/// **`Ctrl+Shift+E` with the caret on a task reveals the task**, by the same
/// path the chip's row takes — which is why that row teaches this chord.
/// Anywhere else the chord is the pane toggle it always was.
#[test]
fn ctrl_shift_e_on_a_task_row_reveals_it_on_the_tasks_page() {
    let (mut v, w, _) = a_focused_document_task();
    assert!(v.focus_or_toggle_sidebar(&w.ws));
    assert!(matches!(v.tabs().active_tab().view, TabView::Tasks(_)));
    assert!(v.sidebar.is_open() && v.explore.showing_tasks());

    // On a plain row the chord is the toggle: the column shuts.
    let (mut v, mut w) = on_document();
    let doc = v.document_root().expect("a document");
    let row =
        w.ws.insert(Parent::Node(doc), Node::paragraph("a line").unwrap())
            .expect("insert");
    v.adopt(&w.ws);
    if let TabView::Leaf(leaf) = &mut v.tabs_mut().active_mut().view {
        Editing::body_mut(leaf).focus_on(row);
    }
    let before = v.sidebar.is_open();
    v.focus_or_toggle_sidebar(&w.ws);
    assert!(
        matches!(v.tabs().active_tab().view, TabView::Leaf(_)),
        "a plain row opens no Tasks page"
    );
    let _ = before;
}

/// **The number beside the queue moves with the row.** A tick on the page
/// computes what it will do to the rail's numbers, and the receipt applies it
/// — before the store has counted anything.
#[test]
fn a_tick_moves_the_rails_number_when_its_receipt_lands() {
    use koru_store_wire::tasks::TaskGroup;
    let (mut v, w, document, node) = on_a_task_row(Vec::new());
    let t0 = std::time::Instant::now();
    v.task_counts.maintain(w.ws.id(), true, t0);
    let counted = |list: &str, count: usize| koru_store_wire::tasks::TaskQueryResponse {
        rows: Vec::new(),
        definitions: Vec::new(),
        tags: Vec::new(),
        groups: vec![TaskGroup {
            workspace: koru_store_core::ids::WorkspaceId::new(),
            list: list.to_owned(),
            count,
            overdue: 0,
            trashed: 0,
        }],
        projected: true,
        updating_workspaces: Vec::new(),
        limit: 1,
        next_after: None,
        restarted: false,
        covered: None,
    };
    v.task_counts
        .accept_carrying_for_test(&counted("later", 1), &counted("x", 0), &[], t0);
    assert_eq!(v.task_counts.counts().later, 1, "the dateless row is Later");

    assert!(v.run_menu_action_for_test(
        MenuSubject::Task { document, node },
        MenuAction::TaskComplete
    ));
    let mut pane = crate::manager_view::ManagerView::Container(v);
    assert_eq!(View::take_wants_task_commands(&mut pane).len(), 1);
    View::task_command_landed(&mut pane, &a_receipt(&w, document, true));
    let crate::manager_view::ManagerView::Container(v) = &mut pane else {
        panic!("container")
    };
    let counts = v.task_counts.counts();
    assert_eq!(
        (counts.later, counts.completed),
        (0, 1),
        "moved on the receipt"
    );
    assert_eq!(v.explore.rail().counts().later, 0, "and the rail was told");
}

/// **Selecting a session in the Agents navigator opens or focuses** (W4.2).
/// The first selection opens a tab bound to the existing session — an attach,
/// never a launch: `session` set, no pending launch. Selecting the same
/// session again focuses the tab it already has rather than opening a second
/// claim on the window's one controller.
#[test]
fn the_agents_navigator_opens_or_focuses_a_session_tab() {
    let (mut v, workspace) = container();
    let session = koru_session_protocol::SessionId::mint();
    let before = v.tabs().len();

    assert!(v.apply(&workspace.ws, Outcome::OpenSession(session)));
    assert_eq!(v.tabs().len(), before + 1, "a tab opened");
    {
        let TabView::Session(view) = &v.tabs().active_tab().view else {
            panic!("the new tab is a session tab")
        };
        assert_eq!(view.session, Some(session), "bound to the existing session");
        assert!(view.pending_launch.is_none(), "an attach, never a launch");
    }

    // Switch away and choose the same session again: focused, not duplicated.
    v.select(0);
    assert!(!matches!(v.tabs().active_tab().view, TabView::Session(_)));
    assert!(v.apply(&workspace.ws, Outcome::OpenSession(session)));
    assert_eq!(
        v.tabs().len(),
        before + 1,
        "no second tab for the same session"
    );
    let TabView::Session(view) = &v.tabs().active_tab().view else {
        panic!("the session tab is the active one again")
    };
    assert_eq!(view.session, Some(session));
}

// ==================================================== the editor's adopt path
//
// **One editor, two hosts, and they must not drift.** `koru-editor` is shown a
// document that changed underneath it — a peer's edit, or another window's
// typing — through exactly one call, `Editing::catch_up`. A single-view window
// reaches it from `Editing::adopt`; the container reaches it from
// `TabView::catch_up`, for the tab about to be drawn, having already drained
// every other tab.
//
// Until Q-B4 the container did not: it re-read the rows and stopped there, so a
// document open in a *tab* kept a picker hanging from a node a peer had deleted
// while the same document popped out into its own window closed it. The tests
// below drive **both hosts through one set of assertions** — that is the whole
// point of `AdoptHost` — so the two cannot separate again without a failure.
// ADR-343-overlaycatchup.

/// The two hosts of one editor, behind one name.
///
/// Deliberately tiny: everything the scenarios below need, and nothing that
/// would let one host be asserted about and the other quietly skipped.
trait AdoptHost {
    /// The editor underneath — whichever host is holding it.
    fn editor(&self) -> &koru_editor::Editor;

    /// Land the caret in a row, through the editor's own reveal.
    fn reveal(&mut self, row: NodeId);

    /// One key, as the shell delivers it.
    fn press(&mut self, ws: &mut WorkspaceDoc, key: Key, text: Option<&str>);

    /// **The call the shell makes when a change lands from somewhere else.**
    /// `MainView::adopt` for the container, `BuiltInDocument::adopt` for a
    /// window — the real entry points, not the shared step underneath them.
    fn arrive(&mut self, ws: &WorkspaceDoc);

    /// The three pieces of work a window-level adopt arranges: the title
    /// refresh, the search re-scan and the spelling observation.
    fn work(&self) -> koru_document::AdoptWork;

    /// The paragraphs under the title, in order.
    fn body_rows(&self) -> Vec<NodeId> {
        let editor = self.editor();
        editor
            .body
            .rows
            .iter()
            .enumerate()
            .filter(|(i, _)| !editor.body.is_title_row(*i))
            .map(|(_, row)| row.id)
            .collect()
    }

    /// Whether a picker is open over the document.
    fn overlay_open(&self) -> bool {
        self.editor().view.overlay.is_open()
    }

    /// What the editor last had to tell the person.
    fn notice(&self) -> Option<&str> {
        self.editor().view.notice.as_deref()
    }

    /// How many times this view has been laid out.
    fn reflows(&self) -> u64 {
        self.editor().view.reflows
    }

    /// Where the caret is in the focused row.
    fn caret(&self) -> usize {
        let editor = self.editor();
        editor.body.rows[editor.body.focus].caret()
    }
}

impl AdoptHost for MainView {
    fn editor(&self) -> &koru_editor::Editor {
        let TabView::Leaf(Leaf::Document(d)) = &self.tabs().active_tab().view else {
            panic!("the active tab is a document")
        };
        &d.editor
    }

    fn reveal(&mut self, row: NodeId) {
        let TabView::Leaf(l) = &mut self.tabs_mut().active_mut().view else {
            panic!("the active tab is a document")
        };
        assert!(Editing::reveal(l, row), "the editor would not reveal a row");
    }

    fn press(&mut self, ws: &mut WorkspaceDoc, key: Key, text: Option<&str>) {
        View::key(self, ws, &key, text, ModifiersState::empty());
    }

    fn arrive(&mut self, ws: &WorkspaceDoc) {
        View::adopt(self, ws);
    }

    fn work(&self) -> koru_document::AdoptWork {
        let TabView::Leaf(Leaf::Document(d)) = &self.tabs().active_tab().view else {
            panic!("the active tab is a document")
        };
        d.adopt_work()
    }
}

impl AdoptHost for koru_document::BuiltInDocument {
    fn editor(&self) -> &koru_editor::Editor {
        &self.editor
    }

    fn reveal(&mut self, row: NodeId) {
        assert!(
            Editing::reveal(self, row),
            "the editor would not reveal a row"
        );
    }

    fn press(&mut self, ws: &mut WorkspaceDoc, key: Key, text: Option<&str>) {
        View::key(self, ws, &key, text, ModifiersState::empty());
    }

    fn arrive(&mut self, ws: &WorkspaceDoc) {
        View::adopt(self, ws);
    }

    fn work(&self) -> koru_document::AdoptWork {
        self.adopt_work()
    }
}

/// A workspace holding one document of `texts`, and nothing else.
fn a_document_of(texts: &[&str]) -> (Workspace, NodeId) {
    let ws = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "t").expect("a workspace");
    let mut w = Workspace::new(ws);
    // Inserted directly rather than through `koru_workspace::document`, which
    // leaves an empty paragraph behind — a row nobody asked for is a row every
    // index below would be wrong by.
    let doc =
        w.ws.insert(Parent::Root, Node::document("Notes").unwrap())
            .expect("the document node");
    for text in texts {
        w.ws.insert(Parent::Node(doc), Node::paragraph(*text).unwrap())
            .expect("a paragraph");
    }
    (w, doc)
}

/// …shown in a container tab, which is how almost everybody uses Koru.
fn in_a_tab(texts: &[&str]) -> (MainView, Workspace) {
    let (w, doc) = a_document_of(texts);
    let mut v = MainView::new();
    v.set_viewport(1200.0, 700.0);
    v.set_pane_context(
        Vec::new(),
        koru_core::ProfileId::new(),
        Vec::new(),
        w.ws.id(),
    );
    v.open_node(&w.ws, doc);
    v.recents.clear();
    (v, w)
}

/// …and in a window of its own, which is the reference behaviour.
fn in_a_window(texts: &[&str]) -> (koru_document::BuiltInDocument, Workspace) {
    let (w, doc) = a_document_of(texts);
    let mut view = koru_document::BuiltInDocument::open(&w.ws, doc).owns_the_window(&w.ws);
    // Sized as the shell sizes it. Without a viewport nothing is shaped, and a
    // caret is placed against a layout that does not exist — so the comparison
    // with the tab would be between a real editor and an empty one.
    View::set_viewport(&mut view, 1200.0, 700.0);
    (view, w)
}

/// A second replica of this workspace, and the import path the transport really
/// takes: a snapshot, the peer's own updates since our version, `import`.
fn peer_of(w: &Workspace) -> WorkspaceDoc {
    WorkspaceDoc::from_snapshot(&w.ws.export_snapshot().expect("a snapshot"))
        .expect("a peer replica")
}

fn merge(w: &mut Workspace, from: &WorkspaceDoc) {
    let since = w.ws.version();
    let bytes = from
        .export_updates_since(&since)
        .expect("the peer's own changes");
    w.ws.import(&bytes).expect("the import");
}

/// Open the `/` picker at the end of a row, the way a person does.
fn open_a_picker<H: AdoptHost>(host: &mut H, ws: &mut WorkspaceDoc, row: NodeId) {
    host.reveal(row);
    host.press(ws, Key::Named(winit::keyboard::NamedKey::End), None);
    host.press(ws, Key::Character(" ".into()), Some(" "));
    host.press(ws, Key::Character("/".into()), Some("/"));
    assert!(host.overlay_open(), "typing / did not open a picker");
}

/// **The behaviour, in both hosts, asserted once.**
///
/// A peer deletes the node an open picker is anchored to. The picker's byte
/// offsets are a rendering cache; its `stable` endpoints are Loro cursors, and
/// after the merge they resolve to nothing at all. Both hosts close it and say
/// so.
///
/// **Closing is the deliberately visible failure.** A picker that disappears is
/// recoverable — press `/` again. A picker still hanging from a node that no
/// longer exists is not: its next commit would put text somewhere nobody asked
/// for, and `AGENTS.md` is unambiguous that losing content is the one
/// unrecoverable bug.
#[test]
fn a_peer_deleting_a_pickers_anchor_closes_it_in_both_hosts() {
    fn scenario<H: AdoptHost>(host: &mut H, w: &mut Workspace, host_name: &str) {
        let rows = host.body_rows();
        assert_eq!(rows.len(), 2, "{host_name}: the fixture is two paragraphs");
        open_a_picker(host, &mut w.ws, rows[1]);

        let mut peer = peer_of(w);
        peer.delete(rows[1]).expect("the peer's deletion");
        merge(w, &peer);
        host.arrive(&w.ws);

        assert!(
            !host.body_rows().contains(&rows[1]),
            "{host_name}: the peer's deletion did not arrive"
        );
        assert!(
            !host.overlay_open(),
            "{host_name}: the picker is still open over a node that no longer exists"
        );
        assert_eq!(
            host.notice(),
            Some("that insertion point changed elsewhere"),
            "{host_name}: the picker closed without saying why"
        );
    }

    let (mut tab, mut tab_ws) = in_a_tab(&["first", "second"]);
    scenario(&mut tab, &mut tab_ws, "in a tab");

    let (mut window, mut window_ws) = in_a_window(&["first", "second"]);
    scenario(&mut window, &mut window_ws, "in a window");
}

/// **A peer edit that does not invalidate the anchor leaves the picker open,
/// re-anchored** — in both hosts.
///
/// The picker's `start` is a byte offset into the row, and the peer inserted in
/// front of it. Re-resolving the Loro range is what keeps the `/` the panel
/// belongs to under the panel: the offset moves by exactly the bytes that were
/// inserted, and the caret moves with it rather than staying where the old text
/// used to put it.
#[test]
fn a_peer_editing_around_a_picker_re_anchors_it_in_both_hosts() {
    fn scenario<H: AdoptHost>(host: &mut H, w: &mut Workspace, host_name: &str) {
        let rows = host.body_rows();
        open_a_picker(host, &mut w.ws, rows[1]);
        let before = host
            .editor()
            .view
            .overlay
            .trigger()
            .expect("the picker has a trigger")
            .start;
        // The caret sits just past the `/`. The `/` menu carries its own search
        // field, so what is typed into it never reaches the document — which
        // makes the caret's *place* the whole of what has to survive the merge.
        assert_eq!(
            host.caret(),
            before + "/".len(),
            "{host_name}: the caret is just past the trigger"
        );

        let mut peer = peer_of(w);
        peer.insert_text(rows[1], 0, "PEER ")
            .expect("the peer's edit");
        merge(w, &peer);
        host.arrive(&w.ws);

        assert!(
            host.overlay_open(),
            "{host_name}: an edit that left the anchor alone closed the picker"
        );
        let after = host
            .editor()
            .view
            .overlay
            .trigger()
            .expect("the picker still has a trigger")
            .start;
        assert_eq!(
            after,
            before + "PEER ".len(),
            "{host_name}: the picker did not follow its anchor"
        );
        assert_eq!(
            host.caret(),
            after + "/".len(),
            "{host_name}: the caret stayed at its old byte offset instead of \
             following the anchor"
        );
        assert_eq!(
            host.notice(),
            None,
            "{host_name}: nothing went wrong, so there is nothing to say"
        );
    }

    let (mut tab, mut tab_ws) = in_a_tab(&["first", "second"]);
    scenario(&mut tab, &mut tab_ws, "in a tab");

    let (mut window, mut window_ws) = in_a_window(&["first", "second"]);
    scenario(&mut window, &mut window_ws, "in a window");
}

/// **Re-anchoring is not a licence to reflow.**
///
/// `Editing::reflow` is the O(rows) call the whole responsiveness section of
/// `AGENTS.md` is about not making twice, and a second pass over clean rows
/// shapes nothing — so it is invisible to every other assertion and has to be
/// counted (`ViewState::reflows`).
///
/// The claim is a comparison, not a magic number: an import that **invalidates**
/// a picker's anchor costs a host exactly what the same import costs that host
/// with no picker open. Whatever each host's own layout arrangements are, the
/// overlay work adds none.
#[test]
fn re_anchoring_a_picker_costs_no_extra_layout_pass() {
    fn cost<H: AdoptHost>(host: &mut H, w: &mut Workspace, with_a_picker: bool) -> u64 {
        let rows = host.body_rows();
        if with_a_picker {
            open_a_picker(host, &mut w.ws, rows[1]);
        } else {
            host.reveal(rows[1]);
        }
        let mut peer = peer_of(w);
        peer.delete(rows[1]).expect("the peer's deletion");
        merge(w, &peer);
        let before = host.reflows();
        host.arrive(&w.ws);
        host.reflows() - before
    }

    let (mut a, mut aw) = in_a_tab(&["first", "second"]);
    let (mut b, mut bw) = in_a_tab(&["first", "second"]);
    let (tab_plain, tab_anchored) = (cost(&mut a, &mut aw, false), cost(&mut b, &mut bw, true));

    let (mut c, mut cw) = in_a_window(&["first", "second"]);
    let (mut d, mut dw) = in_a_window(&["first", "second"]);
    let (win_plain, win_anchored) = (cost(&mut c, &mut cw, false), cost(&mut d, &mut dw, true));

    println!(
        "peer import reflows — in a tab: plain {tab_plain}, re-anchoring {tab_anchored}; \
         in a window: plain {win_plain}, re-anchoring {win_anchored}"
    );
    assert_eq!(
        tab_anchored, tab_plain,
        "in a tab: closing a stale picker bought an extra O(rows) layout pass"
    );
    assert_eq!(
        win_anchored, win_plain,
        "in a window: closing a stale picker bought an extra O(rows) layout pass"
    );
    assert!(
        tab_anchored <= 2 && win_anchored <= 2,
        "a peer import laid the document out {tab_anchored} times in a tab and \
         {win_anchored} times in a window"
    );
}

/// **Nothing is done twice.**
///
/// A window-level adopt re-titles the caption, re-scans the find bar and shows
/// the document to the spelling controller. A container arranges all three for
/// itself, elsewhere — so the fix for Q-B4 had to reconcile the two hosts'
/// handling of a picker *without* the container picking up the window's
/// housekeeping, and without the window doing its own twice.
///
/// All three are idempotent, so a duplicate leaves no trace in the state it
/// produces. Counted at the three seams instead (`AdoptWork`).
#[test]
fn a_peer_import_does_the_windows_housekeeping_once_and_the_containers_not_at_all() {
    let (mut tab, mut tab_ws) = in_a_tab(&["first", "second"]);
    let rows = tab.body_rows();
    open_a_picker(&mut tab, &mut tab_ws.ws, rows[1]);
    let before = tab.work();
    let mut peer = peer_of(&tab_ws);
    peer.delete(rows[1]).expect("the peer's deletion");
    merge(&mut tab_ws, &peer);
    tab.arrive(&tab_ws.ws);
    assert_eq!(
        tab.work(),
        before,
        "the container's leaf picked up the window's housekeeping"
    );

    let (mut window, mut window_ws) = in_a_window(&["first", "second"]);
    let rows = window.body_rows();
    open_a_picker(&mut window, &mut window_ws.ws, rows[1]);
    let before = window.work();
    let mut peer = peer_of(&window_ws);
    peer.delete(rows[1]).expect("the peer's deletion");
    merge(&mut window_ws, &peer);
    window.arrive(&window_ws.ws);
    let after = window.work();
    assert_eq!(
        (
            after.titles - before.titles,
            after.searches - before.searches,
            after.spellings - before.spellings,
        ),
        (1, 1, 1),
        "a window's adopt is one title refresh, one search re-scan and one \
         spelling observation — no more and no fewer"
    );
}

mod task_queue_focus;

/// Settings' *Open Koru AI setup…* opens a tab on the provider setup, not the
/// side panel.
#[test]
fn opening_ai_setup_from_settings_is_a_tab_not_the_side_panel() {
    let (mut v, _workspace) = container();
    let before = v.tabs().len();
    let panel_was = v.ai_visible();
    v.perform_settings_request(koru_settings_ui::Request::OpenAiSetup);
    assert_eq!(v.tabs().len(), before + 1);
    let TabView::AiChat(chat) = &v.tabs().active_tab().view else {
        panic!("an AI tab is showing");
    };
    assert!(chat.setup_open(), "it opens on the provider setup");
    assert_eq!(v.ai_visible(), panel_was, "the side panel did not move");
}

/// The Manage tab names workspaces of every Profile. A verb on one whose
/// Profile is not open finds it in the per-Profile catalog the switcher
/// holds, and a rename checks clashes against that Profile's names only.
#[test]
fn workspace_verbs_find_a_workspace_of_a_profile_that_is_not_open() {
    let (mut view, workspace) = container();
    let open = ProfileId::new();
    let other = ProfileId::new();
    let current = workspace.ws.id().expect("workspace identity");
    let theirs = WorkspaceId::new();
    View::set_pane_context(
        &mut view,
        Vec::new(),
        open,
        vec![
            WorkspaceEntry::new(current, "Roadmap", 0),
            WorkspaceEntry::new(WorkspaceId::new(), "Notes", 1),
        ],
        Some(current),
    );
    let mut catalog = std::collections::HashMap::new();
    catalog.insert(
        other,
        vec![
            WorkspaceEntry::new(theirs, "Theirs", 0),
            WorkspaceEntry::new(WorkspaceId::new(), "Archive", 1),
        ],
    );
    View::set_profile_workspaces(&mut view, catalog);

    assert_eq!(
        view.other_workspace_names(theirs),
        vec!["Archive".to_owned()]
    );
    assert_eq!(
        view.other_workspace_names(current),
        vec!["Notes".to_owned()]
    );

    assert!(view.start_workspace_rename(theirs));
    assert!(matches!(
        view.workspace_rename.as_ref().map(|rename| rename.target),
        Some(RenameTarget::Workspace(id)) if id == theirs
    ));
    view.workspace_rename = None;

    assert!(view.start_workspace_icon(theirs));
    assert!(view.workspace_icon.is_some());
    view.workspace_icon = None;

    assert!(view.open_workspace_menu(theirs, (10.0, 10.0), false));
    assert!(view.menu.is_some());

    assert!(!view.start_workspace_icon(WorkspaceId::new()));
}
