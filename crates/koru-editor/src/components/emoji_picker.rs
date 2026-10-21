// SPDX-License-Identifier: MPL-2.0

//! Native Unicode emoji picker shared by the command, shortcut, and shortcode.
//!
//! The grid lists default skin tones only. A short click inserts the preferred
//! tone; a long-press opens a strip of variants and, when one is chosen, updates
//! that preference.

use std::ops::Range;
use std::time::{Duration, Instant};

use emojis::Group;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::Outcome;
use super::name_prompt::NamePrompt;
use super::text_field::TextField;
use crate::emoji::{self, EmojiSearch};
use crate::trigger::Trigger;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;

pub const PANEL_W: f32 = 380.0;
pub const SEARCH_H: f32 = 30.0;
pub const CELL: f32 = 42.0;
pub const COLS: usize = 8;
pub const ROWS: usize = 6;
pub const VISIBLE: usize = COLS * ROWS;

pub const GRID_X: f32 = 12.0;
const TOP: f32 = 10.0;
const SEARCH_GAP: f32 = 8.0;
const FOOTER_H: f32 = 24.0;
const CATEGORY_H: f32 = 36.0;
const BOTTOM: f32 = 8.0;
const CATEGORY_CELL: f32 = 38.0;
const SCROLLBAR_W: f32 = 5.0;
/// The scrollbar's distance from the grid, and so the room the grid leaves
/// at its right for it.
const SCROLLBAR_GAP: f32 = 8.0;
const SCROLLBAR_GUTTER: f32 = SCROLLBAR_GAP + SCROLLBAR_W;
/// Hold this long on a tone-capable cell before the tone strip opens.
pub const LONG_PRESS_MS: u64 = 400;
const TONE_PAD: f32 = 6.0;
const TONE_CELL: f32 = 36.0;

pub const GROUPS: [Group; 9] = [
    Group::SmileysAndEmotion,
    Group::PeopleAndBody,
    Group::AnimalsAndNature,
    Group::FoodAndDrink,
    Group::TravelAndPlaces,
    Group::Activities,
    Group::Objects,
    Group::Symbols,
    Group::Flags,
];

/// Open skin-tone strip for one grid cell.
#[derive(Clone, Debug)]
pub struct ToneStrip {
    /// Index into [`EmojiPicker::hits`].
    pub hit: usize,
    pub tones: Vec<&'static str>,
    /// Panel-local origin of the cell that was held.
    pub cell: Rect,
    /// The panel the strip is kept inside: its width and height.
    pub bounds: (f32, f32),
}

/// Columns in the tone grid — same pitch as the main picker so long lists
/// (handshake has 26) wrap instead of a single overflowing row.
pub const TONE_COLS: usize = COLS;

/// Armed press waiting for release or long-press.
struct CellPress {
    hit: usize,
    since: Instant,
}

pub struct EmojiPicker {
    pub trigger: Trigger,
    /// `Some` for the full picker. Shortcode completion leaves its query in the
    /// document and therefore has no second editable field.
    pub query: Option<TextField>,
    pub search: EmojiSearch,
    /// Every matching record. Drawing exposes only the window beginning at
    /// `first`, so selection can move continuously through the complete list.
    pub hits: Vec<usize>,
    pub sel: usize,
    pub first: usize,
    pub category: Option<Group>,
    /// Rename prompt temporarily covered by a page-icon picker. Moving the
    /// prompt here preserves its text, selection, undo history, and caret.
    pub resume_name: Option<NamePrompt>,
    /// Offset from the scrollbar thumb's top where the press landed. Kept in
    /// the picker so the drag remains captured even after leaving the panel.
    scrollbar_drag: Option<f32>,
    press: Option<CellPress>,
    pub tone_strip: Option<ToneStrip>,
    /// Tone cell under the pointer while the strip is open.
    pub tone_hover: Option<usize>,
    /// `Some` when the picker fills a given width edge to edge (an
    /// appearance panel) rather than being the editor's fixed popover.
    fill_width: Option<f32>,
}

impl EmojiPicker {
    pub fn standalone(engine: &mut TextEngine, trigger: Trigger) -> Self {
        let mut picker = Self {
            trigger,
            query: Some(TextField::new(engine, koru_draw::theme::role::ui())),
            search: EmojiSearch::new(),
            hits: Vec::new(),
            sel: 0,
            first: 0,
            category: Some(Group::SmileysAndEmotion),
            resume_name: None,
            scrollbar_drag: None,
            press: None,
            tone_strip: None,
            tone_hover: None,
            fill_width: None,
        };
        picker.refresh_hits();
        picker
    }

