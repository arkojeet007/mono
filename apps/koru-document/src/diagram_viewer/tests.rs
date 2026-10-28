// SPDX-License-Identifier: MPL-2.0

//! The viewer's geometry and its input, on a drawing of a known size. The
//! drawing is empty — the viewer never looks inside one, only at its size —
//! and the window is 900 × 600, so the stage above the toolbar's band is
//! 900 × 516 and the numbers below can be checked by hand.

use std::sync::Arc;

use koru_diagram::{Counts, DiagramScene, FlowchartScene, Kind as DiagramKind, SceneBody};
use koru_draw::diagram::PreparedDiagram;
use vello::kurbo::Rect;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;

const VIEWPORT: (f32, f32) = (900.0, 600.0);
/// The stage's height: the window above the toolbar's band.
const STAGE_H: f32 = 600.0 - STAGE_BOTTOM;

fn prepared(width: f64, height: f64) -> Arc<PreparedDiagram> {
    Arc::new(PreparedDiagram::new(Arc::new(DiagramScene {
        kind: DiagramKind::Flowchart,
        bounds: Rect::new(0.0, 0.0, width, height),
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
    })))
}

fn viewer(width: f64, height: f64) -> DiagramViewer {
    DiagramViewer::open(NodeId::new(), prepared(width, height), VIEWPORT)
}

fn named(key: NamedKey) -> Key {
    Key::Named(key)
}

fn ch(s: &str) -> Key {
    Key::Character(s.into())
}

