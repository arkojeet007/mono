// SPDX-License-Identifier: MPL-2.0

//! The single-window runner, and the one window it owns.
//!
//! Deliberately small, and deliberately not `koru-workspace-host`'s. That one is 11,450
//! lines because it hosts tabs, sessions, undo, drag and drop, global search
//! and a workspace — none of which exists on a chooser screen or a search
//! overlay. What both need is the *window*, and that is here.
//!
//! ## The discipline is not in this file
//!
//! Everything between winit and a [`WindowApp`] — the key quarantine, the
//! resize border, drag timing, click counting, IME, the scale divide,
//! settle/reschedule — lives in [`crate::dispatch`], because a second runner
//! ([`crate::multi`]) obeys the same rules and ADR-262 says window behaviour
//! lands once. What is left here is the part that is genuinely about having
//! *exactly one, optional* window.
//!
//! ## Everything above here is in logical pixels
//!
//! winit reports the surface and the pointer in **physical** pixels; the rects
//! an app builds are logical. The two are converted in exactly two places —
//! the frame, which appends scaled, and `CursorMoved`, which divides — so
//! nothing above has to know what a scale factor is.
//!
//! ## A frame only when something changed
//!
//! The rule ADR-26 sets for the main app. Every input method answers whether
//! anything moved, and only `true` asks for a frame. `ControlFlow::Wait`, not
//! `Poll`: an app with nothing animating should sit at zero frames, and one
//! with no window at all should sit at zero CPU.
//!
//! ## Three things the Agent loop needs
//!
//! - **A window is optional, and closing one does not end the loop.** An agent
//!   lives in the notification area with nothing on screen and opens its
//!   overlay on a hotkey. [`WindowApp::wants_window`] is the single authority:
//!   the loop creates or drops the surface to match it after every event, and
//!   [`WindowApp::should_exit`] is the only thing that ends the process.
//! - **A `Renderer` outlives its window.** Renderers are kept per *device*, not
//!   per surface, so an overlay reopened on the next hotkey press pays nothing.
//!   See [`crate::gpu`].
//! - **Other threads can wake it.** A global hotkey has to be registered on the
//!   thread that pumps for it, an IPC listener blocks on `accept`, and neither
//!   may touch a `Window`. They send a [`WindowApp::Event`] through winit's
//!   `EventLoopProxy` and it arrives at [`WindowApp::wake`] on this thread.

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

use crate::WindowActivation;
use crate::dispatch::{self, FrameWork, Single, WindowInput};
use crate::gpu::{Frame, Gpu, LiveWindow};
use crate::key_quarantine::KeyQuarantine;
use crate::request::WindowRequest;
use crate::{ImeEvent, RichKeyEvent, RichMouseEvent};

/// The live OS state captured once at a window's lifecycle boundary.
///
/// Move and resize notifications are still the cheap, ordinary path. This
/// final snapshot closes the race where a background exit request reaches the
/// winit proxy before already-queued OS geometry events.
#[derive(Copy, Clone, Debug)]
pub struct ClosingWindowState {
    pub position: Option<PhysicalPosition<i32>>,
    pub inner_size: PhysicalSize<u32>,
    pub scale_factor: f64,
    pub maximised: bool,
    pub pinned: bool,
}

/// What the loop shows, and what it asks of it.
///
/// Spelled in winit, vello and `koru-draw` terms. It names no node, no
/// workspace and no document — check `Cargo.toml` rather than believing this
/// sentence.
///
/// Every input method answers **whether anything changed**, so the loop redraws
/// only when it has to.
pub trait WindowApp {
    /// What another thread may send in, delivered to [`wake`](Self::wake).
    ///
    /// `()` for an app that has no background threads.
    type Event: Send + 'static;

    /// Observe process-wide Windows messages without consuming them. This is
    /// used for power/time facts that have no window event; callbacks must be
    /// constant-time and return false so winit still dispatches the message.
    #[cfg(target_os = "windows")]
    fn windows_message_hook(&self) -> Option<Box<dyn FnMut(*const std::ffi::c_void) -> bool>> {
        None
    }

    /// The window to open, when one is wanted.
    ///
    /// Asked each time a window is created, so an app whose overlay changes
    /// size between openings gets what it asked for rather than what it asked
    /// for first.
    fn window(&self) -> WindowAttributes {
        // Undecorated by default, because every window in this product draws its
        // own caption — which is the reason this crate's platform traps exist.
        Window::default_attributes().with_decorations(false)
    }

