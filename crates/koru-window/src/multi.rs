// SPDX-License-Identifier: MPL-2.0

//! The multi-window runner: a set of windows over the same dispatch core.
//!
//! [`MultiWindowApp`] is [`WindowApp`](crate::WindowApp) multiplied by window.
//! Every method that was about *the* window takes a [`WindowKey`]; the handful
//! that were about the *process* — `should_exit`, `accessory`, `attach`,
//! `wake`, `take_key_quarantine` — stay app-level, because that is what they
//! always were. Nothing here is shell policy: a tab, a banner, a surface and a
//! pop-out are `koru-app-shell`'s, and this runner has never heard of them.
//!
//! ## Three differences from the single-window runner, and why
//!
//! - **The app names its windows.** A winit `WindowId` does not exist until the
//!   window does, so an app could not use one to *ask* for a window. It mints a
//!   [`WindowKey`] instead and [`MultiWindowApp::windows`] is the single
//!   authority, exactly as `wants_window` is for one window: the runner opens
//!   and drops to match after every event. [`MultiWindowApp::seated`] hands
//!   back the `WindowId` once the window and its surface exist, which is the
//!   commit point a two-phase pop-out needs.
//! - **Settle is per window, reschedule is not.** `ControlFlow` is a property
//!   of the loop, so each window is drained and asked separately and the
//!   answers are folded: `animating` is the OR, the maintenance deadline is the
//!   `min`. A frame is requested for the window that changed and no other.
//! - **A window can close while the process lives.** The single-window runner
//!   only ever snapshots at process exit, so
//!   [`MultiWindowApp::window_closing`] is new: it is the moment a per-window
//!   geometry row has to be written, since the OS state is gone a line later.
//!
//! ## Geometry rows
//!
//! ADR-226 is *caller owns the key*: this runner supplies the live
//! [`ClosingWindowState`] per window at both boundaries and never touches
//! `windows.conf` itself. A host keeps one
//! [`WindowRowWriter`](crate::WindowRowWriter) per key — the row name and the
//! value schema are its own, which is what lets two hosts share one file.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use vello::Scene;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, PhysicalKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowAttributes, WindowId};

use koru_draw::text::TextEngine;

use crate::app::ClosingWindowState;
use crate::dispatch::{self, DispatchApp, FrameWork, WindowInput};
use crate::gpu::{Frame, Gpu, LiveWindow};
use crate::key_quarantine::KeyQuarantine;
use crate::request::WindowRequest;
use crate::{ImeEvent, RichKeyEvent, RichMouseEvent};

/// A window's name, minted by the application.
///
/// Opaque to the runner, which only ever compares and hashes it. An app that
/// pops a tab out into a window allocates one, puts it in
/// [`MultiWindowApp::windows`], and is handed the `WindowId` when the window
/// exists.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WindowKey(pub u64);

/// What the multi-window loop shows, and what it asks of it.
///
/// Read it beside [`WindowApp`](crate::WindowApp): every method with a
/// [`WindowKey`] is that trait's method, asked of one window.
pub trait MultiWindowApp {
    /// What another thread may send in, delivered to [`wake`](Self::wake).
    type Event: Send + 'static;

    /// The windows that should exist right now, in creation order.
    ///
    /// The **single authority**, the twin of
    /// [`WindowApp::wants_window`](crate::WindowApp::wants_window): the runner
    /// opens and drops windows to match after every event, and nothing else
    /// creates or destroys one. A slice rather than an iterator so a settle
    /// after every event allocates nothing.
    fn windows(&self) -> &[WindowKey];

    /// The window to open for `key`, asked each time one is created.
    fn window(&self, _key: WindowKey) -> WindowAttributes {
        Window::default_attributes().with_decorations(false)
    }

    fn window_activation(&self, _key: WindowKey) -> crate::WindowActivation {
        crate::WindowActivation::Focused
    }

    fn window_generation(&self, _key: WindowKey) -> u64 {
        0
    }

    /// Whether this process is an *accessory* on macOS. See
    /// [`WindowApp::accessory`](crate::WindowApp::accessory).
    fn accessory(&self) -> bool {
        false
    }

