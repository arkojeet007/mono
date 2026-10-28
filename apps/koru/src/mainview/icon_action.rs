// SPDX-License-Identifier: MPL-2.0

//! Typed icon controls owned by the container.
//!
//! `koru_ui::icon_button::IconButton` owns the common geometry, paint, hover
//! state, and hit testing.  This module supplies the small amount of container
//! vocabulary that the UI crate must not know: which app action a button asks
//! for and where a page can place it.

use super::{MainView, TabView};
use crate::panes::Outcome;
use koru_draw::geometry::Rect;
use koru_draw::icons;
use koru_shell_chrome::regions::Hit;
use koru_ui::TooltipContent;
use koru_ui::caption::Action as CaptionAction;
use koru_ui::icon_button::{IconButton, State};
use koru_window::WindowRequest;

/// The app-level meaning returned by an [`IconButton`].
///
/// A typed value is the Rust equivalent of an `onClick` callback here.  It
/// keeps dispatch with `MainView`, which owns the mutable state, instead of
/// making a reusable drawing crate capture a particular window.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum IconAction {
    ToggleAi,
    ToggleContext,
    ToggleWindowMenu,
}

impl MainView {
    /// Dispatch an action emitted by one of this module's controls.
    pub(super) fn run_icon_action(&mut self, action: IconAction, x: f32, y: f32) -> Outcome {
        match action {
            IconAction::ToggleAi => {
                self.toggle_ai();
                Outcome::Consumed
            }
            IconAction::ToggleContext => Outcome::ToggleContext,
            // The support window's ⋯ slot is the pin (no menu there at all).
            IconAction::ToggleWindowMenu if self.support_only => {
                self.ask_window(WindowRequest::TogglePin);
                Outcome::Consumed
            }
            IconAction::ToggleWindowMenu if self.window_menu_open() => {
                self.menu = None;
                self.submenu = None;
                self.nested = None;
                Outcome::Consumed
            }
            IconAction::ToggleWindowMenu => {
                let rect = self
                    .window_menu_button()
                    .map(|button| button.rect())
                    .unwrap_or_else(|| Rect::new(x, y, 0.0, 0.0));
                self.open_window_menu(rect);
                Outcome::Consumed
            }
        }
    }

    /// The overflow control in the caption. It is an ordinary icon button;
    /// opening its menu changes its active state, never its meaning or glyph.
    pub(super) fn window_menu_button(&self) -> Option<IconButton<IconAction>> {
        koru_ui::caption::controls(self.drawn_caption())
            .into_iter()
            .find(|control| control.action == CaptionAction::WindowMenu)
            .map(|control| {
                IconButton::new(koru_draw::icons::DOTS_THREE, control.rect)
                    .on_click(IconAction::ToggleWindowMenu)
                    .with_state(State {
                        hovered: self.hover == Some(Hit::Window(CaptionAction::WindowMenu)),
                        // A menu being open is not a pressed/toggled state.
                        // Keep the caption control in the same neutral hover
                        // vocabulary as its neighbours.
                        active: false,
                        disabled: false,
                    })
            })
    }

    /// The document-page context control. `Leaf` gives its content-local
    /// location; the container translates it once into window coordinates.
    pub(super) fn document_context_button(&self) -> Option<IconButton<IconAction>> {
        let content = self.focused_view_rect();
        let TabView::Leaf(leaf) = &self.tabs().active_tab().view else {
            return None;
        };
        // Painted after the pane's content, so they would sit over the
        // diagram viewer's scrim; while it is up they are neither drawn nor
        // pressable, like everything else under a modal.
        if leaf.diagram_viewer_open() {
            return None;
        }
        let rect = leaf.title_action_rect()?;
        let rect = Rect::new(content.x + rect.x, content.y + rect.y, rect.w, rect.h);
        if rect.y < content.y || rect.y + rect.h > content.y + content.h {
            return None;
        }
        Some(
            IconButton::new(icons::PANEL_RIGHT, rect)
                .on_click(IconAction::ToggleContext)
                .with_state(State {
                    hovered: rect.contains(self.pointer.0, self.pointer.1),
                    active: self.context.is_open(),
                    disabled: false,
                }),
        )
    }

    /// Koru AI beside the page Context action, anchored to the same title row.
    pub(super) fn document_ai_button(&self) -> Option<IconButton<IconAction>> {
        let content = self.focused_view_rect();
        let TabView::Leaf(leaf) = &self.tabs().active_tab().view else {
            return None;
        };
        // Painted after the pane's content, so they would sit over the
        // diagram viewer's scrim; while it is up they are neither drawn nor
        // pressable, like everything else under a modal.
        if leaf.diagram_viewer_open() {
            return None;
        }
        let rect = leaf.title_ai_action_rect()?;
        let rect = Rect::new(content.x + rect.x, content.y + rect.y, rect.w, rect.h);
        if rect.y < content.y || rect.y + rect.h > content.y + content.h {
            return None;
        }
        Some(
            IconButton::new(icons::SPARKLE, rect)
                .on_click(IconAction::ToggleAi)
                .with_state(State {
                    hovered: rect.contains(self.pointer.0, self.pointer.1),
                    active: self.ai_panel.is_open(),
                    disabled: false,
                }),
        )
    }

    /// Caption-style explanation for the page-furniture context toggle.
    ///
    /// Not a caption `Hit`, so [`super::MainView::caption_tooltip`] never sees
    /// it. The pointer check lives next to the button that owns the rect.
    pub(super) fn document_context_tooltip(&self) -> Option<TooltipContent> {
        if self.group_drag().is_some() || self.sidebar.animating() || self.ai_panel.animating() {
            return None;
        }
        let button = self.document_context_button()?;
        if !button.contains(self.pointer.0, self.pointer.1) {
            return None;
        }
        Some(
            TooltipContent::new(5, button.rect(), "Document context").with_optional_shortcut(
                koru_keys::show_hint(
                    koru_keys::ShortcutId::ToggleContext,
                    koru_keys::Section::Tooltip,
                ),
            ),
        )
    }

    pub(super) fn document_ai_tooltip(&self) -> Option<TooltipContent> {
        if self.group_drag().is_some() || self.sidebar.animating() || self.ai_panel.animating() {
            return None;
        }
        let button = self.document_ai_button()?;
        button.contains(self.pointer.0, self.pointer.1).then(|| {
            TooltipContent::new(7, button.rect(), "Koru AI pane").with_optional_shortcut(
                koru_keys::show_hint(
                    koru_keys::ShortcutId::ToggleAiPane,
                    koru_keys::Section::Tooltip,
                ),
            )
        })
    }
}