    pub fn shortcode(trigger: Trigger, query: &str) -> Option<Self> {
        let mut picker = Self {
            trigger,
            query: None,
            search: EmojiSearch::new(),
            hits: Vec::new(),
            sel: 0,
            first: 0,
            category: None,
            resume_name: None,
            scrollbar_drag: None,
            press: None,
            tone_strip: None,
            tone_hover: None,
            fill_width: None,
        };
        picker.search.set_query(query);
        picker.refresh_hits();
        (!picker.hits.is_empty()).then_some(picker)
    }

    pub fn height(&self) -> f32 {
        self.geometry().panel_height()
    }

    /// Fill `width` edge to edge: no inset, and cells stretched so the grid
    /// and its scrollbar span it. The column count — and so every keyboard
    /// and scroll rule — is unchanged.
    pub fn set_fill_width(&mut self, width: f32) {
        self.fill_width = Some(width);
    }

    /// Where everything in this picker sits, panel-local.
    pub fn geometry(&self) -> Geometry {
        let has_search = self.query.is_some();
        match self.fill_width {
            Some(width) => Geometry::filling(width, has_search),
            None => Geometry::popup(has_search),
        }
    }

    pub fn refilter(&mut self, query: &str) -> bool {
        if self.query.is_some() {
            return true;
        }
        self.search.set_query(query);
        self.sel = 0;
        self.first = 0;
        self.refresh_hits();
        !self.hits.is_empty()
    }

    fn refresh_hits(&mut self) {
        self.hits = self.search.results(emoji::catalog().len());
        if let Some(category) = self.category {
            self.hits
                .retain(|&index| emoji::catalog()[index].category == category);
        }
        self.sel = self.sel.min(self.hits.len().saturating_sub(1));
        self.clamp_first();
        self.ensure_selection_visible();
        self.press = None;
        self.tone_strip = None;
        self.tone_hover = None;
        self.tone_hover = None;
    }

    pub fn visible_range(&self) -> Range<usize> {
        self.first..(self.first + VISIBLE).min(self.hits.len())
    }

    pub fn visible_hits(&self) -> &[usize] {
        &self.hits[self.visible_range()]
    }

    /// Glyph drawn in a grid cell: preferred tone applied when the base supports it.
    pub fn display_value(base: &str) -> String {
        emoji::with_preferred_tone(base)
    }