fn close_to(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

// ------------------------------------------------------------- opening

/// The stage is the window less the toolbar's band: the bar's height with
/// the same air above it as below, so the bar never sits on the drawing.
#[test]
fn the_stage_stops_above_the_toolbars_band() {
    let v = viewer(400.0, 300.0);
    let stage = v.stage();
    assert_eq!((stage.x, stage.y, stage.w), (0.0, 0.0, 900.0));
    assert_eq!(stage.h, STAGE_H);
    let bar = v.toolbar_rect();
    assert!(
        stage.y + stage.h + TOOLBAR_BOTTOM <= bar.y + 0.01,
        "the bar's air above it is outside the stage: {} vs {}",
        stage.h,
        bar.y
    );
}

/// A drawing that fits opens at natural size, centred in the stage; one
/// that does not opens fitted, so it is seen whole before it is zoomed into.
#[test]
fn opens_at_natural_size_when_it_fits_and_fitted_when_it_does_not() {
    let small = viewer(400.0, 300.0);
    assert_eq!(small.zoom(), 1.0);
    assert_eq!(small.pan(), (0.0, 0.0));
    let rect = small.drawing_rect();
    assert!(
        close_to(rect.x, 250.0) && close_to(rect.y, (STAGE_H - 300.0) * 0.5),
        "{rect:?}"
    );
    assert!(!small.draggable());

    let large = viewer(2000.0, 1000.0);
    // 804 / 2000 is the tighter of the two fits (420 / 1000 the other).
    assert!((large.zoom() - 0.402).abs() < 1e-6, "{}", large.zoom());
    assert_eq!(large.pan(), (0.0, 0.0));
    let rect = large.drawing_rect();
    assert!(
        rect.x >= FIT_MARGIN - 0.01 && rect.y >= FIT_MARGIN - 0.01,
        "{rect:?}"
    );
    assert!(
        rect.y + rect.h <= STAGE_H - FIT_MARGIN + 0.01,
        "fitted inside the stage, clear of the bar: {rect:?}"
    );
    assert!(!large.draggable(), "fitted, so nothing to drag");
}

/// A drawing with no size — a scene with nothing in it — opens at 1 and
/// breaks no arithmetic.
#[test]
fn an_empty_drawing_opens_at_natural_size() {
    let empty = viewer(0.0, 0.0);
    assert_eq!(empty.zoom(), 1.0);
    assert_eq!(empty.fit_zoom(), 1.0);
    assert_eq!(empty.percent_label(), "100%");
}

// ---------------------------------------------------------------- zoom

/// A step in multiplies by the step, a step out divides, both clamped,
/// and the label prints a whole percentage.
#[test]
fn zoom_steps_are_multiplicative_and_bounded() {
    let mut v = viewer(400.0, 300.0);
    v.zoom_in();
    assert_eq!(v.zoom(), ZOOM_STEP);
    assert_eq!(v.percent_label(), "125%");
    v.zoom_out();
    assert_eq!(v.zoom(), 1.0);
    for _ in 0..100 {
        v.zoom_in();
    }
    assert_eq!(v.zoom(), ZOOM_MAX);
    for _ in 0..100 {
        v.zoom_out();
    }
    assert_eq!(v.zoom(), ZOOM_MIN);
    assert_eq!(v.percent_label(), "10%");
}

/// Zooming around a point keeps that point of the drawing under the
/// anchor — once the drawing overflows the stage. While it still fits it
/// is centred whatever the anchor, which is what keeps a small drawing from
/// wandering off centre as it is zoomed; the clamp is the one rule.
#[test]
fn zooming_keeps_the_point_under_the_anchor_still() {
    // Fits: centred, anchor or no anchor.
    let mut v = viewer(400.0, 300.0);
    v.zoom_to(ZOOM_STEP, (250.0, 150.0));
    let rect = v.drawing_rect();
    assert!(
        close_to(rect.x, 200.0) && close_to(rect.y, (STAGE_H - 375.0) * 0.5),
        "{rect:?}"
    );
    assert_eq!(v.pan(), (0.0, 0.0));

    // Overflows: the drawing point under (100, 100) at natural size is
    // (650, 342) of the drawing; at 2× it is still under (100, 100).
    let mut v = viewer(2000.0, 1000.0);
    v.actual_size();
    let rect = v.drawing_rect();
    assert!(
        close_to(rect.x, -550.0) && close_to(rect.y, -242.0),
        "{rect:?}"
    );
    v.zoom_to(2.0, (100.0, 100.0));
    let rect = v.drawing_rect();
    assert!(
        close_to(rect.x, -1200.0) && close_to(rect.y, -584.0),
        "{rect:?}"
    );
    assert_eq!(rect.w, 4000.0);

    // And around the stage's centre, the drawing's centre stays put.
    let mut v = viewer(2000.0, 1000.0);
    v.zoom_to(1.0, (450.0, STAGE_H * 0.5));
    let rect = v.drawing_rect();
    assert!(close_to(rect.x + rect.w * 0.5, 450.0), "{rect:?}");
    assert!(close_to(rect.y + rect.h * 0.5, STAGE_H * 0.5), "{rect:?}");
}

/// Actual size and fit both land centred, whatever the pan was.
#[test]
fn actual_size_and_fit_recentre() {
    let mut v = viewer(2000.0, 1000.0);
    v.actual_size();
    assert_eq!(v.zoom(), 1.0);
    assert_eq!(v.pan(), (0.0, 0.0));
    assert!(v.draggable(), "2000 wide in a 900 stage");
    v.mouse_down(450.0, 300.0);
    v.mouse_move(350.0, 250.0);
    v.mouse_up();
    assert_ne!(v.pan(), (0.0, 0.0), "dragged off centre");
    v.fit();
    assert_eq!(v.pan(), (0.0, 0.0));
    assert!((v.zoom() - 0.402).abs() < 1e-6);
}

/// `Ctrl`+wheel zooms around the pointer; a notch away is a step in, a
/// notch back a step out, and a pixel delta of a hundred is one step too.
#[test]
fn the_zoom_wheel_zooms_around_the_pointer() {
    let mut v = viewer(2000.0, 1000.0);
    v.actual_size();
    v.zoom_wheel(MouseScrollDelta::LineDelta(0.0, 1.0), 100.0, 100.0);
    assert_eq!(v.zoom(), ZOOM_STEP);
    let rect = v.drawing_rect();
    assert!(
        close_to(rect.x, -712.5) && close_to(rect.y, -327.5),
        "{rect:?}"
    );
    v.zoom_wheel(MouseScrollDelta::LineDelta(0.0, -1.0), 100.0, 100.0);
    assert!((v.zoom() - 1.0).abs() < 1e-9);
    let rect = v.drawing_rect();
    assert!(
        close_to(rect.x, -550.0) && close_to(rect.y, -242.0),
        "back where it was: {rect:?}"
    );
    v.zoom_wheel(
        MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, 100.0)),
        450.0,
        300.0,
    );
    assert!((v.zoom() - ZOOM_STEP).abs() < 1e-9);
    // Nothing from nothing.
    v.zoom_wheel(MouseScrollDelta::LineDelta(0.0, 0.0), 450.0, 300.0);
    assert!((v.zoom() - ZOOM_STEP).abs() < 1e-9);
}