    /// Whether a newly-created window is an input destination or a passive
    /// status surface. Passive windows are revealed without activation and,
    /// on Windows, retain `WS_EX_NOACTIVATE` across reveal and every
    /// runner-owned native-style mutation.
    fn window_activation(&self) -> WindowActivation {
        WindowActivation::Focused
    }

    /// Identity of the requested window shape. A changed value recreates the
    /// native window while retaining the expensive device renderer.
    fn window_generation(&self) -> u64 {
        0
    }

    /// Whether there should be a window on screen at all.
    ///
    /// The **single authority**. The loop creates or drops the surface to match
    /// after every event; nothing else opens or closes a window, including
    /// [`WindowRequest::Close`].
    fn wants_window(&self) -> bool {
        true
    }

    /// Whether this process is an *accessory* on macOS: no Dock tile, no ⌘-Tab
    /// entry — a status item and its windows are the whole surface. The
    /// tray-only shape the agent has on Windows, spoken in AppKit.
    ///
    /// Applied by the loop in `resumed`, deliberately: winit sets its own
    /// (Regular) activation policy during application launch, so anything set
    /// before the loop runs is overwritten — which is why this is a question
    /// the loop asks rather than a call an app makes. No effect off macOS.
    fn accessory(&self) -> bool {
        false
    }

    /// Whether the process should end. Checked after every event.
    fn should_exit(&self) -> bool;

    /// The process is about to leave the event loop.
    ///
    /// Called once with the live window's final OS state when one exists. Disk
    /// persistence belongs here rather than in [`wake`](Self::wake), because a
    /// proxy event can overtake queued `Moved`/`Resized` notifications.
    fn exiting(&mut self, _window: Option<ClosingWindowState>) {}

    /// The window's ✕, or Alt+F4, or the taskbar's close.
    ///
    /// **Required, with no default, on purpose.** What closing means is the one
    /// thing the loop cannot guess — a chooser exits, an overlay hides, a
    /// document window may want to ask first — and a default that picked one of
    /// them would make the other two a bug you find by clicking rather than by
    /// compiling.
    fn close_requested(&mut self);

    /// Something a background thread sent through the proxy.
    fn wake(&mut self, _event: Self::Event) -> bool {
        false
    }

    /// Hand the app the proxy its threads will send on. Called once, before the
    /// loop starts, so an app can pass it to whatever it spawns.
    fn attach(&mut self, _proxy: EventLoopProxy<Self::Event>) {}

    /// Whether a click and a drag should be tracked as one gesture — true only
    /// where there is something a drag would extend, such as a text selection.
    fn wants_drag(&self) -> bool {
        false
    }

    /// The band that drags the window: the caption **except** its buttons.
    ///
    /// Asked before the press is handled, because `drag_window` only works
    /// inside the press that asked for it.
    fn is_caption(&self, _x: f32, _y: f32) -> bool {
        false
    }

    /// The pointer shown at a logical window position.
    ///
    /// Kept beside caption hit-testing so a custom drag band can advertise
    /// itself without either the app or the shared loop retaining hover state.
    fn cursor_icon(&self, _x: f32, _y: f32) -> CursorIcon {
        CursorIcon::Default
    }

    /// The resize edge owned by the host at this logical point.
    ///
    /// Undecorated Windows windows have no native non-client border, so a host
    /// that is resizable opts into exactly the directions it supports. The
    /// shared loop claims that press before caption or content input.
    fn resize_direction(
        &self,
        _width: f32,
        _height: f32,
        _x: f32,
        _y: f32,
        _maximised: bool,
    ) -> Option<ResizeDirection> {
        None
    }

    /// The drawable size changed. `size` is physical; `scale_factor` converts
    /// it to the logical units used everywhere above this loop.
    fn resized(&mut self, _size: PhysicalSize<u32>, _scale_factor: f64) {}

    /// The display's DPI changed, with or without a change of drawable size.
    ///
    /// **The gap this closes:** the loop only ever re-read `scale_factor()` on
    /// a resize, so moving a window to a display at a different scale that
    /// happens to leave the drawable size alone was invisible to the app. Most
    /// hosts still need nothing — the frame is composed at the live scale and
    /// a `Resized` normally follows — so this defaults to "nothing changed".
    /// A host that caches anything derived from the scale overrides it.
    fn scale_factor_changed(&mut self, _scale_factor: f64) -> bool {
        false
    }

