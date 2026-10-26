// SPDX-License-Identifier: MPL-2.0

//! The Agent-owned global shortcuts.
//!
//! One machine-wide key combination that brings Koru's search up from any
//! application. It is a *platform* concern and not a search one — nothing here
//! reads an index.
//!
//! ## The election is gone, and that is the point of the agent
//!
//! `RegisterHotKey` is machine-wide: two Koru windows both registering means
//! the second loses `Ctrl+Shift+K`, silently falls through to another search chord,
//! and tells the person their shortcut was taken — by themselves. So in
//! `apps/koru` this was gated on winning the taskbar activation pipe, an
//! election that existed for another purpose and was borrowed for this one.
//!
//! **The agent is the owner by construction.** Binding its endpoint is the
//! exclusion; a second agent stands down before it reaches this file. One
//! election per machine, and it is the one that was already load-bearing.
//!
//! ## On Windows it needs a thread; on macOS it must not have one
//!
//! `RegisterHotKey(None, ..)` posts `WM_HOTKEY` to the **registering thread's**
//! message queue, and winit does not surface thread messages through
//! `ApplicationHandler`. `GetMessageW` blocks, so the thread is its own. It
//! reaches the loop the way every other producer does: through a [`Waker`].
//!
//! **That shape is a real structural difference, not a default to copy.**
//! Carbon delivers hotkey events on the main run loop, so on macOS
//! registration happens inline on the winit thread and
//! `GlobalHotKeyEvent::set_event_handler` is the whole delivery — the handler
//! only wakes the loop, the same [`AgentEvent::Hotkey`] the Windows pump
//! raises.
//!
//! ## The shortcut is chosen, not assumed — and the menu is told which
//!
//! A person's preference first, then three fall-backs, because the combination
//! we want may belong to something else already. Whichever is taken is reported
//! so the surface can say what happened rather than leaving a dead shortcut.
//!
//! **That report used to go only to stderr, which a released agent does not
//! have** — it is `windows_subsystem = "windows"`, so the line explaining that
//! `Ctrl+Shift+K` was taken and a fallback opens search instead was written
//! into nothing. Meanwhile the tray drew `Ctrl+Shift+K` beside *Search* from a
//! hardcoded accelerator, so the one surface a person could actually consult
//! confidently told them the wrong chord.
//!
//! So registration ends by raising [`AgentEvent::HotkeyRegistered`], carrying
//! the label that was taken or `None` when every candidate was. The tray shows
//! that, and shows it as the plain truth when there is none — and the agent
//! pushes the same truth to every connected shell (`Push::Shortcut`), so a
//! window's own surface (the taskbar Jump List's *Search* task) can say it too,
//! from one answer rather than two guesses.

use crate::event::{AgentEvent, Waker};

#[derive(Clone, Default)]
pub struct Control {
    #[cfg(windows)]
    windows: Option<std::sync::Arc<WindowsControl>>,
}

#[cfg(windows)]
struct WindowsControl {
    commands: std::sync::Mutex<std::collections::VecDeque<DictationCommand>>,
    thread_id: std::sync::atomic::AtomicU32,
}

#[cfg(windows)]
fn enqueue_windows_command(
    control: &WindowsControl,
    command: DictationCommand,
    post: impl FnOnce(u32) -> bool,
) -> bool {
    use std::sync::atomic::Ordering;
    let mut commands = control
        .commands
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    commands.push_back(command);
    let thread = control.thread_id.load(Ordering::Acquire);
    if thread != 0 && !post(thread) {
        let _ = commands.pop_back();
        return false;
    }
    true
}

// Constructed on every platform, read only by the Windows pump.
#[cfg_attr(not(windows), allow(dead_code))]
enum DictationCommand {
    Prepare { generation: u64, shortcut: String },
    Commit { generation: u64 },
    Rollback { generation: u64 },
}

#[cfg(any(windows, test))]
struct PreparedBinding<T> {
    active: Option<T>,
    candidate: Option<(u64, T)>,
    generation: Option<u64>,
}

#[cfg(any(windows, test))]
impl<T> PreparedBinding<T> {
    fn new(active: Option<T>) -> Self {
        Self {
            active,
            candidate: None,
            generation: None,
        }
    }

    fn prepare(&mut self, generation: u64, candidate: Option<T>) {
        self.candidate = candidate.map(|value| (generation, value));
        self.generation = Some(generation);
    }

    fn reject(&mut self) {
        self.candidate = None;
        self.generation = None;
    }

    fn commit(&mut self, generation: u64) -> bool {
        if self.generation != Some(generation) {
            return false;
        }
        if self
            .candidate
            .as_ref()
            .is_some_and(|(seen, _)| *seen == generation)
        {
            self.active = self.candidate.take().map(|(_, value)| value);
        }
        self.generation = None;
        true
    }

    fn rollback(&mut self, generation: u64) -> bool {
        if self.generation != Some(generation) {
            return false;
        }
        self.candidate = None;
        self.generation = None;
        true
    }

    fn effective(&self) -> Option<&T> {
        self.candidate
            .as_ref()
            .map(|(_, value)| value)
            .or(self.active.as_ref())
    }
}

impl Control {
    pub fn prepare_dictation(&self, generation: u64, shortcut: &str) -> bool {
        self.enqueue_dictation(DictationCommand::Prepare {
            generation,
            shortcut: shortcut.to_owned(),
        })
    }

    pub fn commit_dictation(&self, generation: u64) -> bool {
        self.enqueue_dictation(DictationCommand::Commit { generation })
    }

    pub fn rollback_dictation(&self, generation: u64) -> bool {
        self.enqueue_dictation(DictationCommand::Rollback { generation })
    }