/// The display-zoom chords, offered to the viewer, move the drawing.
#[test]
fn the_zoom_chords_are_the_viewers() {
    let mut v = viewer(400.0, 300.0);
    v.zoom_key(ZoomIntent::In);
    assert_eq!(v.zoom(), ZOOM_STEP);
    v.zoom_key(ZoomIntent::Out);
    assert_eq!(v.zoom(), 1.0);
    v.zoom_key(ZoomIntent::In);
    v.zoom_key(ZoomIntent::ActualSize);
    assert_eq!(v.zoom(), 1.0);
}

// ----------------------------------------------------------------- pan

/// A drawing that overflows can be dragged until its edge meets the
/// stage's and no further — its bottom edge stops above the toolbar's
/// band, never under the bar; one that fits does not move at all.
#[test]
fn a_drag_is_clamped_to_the_overflow() {
    let mut v = viewer(2000.0, 1000.0);
    v.actual_size();
    assert!(v.draggable());
    // Overflow: 1100 wide, 484 tall; half of each is the reach.
    assert_eq!(v.mouse_down(450.0, 300.0), Outcome::Handled);
    assert!(v.mouse_move(450.0 + 5000.0, 300.0 + 5000.0));
    assert_eq!(v.pan(), (550.0, 242.0));
    let rect = v.drawing_rect();
    assert!(close_to(rect.x, 0.0) && close_to(rect.y, 0.0), "{rect:?}");
    assert!(v.mouse_move(450.0 - 5000.0, 300.0 - 5000.0));
    assert_eq!(v.pan(), (-550.0, -242.0));
    let rect = v.drawing_rect();
    assert!(close_to(rect.x + rect.w, 900.0), "{rect:?}");
    assert!(close_to(rect.y + rect.h, STAGE_H), "{rect:?}");
    assert!(
        rect.y + rect.h + TOOLBAR_BOTTOM <= v.toolbar_rect().y + 0.01,
        "the last row of the drawing is clear of the bar"
    );
    assert_eq!(v.mouse_up(), Outcome::Handled, "a drag is not a dismissal");

    let mut small = viewer(400.0, 300.0);
    small.mouse_down(450.0, 300.0);
    small.mouse_move(650.0, 500.0);
    assert_eq!(small.pan(), (0.0, 0.0), "a drawing that fits stays centred");
    assert_eq!(small.mouse_up(), Outcome::Handled);
}