    /// Reveal a newly created window by expanding from its own centre.
    ///
    /// The loop creates it hidden and paints once before revealing it, so an
    /// app opting in never flashes at the OS's provisional placement.
    ///
    /// **Named for what it does now.** It was `reveal_from_center`, when the
    /// reveal was also an expand-from-centre animation; that animation is gone
    /// (see [`crate::reveal`]) and the ordering is the whole of what is left.
    fn paint_before_showing(&self) -> bool {
        false
    }

    /// A window-level action the app asked for, taken by the asking.
    fn take_window_request(&mut self) -> Option<WindowRequest> {
        None
    }

    /// Bring an existing window to the foreground.
    ///
    /// Drained after every event. A singleton companion process uses this when
    /// a second invocation asks the first one to activate; the app cannot do
    /// it itself because the loop is the only owner of the [`Window`].
    fn take_activation_request(&mut self) -> bool {
        false
    }

    /// Physical keys that must not reach this app until they are released.
    ///
    /// **The press that summoned a window is delivered to it.** Windows sends
    /// the repeat — and often the release — of a chord to whichever window has
    /// the focus, not to whoever consumed the original press. So an overlay
    /// opened on Ctrl+Shift+K receives the `K` and types it into its search
    /// box, and receives no release for Shift, which then behaves as held: the
    /// mouse selects as it moves. Both were reported as bugs in exactly those
    /// words.
    ///
    /// **This has to be the loop's job, not the app's.** The quarantine is
    /// keyed on the *physical* key so layout and composition cannot change its
    /// meaning, and [`key`](Self::key) is handed a logical key — by design,
    /// since a view has no business knowing about scan codes. `KeyEvent` exists
    /// only here, which is the same reason `alt_digit` and Ctrl+Tab are
    /// resolved in a loop rather than in a key map.
    ///
    /// Drained, like [`take_window_request`](Self::take_window_request): the
    /// app names the keys once, when it opens the thing that must not receive
    /// them.
    fn take_key_quarantine(&mut self) -> Vec<PhysicalKey> {
        Vec::new()
    }

    /// Whether the window now fills the screen, pushed whenever it changes.
    ///
    /// A view that draws its own caption needs this — a maximise button has to
    /// become a restore button — and it has no `Window` to ask.
    fn set_maximised(&mut self, _maximised: bool) -> bool {
        false
    }

    /// Whether the window is now above the others, pushed after a pin request.
    ///
    /// A custom caption needs this for its checked menu row, and a host that
    /// persists geometry needs the actual state rather than the request that
    /// happened to toggle it.
    fn set_pinned(&mut self, _pinned: bool) -> bool {
        false
    }

    /// The pin state restored before this window was created.
    fn pinned(&self) -> bool {
        false
    }

    /// The OS moved the window, in physical desktop coordinates.
    ///
    /// A host that remembers position records it here and persists at a
    /// lifecycle boundary. This callback itself must never write to disk: a
    /// drag produces one event per pixel.
    fn moved(&mut self, _position: PhysicalPosition<i32>) {}

    fn pointer_moved(&mut self, x: f32, y: f32) -> bool;

    /// The pointer left this window.
    ///
    /// **The gap this closes:** winit sends `CursorLeft` and the loop dropped
    /// it, so hover was cleared only by the next `CursorMoved` — which never
    /// arrives if the pointer has gone somewhere else entirely, leaving a row
    /// highlighted under a window that no longer has the pointer. There is no
    /// position, because there is no longer one inside this window.
    fn pointer_left(&mut self) -> bool {
        false
    }