    /// Whether the process should end. Checked after every event.
    fn should_exit(&self) -> bool;

    /// The process is about to leave the event loop, with the final OS state of
    /// every window still open, in creation order.
    fn exiting(&mut self, _windows: &[(WindowKey, ClosingWindowState)]) {}

    /// The window for `key` exists and has a surface.
    ///
    /// The `WindowId` is handed over because an app that talks to the OS about
    /// one of its windows — or correlates a late event with the window it came
    /// from — has no other way to learn it.
    fn seated(&mut self, _key: WindowKey, _id: WindowId) {}

    /// This window is about to be dropped, with its last live OS state.
    ///
    /// **The one hook the single-window runner has no need for**: it snapshots
    /// only at process exit. Here a window can go while the process lives, and
    /// after this call the position and size are unrecoverable.
    fn window_closing(&mut self, _key: WindowKey, _state: ClosingWindowState) {}

    /// This window's ✕, or Alt+F4, or the taskbar's close. Required for the
    /// reason [`WindowApp::close_requested`](crate::WindowApp::close_requested)
    /// is: what closing means is the one thing a runner cannot guess.
    fn close_requested(&mut self, key: WindowKey);

    /// Something a background thread sent through the proxy.
    fn wake(&mut self, _event: Self::Event) -> bool {
        false
    }

    /// Hand the app the proxy its threads will send on, once, before the loop
    /// starts.
    fn attach(&mut self, _proxy: EventLoopProxy<Self::Event>) {}

    /// Physical keys that must not reach this app until they are released.
    ///
    /// **App-level rather than per window, deliberately.** The stray repeat and
    /// the missing release of a chord that summoned a window go to whichever
    /// window has the focus, which is frequently not the one that asked — a
    /// per-window quarantine would suppress them in the wrong place.
    fn take_key_quarantine(&mut self) -> Vec<PhysicalKey> {
        Vec::new()
    }

    /// Bring this window to the foreground. Drained after every event.
    fn take_activation_request(&mut self, _key: WindowKey) -> bool {
        false
    }

    fn take_window_request(&mut self, _key: WindowKey) -> Option<WindowRequest> {
        None
    }

    fn wants_drag(&self, _key: WindowKey) -> bool {
        false
    }

    fn is_caption(&self, _key: WindowKey, _x: f32, _y: f32) -> bool {
        false
    }

    fn cursor_icon(&self, _key: WindowKey, _x: f32, _y: f32) -> CursorIcon {
        CursorIcon::Default
    }

    fn resize_direction(
        &self,
        _key: WindowKey,
        _width: f32,
        _height: f32,
        _x: f32,
        _y: f32,
        _maximised: bool,
    ) -> Option<ResizeDirection> {
        None
    }

    fn resized(&mut self, _key: WindowKey, _size: PhysicalSize<u32>, _scale_factor: f64) {}

    /// A DPI change, with or without a change of drawable size. See
    /// [`WindowApp::scale_factor_changed`](crate::WindowApp::scale_factor_changed).
    fn scale_factor_changed(&mut self, _key: WindowKey, _scale_factor: f64) -> bool {
        false
    }

    fn paint_before_showing(&self, _key: WindowKey) -> bool {
        false
    }

    fn set_maximised(&mut self, _key: WindowKey, _maximised: bool) -> bool {
        false
    }

    fn set_pinned(&mut self, _key: WindowKey, _pinned: bool) -> bool {
        false
    }

    fn pinned(&self, _key: WindowKey) -> bool {
        false
    }

    fn moved(&mut self, _key: WindowKey, _position: PhysicalPosition<i32>) {}

    fn pointer_moved(&mut self, key: WindowKey, x: f32, y: f32) -> bool;

    /// The pointer left this window. See
    /// [`WindowApp::pointer_left`](crate::WindowApp::pointer_left).
    fn pointer_left(&mut self, _key: WindowKey) -> bool {
        false
    }

    fn click(
        &mut self,
        key: WindowKey,
        engine: &mut TextEngine,
        x: f32,
        y: f32,
        ctrl: bool,
    ) -> bool;

