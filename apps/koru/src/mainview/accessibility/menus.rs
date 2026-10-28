// SPDX-License-Identifier: MPL-2.0

//! **The surface that owns everything beneath it**, while one is standing.
//!
//! Two shapes, and the difference matters:
//!
//! - **A context menu** — the right-click menu, the caption's overflow, a tab's
//!   `…`, and their submenus. Its rows are projected in full, because a menu
//!   *is* a list of things to choose, and a client that could see one but not
//!   read it would be worse off than one that saw nothing.
//! - **Everything else in the blocker table** — the confirmations, the error
//!   dialog, the prompts, the import preview, the reminder surfaces. A named
//!   `Dialog` node that takes focus, and no contents. Projecting those forms is
//!   real work with its own owners (U0.1 does onboarding's, 1b.4 does
//!   persistence's), and a node with a name says *a dialog is here* honestly,
//!   rather than leaving a screen reader on a window whose keyboard has been
//!   taken by something it cannot see. Recorded in
//!   `docs/features/accessibility/README.md`.
//!
//! ## Instance identity, and the stale action it refuses
//!
//! A menu's rows are fixed to a subject when it opens: *the tab menu* over tab
//! 3 and *the tab menu* over tab 5 are two different lists. So a row's identity
//! comes out of the block `ContextMenu::instance` reserved when the menu opened
//! — never reused, and never inherited. Close the menu and its nodes leave the
//! published tree, so a request queued against one resolves to nothing and the
//! host counts it refused; reopen and the successor holds different numbers, so
//! it cannot pick the request up either. That is the mechanism §5 asks for,
//! and it is why the identity is not the subject, the label, or the position.
//!
//! ## The keys are the menu's own
//!
//! A row is chosen by putting the menu's highlight on it and pressing Enter —
//! literally: [`MainView::menu_key`] with `NamedKey::Enter`. Submenu depth, the
//! enabled test, closing the popover behind a workspace-switcher row and
//! `run_menu_action` are all reached through it, because they were all already
//! there.

use accesskit::{Action, Role};
use koru_i18n::{Key, t};
use koru_workspace_host::accessibility::{ControlPart, Occurrence, SemanticNode};
use koru_workspace_host::routing::Blockers;

use super::{MENU, SHELL, Shell, instance_of, parts};
use crate::mainview::MainView;
use crate::mainview::context_menu::ContextMenu;

/// Which of the three live menus a request is about. A nested flyout sits on a
/// submenu sits on the menu, and that is the order `menu_key` answers in.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Level {
    Root,
    Sub,
    Nested,
}

/// One row of a menu, addressed the way a client has to address it: by reading
/// the tree.
const ITEM: ControlPart = ControlPart::new("item");

impl MainView {
    /// The blocking surface, alone.
    pub(super) fn blocker_semantics(&self, cx: &Shell, blockers: Blockers) -> SemanticNode {
        if let Some(node) = self.menu_semantics(cx) {
            return node;
        }
        // The reminder popover is the second surface projected in full rather
        // than named and left hollow: setting, correcting and revisiting a
        // reminder is the whole of A3, and a dialog a person cannot see the
        // inside of closes no gate. Plan 2.2 owns it; see `reminders.rs`.
        //
        // **Only when it is the thing standing.** The bell's All-Reminders
        // overlay sits *above* it in the blocker table, so a popover left open
        // underneath one must not be published as though it had the keyboard.
        if is_reminder_popover(blockers)
            && let Some(node) = self.reminder_popover_semantics(cx)
        {
            return node;
        }
        // **The part says which kind it is**, in the code's own keyword:
        // `koru:w1/v1/s:shell#confirm`. That is an identity a client and an
        // acceptance check can match on and it is the same in every language,
        // while the *name* a person hears is the localized word above it.
        SemanticNode::new(cx.surface(SHELL, blocker_part(blockers)), Role::Dialog)
            .with_label(t(cx.lang, Key::A11yDialog))
            .with_bounds(koru_draw::geometry::Rect::new(
                0.0,
                0.0,
                cx.viewport.0,
                cx.viewport.1,
            ))
            .modal()
    }

