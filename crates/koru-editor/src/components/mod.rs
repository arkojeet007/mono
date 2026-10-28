// SPDX-License-Identifier: MPL-2.0

//! The panels that float over a row: pickers and prompts.
//!
//! One file each. A component owns its state, its keys and its clicks, and says
//! what it wants to happen through [`Outcome`] rather than reaching into the
//! editor. Nothing here knows what a node is — a component hands back a
//! [`PillSpec`] and the editor decides what that means for the document.
//! (Honestly: several outcomes now carry `NodeId`s — `SetLanguage` first, the
//! attribute outcomes after it — as opaque addresses the editor resolves; no
//! component reads or writes a node.)
//!
//! Detecting *when* to open one is `trigger.rs`; that is a property of the text,
//! not of the panel.

pub mod attr_menu;
pub mod color_picker;
pub mod command_menu;
pub mod date_picker;
pub mod draw;
pub mod emoji_picker;
pub mod entity_picker;
pub mod language_picker;
pub mod link_prompt;
pub mod list_picker;
pub mod name_prompt;
pub mod priority_picker;
pub mod tag_picker;

/// The one-line field, which now lives in `koru-ui`.
///
/// It moved so prompts can use a text box without linking this crate — the node
/// model, the workspace and the syntax engine — to draw a name field. It is
/// re-exported under its old path rather than relocated at the call sites,
/// because the four panels below reach for it by that name and the move is not
/// something any of them has an opinion about.
pub use koru_ui::text_field;
// `text_input` followed `text_field` to `koru-ui` in stage 5, so the search
// overlay can be a box you type into without linking the node model. Both are
// re-exported under their old paths, so no call site here changed.
pub use koru_ui::text_input;

use winit::keyboard::{Key, ModifiersState, NamedKey};

use attr_menu::AttrMenu;
use color_picker::ColorPicker;
use command_menu::{Command, CommandMenu};
use date_picker::DatePicker;
use emoji_picker::EmojiPicker;
use entity_picker::EntityPicker;
use language_picker::LanguagePicker;
use link_prompt::LinkPrompt;
use list_picker::ListPicker;
use name_prompt::NamePrompt;
use priority_picker::PriorityPicker;
use tag_picker::TagPicker;

use crate::body::Body;
use crate::trigger::{self, Anchor, PillSpec, Trigger};
use koru_draw::text::TextEngine;

/// What a component wants to happen next.
#[derive(Debug)]
pub enum Outcome {
    /// Not ours — let the document have the key.
    Ignored,
    /// Handled; the panel stays open.
    Consumed,
    /// Dismiss, leaving any trigger text in place as ordinary text.
    Close,
    /// Dismiss and insert this pill in place of the trigger text.
    Commit(PillSpec),
    /// Create a page named from the entity query. An empty name keeps the
    /// Ctrl+T blank-draft behavior; a typed name is enough to create directly.
    /// The container owns tabs and document creation, so the picker can only
    /// ask and carry the proposed title.
    NewPage(String),
    /// Dismiss and set a code block's grammar. Carries the target node so the
    /// editor does not have to remember which row opened the picker. `None` is
    /// "Auto" — no language stored.
    SetLanguage(koru_core::NodeId, Option<String>),
    /// Replace the anchored range with ordinary Unicode text.
    CommitText(String),
    /// Replace this panel with the one that command opens.
    ///
    /// Only the command menu does this, and it is why the variant exists:
    /// choosing `/document` is not a commit, it is a different picker. It carries
    /// the *command* rather than a built panel because building one needs the
    /// text engine, which a component deliberately has no handle on.
    Become(Command),
    /// [`Become`](Self::Become) with a typed argument — `due friday`,
    /// `priority high` (D7's typed-argument sugar). The remainder is carried
    /// **unparsed**: what it means needs the clock and the document, which
    /// are the editor's; the editor commits directly when it parses and
    /// opens the command's picker when it does not.
    CommandArg(Command, String),
    /// Replace a document rename prompt with the Unicode page-icon picker.
    BecomePageIcon(koru_core::NodeId),
    /// Dismiss and set a system attribute — key, validated value — on these
    /// nodes. Several nodes so a multi-select apply is **one** outcome and
    /// therefore one undo step; the everyday case carries one.
    SetAttr(Vec<koru_core::NodeId>, String, koru_core::node::AttrValue),
    /// Dismiss and remove every value under this key from these nodes.
    ClearAttr(Vec<koru_core::NodeId>, String),
    /// Give these callout boxes this tint, `None` being the default tint; the
    /// rows inside draw from it. Not an attribute: a callout's tint is
    /// presentation in `NodeMeta::colour`, like a page's icon. The `/color`
    /// picker dismisses on it; the look picker stays open for the emoji.
    SetCalloutTint(Vec<koru_core::NodeId>, Option<koru_core::AccentColor>),
    /// Dismiss and put these nodes in a list — making each a task first when
    /// it is not one. `/task_list`'s pick of an existing list.
    JoinList(Vec<koru_core::NodeId>, koru_core::ids::ListId),
    /// Dismiss and ask the host to make a list — name and accent token — and
    /// put these nodes in it. The table lives in the system document, which
    /// this crate cannot see, so creating is the host's; the editor takes the
    /// trigger text out and hands the rest up, as it does for a new tag.
    CreateList(Vec<koru_core::NodeId>, String, Option<String>),
    /// Dismiss and tag these nodes with an existing tag.
    AddTag(Vec<koru_core::NodeId>, koru_core::ids::TagId),
    /// Dismiss and untag these nodes.
    ///
    /// Two gestures produce it, and both name exactly one tag: the **tag
    /// chip's `×`**, and a **ticked row of the chooser chosen again**
    /// ([ADR-188], amended twice). The `#tag` grammar and a multi-node apply
    /// never do — `TagPicker::toggles_off` is where that is decided.
    ///
    /// [ADR-188]: ../../../../docs/decisions/ADR-201-225.md#adr-211-chipclick--an-attribute-chip-is-the-control-for-its-attribute-and-a-tag-comes-off-at-the-chip
    RemoveTag(Vec<koru_core::NodeId>, koru_core::ids::TagId),
    /// Dismiss, create this tag in the workspace tag table, and apply it to
    /// these nodes. The table lives in the workspace **system document**,
    /// which neither a component nor the editor can see — so this travels on
    /// to the host through `ViewState::wants_create_tag`.
    CreateTag(Vec<koru_core::NodeId>, String),
}

