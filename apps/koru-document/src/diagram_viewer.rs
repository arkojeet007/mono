// SPDX-License-Identifier: MPL-2.0

//! A drawn diagram in its own view: the page dimmed behind it, the drawing
//! at a zoom the person chooses, scrolled or dragged where it overflows,
//! with a toolbar to zoom, fit, copy, save and close.
//!
//! Opened from a drawing's magnifier control, a double click on it, or the
//! menu's *Open*; closed by `Esc`, the toolbar's `×`, or a click on the
//! dimmed page. While it is up it is **modal** in the routing sense
//! (`Blockers::Modal(ModalKind::DiagramViewer)`): every key and every press
//! is its own, and nothing underneath answers — the same claim the
//! document's confirm makes, through the same shared scrim
//! (`koru_ui::modal`).
//!
//! **What it holds is a drawing, not a block.** It takes the
//! `PreparedDiagram` the page paints — the very same shaped labels — so the
//! viewer cannot show a different picture from the one the person clicked,
//! and a later keystroke in the text cannot change the picture under them.
//! The drawing is painted through the same painter at the chosen zoom, so
//! zooming in is a vector redraw: lines stay one pixel wide and the text
//! stays sharp at 800%, where a stretched bitmap would blur. The painter
//! divides stroke widths by the scale it is given; the viewer hands it the
//! zoom, so a zoomed-in drawing has proportionally heavier lines, as a
//! magnified picture should.
//!
//! **The stage is the window above the toolbar.** The drawing is centred,
//! fitted and clamped against the window minus the band the toolbar sits
//! in ([`STAGE_BOTTOM`]), so no part of a drawing is ever only reachable
//! under the toolbar: scrolled to its bottom edge, the drawing's last row
//! sits clear above the buttons.
//!
//! **Zoom and pan are plain geometry**, kept as two numbers and clamped
//! after every change by [`DiagramViewer::settle`]: a drawing smaller than
//! the stage sits centred and cannot be moved; one larger can be moved
//! until its edge meets the stage's, never past it, so no gesture can push
//! the picture out of sight. The plain wheel scrolls it, as a page
//! scrolls — a drawing taller than the window is read by wheeling down it,
//! not by finding the drag — `Shift`+wheel scrolls it sideways, the arrow
//! keys step it, and a drag moves it under the hand. `Ctrl`+wheel and a
//! pinch zoom around the pointer; the toolbar and the keys zoom around the
//! stage's centre; either way the drawing point under the anchor stays
//! where it was.
//!
//! **The viewer only asks for copy and save.** The shell owns the clipboard
//! and the save dialog, the raster and the file, so [`Outcome::Copy`] and
//! [`Outcome::SaveAs`] are what the viewer leaves behind for the document
//! app to hand on — the same route the block's own *Save as…* takes. The
//! copy button confirms at once, the way every copy icon in Koru does
//! (`CopyAffordance`): the glyph turns to the check and holds, while the
//! shell writes the clipboard on its next pass.

use std::sync::Arc;
use std::time::Instant;

use koru_draw::diagram::PreparedDiagram;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{icons, inspect, scene, theme};
use koru_ui::CopyAffordance;
use koru_ui::button_group::ButtonGroup;
use koru_ui::icon_button::{IconButton, State as ButtonState};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

use koru_core::NodeId;
use koru_workspace_host::ZoomIntent;

#[cfg(test)]
mod tests;