    /// Where the keyboard is while a blocker is up.
    ///
    /// The menu's own highlight — the thing its arrows move and its rows are
    /// drawn from — rather than a second idea of focus.
    pub(super) fn blocker_focus(&self, cx: &Shell, blockers: Blockers) -> Occurrence {
        // Innermost first: the flyout has the keys while it is up.
        for (_, menu) in self.live_menus().into_iter().rev() {
            let Some(menu) = menu else { continue };
            return match menu.hover {
                Some(index) => cx.instance(MENU, menu.row_instance(index), ITEM),
                None => cx.instance(MENU, menu.instance, parts::ROOT),
            };
        }
        // The reminder popover's own focus, which is the state its arrows move
        // and its ring is painted from.
        if is_reminder_popover(blockers)
            && let Some(occurrence) = self.reminder_focus(cx)
        {
            return occurrence;
        }
        cx.surface(SHELL, blocker_part(blockers))
    }

    /// The three menu levels, outermost first. `None` for a level that is not
    /// standing, so a caller can walk the stack in either direction.
    fn live_menus(&self) -> [(Level, Option<&ContextMenu>); 3] {
        [
            (Level::Root, self.menu.as_ref()),
            (Level::Sub, self.submenu.as_ref().map(|(_, m)| m)),
            (Level::Nested, self.nested.as_ref().map(|(_, m)| m)),
        ]
    }

    /// The open menu, its submenu and its flyout, as one nested tree.
    fn menu_semantics(&self, cx: &Shell) -> Option<SemanticNode> {
        let root = self.menu.as_ref()?;
        let mut node = self.one_menu(cx, root);
        // A submenu is a peer field rather than a child of the menu — see
        // `MainView::submenu`'s note — but to a client it *is* a child, and
        // that is what keeps a reader inside the surface holding the keyboard.
        if let Some((_, sub)) = self.submenu.as_ref() {
            let mut sub_node = self.one_menu(cx, sub);
            if let Some((_, nested)) = self.nested.as_ref() {
                let mut rows = sub_node.children.clone();
                rows.push(self.one_menu(cx, nested));
                sub_node = sub_node.with_children(rows);
            }
            let mut rows = node.children.clone();
            rows.push(sub_node);
            node = node.with_children(rows);
        }
        Some(node.modal())
    }

    fn one_menu(&self, cx: &Shell, menu: &ContextMenu) -> SemanticNode {
        let enabled = self.enabled_items(menu);
        let items = menu.items();
        // Dividers, captions and stepper rows answer `false`: publishing them
        // would offer a client a control that does nothing.
        let selectable = items.iter().filter(|action| action.selectable()).count();
        let mut index = 0;
        let children = items
            .iter()
            .enumerate()
            .filter(|(_, action)| action.selectable())
            .map(|(i, action)| {
                let node = SemanticNode::new(
                    cx.instance(MENU, menu.row_instance(i), ITEM),
                    Role::MenuItem,
                )
                .with_label(action.label().to_string())
                .with_bounds(menu.item_rect(cx.viewport, i))
                .with_actions([Action::Click, Action::Focus])
                .disabled(!enabled.get(i).copied().unwrap_or(true))
                .with_position(index);
                index += 1;
                node
            })
            .collect();
        SemanticNode::new(cx.instance(MENU, menu.instance, parts::ROOT), Role::Menu)
            .with_label(t(cx.lang, Key::A11yMenu))
            .with_bounds(menu.panel(cx.viewport))
            // `Role::Menu` is one of the roles AccessKit counts as a selection
            // container, which is what makes an item's total readable.
            .with_size_of_set(selectable)
            .with_children(children)
    }

    /// Which row of which live menu an id names.
    fn menu_row_at(&self, id: u64) -> Option<(Level, usize)> {
        self.live_menus().into_iter().find_map(|(level, menu)| {
            menu.and_then(|menu| menu.row_of_instance(id))
                .map(|index| (level, index))
        })
    }

    fn set_menu_hover(&mut self, level: Level, index: usize) {
        let slot = match level {
            Level::Root => self.menu.as_mut(),
            Level::Sub => self.submenu.as_mut().map(|(_, menu)| menu),
            Level::Nested => self.nested.as_mut().map(|(_, menu)| menu),
        };
        if let Some(menu) = slot {
            menu.hover = Some(index);
        }
    }