/// A piece of a panel that is not one of its rows, and what the pointer does
/// over it.
///
/// Two, because the pointer has two answers for them: a button is pressed and a
/// thumb is picked up. `docs/features/design/DESIGN.md` — anything you can act on says so.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PanelPart {
    /// Pressed: the emoji categories, the rename prompt's page icon.
    Button,
    /// Grabbed and dragged: a scrollbar thumb.
    Thumb,
}

pub enum Overlay {
    None,
    Command(CommandMenu),
    Entity(EntityPicker),
    Date(DatePicker),
    Name(NamePrompt),
    Link(LinkPrompt),
    /// The language chip's dropdown over a code block.
    Language(LanguagePicker),
    Emoji(EmojiPicker),
    /// The `/attr` chooser: which attribute to set.
    Attr(AttrMenu),
    /// The tag chooser — `/tag`, `#tag`, and the menus' Tags….
    Tag(TagPicker),
    /// The accent color picker — `/color` and the menus' Color….
    Color(ColorPicker),
    /// The priority picker — `/priority` and the menus' Priority….
    Priority(PriorityPicker),
    /// The list picker — `/task_list`.
    List(ListPicker),
}

impl Overlay {
    pub fn is_open(&self) -> bool {
        !matches!(self, Self::None)
    }

    pub fn trigger(&self) -> Option<Trigger> {
        match self {
            Self::None => None,
            Self::Command(c) => Some(c.trigger.clone()),
            Self::Entity(c) => Some(c.trigger.clone()),
            Self::Date(d) => Some(d.trigger.clone()),
            Self::Name(n) => Some(n.trigger.clone()),
            Self::Link(l) => Some(l.trigger.clone()),
            Self::Emoji(e) => Some(e.trigger.clone()),
            Self::Attr(a) => Some(a.trigger.clone()),
            Self::Tag(t) => Some(t.trigger.clone()),
            Self::Color(c) => Some(c.trigger.clone()),
            Self::Priority(p) => Some(p.trigger.clone()),
            Self::List(l) => Some(l.trigger.clone()),
            // Not trigger-based: it opens from a hover chip, not typed text.
            Self::Language(_) => None,
        }
    }