/// One zoom step, in or out: a quarter at a time, so five steps in are
/// about three times the size and the percentages read as round numbers.
pub const ZOOM_STEP: f64 = 1.25;
/// The smallest zoom: a tenth of natural size.
pub const ZOOM_MIN: f64 = 0.1;
/// The largest: eight times natural size — enough to read the smallest
/// label of a large flowchart, and the point past which a page of vector
/// lines stops being a picture.
pub const ZOOM_MAX: f64 = 8.0;
/// Air between the stage's edge and a fitted drawing, on every side.
pub const FIT_MARGIN: f32 = 48.0;
/// The card the drawing sits on: the page colour, padded this much past
/// the drawing on every side, so a drawing with no frame of its own still
/// reads as a picture on the dimmed page rather than lines floating on it.
pub const CARD_PAD: f32 = 24.0;
/// The toolbar's height, and the height of each of its button slots.
pub const TOOLBAR_H: f32 = 36.0;
/// Each toolbar button's width.
pub const BUTTON_W: f32 = 32.0;
/// The toolbar's distance from the window's bottom edge.
pub const TOOLBAR_BOTTOM: f32 = 24.0;
/// The band at the window's bottom the toolbar lives in — the bar with as
/// much air above it as below — which the stage stops short of, so the
/// drawing is never centred, fitted or scrolled under the buttons.
pub const STAGE_BOTTOM: f32 = TOOLBAR_BOTTOM * 2.0 + TOOLBAR_H;
/// How far one wheel notch scrolls the drawing: about two lines of a
/// diagram's text, the way a page's wheel step is a few lines of prose.
pub const PAN_LINE: f32 = 48.0;
/// How far an arrow key moves the drawing.
pub const PAN_KEY: f32 = 40.0;
/// Air between the toolbar's button groups, where a divider would be.
const GROUP_GAP: f32 = 10.0;
/// The toolbar's side padding.
const TOOLBAR_PAD: f32 = 6.0;
/// The percent label's slot between the zoom-out and zoom-in buttons.
const PERCENT_W: f32 = 48.0;
/// The toolbar's corner radius.
const TOOLBAR_RADIUS: f32 = 8.0;
/// How far a press has to travel before it is a drag rather than a click
/// that happens to have moved a pixel.
const DRAG_SLOP: f32 = 3.0;

/// One of the toolbar's buttons.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Action {
    ZoomOut,
    ZoomIn,
    /// The whole drawing in the stage, as large as fits.
    Fit,
    /// A picture of the drawing to the clipboard, through the shell.
    Copy,
    /// *Save as…* through the shell's dialog.
    SaveAs,
    Close,
}

/// What the document app does after an input the viewer handled.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Handled; nothing to do beyond the redraw the handling asks for.
    Handled,
    /// Hand the drawing to the shell for the clipboard.
    Copy,
    /// Raise the block's *Save as…*.
    SaveAs,
    /// Take the viewer down.
    Close,
}

/// A press on the drawing that may become a drag.
#[derive(Copy, Clone, Debug)]
struct Drag {
    /// Where the press landed.
    from: (f32, f32),
    /// The pan when it landed; the drag adds the pointer's travel to it.
    pan: (f32, f32),
    /// Whether it has travelled past [`DRAG_SLOP`].
    moved: bool,
}

/// Which way a wheel notch moves the drawing, from the modifier held with
/// it. `Shift`+wheel scrolling sideways is every browser's and editor's
/// convention for a mouse with one wheel; `Alt`+wheel is its mirror, so a
/// trackpad's sideways swipe can scroll down. `Ctrl`+wheel is not here: it
/// zooms, through `zoom_wheel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WheelAxis {
    /// As the wheel turned: down scrolls down, a sideways wheel scrolls across.
    AsTurned,
    /// `Shift`: the wheel scrolls across whichever way it turned.
    Sideways,
    /// `Alt`: the wheel scrolls up and down whichever way it turned.
    UpAndDown,
}

impl WheelAxis {
    /// The axis the modifiers held ask for; `Shift` wins over `Alt`.
    pub fn from_mods(mods: winit::keyboard::ModifiersState) -> Self {
        if mods.shift_key() {
            Self::Sideways
        } else if mods.alt_key() {
            Self::UpAndDown
        } else {
            Self::AsTurned
        }
    }
}

/// The viewer's state: which drawing, at what zoom and pan, and what the
/// pointer is doing to it.
pub struct DiagramViewer {
    node: NodeId,
    prepared: Arc<PreparedDiagram>,
    viewport: (f32, f32),
    /// Natural size is 1.
    zoom: f64,
    /// The drawing's offset from centred in the stage, in window pixels.
    /// `(0, 0)` is centred; clamped by [`Self::settle`] so the drawing never
    /// leaves a gap on a side it overflows.
    pan: (f32, f32),
    drag: Option<Drag>,
    /// A press on the dimmed page, waiting for its release to close.
    press_on_scrim: bool,
    hover: Option<Action>,
    pointer: (f32, f32),
    /// The copy button's confirmation: check, hold, back to copy.
    copy: CopyAffordance,
}