/// The wheel scrolls a drawing that overflows, a notch at a time: wheeling
/// down reveals the lower part, as on a page; a pixel delta is itself;
/// `Shift` turns the wheel sideways and `Alt` turns a sideways one upright;
/// a drawing that fits does not move, and says so.
#[test]
fn the_plain_wheel_scrolls_the_drawing() {
    use super::WheelAxis::{AsTurned, Sideways, UpAndDown};
    let mut v = viewer(2000.0, 1000.0);
    v.actual_size();
    // Down a notch: the drawing moves up, so its lower part comes into view.
    assert!(v.pan_wheel(MouseScrollDelta::LineDelta(0.0, -1.0), AsTurned));
    assert_eq!(v.pan(), (0.0, -PAN_LINE));
    assert!(v.pan_wheel(MouseScrollDelta::LineDelta(0.0, 1.0), AsTurned));
    assert_eq!(v.pan(), (0.0, 0.0));
    // A horizontal wheel moves it sideways; `Shift` makes a vertical one do.
    assert!(v.pan_wheel(MouseScrollDelta::LineDelta(-1.0, 0.0), AsTurned));
    assert_eq!(v.pan(), (-PAN_LINE, 0.0));
    assert!(v.pan_wheel(MouseScrollDelta::LineDelta(0.0, 1.0), Sideways));
    assert_eq!(v.pan(), (0.0, 0.0));
    // `Alt` makes a sideways wheel scroll up and down.
    assert!(v.pan_wheel(MouseScrollDelta::LineDelta(-1.0, 0.0), UpAndDown));
    assert_eq!(v.pan(), (0.0, -PAN_LINE));
    assert!(v.pan_wheel(MouseScrollDelta::LineDelta(0.0, 1.0), UpAndDown));
    assert_eq!(v.pan(), (0.0, 0.0));
    // The modifiers decide the axis: `Shift` over `Alt`, neither as turned.
    use winit::keyboard::ModifiersState;
    assert_eq!(
        super::WheelAxis::from_mods(ModifiersState::SHIFT | ModifiersState::ALT),
        Sideways
    );
    assert_eq!(super::WheelAxis::from_mods(ModifiersState::ALT), UpAndDown);
    assert_eq!(
        super::WheelAxis::from_mods(ModifiersState::empty()),
        AsTurned
    );
    // A trackpad's pixels are pixels.
    assert!(v.pan_wheel(
        MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(0.0, -30.0)),
        AsTurned
    ));
    assert_eq!(v.pan(), (0.0, -30.0));
    // Clamped like a drag: a long scroll stops at the edge.
    for _ in 0..100 {
        v.pan_wheel(MouseScrollDelta::LineDelta(0.0, -1.0), AsTurned);
    }
    assert_eq!(v.pan(), (0.0, -242.0));
    assert!(
        !v.pan_wheel(MouseScrollDelta::LineDelta(0.0, -1.0), AsTurned),
        "at the edge, nothing moves and no frame is asked for"
    );
    // The zoom is untouched throughout.
    assert_eq!(v.zoom(), 1.0);

    let mut small = viewer(400.0, 300.0);
    assert!(!small.pan_wheel(MouseScrollDelta::LineDelta(0.0, -1.0), AsTurned));
    assert_eq!(small.pan(), (0.0, 0.0));
}

/// The arrow keys step a drawing that overflows, in the direction of the
/// arrow — down shows more of the bottom — and do nothing to one that fits.
#[test]
fn the_arrow_keys_step_the_drawing() {
    let mut v = viewer(2000.0, 1000.0);
    v.actual_size();
    assert_eq!(
        v.key(&named(NamedKey::ArrowDown), None, NONE),
        Outcome::Handled
    );
    assert_eq!(v.pan(), (0.0, -PAN_KEY));
    v.key(&named(NamedKey::ArrowUp), None, NONE);
    assert_eq!(v.pan(), (0.0, 0.0));
    v.key(&named(NamedKey::ArrowRight), None, NONE);
    assert_eq!(v.pan(), (-PAN_KEY, 0.0));
    v.key(&named(NamedKey::ArrowLeft), None, NONE);
    assert_eq!(v.pan(), (0.0, 0.0));

    let mut small = viewer(400.0, 300.0);
    small.key(&named(NamedKey::ArrowDown), None, NONE);
    assert_eq!(small.pan(), (0.0, 0.0));
}

/// A press that does not travel is not a drag: the pan is untouched and
/// the pointer keeps the open hand rather than the closed one.
#[test]
fn a_press_within_the_slop_is_not_a_drag() {
    let mut v = viewer(2000.0, 1000.0);
    v.actual_size();
    v.mouse_down(450.0, 300.0);
    assert!(!v.mouse_move(451.0, 301.0));
    assert_eq!(v.pan(), (0.0, 0.0));
    assert_eq!(v.cursor(), CursorIcon::Grab);
    v.mouse_move(480.0, 300.0);
    assert_eq!(v.cursor(), CursorIcon::Grabbing);
}

/// The window growing or shrinking keeps the zoom and re-clamps the pan.
#[test]
fn a_resize_reclamps_the_pan() {
    let mut v = viewer(2000.0, 1000.0);
    v.actual_size();
    v.mouse_down(450.0, 300.0);
    v.mouse_move(5000.0, 5000.0);
    v.mouse_up();
    assert_eq!(v.pan(), (550.0, 242.0));
    v.set_viewport(2000.0, 1100.0);
    assert_eq!(v.zoom(), 1.0);
    assert_eq!(
        v.pan(),
        (0.0, 0.0),
        "it fits the stage now, so it is centred"
    );
}

// ------------------------------------------------------------- toolbar