    pub fn trigger_mut(&mut self) -> Option<&mut Trigger> {
        match self {
            Self::None => None,
            Self::Command(c) => Some(&mut c.trigger),
            Self::Entity(c) => Some(&mut c.trigger),
            Self::Date(d) => Some(&mut d.trigger),
            Self::Name(n) => Some(&mut n.trigger),
            Self::Link(l) => Some(&mut l.trigger),
            Self::Emoji(e) => Some(&mut e.trigger),
            Self::Attr(a) => Some(&mut a.trigger),
            Self::Tag(t) => Some(&mut t.trigger),
            Self::Color(c) => Some(&mut c.trigger),
            Self::Priority(p) => Some(&mut p.trigger),
            Self::List(l) => Some(&mut l.trigger),
            Self::Language(_) => None,
        }
    }

    pub fn panel_size(&self) -> (f32, f32) {
        match self {
            Self::None => (0.0, 0.0),
            Self::Command(c) => (koru_draw::theme::PANEL_W, c.height()),
            Self::Entity(c) => (koru_draw::theme::PANEL_W, c.height()),
            Self::Date(d) => d.size(),
            Self::Name(_) => NamePrompt::size(),
            Self::Link(_) => LinkPrompt::size(),
            Self::Language(l) => (l.width.max(96.0), l.height()),
            Self::Emoji(e) => (emoji_picker::PANEL_W, e.height()),
            Self::Attr(a) => (a.width.max(120.0), a.height()),
            Self::Tag(t) => (koru_draw::theme::PANEL_W, t.height()),
            Self::Color(c) => (c.width.max(140.0), c.height()),
            Self::Priority(p) => (p.width.max(140.0), p.height()),
            Self::List(l) => (koru_draw::theme::PANEL_W, l.height()),
        }
    }

    /// Whether this overlay is a list of rows you pick from, as opposed to a
    /// prompt you type into.
    ///
    /// A prompt has no rows, but [`item_at`](Self::item_at) is arithmetic and
    /// answers `Some` for any point below the padding — so hovering a rename
    /// prompt's hint text used to report a row that does not exist. Harmless
    /// while nothing looked at it; visible the moment a hovered row started
    /// lighting up under the pointer.
    pub fn has_rows(&self) -> bool {
        matches!(
            self,
            Self::Command(_)
                | Self::Entity(_)
                | Self::Language(_)
                | Self::Emoji(_)
                | Self::Attr(_)
                | Self::Tag(_)
                | Self::Color(_)
                | Self::Priority(_)
                | Self::List(_)
        )
    }

    /// Whether every non-scrollbar point on this panel is an interactive
    /// surface. Both panels below are flat lists with no text field of their
    /// own, so a point on the frame that is not a row — the padding band, the
    /// edge — still belongs to the panel: claiming it keeps the panel's own
    /// plain arrow and stops the hover falling through to the document
    /// underneath, where it wore the I-beam over rows nobody was looking at.
    pub fn pointer_surface(&self) -> bool {
        matches!(
            self,
            Self::Entity(_)
                | Self::Language(_)
                | Self::Attr(_)
                | Self::Color(_)
                | Self::Priority(_)
        )
    }

    pub fn item_at(&self, local_x: f32, local_y: f32) -> Option<usize> {
        if !self.has_rows() {
            return None;
        }
        match self {
            Self::Emoji(picker) => picker.item_at(local_x, local_y),
            // Past its search box, which owns the top of the panel — and never
            // in the scrollbar's column, so a hovered thumb does not light the
            // row it happens to be beside.
            Self::Command(m) => {
                let (bar_x, _) = command_menu::scrollbar_column(self.panel_size().0);
                (local_x < bar_x).then(|| m.item_at(local_y - koru_draw::theme::PANEL_PAD))?
            }
            Self::Entity(p) => {
                let (bar_x, _) = command_menu::scrollbar_column(self.panel_size().0);
                if p.scrolls() && local_x >= bar_x {
                    return None;
                }
                let local_y = local_y - p.field_band();
                let row = ((local_y - koru_draw::theme::PANEL_PAD - entity_picker::HEADER_H)
                    / koru_draw::theme::ROW_H)
                    .floor();
                (row >= 0.0).then(|| p.visible_item(row as usize))?
            }
            Self::Tag(t) => t.item_at(local_y - koru_draw::theme::PANEL_PAD),
            Self::List(l) => l.item_at(local_y - koru_draw::theme::PANEL_PAD),
            _ => {
                let row =
                    ((local_y - koru_draw::theme::PANEL_PAD) / koru_draw::theme::ROW_H).floor();
                (row >= 0.0).then_some(row as usize)
            }
        }
    }