    fn enqueue_dictation(&self, command: DictationCommand) -> bool {
        #[cfg(windows)]
        {
            use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;
            let Some(control) = &self.windows else {
                return false;
            };
            enqueue_windows_command(control, command, |thread| {
                unsafe {
                    PostThreadMessageW(
                        thread,
                        windows_impl::RECONFIGURE_MESSAGE,
                        windows::Win32::Foundation::WPARAM(0),
                        windows::Win32::Foundation::LPARAM(0),
                    )
                }
                .is_ok()
            })
        }
        #[cfg(not(windows))]
        {
            let _ = command;
            false
        }
    }
}

/// The two configurable global actions owned by one registration pump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Search,
    Dictation,
}

/// Every Search chord this agent will try, preference first, in order.
///
/// Named here rather than only inside the Windows implementation so the tray
/// can assert that it is able to *draw* all of them — the label a registration
/// reports is what the menu parses into an accelerator, and the two live in
/// different files. See `tray::tests`.
pub const SEARCH_CANDIDATES: &[&str] = &["Ctrl+Shift+K", "Ctrl+Alt+Space", "Ctrl+Shift+O"];

/// Dictation's independent fallback list. Search occupying one candidate never
/// advances this action, and vice versa.
pub const DICTATION_CANDIDATES: &[&str] = &["Ctrl+Shift+D", "Ctrl+Alt+D", "Ctrl+Shift+M"];

pub const ACTIONS: &[Action] = &[Action::Search, Action::Dictation];

pub fn candidates(action: Action) -> &'static [&'static str] {
    match action {
        Action::Search => SEARCH_CANDIDATES,
        Action::Dictation => DICTATION_CANDIDATES,
    }
}

/// Register the shortcut on a thread of its own, if this launch opted into the
/// machine-wide input namespace.
///
/// Headless explicit roots remain inert so gates cannot take a person's keys.
/// The interactive root launcher opts in; Windows still arbitrates every chord
/// through `RegisterHotKey` and refuses genuine conflicts.
pub fn start(paths: &crate::Paths, machine_input_enabled: bool, waker: Waker) -> Control {
    if !machine_input_enabled {
        koru_ipc::ipc_debug!("hotkey: not registered — this launch did not opt into machine input");
        // Say so rather than leaving a surface waiting for registration truth.
        report_unavailable(&waker);
        return Control::default();
    }
    let preferred = preferred_shortcut(paths);
    #[cfg(windows)]
    {
        let control = std::sync::Arc::new(WindowsControl {
            commands: std::sync::Mutex::new(std::collections::VecDeque::new()),
            thread_id: std::sync::atomic::AtomicU32::new(0),
        });
        let worker_waker = waker.clone();
        let worker_control = std::sync::Arc::clone(&control);
        let spawned = std::thread::Builder::new()
            .name("koru-agent hotkey".into())
            .spawn(move || {
                windows_impl::register_and_pump(&preferred, worker_waker, worker_control)
            });
        if report_spawn(spawned, &waker).is_some() {
            return Control {
                windows: Some(control),
            };
        }
        return Control::default();
    }
    // Inline, not a thread: Carbon delivers on the main run loop, and `start`
    // is called on the winit thread before the loop parks. See the module note.
    #[cfg(target_os = "macos")]
    {
        macos_impl::register(&preferred, waker);
        Control::default()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        // **There is no global shortcut here yet, and the menu is told so**
        // rather than left waiting for an answer that never comes. Linux has
        // not been through the port ([ROADMAP](../../../docs/deferred/README.md)'s
        // rule: an arm must not claim a platform nobody verified).
        let _ = preferred;
        report_unavailable(&waker);
        Control::default()
    }
}

fn report_unavailable(waker: &Waker) {
    for action in ACTIONS {
        waker.wake(AgentEvent::HotkeyRegistered {
            action: *action,
            shortcut: None,
        });
    }
    waker.wake(AgentEvent::AiHotkeyRegistered { available: false });
}

#[cfg(any(windows, test))]
fn report_spawn<T, E>(result: Result<T, E>, waker: &Waker) -> Option<T> {
    match result {
        Ok(worker) => Some(worker),
        Err(_) => {
            report_unavailable(waker);
            None
        }
    }
}

/// Every candidate, the person's preference first — one ordering both
/// platforms register through, built from the action's candidate table so
/// there is no second literal list to drift from the ones the tray can draw.
///
/// A preference that names no candidate (an old or hand-edited value) falls
/// through to the whole list in its shipped order, which starts with the
/// default — the same answer the Windows arm always gave.
#[cfg(any(windows, target_os = "macos", test))]
fn candidate_order(action: Action, preferred: &str) -> Vec<&'static str> {
    let candidates = candidates(action);
    let mut order: Vec<&'static str> = candidates
        .iter()
        .copied()
        .filter(|candidate| *candidate == preferred)
        .collect();
    order.extend(
        candidates
            .iter()
            .copied()
            .filter(|candidate| *candidate != preferred),
    );
    order
}