    /// `ctrl` is the primary modifier (Ctrl on Windows/Linux, Cmd on macOS) so
    /// a click can open a result in a new window the same way Ctrl+Enter does.
    fn click(&mut self, engine: &mut TextEngine, x: f32, y: f32, ctrl: bool) -> bool;
    fn double_click(&mut self, _engine: &mut TextEngine, _x: f32, _y: f32) -> bool {
        false
    }
    /// The right button was pressed at this logical position.
    fn context_menu(&mut self, engine: &mut TextEngine, x: f32, y: f32) -> bool {
        self.right_click(engine, x, y)
    }
    /// The pointer moved with the button down. Only reached while
    /// [`wants_drag`](Self::wants_drag) said so.
    fn drag(&mut self, _engine: &mut TextEngine, _x: f32, _y: f32) -> bool {
        false
    }
    /// A wheel notch, already converted to logical pixels, plus the modifiers
    /// held when it arrived. The latter keeps primary+wheel display zoom in a
    /// profile-free window rather than forcing every loop user to give it up.
    fn scroll_by(&mut self, _delta: f32, _mods: ModifiersState) -> bool {
        false
    }
    fn key(
        &mut self,
        engine: &mut TextEngine,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool;

    /// A physical key went down, before [`key`](Self::key) sees it.
    ///
    /// Most apps leave this alone. A hold gesture is the exception: its end
    /// has to be paired with the exact physical key that began it, while the
    /// ordinary text path deliberately receives only a logical key. Returning
    /// `true` consumes the press, so a shortcut never types its own letter.
    fn physical_key_pressed(
        &mut self,
        _engine: &mut TextEngine,
        _physical: PhysicalKey,
        _key: &Key,
        _text: Option<&str>,
        _mods: ModifiersState,
        _repeat: bool,
    ) -> bool {
        false
    }

    /// A physical key came up. The counterpart to
    /// [`physical_key_pressed`](Self::physical_key_pressed).
    fn physical_key_released(&mut self, _physical: PhysicalKey) -> bool {
        false
    }

    /// Lossless keyboard input for a terminal or another mode-aware surface.
    ///
    /// Existing applications keep implementing [`key`](Self::key): the default
    /// adapter calls it for presses and preserves the previous behavior.
    /// Releases are visible only to applications which override this hook.
    fn rich_key(&mut self, engine: &mut TextEngine, event: RichKeyEvent<'_>) -> bool {
        if event.state.is_pressed() {
            self.key(engine, event.logical_key, event.text, event.modifiers)
        } else {
            false
        }
    }

    /// Whether this window asks the OS to produce composition events.
    ///
    /// Opt-in preserves the legacy key stream for existing applications.
    fn wants_ime(&self) -> bool {
        false
    }

    fn ime(&mut self, _engine: &mut TextEngine, _event: ImeEvent<'_>) -> bool {
        false
    }

    fn right_click(&mut self, _engine: &mut TextEngine, _x: f32, _y: f32) -> bool {
        false
    }

    /// Opt into lossless button transitions for a terminal-style surface.
    /// Existing apps retain the click/double-click/right-click callbacks.
    fn wants_rich_mouse(&self) -> bool {
        false
    }

    fn rich_mouse(&mut self, _engine: &mut TextEngine, _event: RichMouseEvent) -> bool {
        false
    }

    /// One callback per path; winit emits a separate event for each dropped
    /// file in a multi-file gesture.
    fn dropped_file(&mut self, _path: &Path) -> bool {
        false
    }

    /// The window gained or lost the keyboard focus.
    ///
    /// **What an overlay needs and a document window does not.** A panel that
    /// appears over other applications is dismissed by clicking away from it,
    /// and `Focused(false)` is the only notice of that a window gets — there is
    /// no click to hit-test, because the click went somewhere else entirely.
    fn focus_changed(&mut self, _focused: bool) -> bool {
        false
    }

    /// The compositor stopped or resumed presenting this window. Metrics use
    /// this boundary so time spent minimized or fully occluded is not counted
    /// as visible experience time.
    fn occluded(&mut self, _occluded: bool) {}

    /// The operating-system colour scheme changed.
    ///
    /// An app following the System preference updates its process-global
    /// palette here. Apps with an explicit light or dark choice ignore it.
    fn system_theme_changed(&mut self, _dark: bool) -> bool {
        false
    }
    /// A background deadline this app owns — the next moment it has work to do
    /// that no input will trigger.
    ///
    /// **Without this the loop never wakes on its own**, and an app with
    /// anything in flight waits for a keystroke to notice it arrived. That is
    /// exactly what happened to the agent's search overlay: its reader answers
    /// on another thread, `maintain` is what drains the answer, and with no
    /// deadline the surface sat on *Searching…* until the pointer moved.
    ///
    /// `None` means nothing is pending and keeps the loop at zero CPU while idle.
    fn maintenance_deadline(&self) -> Option<Instant> {
        None
    }

    /// Do whatever is due. Returns whether anything visible changed.
    ///
    /// Called when a deadline this app named has passed, and again after every
    /// event — draining is cheap and a reply that arrived during a keystroke
    /// should not wait for the next one.
    fn maintain(&mut self, _now: Instant) -> bool {
        false
    }

    /// Whether this app has motion in flight and needs another frame.
    ///
    /// **It must return to `false`**, or the loop spins at frame rate forever.
    /// Drive it from a deadline rather than a per-frame decrement, so a dropped
    /// frame cannot leave it stuck on. The one exception ADR-26 allows.
    fn animating(&self) -> bool {
        false
    }

    /// The left button came up.
    fn mouse_up(&mut self) -> bool {
        false
    }

    /// Draw into `scene`, in **logical** pixels. The loop scales.
    fn draw(&mut self, scene: &mut Scene, engine: &mut TextEngine, w: f32, h: f32);

    /// The frame built by the preceding [`Self::draw`] was successfully
    /// submitted and presented. Use this for presentation acknowledgements and
    /// user-visible latency samples, never for drawing or blocking work.
    fn presented(&mut self) {}
}

/// Open a window and run until the app says to stop.
///
/// **The app is built by a closure, and handed the loop's `TextEngine`.** Both
/// apps that use this need one before they can exist — a screen with a text
/// field on it constructs the field with an engine — and an app that made its
/// own would leave two per window. Measured: 7.3 ms for the first engine in a
/// process and 384 µs for each one after
/// (`perf::opening_a_tab_does_not_re_read_the_fonts`), so a second is not free
/// and is never necessary.
pub fn run<A: WindowApp>(
    build: impl FnOnce(&mut TextEngine) -> A,
) -> Result<(), winit::error::EventLoopError> {
    // The app is built before the loop so the loop can be built *for* it:
    // an accessory's activation policy has to be declared at construction,
    // or the Dock shows a second bouncing Koru for the seconds between
    // launch and `resumed` — which is exactly what the agent did on every
    // start. The build closure only needs the engine, so nothing is owed to
    // the loop yet.
    let mut text = TextEngine::new();
    let app = build(&mut text);
    #[allow(unused_mut)]
    let mut builder = EventLoop::with_user_event();
    #[cfg(target_os = "windows")]
    if let Some(hook) = app.windows_message_hook() {
        use winit::platform::windows::EventLoopBuilderExtWindows;
        builder.with_msg_hook(hook);
    }
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        if app.accessory() {
            builder.with_activation_policy(ActivationPolicy::Accessory);
        }
    }
    // **X11 on Linux, through XWayland on a Wayland desktop.** The clipboard
    // backend (`arboard`, pinned without `wayland-data-control`) speaks X11
    // only, and mutter bridges the two clipboards, so copy and paste work with
    // no dependency change. Chosen here rather than by unsetting
    // `WAYLAND_DISPLAY` in a launcher, which every child process would
    // inherit. Native Wayland deletes this arm, its twin in `multi.rs` and the
    // one in `koru_workspace_host::app`
    // (docs/features/linux/TECHNICAL.final.md, D5).
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let events = builder.build()?;
    // `Wait`, not `Poll`. Nothing here animates and nothing polls, so the
    // process should use no CPU at all while it is being read — or while it is
    // sitting in the notification area with no window.
    events.set_control_flow(ControlFlow::Wait);
    let mut shell = Shell::new(app, text);
    // Before the loop starts, so whatever the app spawns can already reach it.
    shell.app.attach(events.create_proxy());
    events.run_app(&mut shell)
}