impl DiagramViewer {
    /// Open on `prepared`, the drawing `node` shows, in a window `viewport`
    /// large. Opens at natural size when that fits the stage, and fitted
    /// when it does not — a small diagram is not blown up on arrival, and a
    /// large one is seen whole before it is zoomed into.
    pub fn open(node: NodeId, prepared: Arc<PreparedDiagram>, viewport: (f32, f32)) -> Self {
        let mut viewer = Self {
            node,
            prepared,
            viewport,
            zoom: 1.0,
            pan: (0.0, 0.0),
            drag: None,
            press_on_scrim: false,
            hover: None,
            pointer: (viewport.0 * 0.5, viewport.1 * 0.5),
            copy: CopyAffordance::default(),
        };
        viewer.zoom = viewer.fit_zoom().clamp(ZOOM_MIN, 1.0);
        viewer.settle();
        viewer
    }

    /// The block the drawing belongs to.
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// The drawing, as the page paints it.
    pub fn prepared(&self) -> &Arc<PreparedDiagram> {
        &self.prepared
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    pub fn pan(&self) -> (f32, f32) {
        self.pan
    }

    /// The window the viewer lays itself out in.
    pub fn viewport(&self) -> (f32, f32) {
        self.viewport
    }

    /// The window changed size: keep the zoom, re-clamp the pan.
    pub fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        self.settle();
    }

    // ---------------------------------------------------------- geometry

    /// The part of the window the drawing lives in: everything above the
    /// toolbar's band. Never shorter than a pixel, so a window too short
    /// for the toolbar still has somewhere to centre on.
    pub fn stage(&self) -> Rect {
        Rect::new(
            0.0,
            0.0,
            self.viewport.0.max(1.0),
            (self.viewport.1 - STAGE_BOTTOM).max(1.0),
        )
    }

    /// The drawing's size at the current zoom.
    fn scaled(&self) -> (f32, f32) {
        let size = self.prepared.natural_size();
        (
            (size.width * self.zoom) as f32,
            (size.height * self.zoom) as f32,
        )
    }

    /// The zoom at which the whole drawing fits inside the stage with
    /// [`FIT_MARGIN`] on every side, within the zoom bounds. A drawing with
    /// no size fits at 1.
    pub fn fit_zoom(&self) -> f64 {
        let size = self.prepared.natural_size();
        if size.width <= 0.0 || size.height <= 0.0 {
            return 1.0;
        }
        let stage = self.stage();
        let room_w = f64::from((stage.w - 2.0 * FIT_MARGIN).max(1.0));
        let room_h = f64::from((stage.h - 2.0 * FIT_MARGIN).max(1.0));
        (room_w / size.width)
            .min(room_h / size.height)
            .clamp(ZOOM_MIN, ZOOM_MAX)
    }

    /// Where the drawing's top-left is, in window coordinates: centred in
    /// the stage, moved by the pan.
    pub fn origin(&self) -> (f32, f32) {
        let (w, h) = self.scaled();
        let stage = self.stage();
        (
            stage.x + (stage.w - w) * 0.5 + self.pan.0,
            stage.y + (stage.h - h) * 0.5 + self.pan.1,
        )
    }

    /// The drawing's rect in window coordinates.
    pub fn drawing_rect(&self) -> Rect {
        let (x, y) = self.origin();
        let (w, h) = self.scaled();
        Rect::new(x, y, w, h)
    }

    /// The card under the drawing: the drawing's rect, padded.
    pub fn card_rect(&self) -> Rect {
        let drawing = self.drawing_rect();
        Rect::new(
            drawing.x - CARD_PAD,
            drawing.y - CARD_PAD,
            drawing.w + 2.0 * CARD_PAD,
            drawing.h + 2.0 * CARD_PAD,
        )
    }

    /// How far the drawing may be moved from centre on each axis: nothing
    /// where it fits the stage, half the overflow where it does not — so
    /// its edge can reach the stage's edge and no further.
    fn pan_limits(&self) -> (f32, f32) {
        let (w, h) = self.scaled();
        let stage = self.stage();
        (
            ((w - stage.w) * 0.5).max(0.0),
            ((h - stage.h) * 0.5).max(0.0),
        )
    }

    /// Whether the drawing overflows the stage on either axis — the only
    /// case a drag, a wheel or an arrow moves anything.
    pub fn draggable(&self) -> bool {
        let limits = self.pan_limits();
        limits.0 > 0.0 || limits.1 > 0.0
    }