    fn double_click(
        &mut self,
        _key: WindowKey,
        _engine: &mut TextEngine,
        _x: f32,
        _y: f32,
    ) -> bool {
        false
    }

    fn context_menu(&mut self, key: WindowKey, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.right_click(key, engine, x, y)
    }

    fn right_click(&mut self, _key: WindowKey, _engine: &mut TextEngine, _x: f32, _y: f32) -> bool {
        false
    }

    fn drag(&mut self, _key: WindowKey, _engine: &mut TextEngine, _x: f32, _y: f32) -> bool {
        false
    }

    fn mouse_up(&mut self, _key: WindowKey) -> bool {
        false
    }

    fn scroll_by(&mut self, _key: WindowKey, _delta: f32, _mods: ModifiersState) -> bool {
        false
    }

    fn key(
        &mut self,
        key: WindowKey,
        engine: &mut TextEngine,
        logical: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool;

    #[allow(clippy::too_many_arguments)]
    fn physical_key_pressed(
        &mut self,
        _key: WindowKey,
        _engine: &mut TextEngine,
        _physical: PhysicalKey,
        _logical: &Key,
        _text: Option<&str>,
        _mods: ModifiersState,
        _repeat: bool,
    ) -> bool {
        false
    }

    fn physical_key_released(&mut self, _key: WindowKey, _physical: PhysicalKey) -> bool {
        false
    }

    fn rich_key(
        &mut self,
        key: WindowKey,
        engine: &mut TextEngine,
        event: RichKeyEvent<'_>,
    ) -> bool {
        if event.state.is_pressed() {
            self.key(key, engine, event.logical_key, event.text, event.modifiers)
        } else {
            false
        }
    }

    fn wants_ime(&self, _key: WindowKey) -> bool {
        false
    }

    fn ime(&mut self, _key: WindowKey, _engine: &mut TextEngine, _event: ImeEvent<'_>) -> bool {
        false
    }

    fn wants_rich_mouse(&self, _key: WindowKey) -> bool {
        false
    }

    fn rich_mouse(
        &mut self,
        _key: WindowKey,
        _engine: &mut TextEngine,
        _event: RichMouseEvent,
    ) -> bool {
        false
    }

    fn dropped_file(&mut self, _key: WindowKey, _path: &Path) -> bool {
        false
    }

    fn focus_changed(&mut self, _key: WindowKey, _focused: bool) -> bool {
        false
    }

    fn occluded(&mut self, _key: WindowKey, _occluded: bool) {}

    /// The operating-system colour scheme changed.
    ///
    /// **Asked of every window**, because the palette is a process global: a
    /// background window's cached layout is stale too.
    fn system_theme_changed(&mut self, _key: WindowKey, _dark: bool) -> bool {
        false
    }

    fn maintenance_deadline(&self, _key: WindowKey) -> Option<Instant> {
        None
    }

    fn maintain(&mut self, _key: WindowKey, _now: Instant) -> bool {
        false
    }

    fn animating(&self, _key: WindowKey) -> bool {
        false
    }

    fn draw(&mut self, key: WindowKey, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32);

    fn presented(&mut self, _key: WindowKey) {}
}

/// One window of a [`MultiWindowApp`], as the dispatch core sees it.
pub(crate) struct WindowTarget<'a, M: MultiWindowApp> {
    pub app: &'a mut M,
    pub key: WindowKey,
}