    /// What a panel-local point is on, when it is on something that is **not**
    /// one of the panel's rows.
    ///
    /// Asked before [`item_at`](Self::item_at), and answered separately from it,
    /// because a row hover pre-selects the row it names: reporting the emoji
    /// picker's category strip as a row would move the selection as the pointer
    /// crossed it.
    pub fn control_at(&self, local_x: f32, local_y: f32) -> Option<PanelPart> {
        match self {
            Self::Emoji(picker) => {
                // Tone strip first: it floats over the grid, so a hover there
                // must not move the grid selection underneath.
                if picker.tone_strip_contains(local_x, local_y) {
                    return Some(PanelPart::Button);
                }
                // The look picker's tint swatches are controls too: a hover
                // there must not read as a grid row.
                if picker.tint_at(local_x, local_y).is_some() {
                    return Some(PanelPart::Button);
                }
                if picker.scrolls()
                    && emoji_picker::scrollbar_thumb(picker).contains(local_x, local_y)
                {
                    return Some(PanelPart::Thumb);
                }
                picker
                    .geometry()
                    .category_at(local_x, local_y)
                    .map(|_| PanelPart::Button)
            }
            Self::Command(menu) => {
                let pw = self.panel_size().0;
                (menu.scrolls() && menu.scrollbar_thumb(pw).contains(local_x, local_y))
                    .then_some(PanelPart::Thumb)
            }
            Self::Entity(picker) => {
                let pw = self.panel_size().0;
                (picker.scrolls() && picker.scrollbar_thumb(pw).contains(local_x, local_y))
                    .then_some(PanelPart::Thumb)
            }
            // The rename prompt's page-icon button, which is the only thing in a
            // prompt you press rather than type into.
            Self::Name(prompt) => prompt
                .page_icon_at(local_x, local_y)
                .map(|_| PanelPart::Button),
            _ => None,
        }
    }

    pub fn hover_item(&mut self, item: usize) {
        match self {
            Self::Emoji(picker) if item < picker.hits.len() => picker.sel = item,
            Self::Command(menu) if item < menu.hits.len() => menu.sel = item,
            Self::Entity(picker) if picker.contains_item(item) => picker.sel = item,
            Self::Language(picker) if item < language_picker::labels().len() => picker.sel = item,
            Self::Attr(menu) if item < attr_menu::rows().len() => menu.sel = item,
            Self::Tag(picker) if item < picker.row_count() => picker.sel = item,
            Self::Color(picker) if item < picker.row_count() => picker.sel = item,
            Self::Priority(picker) if item < picker.row_count() => picker.sel = item,
            Self::List(picker) => picker.hover(item),
            _ => {}
        }
    }

    pub fn scroll(&mut self, delta: f32) -> bool {
        let step = if delta < 0.0 { 1 } else { -1 };
        match self {
            Self::Emoji(picker) => {
                picker.scroll_rows(step);
                true
            }
            Self::Command(menu) if menu.scrolls() => {
                menu.scroll_rows(step);
                true
            }
            Self::Entity(picker) => {
                picker.scroll_rows(step);
                true
            }
            Self::Tag(picker) if picker.scrolls() => {
                picker.scroll_rows(step);
                true
            }
            Self::List(picker) if picker.scrolls() => {
                picker.scroll_rows(step);
                true
            }
            _ => false,
        }
    }

    pub fn is_dragging_scrollbar(&self) -> bool {
        match self {
            Self::Emoji(picker) => picker.is_dragging_scrollbar(),
            Self::Command(menu) => menu.is_dragging_scrollbar(),
            Self::Entity(picker) => picker.is_dragging_scrollbar(),
            _ => false,
        }
    }

    /// Carry a grabbed thumb. `pw` is the panel's width, which the command
    /// menu's track is measured from; the emoji picker's is a constant.
    pub fn drag_scrollbar(&mut self, local_y: f32, pw: f32) {
        match self {
            Self::Emoji(picker) => picker.drag_scrollbar(local_y),
            Self::Command(menu) => menu.drag_scrollbar(pw, local_y),
            Self::Entity(picker) => picker.drag_scrollbar(pw, local_y),
            _ => {}
        }
    }

    pub fn stop_scrollbar_drag(&mut self) -> bool {
        match self {
            Self::Emoji(picker) => picker.stop_scrollbar_drag(),
            Self::Command(menu) => menu.stop_scrollbar_drag(),
            Self::Entity(picker) => picker.stop_scrollbar_drag(),
            _ => false,
        }
    }