/// Try one action's candidates in its own order. The closure is the complete
/// registration effect, which makes fallback and all-taken behavior testable
/// without touching an OS singleton.
#[cfg(any(windows, test))]
fn register_first<T>(
    action: Action,
    preferred: &str,
    mut register: impl FnMut(&'static str) -> Option<T>,
) -> Option<(&'static str, T)> {
    for label in candidate_order(action, preferred) {
        if let Some(registration) = register(label) {
            return Some((label, registration));
        }
    }
    None
}

/// What the person asked for, from the machine-local desktop preferences.
///
/// Redirected under an explicit root — though that path never reaches here,
/// since `start` has already refused. Read through `Paths` rather than
/// `DesktopPreferences::load` directly so the redirection is impossible to
/// forget when a second caller appears.
fn preferred_shortcut(paths: &crate::Paths) -> String {
    match paths.preferences_file() {
        Some(_) => koru_window::DesktopPreferences::default().global_shortcut,
        None => koru_window::DesktopPreferences::load().global_shortcut,
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::{Action, AgentEvent, Waker};
    use crate::event::{ForegroundPid, HotkeyPress};
    use crate::global_input::{
        PressRegistrar, VirtualKey,
        windows::{WindowsChord, WindowsPressRegistrar},
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_OEM_2, VK_OEM_4, VK_OEM_5, VK_OEM_6,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, MSG, PM_NOREMOVE, PeekMessageW,
        WM_APP, WM_HOTKEY,
    };

    pub(super) const SEARCH_ID: i32 = 0x4b53;
    pub(super) const DICTATION_ID: i32 = 0x4b44;
    pub(super) const DICTATION_ALT_ID: i32 = 0x4b45;
    pub(super) const AI_ID: i32 = 0x4b41;
    pub(super) const RECONFIGURE_MESSAGE: u32 = WM_APP + 0x4b;

    /// What a candidate label registers as. Total over both action tables,
    /// and `hotkey::tests` holds it to that.
    pub(super) fn parts(label: &str) -> Option<(WindowsChord, winit::keyboard::KeyCode)> {
        let (modifier, key) = label.rsplit_once('+')?;
        let modifiers = match modifier {
            "Ctrl+Shift" => MOD_CONTROL | MOD_SHIFT,
            "Ctrl+Alt" => MOD_CONTROL | MOD_ALT,
            "Ctrl+Win" => MOD_CONTROL | MOD_WIN,
            "Shift+Win" => MOD_SHIFT | MOD_WIN,
            _ => return None,
        };
        let (key, physical) = key_parts(key)?;
        Some((
            WindowsChord {
                modifiers,
                key: VirtualKey::new(key),
            },
            physical,
        ))
    }

    fn key_parts(key: &str) -> Option<(u16, winit::keyboard::KeyCode)> {
        use winit::keyboard::KeyCode;
        match key {
            "Space" => Some((b' ' as u16, KeyCode::Space)),
            "[" => Some((VK_OEM_4.0, KeyCode::BracketLeft)),
            "]" => Some((VK_OEM_6.0, KeyCode::BracketRight)),
            "\\" => Some((VK_OEM_5.0, KeyCode::Backslash)),
            "/" => Some((VK_OEM_2.0, KeyCode::Slash)),
            _ if key.len() == 1 => {
                let byte = key.as_bytes()[0];
                if byte.is_ascii_uppercase() || byte.is_ascii_digit() {
                    Some((byte as u16, key_code(byte)?))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn key_code(key: u8) -> Option<winit::keyboard::KeyCode> {
        use winit::keyboard::KeyCode::*;
        Some(match key {
            b'A' => KeyA,
            b'B' => KeyB,
            b'C' => KeyC,
            b'D' => KeyD,
            b'E' => KeyE,
            b'F' => KeyF,
            b'G' => KeyG,
            b'H' => KeyH,
            b'I' => KeyI,
            b'J' => KeyJ,
            b'K' => KeyK,
            b'L' => KeyL,
            b'M' => KeyM,
            b'N' => KeyN,
            b'O' => KeyO,
            b'P' => KeyP,
            b'Q' => KeyQ,
            b'R' => KeyR,
            b'S' => KeyS,
            b'T' => KeyT,
            b'U' => KeyU,
            b'V' => KeyV,
            b'W' => KeyW,
            b'X' => KeyX,
            b'Y' => KeyY,
            b'Z' => KeyZ,
            b'0' => Digit0,
            b'1' => Digit1,
            b'2' => Digit2,
            b'3' => Digit3,
            b'4' => Digit4,
            b'5' => Digit5,
            b'6' => Digit6,
            b'7' => Digit7,
            b'8' => Digit8,
            b'9' => Digit9,
            b' ' => Space,
            _ => return None,
        })
    }

    fn register_action(
        registrar: &mut WindowsPressRegistrar,
        action: Action,
        preferred: &str,
        id: i32,
        waker: &Waker,
    ) -> Option<(
        crate::global_input::windows::WindowsPressRegistration,
        winit::keyboard::KeyCode,
        VirtualKey,
        String,
    )> {
        if let Some((label, selected)) = super::register_first(action, preferred, |label| {
            let (chord, trigger) = parts(label)?;
            let registration = registrar.register(id, chord).ok()?;
            Some((registration, trigger, chord.key))
        }) {
            koru_ipc::ipc_debug!("hotkey: registered {label}");
            waker.wake(AgentEvent::HotkeyRegistered {
                action,
                shortcut: Some(label.to_owned()),
            });
            return Some((selected.0, selected.1, selected.2, label.to_owned()));
        }
        waker.wake(AgentEvent::HotkeyRegistered {
            action,
            shortcut: None,
        });
        None
    }

    pub(super) fn dispatch(
        id: i32,
        search: Option<winit::keyboard::KeyCode>,
        dictation: Option<(i32, VirtualKey)>,
        ai: bool,
        foreground: ForegroundPid,
    ) -> Option<AgentEvent> {
        if id == SEARCH_ID {
            search.map(|trigger| AgentEvent::Hotkey(HotkeyPress::Search { trigger }))
        } else if dictation.is_some_and(|(active, _)| id == active) {
            dictation.map(|(_, key)| AgentEvent::Hotkey(HotkeyPress::Dictation { key, foreground }))
        } else if id == AI_ID && ai {
            Some(AgentEvent::AiHotkey)
        } else {
            None
        }
    }

    pub(super) type Binding<R> = (
        i32,
        <R as PressRegistrar>::Registration,
        winit::keyboard::KeyCode,
        VirtualKey,
        String,
    );

    pub(super) fn process_commands<R>(
        registrar: &mut R,
        bindings: &mut super::PreparedBinding<Binding<R>>,
        control: &super::WindowsControl,
        waker: &Waker,
    ) where
        R: PressRegistrar<Chord = WindowsChord>,
    {
        let commands: Vec<_> = control
            .commands
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .drain(..)
            .collect();
        for command in commands {
            match command {
                super::DictationCommand::Prepare {
                    generation,
                    shortcut,
                } => {
                    // The one alternate id may still belong to an older
                    // candidate. Drop that candidate before reusing the id;
                    // the durable active binding remains reserved.
                    bindings.reject();
                    let current = bindings.active.as_ref().map(|binding| binding.4.as_str());
                    if current == Some(shortcut.as_str()) {
                        bindings.prepare(generation, None);
                        waker.wake(AgentEvent::DictationHotkeyPrepared {
                            generation,
                            requested: shortcut.clone(),
                            active: Some(shortcut),
                            prepared: true,
                        });
                        continue;
                    }
                    let next_id = if bindings
                        .active
                        .as_ref()
                        .is_some_and(|binding| binding.0 == DICTATION_ID)
                    {
                        DICTATION_ALT_ID
                    } else {
                        DICTATION_ID
                    };
                    let replacement = parts(&shortcut).and_then(|(chord, trigger)| {
                        registrar.register(next_id, chord).ok().map(|registration| {
                            (next_id, registration, trigger, chord.key, shortcut.clone())
                        })
                    });
                    let prepared = replacement.is_some();
                    if replacement.is_some() {
                        bindings.prepare(generation, replacement);
                    } else {
                        bindings.reject();
                    }
                    let effective = bindings.effective().map(|binding| binding.4.clone());
                    waker.wake(AgentEvent::DictationHotkeyPrepared {
                        generation,
                        requested: shortcut,
                        active: effective,
                        prepared,
                    });
                }
                super::DictationCommand::Commit { generation } if bindings.commit(generation) => {
                    waker.wake(AgentEvent::DictationHotkeySettled {
                        generation,
                        active: bindings.active.as_ref().map(|binding| binding.4.clone()),
                    });
                }
                super::DictationCommand::Rollback { generation }
                    if bindings.rollback(generation) =>
                {
                    waker.wake(AgentEvent::DictationHotkeySettled {
                        generation,
                        active: bindings.active.as_ref().map(|binding| binding.4.clone()),
                    });
                }
                super::DictationCommand::Commit { .. }
                | super::DictationCommand::Rollback { .. } => {}
            }
        }
    }

    fn foreground_pid() -> ForegroundPid {
        let window = unsafe { GetForegroundWindow() };
        if window.0.is_null() {
            return ForegroundPid::Unavailable;
        }
        let mut pid = 0;
        let thread = unsafe { GetWindowThreadProcessId(window, Some(&mut pid)) };
        if thread == 0 || pid == 0 {
            ForegroundPid::Unavailable
        } else {
            ForegroundPid::Known(pid)
        }
    }

    pub fn register_and_pump(
        preferred: &str,
        waker: Waker,
        control: std::sync::Arc<super::WindowsControl>,
    ) {
        let mut queued = MSG::default();
        let _ = unsafe { PeekMessageW(&mut queued, None, 0, 0, PM_NOREMOVE) };
        control.thread_id.store(
            unsafe { GetCurrentThreadId() },
            std::sync::atomic::Ordering::Release,
        );
        let mut registrar = WindowsPressRegistrar;
        let search = register_action(&mut registrar, Action::Search, preferred, SEARCH_ID, &waker);
        let dictation = register_action(
            &mut registrar,
            Action::Dictation,
            super::DICTATION_CANDIDATES[0],
            DICTATION_ID,
            &waker,
        )
        .map(|(registration, trigger, key, label)| {
            (DICTATION_ID, registration, trigger, key, label)
        });
        let mut dictation = super::PreparedBinding::new(dictation);
        if search.is_none() {
            eprintln!("koru-agent: every global Search shortcut is unavailable");
        }
        if dictation.active.is_none() {
            eprintln!("koru-agent: every global Dictation shortcut is unavailable");
        }
        // Not registered in a build that hides Koru AI (koru-features).
        let ai = parts("Ctrl+Shift+I")
            .filter(|_| koru_features::ai())
            .and_then(|(chord, _)| registrar.register(AI_ID, chord).ok());
        waker.wake(AgentEvent::AiHotkeyRegistered {
            available: ai.is_some(),
        });
        process_commands(&mut registrar, &mut dictation, &control, &waker);
        let mut message = MSG::default();
        while unsafe { GetMessageW(&mut message, None, 0, 0) }.as_bool() {
            if message.message == RECONFIGURE_MESSAGE {
                process_commands(&mut registrar, &mut dictation, &control, &waker);
                continue;
            }
            if message.message == WM_HOTKEY
                && let Some(event) = dispatch(
                    message.wParam.0 as i32,
                    search.as_ref().map(|(_, trigger, _, _)| *trigger),
                    dictation.effective().map(|(id, _, _, key, _)| (*id, *key)),
                    ai.is_some(),
                    foreground_pid(),
                )
            {
                waker.wake(event);
            }
        }
        drop((search, dictation, ai));
    }
}

#[cfg(target_os = "macos")]
mod macos_impl {
    use super::{AgentEvent, Waker};
    use global_hotkey::GlobalHotKeyManager;
    use global_hotkey::hotkey::{Code, HotKey, Modifiers};

    /// Dropping the manager unregisters the chord, so it lives as long as the
    /// process — a field would do, but a local would be a silent no-op.
    static MANAGER: std::sync::OnceLock<Option<GlobalHotKeyManager>> = std::sync::OnceLock::new();

    /// What a Search candidate label registers as, and which physical key the overlay
    /// quarantines. Total over [`super::SEARCH_CANDIDATES`], and `tests` holds it to
    /// that — a candidate added there and not here would silently fall through.
    pub(super) fn parts(label: &str) -> Option<(Modifiers, Code, winit::keyboard::KeyCode)> {
        use winit::keyboard::KeyCode;
        Some(match label {
            "Ctrl+Shift+K" => (
                Modifiers::CONTROL | Modifiers::SHIFT,
                Code::KeyK,
                KeyCode::KeyK,
            ),
            "Ctrl+Alt+Space" => (
                Modifiers::CONTROL | Modifiers::ALT,
                Code::Space,
                KeyCode::Space,
            ),
            "Ctrl+Shift+O" => (
                Modifiers::CONTROL | Modifiers::SHIFT,
                Code::KeyO,
                KeyCode::KeyO,
            ),
            _ => return None,
        })
    }

    /// Register on the winit thread. **No pump, no thread** — Carbon delivers
    /// on the main run loop, and the handler only moves the event across to
    /// the agent's own loop through the waker.
    ///
    /// **The chord is registered literally: this is the one place in Koru
    /// where Ctrl does not become Cmd on a Mac.** Everywhere else
    /// `koru_keys::PRIMARY` makes that translation; here it must not. A
    /// global shortcut has to dodge the *system's* chords rather than match
    /// the app's conventions — ⌘⇧K is far likelier to be taken than ⌃⇧K — and
    /// the preference is a user-visible string, so honouring what it says is
    /// the least surprising thing. Do not "fix" this.
    pub fn register(preferred: &str, waker: Waker) {
        let Some(manager) = MANAGER
            .get_or_init(|| GlobalHotKeyManager::new().ok())
            .as_ref()
        else {
            // macOS refused the manager itself. The tray is the surface that
            // survives a release build; there is no console to explain into.
            koru_ipc::ipc_debug!("hotkey: macOS refused the hotkey manager");
            for action in super::ACTIONS {
                waker.wake(AgentEvent::HotkeyRegistered {
                    action: *action,
                    shortcut: None,
                });
            }
            waker.wake(AgentEvent::AiHotkeyRegistered { available: false });
            return;
        };
        let mut search = None;
        for label in super::candidate_order(super::Action::Search, preferred) {
            let Some((modifiers, code, trigger)) = parts(label) else {
                continue;
            };
            let hotkey = HotKey::new(Some(modifiers), code);
            if manager.register(hotkey).is_err() {
                continue;
            }
            koru_ipc::ipc_debug!("hotkey: registered {label}");
            waker.wake(AgentEvent::HotkeyRegistered {
                action: super::Action::Search,
                shortcut: Some(label.to_string()),
            });
            search = Some((hotkey.id(), trigger));
            break;
        }
        if search.is_none() {
            waker.wake(AgentEvent::HotkeyRegistered {
                action: super::Action::Search,
                shortcut: None,
            });
        }
        waker.wake(AgentEvent::HotkeyRegistered {
            action: super::Action::Dictation,
            shortcut: None,
        });
        let ai = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::KeyI);
        // **Not registered at all in a build that hides Koru AI**
        // (koru-features): the chord stays free for whatever else on the
        // machine wants it, and the tray says it is unavailable.
        let ai_id = koru_features::ai()
            .then(|| manager.register(ai).ok().map(|_| ai.id()))
            .flatten();
        waker.wake(AgentEvent::AiHotkeyRegistered {
            available: ai_id.is_some(),
        });
        global_hotkey::GlobalHotKeyEvent::set_event_handler(Some(
            move |event: global_hotkey::GlobalHotKeyEvent| {
                if event.state != global_hotkey::HotKeyState::Pressed {
                    return;
                }
                if search.is_some_and(|(id, _)| event.id == id) {
                    let trigger = search.expect("matched search hotkey").1;
                    waker.wake(AgentEvent::Hotkey(crate::event::HotkeyPress::Search {
                        trigger,
                    }));
                } else if ai_id.is_some_and(|id| event.id == id) {
                    waker.wake(AgentEvent::AiHotkey);
                }
            },
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Paths;

    /// The preference the fall-back list is built around, and the three it
    /// falls through to. A candidate added to one place and not the other is
    /// how the menu ends up unable to draw the chord that was registered.
    #[test]
    fn the_candidate_list_leads_with_the_default_and_repeats_nothing() {
        assert_eq!(SEARCH_CANDIDATES.first(), Some(&"Ctrl+Shift+K"));
        assert_eq!(DICTATION_CANDIDATES.first(), Some(&"Ctrl+Shift+D"));
        for action in ACTIONS {
            let mut seen = std::collections::HashSet::new();
            for candidate in candidates(*action) {
                assert!(seen.insert(candidate), "{candidate} is listed twice");
            }
        }
        // The shipped default has to be one this agent will actually try, or a
        // person's untouched preference falls straight through to a fall-back.
        assert!(
            SEARCH_CANDIDATES.contains(
                &koru_window::DesktopPreferences::default()
                    .global_shortcut
                    .as_str()
            )
        );
    }

    /// One ordering for both platforms: the preference leads, nothing repeats,
    /// nothing outside the action's candidate table appears, and an unknown preference gets
    /// the shipped order rather than a guess.
    #[test]
    fn the_registration_order_is_the_candidate_list_with_the_preference_first() {
        for action in ACTIONS {
            for preferred in candidates(*action) {
                let order = candidate_order(*action, preferred);
                assert_eq!(order.first(), Some(preferred), "the preference leads");
                let mut sorted = order.clone();
                sorted.sort_unstable();
                let mut expected = candidates(*action).to_vec();
                expected.sort_unstable();
                assert_eq!(sorted, expected, "exactly the candidates, once each");
            }
            assert_eq!(
                candidate_order(*action, "Ctrl+Shift+Elsewhere"),
                candidates(*action).to_vec(),
                "an unrecognised preference falls through to the shipped order"
            );
        }
    }

    #[test]
    fn search_and_dictation_fall_back_independently_and_all_taken_is_explicit() {
        let mut search_calls = Vec::new();
        let search = register_first(Action::Search, SEARCH_CANDIDATES[0], |label| {
            search_calls.push(label);
            (label == SEARCH_CANDIDATES[1]).then_some(label)
        });
        assert_eq!(search.map(|(label, _)| label), Some(SEARCH_CANDIDATES[1]));
        assert_eq!(search_calls, SEARCH_CANDIDATES[..=1]);

        let mut dictation_calls = Vec::new();
        let dictation = register_first(Action::Dictation, DICTATION_CANDIDATES[0], |label| {
            dictation_calls.push(label);
            (label == DICTATION_CANDIDATES[2]).then_some(label)
        });
        assert_eq!(
            dictation.map(|(label, _)| label),
            Some(DICTATION_CANDIDATES[2])
        );
        assert_eq!(dictation_calls, DICTATION_CANDIDATES);

        for action in ACTIONS {
            let mut taken = Vec::new();
            let none = register_first(*action, candidates(*action)[0], |label| {
                taken.push(label);
                None::<()>
            });
            assert!(none.is_none());
            assert_eq!(taken, candidates(*action));
        }
    }

    #[test]
    fn prepared_binding_retains_prior_but_dispatches_candidate_and_settles_by_generation() {
        use std::cell::RefCell;
        use std::rc::Rc;

        struct Guard(u8, Rc<RefCell<Vec<u8>>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.1.borrow_mut().push(self.0);
            }
        }

        let dropped = Rc::new(RefCell::new(Vec::new()));
        let mut state = PreparedBinding::new(Some(Guard(1, Rc::clone(&dropped))));
        state.prepare(7, Some(Guard(2, Rc::clone(&dropped))));
        assert_eq!(state.effective().map(|guard| guard.0), Some(2));
        assert!(dropped.borrow().is_empty(), "the prior remains reserved");
        assert!(!state.commit(6), "a stale receipt cannot settle newer work");
        assert_eq!(state.effective().map(|guard| guard.0), Some(2));
        assert!(state.rollback(7));
        assert_eq!(state.effective().map(|guard| guard.0), Some(1));
        assert_eq!(&*dropped.borrow(), &[2]);

        state.prepare(8, Some(Guard(3, Rc::clone(&dropped))));
        state.prepare(9, Some(Guard(4, Rc::clone(&dropped))));
        assert_eq!(
            &*dropped.borrow(),
            &[2, 3],
            "a rapid newer choice retires only the old candidate"
        );
        assert!(!state.commit(8));
        assert!(state.commit(9));
        assert_eq!(state.effective().map(|guard| guard.0), Some(4));
        assert_eq!(
            &*dropped.borrow(),
            &[2, 3, 1],
            "commit promotes then drops the prior"
        );

        state.prepare(10, None);
        assert_eq!(state.effective().map(|guard| guard.0), Some(4));
        assert!(state.commit(10), "same-chord prepare is a successful no-op");
        state.prepare(11, Some(Guard(5, Rc::clone(&dropped))));
        state.reject();
        assert_eq!(state.effective().map(|guard| guard.0), Some(4));
        assert!(!state.commit(11), "an OS refusal cannot later be committed");
    }

    #[cfg(windows)]
    #[test]
    fn commands_queued_before_thread_ready_are_drained_and_post_failure_removes_only_the_new_one() {
        use std::sync::atomic::{AtomicU32, Ordering};

        let control = WindowsControl {
            commands: std::sync::Mutex::new(std::collections::VecDeque::new()),
            thread_id: AtomicU32::new(0),
        };
        assert!(enqueue_windows_command(
            &control,
            DictationCommand::Commit { generation: 1 },
            |_| panic!("a not-yet-ready pump must not be posted")
        ));
        assert_eq!(control.commands.lock().unwrap().len(), 1);

        control.thread_id.store(7, Ordering::Release);
        assert!(!enqueue_windows_command(
            &control,
            DictationCommand::Rollback { generation: 2 },
            |_| false
        ));
        let mut queued = control.commands.lock().unwrap();
        assert_eq!(queued.len(), 1, "only the failed new command is removed");
        assert!(matches!(
            queued.pop_front(),
            Some(DictationCommand::Commit { generation: 1 })
        ));
    }

    #[cfg(windows)]
    #[test]
    fn pump_commands_replace_candidates_without_dropping_active_and_ignore_stale_settlement() {
        use crate::global_input::{PressError, PressRegistrar};
        use std::cell::RefCell;
        use std::collections::HashSet;
        use std::rc::Rc;
        use std::sync::atomic::AtomicU32;

        #[derive(Clone)]
        struct Registrar(Rc<RefCell<HashSet<i32>>>);
        struct Registration {
            id: i32,
            owned: Rc<RefCell<HashSet<i32>>>,
        }
        impl Drop for Registration {
            fn drop(&mut self) {
                self.owned.borrow_mut().remove(&self.id);
            }
        }
        impl PressRegistrar for Registrar {
            type Chord = crate::global_input::windows::WindowsChord;
            type Registration = Registration;

            fn register(
                &mut self,
                action: i32,
                _chord: Self::Chord,
            ) -> Result<Self::Registration, PressError> {
                if !self.0.borrow_mut().insert(action) {
                    return Err(PressError::Unavailable);
                }
                Ok(Registration {
                    id: action,
                    owned: Rc::clone(&self.0),
                })
            }
        }

        let owned = Rc::new(RefCell::new(HashSet::new()));
        let mut registrar = Registrar(Rc::clone(&owned));
        let (chord, trigger) = super::windows_impl::parts("Ctrl+Shift+D").unwrap();
        let active = registrar
            .register(super::windows_impl::DICTATION_ID, chord)
            .unwrap();
        let mut bindings = PreparedBinding::new(Some((
            super::windows_impl::DICTATION_ID,
            active,
            trigger,
            chord.key,
            "Ctrl+Shift+D".to_owned(),
        )));
        let inner = std::sync::Arc::new(WindowsControl {
            commands: std::sync::Mutex::new(std::collections::VecDeque::new()),
            thread_id: AtomicU32::new(0),
        });
        let control = Control {
            windows: Some(std::sync::Arc::clone(&inner)),
        };
        let (waker, seen) = Waker::collecting();

        assert!(control.prepare_dictation(10, "Ctrl+Alt+B"));
        super::windows_impl::process_commands(&mut registrar, &mut bindings, &inner, &waker);
        assert_eq!(bindings.effective().unwrap().4, "Ctrl+Alt+B");
        assert_eq!(owned.borrow().len(), 2, "active and candidate are reserved");

        assert!(control.prepare_dictation(11, "Ctrl+Shift+C"));
        super::windows_impl::process_commands(&mut registrar, &mut bindings, &inner, &waker);
        assert_eq!(bindings.effective().unwrap().4, "Ctrl+Shift+C");
        assert_eq!(
            owned.borrow().len(),
            2,
            "B was retired before C reused its id"
        );
        assert!(matches!(
            super::windows_impl::dispatch(
                bindings.effective().unwrap().0,
                None,
                Some((
                    bindings.effective().unwrap().0,
                    bindings.effective().unwrap().3
                )),
                false,
                crate::event::ForegroundPid::Known(42),
            ),
            Some(AgentEvent::Hotkey(
                crate::event::HotkeyPress::Dictation { .. }
            ))
        ));

        assert!(control.commit_dictation(10));
        super::windows_impl::process_commands(&mut registrar, &mut bindings, &inner, &waker);
        assert_eq!(bindings.effective().unwrap().4, "Ctrl+Shift+C");
        assert!(control.rollback_dictation(11));
        super::windows_impl::process_commands(&mut registrar, &mut bindings, &inner, &waker);
        assert_eq!(bindings.effective().unwrap().4, "Ctrl+Shift+D");
        assert_eq!(
            owned.borrow().len(),
            1,
            "rollback keeps only the durable active chord"
        );
        assert!(seen.lock().unwrap().iter().any(|event| matches!(
            event,
            AgentEvent::DictationHotkeyPrepared {
                generation: 11,
                prepared: true,
                ..
            }
        )));
    }

    #[cfg(windows)]
    #[test]
    fn one_pump_uses_distinct_search_dictation_and_unchanged_ai_ids() {
        let ids = [
            super::windows_impl::SEARCH_ID,
            super::windows_impl::DICTATION_ID,
            super::windows_impl::AI_ID,
        ];
        let unique: std::collections::HashSet<_> = ids.into_iter().collect();
        assert_eq!(unique.len(), ids.len());
        assert_eq!(super::windows_impl::AI_ID, 0x4b41);
    }

    #[cfg(windows)]
    #[test]
    fn every_windows_candidate_maps_exactly_and_ids_dispatch_typed_events() {
        use crate::event::HotkeyPress;
        use crate::global_input::VirtualKey;
        use winit::keyboard::KeyCode;

        for (label, key, trigger) in [
            ("Ctrl+Shift+K", b'K' as u16, KeyCode::KeyK),
            ("Ctrl+Alt+Space", b' ' as u16, KeyCode::Space),
            ("Ctrl+Shift+O", b'O' as u16, KeyCode::KeyO),
            ("Ctrl+Shift+D", b'D' as u16, KeyCode::KeyD),
            ("Ctrl+Alt+Y", b'Y' as u16, KeyCode::KeyY),
            ("Ctrl+Shift+7", b'7' as u16, KeyCode::Digit7),
            ("Ctrl+Alt+[", 0xDB, KeyCode::BracketLeft),
            ("Ctrl+Alt+]", 0xDD, KeyCode::BracketRight),
            ("Ctrl+Alt+\\", 0xDC, KeyCode::Backslash),
            ("Ctrl+Alt+/", 0xBF, KeyCode::Slash),
        ] {
            let (chord, physical) = super::windows_impl::parts(label).unwrap();
            assert_eq!(chord.key.raw(), key, "{label}");
            assert_eq!(physical, trigger, "{label}");
        }

        let search = KeyCode::KeyO;
        let dictation = VirtualKey::new(b'M' as u16);
        let foreground = crate::event::ForegroundPid::Known(42);
        assert!(matches!(
            super::windows_impl::dispatch(
                super::windows_impl::SEARCH_ID,
                Some(search),
                Some((super::windows_impl::DICTATION_ID, dictation)),
                true,
                foreground,
            ),
            Some(AgentEvent::Hotkey(HotkeyPress::Search {
                trigger: KeyCode::KeyO
            }))
        ));
        assert!(matches!(
            super::windows_impl::dispatch(
                super::windows_impl::DICTATION_ID,
                Some(search),
                Some((super::windows_impl::DICTATION_ID, dictation)),
                true,
                foreground,
            ),
            Some(AgentEvent::Hotkey(HotkeyPress::Dictation {
                key,
                foreground: seen,
            })) if key == dictation && seen == foreground
        ));
        assert!(matches!(
            super::windows_impl::dispatch(
                super::windows_impl::AI_ID,
                Some(search),
                Some((super::windows_impl::DICTATION_ID, dictation)),
                true,
                foreground,
            ),
            Some(AgentEvent::AiHotkey)
        ));
        assert!(
            super::windows_impl::dispatch(
                123,
                Some(search),
                Some((super::windows_impl::DICTATION_ID, dictation)),
                true,
                foreground,
            )
            .is_none()
        );
    }

    #[cfg(windows)]
    #[test]
    fn every_settings_valid_picker_chord_maps_in_the_windows_registrar() {
        for modifier in koru_settings::DictationShortcut::WINDOWS_MODIFIERS {
            for key in koru_settings::DictationShortcut::KEYS {
                let shortcut =
                    koru_settings::DictationShortcut::try_new(format!("{modifier}+{key}")).unwrap();
                assert!(
                    super::windows_impl::parts(shortcut.as_str()).is_some(),
                    "{} passed settings validation but not Agent parsing",
                    shortcut.as_str()
                );
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_key_pairs_reach_register_hotkey_as_win_modifiers() {
        use windows::Win32::UI::Input::KeyboardAndMouse::{MOD_CONTROL, MOD_SHIFT, MOD_WIN};

        let (ctrl_win, _) = super::windows_impl::parts("Ctrl+Win+Y").unwrap();
        let (shift_win, _) = super::windows_impl::parts("Shift+Win+/").unwrap();
        assert_eq!(ctrl_win.modifiers, MOD_CONTROL | MOD_WIN);
        assert_eq!(shift_win.modifiers, MOD_SHIFT | MOD_WIN);
    }

    /// Every candidate the order can yield is one the platform table can
    /// register — a label added to [`CANDIDATES`] and not to `parts` would
    /// silently fall through to the next chord.
    #[cfg(target_os = "macos")]
    #[test]
    fn every_candidate_registers_on_macos() {
        for label in SEARCH_CANDIDATES {
            assert!(
                super::macos_impl::parts(label).is_some(),
                "{label} is a candidate the macOS table cannot register"
            );
        }
    }

    /// A non-owning launch says so rather than staying silent.
    #[test]
    fn a_headless_root_reports_that_it_has_no_shortcut() {
        let (waker, seen) = Waker::collecting();
        start(
            &Paths::for_root("/tmp/koru-agent-hotkey".into()),
            false,
            waker,
        );
        let seen = seen.lock().unwrap();
        for action in ACTIONS {
            assert_eq!(
                seen.iter()
                    .filter(|event| matches!(
                        event,
                        AgentEvent::HotkeyRegistered {
                            action: seen_action,
                            shortcut: None
                        } if seen_action == action
                    ))
                    .count(),
                1,
                "a headless rooted agent reports each named action absent exactly once"
            );
        }
        assert_eq!(
            seen.iter()
                .filter(|event| matches!(
                    event,
                    AgentEvent::AiHotkeyRegistered { available: false }
                ))
                .count(),
            1
        );
    }

    #[test]
    fn a_headless_root_registers_nothing() {
        // **The whole reason this is gated.** `RegisterHotKey` is machine-wide,
        // so a gate running several agents would take Ctrl+Shift+K off the
        // person running the suite — and they would find out by pressing it.
        //
        // Reaching the real registration is what this asserts *cannot* happen:
        // `start` returns before any thread is spawned. It used to assert that
        // *nothing at all* was raised, which was the same thing right up until
        // the menu started being told there is no shortcut — so the assertion
        // is now about the one event that may be raised rather than about
        // silence, which was only ever a proxy for it.
        let (waker, seen) = Waker::collecting();
        start(
            &Paths::for_root("/tmp/koru-agent-hotkey".into()),
            false,
            waker,
        );
        let seen = seen.lock().unwrap();
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event, AgentEvent::Hotkey(..))),
            "a headless rooted agent never registers, so it can never fire"
        );
        assert_eq!(
            seen.len(),
            3,
            "and it reports both named actions plus AI on the way out"
        );
    }

    #[test]
    fn owner_thread_spawn_failure_reports_every_shortcut_unavailable() {
        let (waker, seen) = Waker::collecting();
        assert!(report_spawn(Err::<(), ()>(()), &waker).is_none());
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 3);
        for action in ACTIONS {
            assert!(seen.iter().any(|event| matches!(
                event,
                AgentEvent::HotkeyRegistered {
                    action: seen_action,
                    shortcut: None,
                } if seen_action == action
            )));
        }
        assert!(
            seen.iter()
                .any(|event| matches!(event, AgentEvent::AiHotkeyRegistered { available: false }))
        );
    }
}