impl<M: MultiWindowApp> DispatchApp for WindowTarget<'_, M> {
    fn window_attributes(&self) -> WindowAttributes {
        self.app.window(self.key)
    }

    fn window_activation(&self) -> crate::WindowActivation {
        self.app.window_activation(self.key)
    }

    fn wants_ime(&self) -> bool {
        self.app.wants_ime(self.key)
    }

    fn paint_before_showing(&self) -> bool {
        self.app.paint_before_showing(self.key)
    }

    fn wants_drag(&self) -> bool {
        self.app.wants_drag(self.key)
    }

    fn wants_rich_mouse(&self) -> bool {
        self.app.wants_rich_mouse(self.key)
    }

    fn is_caption(&self, x: f32, y: f32) -> bool {
        self.app.is_caption(self.key, x, y)
    }

    fn cursor_icon(&self, x: f32, y: f32) -> CursorIcon {
        self.app.cursor_icon(self.key, x, y)
    }

    fn resize_direction(
        &self,
        width: f32,
        height: f32,
        x: f32,
        y: f32,
        maximised: bool,
    ) -> Option<ResizeDirection> {
        self.app
            .resize_direction(self.key, width, height, x, y, maximised)
    }

    fn set_maximised(&mut self, maximised: bool) -> bool {
        self.app.set_maximised(self.key, maximised)
    }

    fn set_pinned(&mut self, pinned: bool) -> bool {
        self.app.set_pinned(self.key, pinned)
    }

    fn moved(&mut self, position: PhysicalPosition<i32>) {
        self.app.moved(self.key, position);
    }

    fn resized(&mut self, size: PhysicalSize<u32>, scale_factor: f64) {
        self.app.resized(self.key, size, scale_factor);
    }

    fn scale_factor_changed(&mut self, scale_factor: f64) -> bool {
        self.app.scale_factor_changed(self.key, scale_factor)
    }

    fn occluded(&mut self, occluded: bool) {
        self.app.occluded(self.key, occluded);
    }

    fn focus_changed(&mut self, focused: bool) -> bool {
        self.app.focus_changed(self.key, focused)
    }

    fn system_theme_changed(&mut self, dark: bool) -> bool {
        self.app.system_theme_changed(self.key, dark)
    }

    fn close_requested(&mut self) {
        self.app.close_requested(self.key);
    }

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool {
        self.app.pointer_moved(self.key, x, y)
    }

    fn pointer_left(&mut self) -> bool {
        self.app.pointer_left(self.key)
    }

    fn click(&mut self, engine: &mut TextEngine, x: f32, y: f32, ctrl: bool) -> bool {
        self.app.click(self.key, engine, x, y, ctrl)
    }

    fn double_click(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.app.double_click(self.key, engine, x, y)
    }

    fn drag(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.app.drag(self.key, engine, x, y)
    }

    fn mouse_up(&mut self) -> bool {
        self.app.mouse_up(self.key)
    }

    fn scroll_by(&mut self, delta: f32, mods: ModifiersState) -> bool {
        self.app.scroll_by(self.key, delta, mods)
    }

    fn rich_mouse(&mut self, engine: &mut TextEngine, event: RichMouseEvent) -> bool {
        self.app.rich_mouse(self.key, engine, event)
    }

    fn context_menu(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.app.context_menu(self.key, engine, x, y)
    }

    fn physical_key_pressed(
        &mut self,
        engine: &mut TextEngine,
        physical: PhysicalKey,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        repeat: bool,
    ) -> bool {
        self.app
            .physical_key_pressed(self.key, engine, physical, key, text, mods, repeat)
    }

    fn physical_key_released(&mut self, physical: PhysicalKey) -> bool {
        self.app.physical_key_released(self.key, physical)
    }

    fn rich_key(&mut self, engine: &mut TextEngine, event: RichKeyEvent<'_>) -> bool {
        self.app.rich_key(self.key, engine, event)
    }

    fn ime(&mut self, engine: &mut TextEngine, event: ImeEvent<'_>) -> bool {
        self.app.ime(self.key, engine, event)
    }

    fn dropped_file(&mut self, path: &Path) -> bool {
        self.app.dropped_file(self.key, path)
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.app.take_window_request(self.key)
    }

    fn maintain(&mut self, now: Instant) -> bool {
        self.app.maintain(self.key, now)
    }

    fn maintenance_deadline(&self) -> Option<Instant> {
        self.app.maintenance_deadline(self.key)
    }

    fn animating(&self) -> bool {
        self.app.animating(self.key)
    }

    fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32) {
        self.app.draw(self.key, scene, engine, w, h);
    }

    fn presented(&mut self) {
        self.app.presented(self.key);
    }
}