    /// One key press, routed to whichever component is open.
    ///
    /// Escape is handled here rather than five times over: every panel closes on
    /// it, and a component that had to remember to would eventually not.
    pub fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Outcome {
        let named = match key {
            Key::Named(n) => Some(*n),
            _ => None,
        };
        // Escape closes every panel — but the emoji picker's tone strip claims
        // it first, so a long-press does not dismiss the whole picker.
        if named == Some(NamedKey::Escape) {
            if let Self::Emoji(e) = self {
                if e.tone_strip.is_some() {
                    return e.key(engine, key, text, mods);
                }
            }
            return Outcome::Close;
        }
        match self {
            Self::None => Outcome::Ignored,
            Self::Command(c) => c.key(engine, key, text, mods),
            Self::Entity(c) => c.key_full(engine, key, text, mods),
            Self::Date(d) => d.key(engine, key, text, mods),
            Self::Name(n) => n.key(engine, key, text, mods),
            Self::Link(l) => l.key(engine, key, text, mods),
            Self::Language(l) => l.key(named),
            Self::Emoji(e) => e.key(engine, key, text, mods),
            Self::Attr(a) => a.key(named),
            Self::Tag(t) => t.key(engine, key, text, mods),
            Self::Color(c) => c.key(named),
            Self::Priority(p) => p.key(named),
            Self::List(l) => l.key(engine, key, text, mods),
        }
    }

    /// Release after a press inside the panel. Only the emoji picker needs it
    /// today (short-click commit vs long-press tone strip).
    pub fn release(&mut self) -> Outcome {
        match self {
            Self::Emoji(e) => e.release(),
            _ => Outcome::Consumed,
        }
    }

    /// Promote an armed long-press; true when the tone strip just opened.
    pub fn tick_long_press(&mut self) -> bool {
        match self {
            Self::Emoji(e) => e.tick_long_press(),
            _ => false,
        }
    }

    pub fn press_animating(&self) -> bool {
        match self {
            Self::Emoji(e) => e.press_animating(),
            _ => false,
        }
    }

    /// Whether a panel-local point is inside an editable field.
    ///
    /// Read-only, so a press can ask *before* dispatching the click and know
    /// whether the drag that follows belongs to a field — `click` focuses
    /// whatever it lands in, so asking afterwards would say yes for a press on
    /// the panel's hint text.
    pub fn field_at(&self, local_x: f32, local_y: f32, pw: f32) -> bool {
        match self {
            Self::Name(p) => p.field_rect(pw).contains(local_x, local_y),
            Self::Link(_) => link_prompt::Field::BOTH
                .into_iter()
                .any(|f| link_prompt::field_rect(f, pw).contains(local_x, local_y)),
            Self::Emoji(e) => e
                .field_rect()
                .is_some_and(|rect| rect.contains(local_x, local_y)),
            Self::Date(d) => {
                d.has_field() && date_picker::field_rect(pw).contains(local_x, local_y)
            }
            Self::Command(m) => {
                m.has_search() && command_menu::search_rect(pw).contains(local_x, local_y)
            }
            Self::Tag(t) => {
                t.has_search() && command_menu::search_rect(pw).contains(local_x, local_y)
            }
            Self::List(l) => {
                l.has_search() && command_menu::search_rect(pw).contains(local_x, local_y)
            }
            Self::Entity(p) => {
                p.own_query.is_some() && entity_picker::field_rect(pw).contains(local_x, local_y)
            }
            _ => false,
        }
    }

    /// Carry the focused field's selection to a panel-local point.
    ///
    /// Unlike [`click`] this does **not** hit-test: a drag that has wandered
    /// out of the box is still that field's drag, and asking would drop the
    /// selection the moment the pointer strayed.
    ///
    /// [`click`]: Self::click
    pub fn drag(&mut self, engine: &mut TextEngine, local_x: f32, pw: f32) {
        match self {
            Self::Name(p) => p.drag(engine, local_x, pw),
            Self::Link(p) => p.drag(engine, local_x, pw),
            Self::Emoji(p) => p.drag_field(engine, local_x),
            Self::Command(m) => m.drag_field(engine, local_x, pw),
            Self::Date(d) => d.drag_field(engine, local_x, pw),
            Self::Tag(t) => {
                let rect = command_menu::search_rect(pw);
                if let Some(field) = &mut t.query {
                    field.drag_to(engine, local_x - rect.x + field.scroll());
                }
            }
            Self::List(l) if l.has_search() => {
                let rect = command_menu::search_rect(pw);
                l.query.drag_to(engine, local_x - rect.x + l.query.scroll());
            }
            Self::Entity(p) => {
                let rect = entity_picker::field_rect(pw);
                if let Some(field) = &mut p.own_query {
                    field.drag_to(engine, local_x - rect.x + field.scroll());
                }
            }
            _ => {}
        }
    }