    pub fn scroll_rows(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        let row = self.first / COLS;
        let next = (row as isize + delta).clamp(0, self.max_scroll_row() as isize) as usize;
        self.first = next * COLS;

        // Enter must always commit an item that is actually visible. Preserve
        // its column when the scroll window moves past the current selection.
        if self.sel < self.first {
            self.sel = self.first;
        } else if self.sel >= self.first + VISIBLE {
            self.sel = (self.first + VISIBLE - 1).min(self.hits.len() - 1);
        }
        self.press = None;
        self.tone_strip = None;
        self.tone_hover = None;
    }

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
        if named == Some(NamedKey::Escape) && self.tone_strip.is_some() {
            self.tone_strip = None;
            self.tone_hover = None;
            self.press = None;
            return Outcome::Consumed;
        }
        match named {
            Some(NamedKey::ArrowRight) => self.move_by(1),
            Some(NamedKey::ArrowLeft) => self.move_by(-1),
            Some(NamedKey::ArrowDown) => self.move_vertical(1),
            Some(NamedKey::ArrowUp) => self.move_vertical(-1),
            Some(NamedKey::Tab) => {
                self.change_category(engine, if mods.shift_key() { -1 } else { 1 })
            }
            Some(NamedKey::PageDown) => self.move_by(VISIBLE as isize),
            Some(NamedKey::PageUp) => self.move_by(-(VISIBLE as isize)),
            Some(NamedKey::Enter) => return self.choose(self.sel),
            _ => {
                let Some(field) = &mut self.query else {
                    // Printable shortcode keys continue into the document.
                    return Outcome::Ignored;
                };
                field.key(engine, key, text, mods);
                let query = field.raw_text();
                self.search.set_query(query);
                // A typed search spans categories; erasing it returns to the
                // first, most familiar category.
                self.category = query.is_empty().then_some(Group::SmileysAndEmotion);
                self.sel = 0;
                self.first = 0;
                self.refresh_hits();
            }
        }
        Outcome::Consumed
    }

    /// Search-field typing for a host that owns Tab itself (the appearance modal).
    pub fn type_in_search(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) {
        let Some(field) = &mut self.query else {
            return;
        };
        field.key(engine, key, text, mods);
        let query = field.raw_text();
        self.search.set_query(query);
        self.category = query.is_empty().then_some(Group::SmileysAndEmotion);
        self.sel = 0;
        self.first = 0;
        self.refresh_hits();
    }

    pub fn cycle_category(&mut self, engine: &mut TextEngine, delta: isize) {
        self.change_category(engine, delta);
    }

    fn move_by(&mut self, delta: isize) {
        if self.hits.is_empty() {
            return;
        }
        self.sel = (self.sel as isize + delta).clamp(0, self.hits.len() as isize - 1) as usize;
        self.ensure_selection_visible();
        self.press = None;
        self.tone_strip = None;
        self.tone_hover = None;
    }

    fn move_vertical(&mut self, rows: isize) {
        if self.hits.is_empty() {
            return;
        }
        let candidate = self.sel as isize + rows * COLS as isize;
        if candidate >= 0 && candidate < self.hits.len() as isize {
            self.sel = candidate as usize;
        }
        self.ensure_selection_visible();
        self.press = None;
        self.tone_strip = None;
        self.tone_hover = None;
    }

    fn change_category(&mut self, engine: &mut TextEngine, delta: isize) {
        let current = self
            .category
            .and_then(|group| GROUPS.iter().position(|candidate| *candidate == group))
            .unwrap_or(0);
        let next = (current as isize + delta).rem_euclid(GROUPS.len() as isize) as usize;
        self.select_category(engine, next);
    }

    fn select_category(&mut self, engine: &mut TextEngine, index: usize) {
        let Some(&category) = GROUPS.get(index) else {
            return;
        };
        self.category = Some(category);
        if let Some(field) = &mut self.query {
            field.set_text(engine, "");
            self.search.set_query("");
        }
        self.sel = 0;
        self.first = 0;
        self.refresh_hits();
    }

    fn max_scroll_row(&self) -> usize {
        self.hits.len().div_ceil(COLS).saturating_sub(ROWS)
    }

    /// Whether the grid is taller than the window it is shown in — the same
    /// question `CommandMenu::scrolls` answers, so the thumb is only a thing to
    /// grab when there is somewhere for it to go.
    pub fn scrolls(&self) -> bool {
        self.max_scroll_row() > 0
    }

    fn clamp_first(&mut self) {
        self.first = (self.first / COLS).min(self.max_scroll_row()) * COLS;
    }

    fn ensure_selection_visible(&mut self) {
        if self.hits.is_empty() {
            self.first = 0;
            return;
        }
        let selected_row = self.sel / COLS;
        let first_row = self.first / COLS;
        if selected_row < first_row {
            self.first = selected_row * COLS;
        } else if selected_row >= first_row + ROWS {
            self.first = (selected_row - ROWS + 1) * COLS;
        }
        self.clamp_first();
    }

    pub fn click(&mut self, engine: &mut TextEngine, local_x: f32, local_y: f32) -> Outcome {
        if let Some(strip) = &self.tone_strip {
            if let Some(tone) = tone_at(strip, local_x, local_y) {
                return self.commit_value(tone);
            }
            // A press outside the strip dismisses it without inserting.
            self.tone_strip = None;
            self.tone_hover = None;
            self.press = None;
            return Outcome::Consumed;
        }
        if let Some(category) = self.geometry().category_at(local_x, local_y) {
            self.select_category(engine, category);
            return Outcome::Consumed;
        }
        let thumb = scrollbar_thumb(self);
        if thumb.contains(local_x, local_y) {
            self.scrollbar_drag = Some(local_y - thumb.y);
            return Outcome::Consumed;
        }
        let track = self.geometry().scrollbar_rect();
        if track.contains(local_x, local_y) {
            let ratio = ((local_y - track.y) / track.h).clamp(0.0, 1.0);
            self.set_scroll_row((ratio * self.max_scroll_row() as f32).round() as usize);
            return Outcome::Consumed;
        }
        let Some(index) = self.item_at(local_x, local_y) else {
            return Outcome::Consumed;
        };
        self.sel = index;
        // Arm the press; commit happens on release (or the tone strip on hold).
        self.press = Some(CellPress {
            hit: index,
            since: Instant::now(),
        });
        self.tone_strip = None;
        self.tone_hover = None;
        Outcome::Consumed
    }

    /// Release after a cell press: short click commits; long-press already opened
    /// the strip and leaves it open.
    pub fn release(&mut self) -> Outcome {
        let Some(press) = self.press.take() else {
            return Outcome::Consumed;
        };
        if self.tone_strip.is_some() {
            // Strip is open — wait for a tone click.
            return Outcome::Consumed;
        }
        if self.item_base(press.hit).is_some() {
            return self.choose(press.hit);
        }
        Outcome::Consumed
    }

    /// Promote an armed long-press into a tone strip when the hold is long enough.
    /// Returns true when the strip just opened (caller should redraw).
    pub fn tick_long_press(&mut self) -> bool {
        let Some(press) = &self.press else {
            return false;
        };
        if self.tone_strip.is_some() {
            return false;
        }
        if press.since.elapsed() < Duration::from_millis(LONG_PRESS_MS) {
            return false;
        }
        let hit = press.hit;
        let Some(base) = self.item_base(hit) else {
            return false;
        };
        let tones = emoji::skin_tones_of(base);
        if tones.len() <= 1 {
            return false;
        }
        let Some(cell) = self.cell_rect(hit) else {
            return false;
        };
        let geometry = self.geometry();
        self.tone_strip = Some(ToneStrip {
            hit,
            tones,
            cell,
            bounds: (geometry.width, geometry.panel_height()),
        });
        self.tone_hover = None;
        true
    }

    /// Update which tone cell is hovered. Returns true when the highlight moved.
    pub fn set_tone_hover(&mut self, local_x: f32, local_y: f32) -> bool {
        let next = self
            .tone_strip
            .as_ref()
            .and_then(|s| tone_index_at(s, local_x, local_y));
        if next == self.tone_hover {
            return false;
        }
        self.tone_hover = next;
        true
    }

    /// Whether the tone strip (when open) covers this panel-local point.
    pub fn tone_strip_contains(&self, local_x: f32, local_y: f32) -> bool {
        self.tone_strip
            .as_ref()
            .is_some_and(|s| tone_strip_rect(s).contains(local_x, local_y))
    }

    /// Shell keeps waking while a long-press is armed or a strip is open.
    pub fn press_animating(&self) -> bool {
        if self.tone_strip.is_some() {
            return false;
        }
        self.press.as_ref().is_some_and(|p| {
            let base = self.item_base(p.hit).unwrap_or("");
            emoji::has_skin_tones(base) && p.since.elapsed() < Duration::from_millis(LONG_PRESS_MS)
        })
    }

    fn item_base(&self, hit: usize) -> Option<&'static str> {
        self.hits
            .get(hit)
            .map(|&record| emoji::catalog()[record].value)
    }

    pub fn cell_rect(&self, hit: usize) -> Option<Rect> {
        if hit < self.first || hit >= self.first + VISIBLE || hit >= self.hits.len() {
            return None;
        }
        let cell = hit - self.first;
        let col = cell % COLS;
        let row = cell / COLS;
        let geometry = self.geometry();
        let grid = geometry.grid_rect();
        Some(Rect::new(
            grid.x + col as f32 * geometry.cell,
            grid.y + row as f32 * geometry.cell,
            geometry.cell,
            geometry.cell,
        ))
    }

    pub fn item_at(&self, local_x: f32, local_y: f32) -> Option<usize> {
        // The tone strip owns the pointer while open — grid cells under it must
        // not steal hover or selection.
        if self.tone_strip_contains(local_x, local_y) {
            return None;
        }
        let geometry = self.geometry();
        let grid = geometry.grid_rect();
        // Grid cells are half-open so the shared boundary with the footer
        // cannot select the first off-screen item.
        if local_x < grid.x
            || local_x >= grid.x + grid.w
            || local_y < grid.y
            || local_y >= grid.y + grid.h
        {
            return None;
        }
        let col = ((local_x - grid.x) / geometry.cell).floor() as usize;
        let row = ((local_y - grid.y) / geometry.cell).floor() as usize;
        let index = self.first + row * COLS + col.min(COLS - 1);
        (index < self.hits.len()).then_some(index)
    }

    fn choose(&self, index: usize) -> Outcome {
        let Some(base) = self.item_base(index) else {
            return Outcome::Consumed;
        };
        self.commit_value(&emoji::with_preferred_tone(base))
    }

    fn commit_value(&self, value: &str) -> Outcome {
        emoji::remember_tone_from_value(value);
        // Catalog rows are defaults only — rank the base so recents still light
        // the cell the user just used.
        let base = emojis::get(value)
            .and_then(|e| {
                e.skin_tones()
                    .and_then(|mut t| t.next().map(|d| d.as_str()))
            })
            .unwrap_or(value);
        emoji::record_used(base);
        Outcome::CommitText(value.to_owned())
    }

    pub fn is_dragging_scrollbar(&self) -> bool {
        self.scrollbar_drag.is_some()
    }

    pub fn drag_scrollbar(&mut self, local_y: f32) {
        let Some(grab) = self.scrollbar_drag else {
            return;
        };
        let track = self.geometry().scrollbar_rect();
        let thumb = scrollbar_thumb(self);
        let travel = (track.h - thumb.h).max(0.0);
        let top = (local_y - track.y - grab).clamp(0.0, travel);
        let row = if travel > 0.0 {
            (top / travel * self.max_scroll_row() as f32).round() as usize
        } else {
            0
        };
        self.set_scroll_row(row);
    }

    pub fn stop_scrollbar_drag(&mut self) -> bool {
        self.scrollbar_drag.take().is_some()
    }

    fn set_scroll_row(&mut self, row: usize) {
        self.first = row.min(self.max_scroll_row()) * COLS;
        if self.hits.is_empty() {
            self.sel = 0;
        } else if self.sel < self.first {
            self.sel = self.first;
        } else if self.sel >= self.first + VISIBLE {
            self.sel = (self.first + VISIBLE - 1).min(self.hits.len() - 1);
        }
        self.press = None;
        self.tone_strip = None;
        self.tone_hover = None;
    }

    pub fn field_rect(&self) -> Option<Rect> {
        self.query.as_ref().map(|_| self.geometry().search_rect())
    }

    pub fn click_field(&mut self, engine: &mut TextEngine, x: f32) {
        let rect = self.geometry().search_rect();
        if let Some(field) = &mut self.query {
            field.click(engine, x - rect.x + field.scroll());
        }
        self.press = None;
        self.tone_strip = None;
        self.tone_hover = None;
    }

    pub fn drag_field(&mut self, engine: &mut TextEngine, x: f32) {
        let rect = self.geometry().search_rect();
        if let Some(field) = &mut self.query {
            field.drag_to(engine, x - rect.x + field.scroll());
        }
    }
}