    /// Whether `id` is a live menu's *own* id rather than one of its rows'.
    fn names_a_live_menu(&self, id: u64) -> bool {
        self.live_menus()
            .into_iter()
            .any(|(_, menu)| menu.is_some_and(|menu| menu.instance == id))
    }
}

/// Which kind of surface is standing, as an untranslated keyword.
///
/// One table, read twice: it is the blocker node's own identity, and it is what
/// the shell's revision folds in — so the tree and the number that says the
/// tree changed cannot disagree about what is on top.
pub(super) fn blocker_keyword(blockers: Blockers) -> &'static str {
    match blockers {
        Blockers::None => parts::BLOCKER.name(),
        Blockers::Modal(kind) => modal_keyword(kind),
        Blockers::Prompt(kind) => prompt_keyword(kind),
    }
}

fn blocker_part(blockers: Blockers) -> ControlPart {
    ControlPart::new(blocker_keyword(blockers))
}

fn modal_keyword(kind: koru_workspace_host::routing::ModalKind) -> &'static str {
    use koru_workspace_host::routing::ModalKind as K;
    match kind {
        K::Error => "error",
        K::Import => "import",
        K::Updates => "updates",
        K::UpdateNewer => "update-newer",
        K::Confirm => "confirm",
        K::ReminderOverlay => "reminders",
        K::ReminderManagement => "reminder-management",
        K::Recording => "recording",
        K::ContextMenu => "menu",
        K::DiagramViewer => "diagram-viewer",
    }
}

fn prompt_keyword(kind: koru_workspace_host::routing::PromptKind) -> &'static str {
    use koru_workspace_host::routing::PromptKind as K;
    match kind {
        K::MoveTo => "move-to",
        K::Search => "search",
        K::NewDocument => "new-document",
        K::LinkDevice => "link-device",
        K::Appearance => "appearance",
        K::Rename => "rename",
        K::Snapshot => "snapshot",
        K::NewList => "new-list",
        K::ReminderCreation => "reminder-creation",
        K::Delegate => "delegate",
    }
}

/// Whether a request names something inside the standing blocker.
///
/// The other half of publishing the blocker alone: a client may still hold an
/// id from before the menu opened, and performing it would be the shell acting
/// on a control the keyboard cannot currently reach.
pub(super) fn is_blocker_target(view: &MainView, target: Occurrence) -> bool {
    if let Some(id) = instance_of(target, MENU) {
        return view.menu_row_at(id).is_some() || view.names_a_live_menu(id);
    }
    // The reminder popover's own controls, when it is what is standing. A menu
    // and the bell's overlay both sit above it in the table, so asking the
    // table rather than the surface is what stops a request reaching a panel
    // something else has covered.
    //
    // The hollow blocker node performs nothing, so naming it is not a reason to
    // reach past it either.
    is_reminder_popover(view.blockers()) && super::reminders::is_popover_target(view, target)
}

/// Whether the standing blocker is the reminder popover — either of its two
/// modes, which are one surface and one owner.
fn is_reminder_popover(blockers: Blockers) -> bool {
    matches!(
        blockers,
        Blockers::Modal(koru_workspace_host::routing::ModalKind::ReminderManagement)
            | Blockers::Prompt(koru_workspace_host::routing::PromptKind::ReminderCreation)
    )
}

/// Route a request at an open menu, through the menu's own keys.
pub(super) fn action(view: &mut MainView, target: Occurrence, action: Action) -> bool {
    use winit::keyboard::{Key as WKey, NamedKey};

    let Some(id) = instance_of(target, MENU) else {
        return false;
    };
    if !matches!(action, Action::Click | Action::Focus) {
        return false;
    }
    let Some((level, index)) = view.menu_row_at(id) else {
        return false;
    };
    view.set_menu_hover(level, index);
    if action == Action::Focus {
        // The highlight *is* the menu's keyboard position, so moving it is the
        // whole of what a focus request means here.
        return true;
    }
    // Enter is the one key that chooses, at every level, and it is where the
    // enabled test, the popover-closing rule and `run_menu_action` already
    // live.
    view.menu_key(&WKey::Named(NamedKey::Enter))
}