/// The toolbar sits at the bottom centre, its six buttons in order with the
/// percent between the zoom pair, and each answers its own action.
#[test]
fn the_toolbar_buttons_are_where_they_are_drawn() {
    let v = viewer(400.0, 300.0);
    let bar = v.toolbar_rect();
    assert!(close_to(bar.x + bar.w * 0.5, 450.0), "{bar:?}");
    assert_eq!(bar.y + bar.h, 600.0 - TOOLBAR_BOTTOM);
    let group = v.buttons();
    let buttons = group.buttons();
    assert_eq!(buttons.len(), 6);
    let centre = |rect: koru_draw::geometry::Rect| (rect.x + rect.w * 0.5, rect.y + rect.h * 0.5);
    let expect = [
        Action::ZoomOut,
        Action::ZoomIn,
        Action::Fit,
        Action::Copy,
        Action::SaveAs,
        Action::Close,
    ];
    for (button, action) in buttons.iter().zip(expect) {
        let (x, y) = centre(button.rect());
        assert_eq!(v.action_at(x, y), Some(action));
        assert!(bar.contains(x, y));
    }
    // Left to right, no overlap, and room for the percent after zoom-out.
    for pair in buttons.windows(2) {
        assert!(pair[0].rect().x + pair[0].rect().w <= pair[1].rect().x);
    }
    assert!(buttons[1].rect().x - (buttons[0].rect().x + buttons[0].rect().w) >= PERCENT_W);
    // Between two buttons is the bar, not a button.
    let (x, y) = centre(v.percent_rect());
    assert_eq!(v.action_at(x, y), None);
    assert!(bar.contains(x, y));
}

/// Each button does what it says, and the three that need the shell say so
/// in the outcome rather than doing anything here.
#[test]
fn toolbar_actions_act_or_ask() {
    let mut v = viewer(400.0, 300.0);
    assert_eq!(v.act(Action::ZoomIn), Outcome::Handled);
    assert_eq!(v.zoom(), ZOOM_STEP);
    assert_eq!(v.act(Action::ZoomOut), Outcome::Handled);
    assert_eq!(v.zoom(), 1.0);
    assert_eq!(v.act(Action::Fit), Outcome::Handled);
    assert_eq!(v.act(Action::Copy), Outcome::Copy);
    assert_eq!(v.act(Action::SaveAs), Outcome::SaveAs);
    assert_eq!(v.act(Action::Close), Outcome::Close);
}

/// A copy confirms at once, the way every copy icon does: the button's
/// glyph turns to the check, the viewer asks for frames while it morphs
/// and a tick when the hold ends, and the glyph returns to copy after.
#[test]
fn a_copy_confirms_with_the_check_and_returns() {
    let mut v = viewer(400.0, 300.0);
    assert_eq!(v.copy_glyph(), icons::COPY);
    assert!(v.maintenance_deadline().is_none());
    assert_eq!(v.act(Action::Copy), Outcome::Copy);
    assert_eq!(v.copy_glyph(), icons::CHECK);
    let copy = v.buttons().buttons()[3].glyph();
    assert_eq!(copy, icons::CHECK, "the toolbar shows the check");
    assert!(v.animating(Instant::now()));
    let deadline = v.maintenance_deadline().expect("the hold ends sometime");
    // Past the hold and the morph back: copy again.
    let after = deadline + std::time::Duration::from_millis(1);
    v.maintain(after);
    v.maintain(after + std::time::Duration::from_millis(theme::GLYPH_MORPH_MS as u64));
    assert_eq!(v.copy_glyph(), icons::COPY);
    assert!(v.maintenance_deadline().is_none());
}

/// Hovering a button lights it and asks for a frame; moving off it asks
/// for one more and no further.
#[test]
fn hover_over_a_button_is_reported_once_each_way() {
    let mut v = viewer(400.0, 300.0);
    let close = v.buttons().buttons()[5].rect();
    let (x, y) = (close.x + close.w * 0.5, close.y + close.h * 0.5);
    assert!(v.mouse_move(x, y));
    assert_eq!(v.cursor(), koru_ui::cursor::PRESS);
    assert!(!v.mouse_move(x + 1.0, y));
    assert!(v.mouse_move(10.0, 10.0));
    assert_eq!(v.cursor(), koru_ui::cursor::IDLE);
    assert!(!v.mouse_move(11.0, 10.0));
}