/// Size of a tone strip for `n` tones (padding included).
pub fn tone_strip_size(n: usize) -> (f32, f32) {
    let n = n.max(1);
    let cols = n.min(TONE_COLS);
    let rows = n.div_ceil(TONE_COLS).max(1);
    (
        TONE_PAD * 2.0 + cols as f32 * TONE_CELL,
        TONE_PAD * 2.0 + rows as f32 * TONE_CELL,
    )
}

/// Panel-local rect of the tone strip for a cell.
///
/// Long lists (e.g. handshake's 26 combinations) wrap at [`TONE_COLS`] rather
/// than a single overflowing row.
pub fn tone_strip_rect(strip: &ToneStrip) -> Rect {
    let (w, h) = tone_strip_size(strip.tones.len());
    let (panel_w, panel_h) = strip.bounds;
    let x = (strip.cell.x + strip.cell.w * 0.5 - w * 0.5).clamp(4.0, (panel_w - w - 4.0).max(4.0));
    // Prefer above the cell; fall below if there is no room.
    let mut y = if strip.cell.y >= h + 4.0 {
        strip.cell.y - h - 4.0
    } else {
        strip.cell.y + strip.cell.h + 4.0
    };
    // Keep the multi-row panel inside the emoji popover when possible.
    let max_y = (panel_h - h - 4.0).max(4.0);
    y = y.clamp(4.0, max_y);
    Rect::new(x, y, w, h)
}