/// Open the app's windows and run until it says to stop.
///
/// The twin of [`run`](crate::run), including why the app is built by a
/// closure: it is handed the loop's one `TextEngine` rather than making a
/// second.
pub fn run_windows<M: MultiWindowApp>(
    build: impl FnOnce(&mut TextEngine) -> M,
) -> Result<(), winit::error::EventLoopError> {
    let mut text = TextEngine::new();
    let app = build(&mut text);
    #[allow(unused_mut)]
    let mut builder = EventLoop::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        if app.accessory() {
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }
    }
    // X11 on Linux, for the reason given at the same arm in `app.rs`.
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let events = builder.build()?;
    events.set_control_flow(ControlFlow::Wait);
    let mut windows = Windows::new(app, text);
    windows.app.attach(events.create_proxy());
    events.run_app(&mut windows)
}

/// The multi-window runner's state.
struct Windows<M: MultiWindowApp> {
    gpu: Gpu,
    frame: Frame,
    text: TextEngine,
    app: M,
    /// App-level, for the reason [`MultiWindowApp::take_key_quarantine`] gives.
    quarantine: KeyQuarantine,
    live: HashMap<WindowKey, LiveWindow>,
    generations: HashMap<WindowKey, u64>,
    input: HashMap<WindowKey, WindowInput>,
    /// winit delivers by `WindowId`; the app speaks [`WindowKey`].
    ids: HashMap<WindowId, WindowKey>,
    /// Creation order, so a settle visits windows deterministically. Also the
    /// order `exiting` reports.
    order: Vec<WindowKey>,
    /// Scratch for the open/close diff, so a settle after every event does not
    /// allocate.
    wanted: Vec<WindowKey>,
}

impl<M: MultiWindowApp> Windows<M> {
    fn new(app: M, text: TextEngine) -> Self {
        Self {
            gpu: Gpu::new(),
            frame: Frame::default(),
            text,
            app,
            quarantine: KeyQuarantine::default(),
            live: HashMap::new(),
            generations: HashMap::new(),
            input: HashMap::new(),
            ids: HashMap::new(),
            order: Vec::new(),
            wanted: Vec::new(),
        }
    }

    fn settle(&mut self, events: &ActiveEventLoop) {
        let asked = self.app.take_key_quarantine();
        if !asked.is_empty() {
            self.quarantine.suppress_all(asked, Instant::now());
        }
        if self.app.should_exit() {
            let closing: Vec<(WindowKey, ClosingWindowState)> = self
                .order
                .iter()
                .filter_map(|key| {
                    let live = self.live.get(key)?;
                    let pinned = self.input.get(key).is_some_and(|input| input.pinned);
                    Some((*key, dispatch::closing_state(&live.window, pinned)))
                })
                .collect();
            self.app.exiting(&closing);
            events.exit();
            return;
        }

        // **Per window, then folded.** `ControlFlow` belongs to the loop, so an
        // animation anywhere means `Poll` and the earliest deadline anywhere
        // wins — but only the window that changed is asked to draw.
        let now = Instant::now();
        let mut animating = false;
        let mut deadline: Option<Instant> = None;
        let order = std::mem::take(&mut self.order);
        for &key in &order {
            let Self { app, live, .. } = self;
            let settled = dispatch::maintain_window(
                &mut WindowTarget { app, key },
                live.get(&key).map(|live| &live.window),
                now,
            );
            animating |= settled.animating;
            deadline = earliest(deadline, settled.deadline);
        }
        self.order = order;
        events.set_control_flow(dispatch::next_flow(animating, deadline));

        self.match_windows(events);

        let order = std::mem::take(&mut self.order);
        for &key in &order {
            if self.app.take_activation_request(key)
                && self.app.window_activation(key).allows_focus()
                && let Some(live) = self.live.get(&key)
            {
                crate::activation::activate_app();
                live.window.set_minimized(false);
                live.window.focus_window();
            }
        }
        self.order = order;
    }