    /// Clamp the pan to what the drawing's overflow allows.
    fn settle(&mut self) {
        let limits = self.pan_limits();
        self.pan = (
            self.pan.0.clamp(-limits.0, limits.0),
            self.pan.1.clamp(-limits.1, limits.1),
        );
    }

    // -------------------------------------------------------------- zoom

    /// Zoom to `zoom`, keeping the drawing's point under `(x, y)` where it
    /// is — the stage's centre for a keyboard or toolbar zoom, the pointer
    /// for a wheel.
    pub fn zoom_to(&mut self, zoom: f64, around: (f32, f32)) {
        let zoom = if zoom.is_finite() {
            zoom.clamp(ZOOM_MIN, ZOOM_MAX)
        } else {
            1.0
        };
        if zoom == self.zoom {
            return;
        }
        // The drawing point under the anchor, in natural units.
        let origin = self.origin();
        let dx = f64::from(around.0 - origin.0) / self.zoom;
        let dy = f64::from(around.1 - origin.1) / self.zoom;
        self.zoom = zoom;
        // Where the top-left has to be for that point to stay under the
        // anchor, and the pan that puts it there.
        let (w, h) = self.scaled();
        let stage = self.stage();
        let want_x = f64::from(around.0) - dx * zoom;
        let want_y = f64::from(around.1) - dy * zoom;
        self.pan = (
            want_x as f32 - (stage.x + (stage.w - w) * 0.5),
            want_y as f32 - (stage.y + (stage.h - h) * 0.5),
        );
        self.settle();
    }

    /// The stage's centre: what the toolbar and the keys zoom around.
    fn centre(&self) -> (f32, f32) {
        let stage = self.stage();
        (stage.x + stage.w * 0.5, stage.y + stage.h * 0.5)
    }

    /// One step in, around the stage's centre.
    pub fn zoom_in(&mut self) {
        self.zoom_to(self.zoom * ZOOM_STEP, self.centre());
    }

    /// One step out, around the stage's centre.
    pub fn zoom_out(&mut self) {
        self.zoom_to(self.zoom / ZOOM_STEP, self.centre());
    }

    /// Natural size, centred.
    pub fn actual_size(&mut self) {
        self.zoom_to(1.0, self.centre());
        self.pan = (0.0, 0.0);
    }

    /// The whole drawing in the stage, centred.
    pub fn fit(&mut self) {
        self.zoom_to(self.fit_zoom(), self.centre());
        self.pan = (0.0, 0.0);
    }

    /// The display-zoom chords, claimed for the drawing while it is up.
    pub fn zoom_key(&mut self, intent: ZoomIntent) {
        match intent {
            ZoomIntent::In => self.zoom_in(),
            ZoomIntent::Out => self.zoom_out(),
            ZoomIntent::ActualSize => self.actual_size(),
        }
    }

    /// `Ctrl`+wheel or a pinch at `(x, y)`: in on a push away, out on a
    /// pull back, around the pointer. A line delta is one step a notch; a
    /// pixel delta (a trackpad) is scaled so a hundred pixels is one step,
    /// which is about one flick.
    pub fn zoom_wheel(&mut self, delta: MouseScrollDelta, x: f32, y: f32) {
        let steps = match delta {
            MouseScrollDelta::LineDelta(_, lines) => f64::from(lines),
            MouseScrollDelta::PixelDelta(p) => p.y / 100.0,
        };
        if !steps.is_finite() || steps == 0.0 {
            return;
        }
        self.zoom_to(self.zoom * ZOOM_STEP.powf(steps), (x, y));
    }

    /// The zoom as the toolbar prints it: a whole percentage.
    pub fn percent_label(&self) -> String {
        format!("{}%", (self.zoom * 100.0).round() as i64)
    }

    // --------------------------------------------------------------- pan

    /// Move the drawing by `(dx, dy)` window pixels, within its overflow.
    /// `true` when it moved.
    pub fn pan_by(&mut self, dx: f32, dy: f32) -> bool {
        if !dx.is_finite() || !dy.is_finite() {
            return false;
        }
        let before = self.pan;
        self.pan = (self.pan.0 + dx, self.pan.1 + dy);
        self.settle();
        self.pan != before
    }