/// Window-space tone strip anchored to a glyph `cell`, clamped into `viewport`.
pub fn tone_strip_rect_window(cell: Rect, n_tones: usize, viewport: (f32, f32)) -> Rect {
    let (w, h) = tone_strip_size(n_tones);
    let x = (cell.x + cell.w * 0.5 - w * 0.5).clamp(4.0, (viewport.0 - w - 4.0).max(4.0));
    let mut y = if cell.y >= h + 4.0 {
        cell.y - h - 4.0
    } else {
        cell.y + cell.h + 4.0
    };
    y = y.clamp(4.0, (viewport.1 - h - 4.0).max(4.0));
    Rect::new(x, y, w, h)
}

/// Tone cell index under a window-space point for a body strip.
pub fn tone_index_at_window(
    cell: Rect,
    n_tones: usize,
    viewport: (f32, f32),
    x: f32,
    y: f32,
) -> Option<usize> {
    let panel = tone_strip_rect_window(cell, n_tones, viewport);
    if !panel.contains(x, y) {
        return None;
    }
    let col = ((x - panel.x - TONE_PAD) / TONE_CELL).floor() as isize;
    let row = ((y - panel.y - TONE_PAD) / TONE_CELL).floor() as isize;
    if col < 0 || row < 0 {
        return None;
    }
    let i = row as usize * TONE_COLS + col as usize;
    (i < n_tones).then_some(i)
}

pub fn tone_cell_rect_window(
    cell: Rect,
    n_tones: usize,
    viewport: (f32, f32),
    index: usize,
) -> Option<Rect> {
    if index >= n_tones {
        return None;
    }
    let panel = tone_strip_rect_window(cell, n_tones, viewport);
    let col = index % TONE_COLS;
    let row = index / TONE_COLS;
    Some(Rect::new(
        panel.x + TONE_PAD + col as f32 * TONE_CELL,
        panel.y + TONE_PAD + row as f32 * TONE_CELL,
        TONE_CELL,
        TONE_CELL,
    ))
}

pub fn tone_cell_rect(strip: &ToneStrip, index: usize) -> Option<Rect> {
    if index >= strip.tones.len() {
        return None;
    }
    let panel = tone_strip_rect(strip);
    let col = index % TONE_COLS;
    let row = index / TONE_COLS;
    Some(Rect::new(
        panel.x + TONE_PAD + col as f32 * TONE_CELL,
        panel.y + TONE_PAD + row as f32 * TONE_CELL,
        TONE_CELL,
        TONE_CELL,
    ))
}