    /// Select the word under a panel-local point.
    pub fn double_click(&mut self, engine: &mut TextEngine, local_x: f32, local_y: f32, pw: f32) {
        match self {
            Self::Name(p) => p.double_click(engine, local_x, local_y, pw),
            Self::Link(p) => p.double_click(engine, local_x, local_y, pw),
            Self::Emoji(_) => {}
            _ => {}
        }
    }

    /// A click inside the panel, in panel-local coordinates.
    pub fn click(
        &mut self,
        engine: &mut TextEngine,
        local_x: f32,
        local_y: f32,
        pw: f32,
    ) -> Outcome {
        let body_y = local_y - koru_draw::theme::PANEL_PAD;
        match self {
            Self::None => Outcome::Consumed,
            // The prompts have no clickable rows, but they do have fields:
            // clicking one focuses it and takes the caret there. A click that
            // misses is still swallowed, which is what stops it dismissing the
            // panel the way a click *outside* does.
            Self::Name(p) => {
                if let Some(node) = p.page_icon_at(local_x, local_y) {
                    Outcome::BecomePageIcon(node)
                } else {
                    p.click(engine, local_x, local_y, pw);
                    Outcome::Consumed
                }
            }
            Self::Link(p) => {
                p.click(engine, local_x, local_y, pw);
                Outcome::Consumed
            }
            Self::Command(c) => {
                let (bar_x, _) = command_menu::scrollbar_column(pw);
                if c.has_search() && command_menu::search_rect(pw).contains(local_x, local_y) {
                    c.click_field(engine, local_x, pw);
                    Outcome::Consumed
                } else if local_x >= bar_x {
                    // The scrollbar owns its column even where a row is level
                    // with it, the way the document's own gutter does (ADR-28).
                    // The press *grabs*: everything that follows until the
                    // release is this thumb's, wherever the pointer goes.
                    c.press_scrollbar(pw, local_y);
                    Outcome::Consumed
                } else {
                    c.click(body_y)
                }
            }
            Self::Entity(c) => {
                let (bar_x, _) = command_menu::scrollbar_column(pw);
                if c.own_query.is_some() && entity_picker::field_rect(pw).contains(local_x, local_y)
                {
                    c.click_field(engine, local_x, pw);
                    Outcome::Consumed
                } else if c.scrolls() && local_x >= bar_x {
                    c.press_scrollbar(pw, local_y);
                    Outcome::Consumed
                } else {
                    c.click(local_x, local_y, pw)
                }
            }
            Self::Date(d) => {
                if d.has_field() && date_picker::field_rect(pw).contains(local_x, local_y) {
                    d.click_field(engine, local_x, pw);
                    Outcome::Consumed
                } else {
                    d.click(local_x, local_y)
                }
            }
            Self::Language(l) => l.click(body_y),
            Self::Attr(a) => a.click(body_y),
            Self::Color(c) => c.click(body_y),
            Self::Priority(p) => p.click(body_y),
            Self::List(l) => {
                if l.has_search() && command_menu::search_rect(pw).contains(local_x, local_y) {
                    let rect = command_menu::search_rect(pw);
                    l.query.click(engine, local_x - rect.x + l.query.scroll());
                    Outcome::Consumed
                } else {
                    l.click(body_y)
                }
            }
            Self::Tag(t) => {
                if t.has_search() && command_menu::search_rect(pw).contains(local_x, local_y) {
                    let rect = command_menu::search_rect(pw);
                    if let Some(field) = &mut t.query {
                        field.click(engine, local_x - rect.x + field.scroll());
                    }
                    Outcome::Consumed
                } else {
                    t.click(body_y)
                }
            }
            Self::Emoji(p) => {
                if p.field_rect()
                    .is_some_and(|rect| rect.contains(local_x, local_y))
                {
                    p.click_field(engine, local_x);
                    Outcome::Consumed
                } else {
                    p.click(engine, local_x, local_y)
                }
            }
        }
    }