    /// Open and drop windows until the set on screen is the set the app asked
    /// for. The whole of the window lifecycle, and the twin of the
    /// `wants_window` arm in the single-window runner.
    fn match_windows(&mut self, events: &ActiveEventLoop) {
        self.wanted.clear();
        self.wanted.extend_from_slice(self.app.windows());

        let mut index = 0;
        while index < self.order.len() {
            let key = self.order[index];
            let current = self.generations.get(&key).copied();
            let wanted_generation = self.app.window_generation(key);
            if self.wanted.contains(&key) && current == Some(wanted_generation) {
                index += 1;
                continue;
            }
            self.order.remove(index);
            if let Some(live) = self.live.remove(&key) {
                let pinned = self.input.get(&key).is_some_and(|input| input.pinned);
                let state = dispatch::closing_state(&live.window, pinned);
                self.ids.remove(&live.window.id());
                // Before the drop: after it there is no OS state left to read.
                self.app.window_closing(key, state);
            }
            self.input.remove(&key);
            self.generations.remove(&key);
        }

        for index in 0..self.wanted.len() {
            let key = self.wanted[index];
            if !self.live.contains_key(&key) {
                self.open(events, key);
            }
        }
    }

    fn open(&mut self, events: &ActiveEventLoop, key: WindowKey) {
        let opened = {
            let Self {
                gpu,
                frame,
                text,
                app,
                ..
            } = self;
            gpu.open(events, &mut WindowTarget { app, key }, frame, text)
        };
        // A window that could not get a surface or a renderer stays closed; the
        // next settle tries again, and the windows that did open keep drawing.
        let Some(live) = opened else { return };
        let id = live.window.id();
        let mut input = WindowInput::default();
        input.seat(self.app.pinned(key));
        self.ids.insert(id, key);
        self.input.insert(key, input);
        self.live.insert(key, live);
        self.generations
            .insert(key, self.app.window_generation(key));
        self.order.push(key);
        self.app.seated(key, id);
    }

    fn redraw(&mut self, key: WindowKey) {
        let Self {
            gpu,
            frame,
            text,
            app,
            live,
            ..
        } = self;
        let Some(live) = live.get(&key) else { return };
        gpu.redraw(live, &mut WindowTarget { app, key }, frame, text);
    }

    fn resize(&mut self, key: WindowKey, size: PhysicalSize<u32>) {
        let Self { gpu, live, .. } = self;
        if let Some(live) = live.get_mut(&key) {
            gpu.resize(live, size);
        }
    }

    fn invalidate(&self, key: WindowKey, changed: bool) {
        dispatch::invalidate(self.live.get(&key).map(|live| &live.window), changed);
    }
}

/// The earlier of two optional deadlines. `None` is "nothing pending", which
/// never wins over a real moment.
fn earliest(left: Option<Instant>, right: Option<Instant>) -> Option<Instant> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, right) => left.or(right),
    }
}

impl<M: MultiWindowApp> ApplicationHandler<M::Event> for Windows<M> {
    fn new_events(&mut self, events: &ActiveEventLoop, cause: StartCause) {
        if dispatch::starts_maintenance(&cause) {
            self.settle(events);
        }
    }

    fn resumed(&mut self, events: &ActiveEventLoop) {
        if self.app.accessory() {
            crate::activation::become_accessory();
        }
        self.settle(events);
    }

    fn user_event(&mut self, events: &ActiveEventLoop, event: M::Event) {
        // A wake names no window, so a frame it earns is owed to all of them.
        let changed = self.app.wake(event);
        self.settle(events);
        if changed {
            let order = std::mem::take(&mut self.order);
            for &key in &order {
                self.invalidate(key, true);
            }
            self.order = order;
        }
    }

    fn window_event(&mut self, events: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // A window dropped this settle can still have events queued behind it.
        let Some(&key) = self.ids.get(&id) else {
            return;
        };
        let after = {
            let Self {
                quarantine,
                live,
                input,
                text,
                app,
                ..
            } = self;
            let Some(state) = input.get_mut(&key) else {
                return;
            };
            dispatch::dispatch(
                &mut WindowTarget { app, key },
                text,
                state,
                quarantine,
                live.get(&key).map(|live| &live.window),
                event,
            )
        };
        match after.work {
            FrameWork::None => {}
            FrameWork::Redraw => self.redraw(key),
            FrameWork::Resize(size) => self.resize(key, size),
        }
        if after.settle {
            self.settle(events);
        }
        if let Some(changed) = after.invalidate {
            self.invalidate(key, changed);
        }
    }
}

#[cfg(test)]
mod tests;