/// Tone cell index under a panel-local point, if any.
pub fn tone_index_at(strip: &ToneStrip, local_x: f32, local_y: f32) -> Option<usize> {
    let panel = tone_strip_rect(strip);
    if !panel.contains(local_x, local_y) {
        return None;
    }
    let col = ((local_x - panel.x - TONE_PAD) / TONE_CELL).floor() as isize;
    let row = ((local_y - panel.y - TONE_PAD) / TONE_CELL).floor() as isize;
    if col < 0 || row < 0 {
        return None;
    }
    let i = row as usize * TONE_COLS + col as usize;
    (i < strip.tones.len()).then_some(i)
}

fn tone_at(strip: &ToneStrip, local_x: f32, local_y: f32) -> Option<&'static str> {
    tone_index_at(strip, local_x, local_y).map(|i| strip.tones[i])
}

/// Where everything in a picker sits, panel-local. The editor's popover and
/// an appearance panel's embedded picker differ only in these numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub width: f32,
    /// The search field's and the grid's distance from the panel's sides.
    pub inset: f32,
    /// One grid cell's side.
    pub cell: f32,
    pub category_cell: f32,
    pub has_search: bool,
}

impl Geometry {
    /// The editor's popover: [`PANEL_W`] wide, inset, [`CELL`] cells.
    pub const fn popup(has_search: bool) -> Self {
        Self {
            width: PANEL_W,
            inset: GRID_X,
            cell: CELL,
            category_cell: CATEGORY_CELL,
            has_search,
        }
    }

    /// Edge to edge across `width`: the search field and grid start at its
    /// left, the scrollbar ends at its right, the categories share it.
    pub fn filling(width: f32, has_search: bool) -> Self {
        Self {
            width,
            inset: 0.0,
            cell: ((width - SCROLLBAR_GUTTER) / COLS as f32).max(1.0),
            category_cell: width / GROUPS.len() as f32,
            has_search,
        }
    }

    pub fn panel_height(&self) -> f32 {
        self.category_top() + CATEGORY_H + BOTTOM
    }

    pub fn search_rect(&self) -> Rect {
        Rect::new(self.inset, TOP, self.width - self.inset * 2.0, SEARCH_H)
    }

    pub fn grid_top(&self) -> f32 {
        if self.has_search {
            TOP + SEARCH_H + SEARCH_GAP
        } else {
            TOP
        }
    }

    pub fn grid_rect(&self) -> Rect {
        Rect::new(
            self.inset,
            self.grid_top(),
            COLS as f32 * self.cell,
            ROWS as f32 * self.cell,
        )
    }

    pub fn footer_top(&self) -> f32 {
        self.grid_top() + ROWS as f32 * self.cell
    }

    pub fn category_top(&self) -> f32 {
        self.footer_top() + FOOTER_H
    }

    pub fn category_rect(&self, index: usize) -> Option<Rect> {
        if index >= GROUPS.len() {
            return None;
        }
        let width = GROUPS.len() as f32 * self.category_cell;
        let left = (self.width - width) * 0.5;
        Some(Rect::new(
            left + index as f32 * self.category_cell,
            self.category_top(),
            self.category_cell,
            CATEGORY_H,
        ))
    }

    pub fn category_at(&self, x: f32, y: f32) -> Option<usize> {
        (0..GROUPS.len()).find(|&index| {
            self.category_rect(index)
                .is_some_and(|rect| rect.contains(x, y))
        })
    }

    pub fn scrollbar_rect(&self) -> Rect {
        Rect::new(
            self.inset + COLS as f32 * self.cell + SCROLLBAR_GAP,
            self.grid_top(),
            SCROLLBAR_W,
            ROWS as f32 * self.cell,
        )
    }
}

pub fn category_icon(group: Group) -> &'static str {
    match group {
        Group::SmileysAndEmotion => "😀",
        Group::PeopleAndBody => "👋",
        Group::AnimalsAndNature => "🐻",
        Group::FoodAndDrink => "🍔",
        Group::TravelAndPlaces => "🚗",
        Group::Activities => "⚽",
        Group::Objects => "💡",
        Group::Symbols => "🔣",
        Group::Flags => "🏳️",
    }
}