/// The single-window runner's state: one optional window over the shared
/// dispatch core.
struct Shell<A: WindowApp> {
    gpu: Gpu,
    frame: Frame,
    /// Keys the app has asked not to receive until they are released. See
    /// [`WindowApp::take_key_quarantine`].
    quarantine: KeyQuarantine,
    live: Option<LiveWindow>,
    live_generation: Option<u64>,
    input: WindowInput,
    text: TextEngine,
    app: A,
}

impl<A: WindowApp> Shell<A> {
    fn new(app: A, text: TextEngine) -> Self {
        Self {
            gpu: Gpu::new(),
            frame: Frame::default(),
            quarantine: KeyQuarantine::default(),
            live: None,
            live_generation: None,
            input: WindowInput::default(),
            text,
            app,
        }
    }

    /// Bring the window on screen or take it off, to match the app, and end the
    /// loop when the app says to.
    ///
    /// Called after every event. It is the whole of the window's lifecycle:
    /// nothing else creates or drops one, which is what makes
    /// [`WindowApp::wants_window`] answerable rather than merely advisory.
    fn settle(&mut self, events: &ActiveEventLoop) {
        // **Here rather than beside `take_window_request`, because the path
        // that needs it does not go there.** A hotkey arrives as `user_event`,
        // which calls `settle` and never `perform_request` — so a drain placed
        // with the window request would have been silently unreachable on the
        // one path that opens an overlay, which is the whole point of the
        // quarantine. Every dispatch in this file ends in `settle`.
        let asked = self.app.take_key_quarantine();
        if !asked.is_empty() {
            self.quarantine.suppress_all(asked, Instant::now());
        }
        if self.app.should_exit() {
            let window = self
                .live
                .as_ref()
                .map(|live| dispatch::closing_state(&live.window, self.input.pinned));
            self.app.exiting(window);
            events.exit();
            return;
        }
        let settled = {
            let Self { app, live, .. } = self;
            let handle = live.as_ref().map(|live| &live.window);
            dispatch::maintain_window(&mut Single(app), handle, Instant::now())
        };
        events.set_control_flow(dispatch::next_flow(settled.animating, settled.deadline));
        let wanted = self.app.wants_window();
        let generation = self.app.window_generation();
        if wanted && self.live.is_some() && self.live_generation != Some(generation) {
            self.live = None;
            self.live_generation = None;
            self.input.unseat();
        }
        match (wanted, self.live.is_some()) {
            (true, false) => self.open(events),
            // Dropping the `LiveWindow` drops the surface and the window; the
            // renderers stay.
            (false, true) => {
                self.live = None;
                self.live_generation = None;
                self.input.unseat();
            }
            _ => {}
        }
        if self.app.take_activation_request()
            && self.app.window_activation().allows_focus()
            && let Some(live) = &self.live
        {
            crate::activation::activate_app();
            live.window.set_minimized(false);
            live.window.focus_window();
        }
    }