    /// The wheel: scroll the drawing the way a page scrolls, a notch being
    /// [`PAN_LINE`] pixels and a pixel delta itself. Wheeling down reveals
    /// the lower part of the drawing, as it reveals the lower part of a
    /// page; `axis` is the modifier's say over which way. `true` when the
    /// drawing moved; a drawing that fits does not.
    pub fn pan_wheel(&mut self, delta: MouseScrollDelta, axis: WheelAxis) -> bool {
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (x * PAN_LINE, y * PAN_LINE),
            MouseScrollDelta::PixelDelta(p) => (p.x as f32, p.y as f32),
        };
        let (dx, dy) = match axis {
            WheelAxis::AsTurned => (dx, dy),
            WheelAxis::Sideways => (dx + dy, 0.0),
            WheelAxis::UpAndDown => (0.0, dx + dy),
        };
        self.pan_by(dx, dy)
    }

    // ------------------------------------------------------------ toolbar

    /// The toolbar's panel: bottom centre of the window.
    pub fn toolbar_rect(&self) -> Rect {
        let w = TOOLBAR_PAD * 2.0 + BUTTON_W * 6.0 + PERCENT_W + GROUP_GAP * 3.0;
        Rect::new(
            ((self.viewport.0 - w) * 0.5).round(),
            (self.viewport.1 - TOOLBAR_BOTTOM - TOOLBAR_H).round(),
            w,
            TOOLBAR_H,
        )
    }

    /// The toolbar's buttons, in order, with the percent label's slot
    /// between zoom-out and zoom-in, and a gap between the three groups:
    /// zoom, fit, copy and save, close. The copy button wears the copy
    /// affordance's glyph — the check while a copy is being confirmed.
    pub fn buttons(&self) -> ButtonGroup<Action> {
        let bar = self.toolbar_rect();
        let mut x = bar.x + TOOLBAR_PAD;
        let slot = |x: &mut f32, action: Action, glyph: char, active: bool| {
            let rect = Rect::new(*x, bar.y, BUTTON_W, bar.h);
            *x += BUTTON_W;
            IconButton::new(glyph, rect)
                .on_click(action)
                .with_state(ButtonState {
                    hovered: self.hover == Some(action),
                    active,
                    disabled: false,
                })
        };
        let zoom_out = slot(&mut x, Action::ZoomOut, icons::MINUS, false);
        x += PERCENT_W;
        let zoom_in = slot(&mut x, Action::ZoomIn, icons::PLUS, false);
        x += GROUP_GAP;
        let fit = slot(&mut x, Action::Fit, icons::ARROWS_IN_SIMPLE, false);
        x += GROUP_GAP;
        let copy = slot(
            &mut x,
            Action::Copy,
            self.copy.glyph(),
            self.copy.confirming(),
        );
        let save = slot(&mut x, Action::SaveAs, icons::DOWNLOAD, false);
        x += GROUP_GAP;
        let close = slot(&mut x, Action::Close, icons::WINDOW_CLOSE, false);
        ButtonGroup::new(vec![zoom_out, zoom_in, fit, copy, save, close])
    }

    /// Where the percent label sits: between the two zoom buttons.
    fn percent_rect(&self) -> Rect {
        let bar = self.toolbar_rect();
        Rect::new(bar.x + TOOLBAR_PAD + BUTTON_W, bar.y, PERCENT_W, bar.h)
    }

    /// The button under `(x, y)`, if any.
    pub fn action_at(&self, x: f32, y: f32) -> Option<Action> {
        self.buttons().action_at(x, y)
    }

    /// Perform a toolbar action.
    pub fn act(&mut self, action: Action) -> Outcome {
        match action {
            Action::ZoomOut => self.zoom_out(),
            Action::ZoomIn => self.zoom_in(),
            Action::Fit => self.fit(),
            Action::Copy => return self.ask_copy(),
            Action::SaveAs => return Outcome::SaveAs,
            Action::Close => return Outcome::Close,
        }
        Outcome::Handled
    }

    /// Ask for the copy and confirm it at once: the button's glyph turns to
    /// the check and holds while the shell writes the clipboard.
    fn ask_copy(&mut self) -> Outcome {
        self.copy.confirm(Instant::now());
        Outcome::Copy
    }

    /// The glyph the copy button shows right now: the copy icon, or the
    /// check while a copy is being confirmed.
    pub fn copy_glyph(&self) -> char {
        self.copy.glyph()
    }

    // ---------------------------------------------------------- the clock

    /// Whether a frame is owed for the copy confirmation's morph.
    pub fn animating(&self, now: Instant) -> bool {
        self.copy.animating(now)
    }

    /// When the copy confirmation next needs a tick: the end of its hold.
    pub fn maintenance_deadline(&self) -> Option<Instant> {
        self.copy.maintenance_deadline()
    }

    /// Advance the copy confirmation. `true` when something drawn changed.
    pub fn maintain(&mut self, now: Instant) -> bool {
        self.copy.maintain(now)
    }

    // -------------------------------------------------------------- input

    /// A key while the viewer is up. Every key is the viewer's — it is
    /// modal — and the ones it understands act: `Esc` closes; `+`, `=` and
    /// `-` zoom, with or without the primary modifier; `0` returns to natural
    /// size; the arrows scroll a drawing that overflows; `Ctrl+C` copies;
    /// the registered *Save diagram as…* chord saves.
    pub fn key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> Outcome {
        if let Key::Named(named) = key {
            match named {
                NamedKey::Escape => return Outcome::Close,
                NamedKey::ArrowLeft => {
                    self.pan_by(PAN_KEY, 0.0);
                }
                NamedKey::ArrowRight => {
                    self.pan_by(-PAN_KEY, 0.0);
                }
                NamedKey::ArrowUp => {
                    self.pan_by(0.0, PAN_KEY);
                }
                NamedKey::ArrowDown => {
                    self.pan_by(0.0, -PAN_KEY);
                }
                _ => {}
            }
        }
        if koru_keys::registry::get(koru_keys::ShortcutId::SaveDiagramAs)
            .chord
            .matches(key, text, mods)
        {
            return Outcome::SaveAs;
        }
        let primary = koru_keys::has_primary(mods) && !koru_keys::has_foreign_primary(mods);
        if primary
            && !mods.shift_key()
            && !mods.alt_key()
            && koru_keys::letter(key, text) == Some('c')
        {
            return self.ask_copy();
        }
        // Shift is allowed through: on most layouts `+` *is* Shift+`=`, and
        // the chord form `Ctrl+=` arrives as `=`. Alt is not: AltGr makes
        // other characters of these keys.
        if !mods.alt_key() {
            match koru_keys::letter(key, text) {
                Some('+') | Some('=') => self.zoom_in(),
                Some('-') => self.zoom_out(),
                Some('0') => self.actual_size(),
                _ => {}
            }
        }
        Outcome::Handled
    }

    /// A press: a toolbar button acts; the drawing arms a drag; the dimmed
    /// page arms a close on release — a press that turns into a drag is
    /// not a dismissal.
    pub fn mouse_down(&mut self, x: f32, y: f32) -> Outcome {
        self.pointer = (x, y);
        if let Some(action) = self.action_at(x, y) {
            return self.act(action);
        }
        if self.toolbar_rect().contains(x, y) {
            return Outcome::Handled;
        }
        if self.card_rect().contains(x, y) {
            self.drag = Some(Drag {
                from: (x, y),
                pan: self.pan,
                moved: false,
            });
            return Outcome::Handled;
        }
        self.press_on_scrim = true;
        Outcome::Handled
    }

    /// The pointer moved: the toolbar's hover, or the drag's pan. `true`
    /// when something drawn changed.
    pub fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if let Some(drag) = self.drag.as_mut() {
            let dx = x - drag.from.0;
            let dy = y - drag.from.1;
            if !drag.moved && dx * dx + dy * dy < DRAG_SLOP * DRAG_SLOP {
                return false;
            }
            drag.moved = true;
            let pan = drag.pan;
            self.pan = (pan.0 + dx, pan.1 + dy);
            let before = self.pan;
            self.settle();
            // Drawn only when the clamp let it move; a drawing that fits
            // stays put under a drag, and no frame is spent saying so.
            return before == self.pan || self.draggable();
        }
        let hover = self.action_at(x, y);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    /// A release: a press on the dimmed page that stayed a press closes;
    /// a drag ends where it is.
    pub fn mouse_up(&mut self) -> Outcome {
        self.drag = None;
        if std::mem::take(&mut self.press_on_scrim) {
            return Outcome::Close;
        }
        Outcome::Handled
    }

    /// A double click on the drawing: natural size if it is fitted, fitted
    /// if it is not — the two sizes a person switches between most.
    pub fn double_click(&mut self, x: f32, y: f32) -> Outcome {
        if self.action_at(x, y).is_some() || self.toolbar_rect().contains(x, y) {
            return Outcome::Handled;
        }
        if (self.zoom - 1.0).abs() < 1e-6 {
            self.fit();
        } else {
            self.actual_size();
        }
        Outcome::Handled
    }

    /// The pointer's shape: a press target over the toolbar, the closed
    /// hand mid-drag, the open hand over a drawing that can be dragged, and
    /// the plain arrow everywhere else — the dimmed page included, which is
    /// a thing you click to close but, by the design rule, does not say so
    /// with the pointer.
    pub fn cursor(&self) -> CursorIcon {
        if self.drag.as_ref().is_some_and(|drag| drag.moved) {
            return CursorIcon::Grabbing;
        }
        if self.hover.is_some() {
            return koru_ui::cursor::PRESS;
        }
        let (x, y) = self.pointer;
        if self.draggable() && self.card_rect().contains(x, y) {
            return CursorIcon::Grab;
        }
        koru_ui::cursor::IDLE
    }

    // -------------------------------------------------------------- paint

    /// Paint the viewer over the whole window: the scrim, the card and the
    /// drawing on it, then the toolbar.
    pub fn draw(&self, canvas: &mut Scene, root: Affine, engine: &mut TextEngine) {
        let viewport = self.viewport;
        let p = theme::p();
        koru_ui::modal::draw(canvas, root, viewport, |canvas| {
            let card = self.card_rect();
            let drawing = self.drawing_rect();
            let _viewer = inspect::scope(
                "Diagram viewer",
                "unregistered",
                root,
                scene::rect(0.0, 0.0, viewport.0, viewport.1),
                |r| {
                    r.property(
                        "implementation",
                        "koru_document::diagram_viewer",
                        "diagram viewer; not an approved Koru component",
                    );
                    r.property("zoom", self.percent_label(), "DiagramViewer::zoom");
                    r.property(
                        "pan",
                        format!("{:.0}, {:.0}", self.pan.0, self.pan.1),
                        "DiagramViewer::pan; window pixels from centred",
                    );
                    r.property(
                        "draggable",
                        self.draggable(),
                        "the drawing overflows the stage",
                    );
                },
            );
            // The card: the page colour under the drawing, so the painter's
            // background role is honoured, raised off the dimmed page.
            {
                let _card = inspect::scope(
                    "Card",
                    "part",
                    root,
                    scene::rect(card.x, card.y, card.w, card.h),
                    |r| {
                        r.property("fill", inspect::color(p.bg), "theme::p().bg");
                    },
                );
                scene::fill(
                    canvas,
                    root,
                    p.panel_shadow,
                    &scene::rrect(card.x + 2.0, card.y + 4.0, card.w, card.h, 6.0),
                );
                scene::fill(
                    canvas,
                    root,
                    p.bg,
                    &scene::rrect(card.x, card.y, card.w, card.h, 6.0),
                );
            }
            let transform = root
                * Affine::translate((f64::from(drawing.x), f64::from(drawing.y)))
                * Affine::scale(self.zoom);
            koru_draw::diagram::paint(&self.prepared, p, transform, self.zoom, canvas);

            // The toolbar: a popover panel with the buttons and the percent
            // between the two that change it.
            let bar = self.toolbar_rect();
            scene::floating_panel(
                canvas,
                root,
                bar.x,
                bar.y,
                bar.w,
                bar.h,
                &scene::Panel::popover(TOOLBAR_RADIUS),
            );
            self.buttons().draw(canvas, root, engine);
            let label = engine.simple_layout(&self.percent_label(), theme::role::hint());
            let slot = self.percent_rect();
            let lx = slot.x + (slot.w - label.width()) * 0.5;
            let ly = slot.y + (slot.h - label.height()) * 0.5;
            scene::draw_layout_first_line(canvas, root, &label, lx, ly, Some(p.text_dim));
        });
    }
}