    /// Re-filter as the query behind the panel changes, and close if the caret
    /// has left the row the panel belongs to.
    ///
    /// Only the two text-driven pickers follow the document. The prompts own
    /// their own input, so the query behind them is frozen.
    pub fn refresh(&mut self, outline: &Body) {
        let Some(trigger) = self.trigger() else {
            return;
        };
        if trigger.anchor != Anchor::TriggerText {
            return;
        }
        let query = outline
            .row(trigger.node)
            .filter(|_| outline.focused_id() == Some(trigger.node))
            .and_then(|row| trigger::query(row, &trigger));
        let Some(q) = query else {
            *self = Overlay::None;
            return;
        };

        // A command menu with its own field is not document-driven: the query
        // is typed into the panel, so the text behind it never changes and
        // re-filtering from it would erase what was typed on the first frame.
        // The row-left dismissal above still applies. The tag chooser's field
        // mode is the same shape; the `/attr` chooser has no query at all.
        if matches!(self, Self::Command(m) if m.has_search())
            || matches!(self, Self::Tag(t) if t.has_search())
            || matches!(self, Self::Attr(_))
        {
            return;
        }

        // Compute the replacement before assigning: the arms below borrow `self`
        // immutably while the assignment needs it mutably.
        if let Self::Emoji(picker) = self {
            if picker.query.is_none() && !picker.refilter(q) {
                *self = Overlay::None;
            }
            return;
        }

        let next = match &*self {
            Self::Command(c) => c.refilter(trigger.clone(), q).map(Overlay::Command),
            // `#tag` mode: the query is document text, so it re-filters here —
            // the emoji shortcode's contract. It stays open on no hits (the
            // create row is the offer); a dead trigger was dismissed above.
            Self::Tag(t) => Some(Overlay::Tag(t.refilter(trigger.clone(), q))),
            Self::Entity(c) => {
                // **Cloned, never rebuilt field by field.** This ran on every
                // frame the query changed, so a field the literal forgot was
                // silently reset before anyone could see it holding a value.
                // `dirty` is the one field that must not carry over: it says
                // "a request is owed", and `refilter` decides that afresh.
                let mut next = c.clone();
                next.dirty = false;
                next.refilter(trigger, q);
                Some(Overlay::Entity(next))
            }
            _ => return,
        };
        *self = next.unwrap_or(Overlay::None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use koru_core::NodeId;
    use koru_draw::theme;

    /// A menu with everything on the table, which is what these two panels want:
    /// they are about the *frame*, not about what it lists.
    const ALL: command_menu::Allows = command_menu::Allows {
        image: true,
        image_offline: false,
        divider: true,
        quote: true,
        headings: true,
        code: true,
        table: true,
        tasks: true,
        blocks: true,
        pills: true,
        attrs: true,
        task_capable: true,
        task: true,
        voice: true,
    };

    /// **Everything in a panel that is not a row still answers the pointer.**
    /// A scrollbar thumb is grabbed, a category and the rename prompt's page
    /// icon are pressed — and none of them may report as a *row*, because a row
    /// hover pre-selects the row it names.
    #[test]
    fn a_panels_controls_are_told_apart_from_its_rows() {
        let mut engine = TextEngine::new();

        // The rename prompt has no rows at all, and one button — offered only
        // by a `Rename`, which is what makes the anchor load-bearing here.
        let icon = name_prompt::icon_rect();
        let named = Overlay::Name(NamePrompt {
            trigger: Trigger {
                anchor: Anchor::Rename(koru_core::NodeId::new()),
                ..Trigger::at_start()
            },
            input: text_field::TextField::new(&mut engine, koru_draw::theme::role::ui()),
            page_icon: None,
        });
        assert_eq!(
            named.control_at(icon.centre().0, icon.centre().1),
            Some(PanelPart::Button)
        );
        assert_eq!(named.item_at(icon.centre().0, icon.centre().1), None);

        // The emoji picker's strip is a button; its thumb is a thumb.
        let picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        let scrolls = picker.scrolls();
        let thumb = emoji_picker::scrollbar_thumb(&picker);
        let strip = picker.geometry().category_rect(1).expect("a category");
        let emoji = Overlay::Emoji(picker);
        assert_eq!(
            emoji.control_at(strip.centre().0, strip.centre().1),
            Some(PanelPart::Button)
        );
        if scrolls {
            assert_eq!(
                emoji.control_at(thumb.centre().0, thumb.centre().1),
                Some(PanelPart::Thumb)
            );
        }

        // A panel with neither reports nothing rather than guessing.
        let date = Overlay::Date(DatePicker::for_pill(
            Trigger::at_start(),
            chrono::NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
        ));
        assert_eq!(date.control_at(10.0, 10.0), None);
    }

    #[test]
    fn the_entity_picker_is_a_pointer_surface_not_a_text_field() {
        let entity = Overlay::Entity(EntityPicker::new(Trigger::at_start()));
        assert!(entity.pointer_surface());
        assert!(!entity.field_at(20.0, 20.0, koru_draw::theme::PANEL_W));

        let named = Overlay::Name(NamePrompt {
            trigger: Trigger::at_start(),
            input: text_field::TextField::new(&mut TextEngine::new(), koru_draw::theme::role::ui()),
            page_icon: None,
        });
        assert!(!named.pointer_surface(), "a real prompt remains text");
    }

    /// **The language dropdown owns its whole surface, and hover pre-selects
    /// its rows.** It is a flat list like the entity picker, so a point on it
    /// that is not a row — a padding band — still belongs to the panel rather
    /// than falling through to the document underneath; and a hovered row
    /// moves the selection the way a native menu does.
    #[test]
    fn the_language_dropdown_is_a_pointer_surface_whose_rows_hover() {
        let mut overlay = Overlay::Language(LanguagePicker::open_for(NodeId::new(), None));
        let (_, h) = overlay.panel_size();

        assert!(overlay.pointer_surface());
        // The middle of the second row ("Rust") reports that row…
        let mid_row = theme::PANEL_PAD + theme::ROW_H * 1.5;
        assert_eq!(overlay.item_at(10.0, mid_row), Some(1));
        // …and the top padding band is no row at all — but the panel is still
        // the surface, which is what keeps the panel's own cursor and leaves
        // the document underneath unhovered.
        assert_eq!(overlay.item_at(10.0, 2.0), None);

        overlay.hover_item(3);
        match &overlay {
            Overlay::Language(picker) => assert_eq!(picker.sel, 3, "hover pre-selects"),
            _ => unreachable!(),
        }
        assert!(h > theme::PANEL_PAD * 2.0);
    }

    /// Every panel closes on Escape, and it is handled once rather than in each
    /// component — so a new component cannot forget to.
    #[test]
    fn escape_closes_whatever_is_open() {
        let mut engine = TextEngine::new();
        let trigger = Trigger::at_start();
        let panels = [
            Overlay::Command(CommandMenu {
                instance: 0,
                trigger: trigger.clone(),
                query: None,
                hits: command_menu::matching("", &ALL),
                sel: 0,
                first: 0,
                scrollbar_drag: None,
                allows: ALL,
            }),
            Overlay::Entity(EntityPicker::new(trigger.clone())),
            Overlay::Date(DatePicker::for_pill(
                trigger,
                chrono::NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
            )),
        ];
        for mut panel in panels {
            let out = panel.key(
                &mut engine,
                &Key::Named(NamedKey::Escape),
                None,
                ModifiersState::empty(),
            );
            assert!(matches!(out, Outcome::Close));
        }
    }

    #[test]
    fn a_closed_overlay_ignores_everything() {
        let mut engine = TextEngine::new();
        let out = Overlay::None.key(
            &mut engine,
            &Key::Character("x".into()),
            Some("x"),
            ModifiersState::empty(),
        );
        assert!(matches!(out, Outcome::Ignored));
        assert!(!Overlay::None.is_open());
        assert_eq!(Overlay::None.panel_size(), (0.0, 0.0));
    }

    /// A panel has to be at least as tall as its padding, or it draws inside
    /// itself. Cheap, and it catches a zero-hit list.
    #[test]
    fn every_panel_has_a_size() {
        let trigger = Trigger::at_start();
        let panels = [
            Overlay::Command(CommandMenu {
                instance: 0,
                trigger: trigger.clone(),
                query: None,
                hits: vec![],
                sel: 0,
                first: 0,
                scrollbar_drag: None,
                allows: ALL,
            }),
            Overlay::Entity(EntityPicker::new(trigger.clone())),
            Overlay::Date(DatePicker::for_pill(
                trigger,
                chrono::NaiveDate::from_ymd_opt(2026, 8, 2).unwrap(),
            )),
        ];
        for panel in panels {
            let (w, h) = panel.panel_size();
            assert!(w > 0.0 && h > koru_draw::theme::PANEL_PAD * 2.0);
        }
    }
}