    fn open(&mut self, events: &ActiveEventLoop) {
        let opened = {
            let Self {
                gpu,
                frame,
                text,
                app,
                ..
            } = self;
            gpu.open(events, &mut Single(app), frame, text)
        };
        if let Some(live) = opened {
            self.live = Some(live);
            self.live_generation = Some(self.app.window_generation());
            self.input.seat(self.app.pinned());
        }
    }

    fn redraw(&mut self) {
        let Self {
            gpu,
            frame,
            text,
            app,
            live,
            ..
        } = self;
        let Some(live) = live.as_ref() else {
            return;
        };
        gpu.redraw(live, &mut Single(app), frame, text);
    }

    fn invalidate(&self, changed: bool) {
        dispatch::invalidate(self.live.as_ref().map(|live| &live.window), changed);
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        let Self { gpu, live, .. } = self;
        if let Some(live) = live.as_mut() {
            gpu.resize(live, size);
        }
    }
}

impl<A: WindowApp> ApplicationHandler<A::Event> for Shell<A> {
    fn new_events(&mut self, events: &ActiveEventLoop, cause: StartCause) {
        // `WaitUntil` waking starts a new event batch; it does not manufacture a
        // window or user event. Without handling the cause, an asynchronous
        // search reply sits ready until the next key or pointer event happens
        // to call `settle`.
        if dispatch::starts_maintenance(&cause) {
            self.settle(events);
        }
    }

    fn resumed(&mut self, events: &ActiveEventLoop) {
        // Belt to the builder's braces: the policy is already Accessory from
        // `with_activation_policy` at construction, which is what keeps the
        // Dock from ever seeing this process. This re-assertion costs nothing,
        // is idempotent, and covers any path that built the loop elsewhere.
        if self.app.accessory() {
            crate::activation::become_accessory();
        }
        self.settle(events);
    }

    fn user_event(&mut self, events: &ActiveEventLoop, event: A::Event) {
        let changed = self.app.wake(event);
        self.settle(events);
        self.invalidate(changed);
    }

    fn window_event(&mut self, events: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let after = {
            let Self {
                quarantine,
                live,
                input,
                text,
                app,
                ..
            } = self;
            let handle = live.as_ref().map(|live| &live.window);
            dispatch::dispatch(&mut Single(app), text, input, quarantine, handle, event)
        };
        match after.work {
            FrameWork::None => {}
            FrameWork::Redraw => self.redraw(),
            FrameWork::Resize(size) => self.resize(size),
        }
        if after.settle {
            self.settle(events);
        }
        if let Some(changed) = after.invalidate {
            self.invalidate(changed);
        }
    }
}

#[cfg(test)]
mod tests;