pub fn scrollbar_thumb(picker: &EmojiPicker) -> Rect {
    let track = picker.geometry().scrollbar_rect();
    let total_rows = picker.hits.len().div_ceil(COLS).max(1);
    let thumb_h = (track.h * ROWS as f32 / total_rows as f32).clamp(24.0, track.h);
    let travel = track.h - thumb_h;
    let y = if picker.max_scroll_row() == 0 {
        track.y
    } else {
        track.y + travel * (picker.first / COLS) as f32 / picker.max_scroll_row() as f32
    };
    Rect::new(track.x, y, track.w, thumb_h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use emojis::SkinTone;

    #[test]
    fn keyboard_selection_scrolls_continuously_and_commits_unicode_text() {
        let mut engine = TextEngine::new();
        let mut picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        picker.sel = VISIBLE - COLS;
        picker.move_vertical(1);
        assert_eq!(picker.sel, VISIBLE);
        assert_eq!(picker.first, COLS);
        assert!(matches!(picker.choose(0), Outcome::CommitText(_)));
    }

    #[test]
    fn compact_panel_contains_grid_footer_and_categories() {
        let bottom = Geometry::popup(false).category_top() + CATEGORY_H;
        assert!(Geometry::popup(false).panel_height() >= bottom);
        assert!(
            Geometry::popup(false).footer_top()
                >= Geometry::popup(false).grid_rect().y + Geometry::popup(false).grid_rect().h
        );
    }

    #[test]
    fn the_category_strip_is_a_control_and_not_a_cell() {
        // A category is pressable, and it must not be mistaken for a grid
        // cell, because a cell hover moves the selection. (It wears the plain
        // arrow like everything else: Koru does not use the hand cursor.)
        let has_search = true;
        let strip = Geometry::popup(has_search).category_rect(2).unwrap();
        let (cx, cy) = strip.centre();
        assert!(Geometry::popup(has_search).category_at(cx, cy).is_some());
        let picker = EmojiPicker {
            query: Some(TextField::new(
                &mut TextEngine::new(),
                koru_draw::theme::role::ui(),
            )),
            ..EmojiPicker::standalone(&mut TextEngine::new(), Trigger::at_start())
        };
        assert_eq!(
            picker.item_at(cx, cy),
            None,
            "the strip is below the grid, and the grid stops at its own edge"
        );
    }

    #[test]
    fn category_and_item_hit_tests_round_trip() {
        let mut engine = TextEngine::new();
        let picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        let category = Geometry::popup(true).category_rect(3).unwrap();
        assert_eq!(
            Geometry::popup(true).category_at(category.centre().0, category.centre().1),
            Some(3)
        );
        let grid = Geometry::popup(true).grid_rect();
        assert_eq!(
            picker.item_at(grid.x + CELL * 0.5, grid.y + CELL * 0.5),
            Some(0)
        );
        assert_eq!(
            picker.item_at(grid.x, Geometry::popup(true).footer_top() + 1.0),
            None
        );
    }

    #[test]
    fn scrollbar_stays_inside_its_track_and_moves_with_the_window() {
        let mut engine = TextEngine::new();
        let mut picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        let first = scrollbar_thumb(&picker);
        picker.scroll_rows(2);
        let moved = scrollbar_thumb(&picker);
        let track = Geometry::popup(true).scrollbar_rect();
        assert!(moved.y >= first.y);
        assert!(moved.y + moved.h <= track.y + track.h + f32::EPSILON);
    }

    #[test]
    fn scrollbar_thumb_can_be_grabbed_and_dragged_to_the_end() {
        let mut engine = TextEngine::new();
        let mut picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        let thumb = scrollbar_thumb(&picker);
        let (x, y) = thumb.centre();
        assert!(matches!(picker.click(&mut engine, x, y), Outcome::Consumed));
        assert!(picker.is_dragging_scrollbar());

        let track = Geometry::popup(true).scrollbar_rect();
        picker.drag_scrollbar(track.y + track.h + 100.0);
        assert_eq!(picker.first / COLS, picker.max_scroll_row());
        assert!(picker.stop_scrollbar_drag());
    }

    /// First tone-capable emoji currently visible in the picker's hits.
    fn first_tone_hit(picker: &EmojiPicker) -> usize {
        picker
            .hits
            .iter()
            .copied()
            .enumerate()
            .find(|&(hit, record)| {
                hit < VISIBLE && emoji::has_skin_tones(emoji::catalog()[record].value)
            })
            .map(|(hit, _)| hit)
            .expect("people category opens with tone-capable hands")
    }

    #[test]
    fn short_click_commits_with_preferred_tone() {
        let _guard = emoji::tone_test_lock();
        emoji::set_preferred_skin_tone(SkinTone::Medium);
        let mut engine = TextEngine::new();
        let mut picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        // People & Body is where hands live.
        picker.select_category(&mut engine, 1);
        let hit = first_tone_hit(&picker);
        let base = emoji::catalog()[picker.hits[hit]].value;
        let expected = emoji::with_preferred_tone(base);
        let Outcome::CommitText(value) = picker.choose(hit) else {
            panic!("expected commit");
        };
        assert_eq!(value, expected);
        assert_ne!(value, base, "medium tone should change the glyph");
        emoji::set_preferred_skin_tone(SkinTone::Default);
    }

    #[test]
    fn long_press_opens_tone_strip_for_tone_capable_emoji() {
        let mut engine = TextEngine::new();
        let mut picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        picker.select_category(&mut engine, 1);
        let hit = first_tone_hit(&picker);
        let cell = picker.cell_rect(hit).expect("visible");
        let (x, y) = cell.centre();
        assert!(matches!(picker.click(&mut engine, x, y), Outcome::Consumed));
        assert!(picker.tone_strip.is_none());
        // Force the armed press past the threshold.
        if let Some(press) = &mut picker.press {
            press.since = Instant::now() - Duration::from_millis(LONG_PRESS_MS + 50);
        }
        assert!(picker.tick_long_press());
        let strip = picker.tone_strip.as_ref().expect("strip open");
        assert!(strip.tones.len() > 1);
    }

    #[test]
    fn choosing_a_tone_from_the_strip_updates_preference() {
        let _guard = emoji::tone_test_lock();
        emoji::set_preferred_skin_tone(SkinTone::Default);
        let mut engine = TextEngine::new();
        let mut picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        picker.select_category(&mut engine, 1);
        let hit = first_tone_hit(&picker);
        picker.press = Some(CellPress {
            hit,
            since: Instant::now() - Duration::from_millis(LONG_PRESS_MS + 1),
        });
        assert!(picker.tick_long_press());
        let strip = picker.tone_strip.clone().unwrap();
        // Pick a non-default primary tone when one exists.
        let tone_i = strip
            .tones
            .iter()
            .position(|t| emoji::primary_tone_of(t) == Some(SkinTone::MediumDark))
            .or_else(|| strip.tones.get(1).map(|_| 1))
            .expect("at least a second tone");
        let chosen = strip.tones[tone_i];
        let rect = tone_cell_rect(&strip, tone_i).unwrap();
        let (x, y) = rect.centre();
        let Outcome::CommitText(value) = picker.click(&mut engine, x, y) else {
            panic!("expected commit");
        };
        assert_eq!(value, chosen);
        if let Some(tone) = emoji::primary_tone_of(chosen) {
            assert_eq!(emoji::preferred_skin_tone(), tone);
        }
        emoji::set_preferred_skin_tone(SkinTone::Default);
    }

    #[test]
    fn release_without_long_press_commits() {
        // The tone is process-wide: an unguarded reset here once landed in the
        // middle of short_click_commits_with_preferred_tone's Medium.
        let _guard = emoji::tone_test_lock();
        emoji::set_preferred_skin_tone(SkinTone::Default);
        let mut engine = TextEngine::new();
        let mut picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        let hit = 0;
        let cell = picker.cell_rect(hit).unwrap();
        let (x, y) = cell.centre();
        picker.click(&mut engine, x, y);
        assert!(matches!(picker.release(), Outcome::CommitText(_)));
    }

    #[test]
    fn long_tone_lists_wrap_to_eight_columns() {
        let mut strip = ToneStrip {
            hit: 0,
            tones: vec!["👍"; 26],
            cell: Rect::new(40.0, 80.0, CELL, CELL),
            bounds: (PANEL_W, Geometry::popup(true).panel_height()),
        };
        let panel = tone_strip_rect(&strip);
        assert!(
            panel.w <= TONE_PAD * 2.0 + TONE_COLS as f32 * TONE_CELL + 0.5,
            "width stays within 8 columns, got {}",
            panel.w
        );
        assert!(
            panel.h >= TONE_PAD * 2.0 + 4.0 * TONE_CELL - 0.5,
            "26 tones need 4 rows"
        );
        // Every cell is inside the panel and none overlap.
        for i in 0..26 {
            let c = tone_cell_rect(&strip, i).unwrap();
            assert!(c.x >= panel.x && c.x + c.w <= panel.x + panel.w + 0.5);
            assert!(c.y >= panel.y && c.y + c.h <= panel.y + panel.h + 0.5);
            let (cx, cy) = c.centre();
            assert_eq!(tone_index_at(&strip, cx, cy), Some(i));
        }
        strip.tones.truncate(6);
        let short = tone_strip_rect(&strip);
        assert!(
            (short.w - (TONE_PAD * 2.0 + 6.0 * TONE_CELL)).abs() < 0.5,
            "six tones stay one row"
        );
    }

    #[test]
    fn tone_strip_owns_the_pointer_over_the_grid() {
        let mut engine = TextEngine::new();
        let mut picker = EmojiPicker::standalone(&mut engine, Trigger::at_start());
        picker.select_category(&mut engine, 1);
        let hit = first_tone_hit(&picker);
        let cell = picker.cell_rect(hit).unwrap();
        picker.tone_strip = Some(ToneStrip {
            hit,
            tones: vec!["👍", "👍🏻", "👍🏼"],
            cell,
            bounds: (PANEL_W, picker.height()),
        });
        let panel = tone_strip_rect(picker.tone_strip.as_ref().unwrap());
        let (cx, cy) = panel.centre();
        assert!(picker.tone_strip_contains(cx, cy));
        assert_eq!(
            picker.item_at(cx, cy),
            None,
            "grid under the strip must not receive hover"
        );
    }
}