// --------------------------------------------------------------- input

/// A press on the dimmed page closes on release; one on the toolbar's
/// background does nothing; one on a button acts at once.
#[test]
fn a_click_on_the_scrim_closes_and_one_on_the_bar_does_not() {
    let mut v = viewer(400.0, 300.0);
    assert_eq!(v.mouse_down(10.0, 10.0), Outcome::Handled);
    assert_eq!(v.mouse_up(), Outcome::Close);

    let mut v = viewer(400.0, 300.0);
    let (x, y) = {
        let r = v.percent_rect();
        (r.x + r.w * 0.5, r.y + r.h * 0.5)
    };
    assert_eq!(v.mouse_down(x, y), Outcome::Handled);
    assert_eq!(v.mouse_up(), Outcome::Handled);

    let close = v.buttons().buttons()[5].rect();
    assert_eq!(
        v.mouse_down(close.x + close.w * 0.5, close.y + close.h * 0.5),
        Outcome::Close
    );
}

/// `Esc` closes; `+`, `=` and `-` zoom with or without the primary
/// modifier; `0` returns to natural size; `Ctrl+C` asks for a copy and
/// confirms it; the registered save chord asks for a save; anything else
/// is swallowed.
#[test]
fn keys_zoom_close_copy_and_save() {
    let mut v = viewer(400.0, 300.0);
    assert_eq!(v.key(&named(NamedKey::Escape), None, NONE), Outcome::Close);
    assert_eq!(v.key(&ch("+"), Some("+"), NONE), Outcome::Handled);
    assert_eq!(v.zoom(), ZOOM_STEP);
    // `+` is Shift+`=` on most layouts, so Shift is let through.
    assert_eq!(
        v.key(&ch("+"), Some("+"), ModifiersState::SHIFT),
        Outcome::Handled
    );
    assert_eq!(v.zoom(), ZOOM_STEP * ZOOM_STEP);
    assert_eq!(v.key(&ch("-"), Some("-"), NONE), Outcome::Handled);
    assert_eq!(v.key(&ch("-"), Some("-"), NONE), Outcome::Handled);
    assert_eq!(v.zoom(), 1.0);
    assert_eq!(
        v.key(&ch("="), Some("="), koru_keys::PRIMARY),
        Outcome::Handled
    );
    assert_eq!(v.zoom(), ZOOM_STEP);
    assert_eq!(
        v.key(&ch("0"), Some("0"), koru_keys::PRIMARY),
        Outcome::Handled
    );
    assert_eq!(v.zoom(), 1.0);
    assert_eq!(
        v.key(&ch("c"), Some("c"), koru_keys::PRIMARY),
        Outcome::Copy
    );
    assert_eq!(v.copy_glyph(), icons::CHECK, "confirmed at once");
    assert_eq!(
        v.key(
            &ch("x"),
            Some("x"),
            koru_keys::PRIMARY | ModifiersState::SHIFT
        ),
        Outcome::SaveAs
    );
    // A letter that means nothing here is still the viewer's.
    assert_eq!(v.key(&ch("q"), Some("q"), NONE), Outcome::Handled);
    assert_eq!(v.key(&named(NamedKey::Enter), None, NONE), Outcome::Handled);
}

const NONE: ModifiersState = ModifiersState::empty();

/// A double click on the drawing goes to fit from natural size and back;
/// on the toolbar it does nothing.
#[test]
fn a_double_click_toggles_natural_size_and_fit() {
    let mut v = viewer(2000.0, 1000.0);
    assert!((v.zoom() - 0.402).abs() < 1e-6, "opened fitted");
    assert_eq!(v.double_click(450.0, 300.0), Outcome::Handled);
    assert_eq!(v.zoom(), 1.0, "to natural size");
    assert_eq!(v.double_click(450.0, 300.0), Outcome::Handled);
    assert!((v.zoom() - 0.402).abs() < 1e-6, "and back to fit");
    let bar = v.toolbar_rect();
    assert_eq!(v.double_click(bar.x + 1.0, bar.y + 1.0), Outcome::Handled);
    assert!(
        (v.zoom() - 0.402).abs() < 1e-6,
        "the bar is not the drawing"
    );
}
