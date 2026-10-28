// SPDX-License-Identifier: MPL-2.0

//! One event loop, many windows.
//!
//! Generic over [`Host`]: this file decides *when* something is drawn, resized,
//! saved or opened, and the host decides *what* a window shows. Nothing here
//! knows what a row is.

mod delegate_folder;
mod inspector_window;
mod key_release;
mod modifiers;
mod profile_removal;
mod recording;
mod trash;
mod workspace_arrival;
mod zoom;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vello::kurbo::Affine;
use vello::util::{RenderContext, RenderSurface};
use vello::wgpu;
use vello::{AaConfig, Renderer, Scene};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, ModifiersState, NamedKey, PhysicalKey};
use winit::monitor::MonitorHandle;
use winit::window::{CursorIcon, Icon, ResizeDirection, Theme, Window, WindowId, WindowLevel};

use koru_dictation::{Dictation, Interruption};

use crate::cli;
use crate::profile_runtime::{
    ProfilePersistence, ProfileRuntime, StoreBlobSink, StoreBlobSource, StoreCatalogRefresh,
};
use crate::settings::{
    PreIdentityPreferences, PreIdentitySettings, Settings, Written, restorable_position,
    usable_window_position,
};

use crate::startup;
use crate::{
    ErrorAction, ErrorReport, Host, MoveSelectionPayload, PaneRequest, PasteTarget, PastedImage,
    ReminderStartupRequest, ReminderStartupRequestId, ReminderStartupResult, RichBlock,
    SearchTargetDisposition, View, ZoomIntent,
};
use crate::{SyncState, WorkspaceStat, export, reveal};
use koru_core::node::BlobHash;
use koru_core::timing;
use koru_core::{NodeId, WorkspaceId};
use koru_draw::ImageCache;
use koru_draw::theme;
use koru_ui::keys::debug as inputdebug;
use koru_window::{
    WindowRequest, desktop_preferences, frame, framestats, hairline, resize, snap, system_theme,
};
use koru_workspace::Workspace;
use koru_workspace::session;
#[cfg(test)]
use modifiers::reconcile_physical_modifiers;
use modifiers::{current_modifier_state, modifiers_after_key_release, modifiers_with_pressed_keys};
use profile_removal::{
    HostedProfileRemovalSource, ProfileCascadeCompletion, ProfileOfflineCompletion,
    ProfileRemovalCleanup,
};

/// Window chrome theme from the preference. `None` follows the OS and is what
/// lets winit deliver `ThemeChanged` on Windows (a `Some` preferred theme
/// suppresses that event).
fn window_theme_for_pref() -> Option<Theme> {
    match theme::theme_preference() {
        theme::ThemePreference::System => None,
        theme::ThemePreference::Light => Some(Theme::Light),
        theme::ThemePreference::Dark => Some(Theme::Dark),
    }
}

fn centered_window_position(
    origin: winit::dpi::PhysicalPosition<i32>,
    monitor_size: winit::dpi::PhysicalSize<u32>,
    window_size: winit::dpi::PhysicalSize<u32>,
) -> winit::dpi::PhysicalPosition<i32> {
    let x =
        i64::from(origin.x) + (i64::from(monitor_size.width) - i64::from(window_size.width)) / 2;
    let y =
        i64::from(origin.y) + (i64::from(monitor_size.height) - i64::from(window_size.height)) / 2;
    winit::dpi::PhysicalPosition::new(
        x.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        y.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
    )
}

fn center_window(window: &Window, preferred_monitor: Option<MonitorHandle>) {
    if let Some(monitor) = preferred_monitor.or_else(|| window.current_monitor()) {
        window.set_outer_position(centered_window_position(
            monitor.position(),
            monitor.size(),
            window.outer_size(),
        ));
    }
}

/// The point `global` in a window's **view space** — logical and zoom-divided,
/// the space views are hit-tested in — or `None` when it is not over the
/// window at all. Pure arithmetic beside the cross-window drop broker, so a
/// test can hold it: windows come and go, the math does not.
fn window_view_point(
    origin: winit::dpi::PhysicalPosition<i32>,
    size: winit::dpi::PhysicalSize<u32>,
    scale: f64,
    zoom: f64,
    global: (f64, f64),
) -> Option<(f32, f32)> {
    let x = global.0 - f64::from(origin.x);
    let y = global.1 - f64::from(origin.y);
    if x < 0.0 || y < 0.0 || x >= f64::from(size.width) || y >= f64::from(size.height) {
        return None;
    }
    Some(((x / scale / zoom) as f32, (y / scale / zoom) as f32))
}

fn remembered_search_height(physical_height: u32, scale_factor: f64) -> u32 {
    ((f64::from(physical_height) / scale_factor.max(0.01)).round() as u32).clamp(
        desktop_preferences::GLOBAL_SEARCH_MIN_HEIGHT,
        desktop_preferences::GLOBAL_SEARCH_MAX_HEIGHT,
    )
}

/// How tall the ephemeral search window opens.
///
/// **Read out of the `global-search` row, which the Agent has always owned.**
/// It lived in `desktop-v1.json` until SP-6b, beside preferences it is not one
/// of: the height is geometry, and it was only ever in that file by adjacency
/// (ADR-266). The Agent's overlay and this window are two surfaces onto one
/// search, so they remember one height rather than two.
fn search_window_height(geometry: &koru_window::SearchGeometry) -> f32 {
    geometry
        .height()
        .unwrap_or(desktop_preferences::GLOBAL_SEARCH_DEFAULT_HEIGHT as f32)
        .clamp(
            desktop_preferences::GLOBAL_SEARCH_MIN_HEIGHT as f32,
            desktop_preferences::GLOBAL_SEARCH_MAX_HEIGHT as f32,
        )
}

/// Compose the OS-visible title used by the taskbar and Alt+Tab.
///
/// Profile is the useful distinction during ordinary use; a named development
/// replica remains visible as the final component. An empty recovered profile
/// name is omitted rather than drawing an unexplained blank segment.
fn profiled_window_title(subject: &str, profile: Option<&str>, slot: &str) -> String {
    let mut parts = vec![subject.to_owned()];
    if let Some(profile) = profile.map(str::trim).filter(|profile| !profile.is_empty()) {
        parts.push(profile.to_owned());
    }
    if slot != session::DEFAULT_PEER {
        parts.push(slot.to_owned());
    }
    parts.join(" — ")
}

fn trace_node(id: Option<NodeId>) -> String {
    id.map(|id| id.to_storage()).unwrap_or_else(|| "-".into())
}

/// Reopen the documents the remembered tabs are over, before any view is asked
/// to rebuild them.
///
/// **A tab is a view of a document, and a view cannot fetch one.** Under the
/// store an unopened document is bytes in a database; `View::restore` is handed
/// a `&Workspace` and finds each with `Workspace::document`, which answers only
/// for what is already open. So the shell opens them here, and finishes on the
/// one that was showing — `open_document` makes its argument the active one, and
/// the active document is what the window title and the sidebar highlight are
/// about.
///
/// A document that will not open is **skipped, not fatal**: one binned by a peer
/// while this window was shut should cost that tab and nothing else, and
/// `restore_session` drops a tab whose document is not there.
///
/// This is one snapshot fetch per remembered tab, which is why `reopen
/// documents` is its own span in `KORU_STARTUP_STATS`. Deferring them would mean
/// a strip of tabs that draw nothing until each is clicked, which is a worse
/// answer than a slower start.
fn reopen_remembered_documents(workspace: &mut koru_workspace::Workspace, settings: &Settings) {
    if !workspace.on_store() {
        return;
    }
    // The active tab's document goes last, so it ends up active.
    let showing = settings
        .tabs
        .get(settings.tab)
        .and_then(|(w, _)| w.document_id());
    // Whatever the open landed on, which is the workspace's *first* document
    // and not necessarily one of these.
    let landed = workspace.active();
    let span = timing::Span::start("reopen documents");
    let mut wanted = Vec::new();
    let showing_pin = settings
        .pinned_tab
        .and_then(|i| settings.pinned_tabs.get(i).map(|p| p.document));
    let current = settings.workspace;
    for pin in &settings.pinned_tabs {
        wanted.push(pin.document);
        if Some(pin.document) != showing_pin {
            let _ = workspace.open_document_in(pin.workspace, pin.document);
        }
    }
    for (which, _) in &settings.tabs {
        if let crate::TabRef::Document {
            id,
            workspace: home,
        } = which
        {
            wanted.push(*id);
            if Some(*id) != showing
                && let Some(home) = home.or(current)
            {
                let _ = workspace.open_document_in(home, *id);
            }
        }
    }
    if let Some(id) = showing_pin {
        if let Some(pin) = settings.pinned_tabs.iter().find(|p| p.document == id) {
            let _ = workspace.open_document_in(pin.workspace, id);
        }
    } else if let Some(id) = showing {
        let home = settings
            .tabs
            .get(settings.tab)
            .and_then(|(w, _)| match w {
                crate::TabRef::Document { workspace, .. } => *workspace,
                _ => None,
            })
            .or(current);
        if let Some(home) = home {
            let _ = workspace.open_document_in(home, id);
        }
    }
    // A document nothing is showing does not stay open. It is only reachable
    // here — no tab will ever ask for it, so no `CloseDocument` would ever come.
    if let Some(landed) = landed.filter(|id| !wanted.contains(id)) {
        workspace.close_document(landed);
    }
    span.stop();
}

fn trace_profile(id: Option<koru_core::ProfileId>) -> String {
    id.map(|id| id.to_storage()).unwrap_or_else(|| "-".into())
}

/// Resolve paint dark/light from the preference (+ OS when System).
fn effective_dark(pref: theme::ThemePreference) -> bool {
    let system_dark = system_theme::system_is_dark();
    theme::set_system_dark(system_dark);
    match pref {
        theme::ThemePreference::System => system_dark,
        theme::ThemePreference::Light => false,
        theme::ThemePreference::Dark => true,
    }
}

/// **Push every preference into the process globals the views read.**
///
/// One function rather than a block inside `App::new`, because it is called
/// twice and the second time is the whole point: a window opens on the
/// registry's defaults and the authoritative section arrives afterwards, so
/// the correction has to travel exactly the path a settings change already
/// takes — the globals, then `apply_theme_preference` to repaint.
///
/// `capture_preferences` is the inverse. A global written here and not
/// read back there is a setting the next poll will quietly reset.
///
/// The theme has to be set before the first layout is built, since brushes are
/// baked into parley layouts.
fn apply_settings(settings: &Settings) {
    theme::set_theme_preference(settings.theme);
    theme::set_dark(effective_dark(settings.theme));
    theme::set_caret_color(settings.caret_color);
    theme::set_caret_width(settings.caret_width);
    theme::set_caret_height(settings.caret_height);
    theme::set_ui_font(settings.font);
    theme::set_body_font(settings.body_font);
    theme::set_headings_font(settings.headings_font);
    theme::set_code_font(settings.code_font);
    theme::set_density(settings.density);
    theme::set_zoom(clamp_zoom(settings.zoom));
    theme::set_default_view_mode(settings.view_mode);
    theme::set_default_canvas_bg(settings.canvas_bg);
    // **The four dictation preferences, seeded the same way.** They are
    // process globals over `Settings` for the reason the type roles are: two
    // windows' Settings tabs are two views of one preference, and a view
    // cannot reach `Settings`. See `crate::voice_prefs`.
    crate::voice_prefs::set_language(settings.dictation_language);
    crate::voice_prefs::set_tier(settings.dictation_tier);
    crate::voice_prefs::set_toggle(settings.dictation_toggle);
    crate::voice_prefs::set_input(settings.dictation_input.clone());
    // The two spelling preferences travel the same road (spell-check plan P5).
    crate::spelling::set_enabled(settings.spellcheck_enabled);
    crate::spelling::set_preferred(settings.spellcheck_language);
}

fn apply_pre_identity_preferences(settings: &PreIdentityPreferences) {
    theme::set_theme_preference(settings.theme);
    theme::set_dark(effective_dark(settings.theme));
    theme::set_caret_color(settings.caret_color);
    theme::set_caret_width(settings.caret_width);
    theme::set_caret_height(settings.caret_height);
    theme::set_ui_font(settings.font);
    theme::set_body_font(settings.body_font);
    theme::set_headings_font(settings.headings_font);
    theme::set_code_font(settings.code_font);
    theme::set_density(settings.density);
    theme::set_zoom(clamp_zoom(settings.zoom));
    theme::set_default_view_mode(settings.view_mode);
    theme::set_default_canvas_bg(settings.canvas_bg);
    crate::voice_prefs::set_language(settings.dictation_language);
    crate::voice_prefs::set_tier(settings.dictation_tier);
    crate::voice_prefs::set_toggle(settings.dictation_toggle);
    crate::voice_prefs::set_input(settings.dictation_input.clone());
    crate::spelling::set_enabled(settings.spellcheck_enabled);
    crate::spelling::set_preferred(settings.spellcheck_language);
}
fn theme_debug_startup() {
    let p = system_theme::probe();
    system_theme::debug_log(format!(
        "startup pref={:?} paint_will_be_dark={} probe={{apps_dark={}, apps_use_light={:?}, system_use_light={:?}, source={}}}",
        theme::theme_preference(),
        effective_dark(theme::theme_preference()),
        p.apps_dark,
        p.apps_use_light,
        p.system_use_light,
        p.source,
    ));
}

/// The shared product mark used by the taskbar, dock, and window switcher.
///
/// The PNG is embedded so installed builds do not depend on the repository's
/// `assets/` directory being present beside the executable. Tinted for the
/// channel so a nightly window in the taskbar looks nightly — the same recipe
/// the embedded `.ico` went through in `scripts/icons/set-logo.py`.
fn app_icon() -> Option<Icon> {
    let rgba = image::load_from_memory(include_bytes!("../../../assets/logo.png"))
        .ok()?
        .to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut pixels = rgba.into_raw();
    koru_channel::tint::apply(&mut pixels, koru_channel::tint::tint());
    Icon::from_rgba(pixels, width, height).ok()
}

/// Hide or unhide the whole application, macOS's own way.
///
/// **Hiding each window is not hiding the app.** `set_visible(false)` per
/// window leaves AppKit believing Koru still has windows, so a Dock click has
/// nothing to restore and does nothing at all — while *minimising* a window
/// works, because that is a state AppKit owns. That is exactly the difference
/// the bug report described.
///
/// `NSApplication::hide:` is the state ⌘H puts an app in, and macOS restores it
/// from the Dock, from ⌘-Tab and from the app switcher **without the
/// application being asked** — no `applicationShouldHandleReopen:`, which winit
/// owns and does not forward. So this replaces a poll that could not work:
/// an app with no visible windows may stay frontmost indefinitely, so watching
/// for it to "become active" never fired.
///
/// The agent's `Restore` still ends in [`App::restore_from_tray`], which
/// unhides explicitly — both routes back now end in the same place.
#[cfg(target_os = "macos")]
fn hide_application(hidden: bool) {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    if hidden {
        app.hide(None);
    } else {
        app.unhide(None);
    }
}

#[cfg(not(target_os = "macos"))]
fn hide_application(_hidden: bool) {}

/// Whether the Agent has a notification-area icon to come back from.
///
/// Linux has none yet (the Agent's tray is Windows and macOS only), and a
/// window hidden there is a process nobody can reach — the case
/// `WindowRequest::HideToTray` already says must fall back to closing. So on
/// Linux every "go to the tray" is a close.
const HAS_NOTIFICATION_AREA: bool = cfg!(any(windows, target_os = "macos"));

/// Has macOS brought the application back without asking us?
///
/// A Dock click, ⌘-Tab or the app switcher all unhide directly — none of them
/// is an event winit forwards. So the one piece of state this shell sets
/// (`hide:`) is also the one it can read back, and a mismatch means the OS
/// changed it. Without this `in_tray` would stay true over a window the user is
/// already looking at — and the agent would keep offering to restore it.
#[cfg(target_os = "macos")]
fn application_was_unhidden() -> bool {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    !NSApplication::sharedApplication(mtm).isHidden()
}

#[cfg(not(target_os = "macos"))]
fn application_was_unhidden() -> bool {
    false
}

// Moved to `koru-window` (its stated home: the window and its platform
// traps), because the agent's overlay needs the same call and may not link
// this crate. The doc — why `focus_window` cannot substitute — moved with it.
use koru_window::activate_app;

// Moved to `koru-window` (with `activate_app`, for the same reason): the
// agent's overlay window puts a second binary in the Dock, and without this
// call it wears the generic black `exec` tile. The doc — why winit's window
// icon cannot substitute — moved with it.
use koru_window::set_dock_icon;

/// Decode image bytes into a peniko brush, its pixel size, and a mime string.
/// `None` if the bytes are not a recognised image.
pub(crate) fn decode_image(bytes: &[u8]) -> Option<(peniko::ImageBrush, u32, u32, String)> {
    let mime = match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => "image/png",
        image::ImageFormat::Jpeg => "image/jpeg",
        image::ImageFormat::Gif => "image/gif",
        image::ImageFormat::WebP => "image/webp",
        image::ImageFormat::Bmp => "image/bmp",
        image::ImageFormat::Tiff => "image/tiff",
        _ => "application/octet-stream",
    };
    let rgba = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = rgba.dimensions();
    let data = peniko::ImageData {
        data: peniko::Blob::new(Arc::new(rgba.into_raw())),
        format: peniko::ImageFormat::Rgba8,
        alpha_type: peniko::ImageAlphaType::Alpha,
        width: w,
        height: h,
    };
    Some((peniko::ImageBrush::new(data), w, h, mime.to_string()))
}

fn clipboard_debug_on() -> bool {
    koru_env::switch(koru_env::name::KORU_CLIPBOARD_DEBUG)
}

/// Clipboard diagnostic. Off unless
/// `KORU_CLIPBOARD_DEBUG` is truthy (`1`/`true`; not `0`/`false`/empty); writes
/// redacted event metadata to stderr and `%TEMP%\koru-clipboard-debug.log`.
/// Clipboard contents are never included.
fn clip_dbg(line: impl AsRef<str>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static ANNOUNCED: AtomicBool = AtomicBool::new(false);
    if !clipboard_debug_on() {
        return;
    }
    let line = line.as_ref();
    eprintln!("[koru:clip] {line}");
    let path = std::env::temp_dir().join("koru-clipboard-debug.log");
    if !ANNOUNCED.swap(true, Ordering::Relaxed) {
        eprintln!("[koru:clip] (also logging to {})", path.display());
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
    }
}

/// The native file picker, restricted to image extensions.
fn pick_image_file() -> Option<PathBuf> {
    crate::dialogs::pick_open_file(
        "Choose an image",
        &[crate::dialogs::FileFilter {
            name: "Images",
            extensions: &["png", "jpg", "jpeg", "gif", "webp", "bmp", "tif", "tiff"],
        }],
        None,
        None,
    )
}

/// What to call this event in a stall line.
///
/// **A category, never the content.** The point of `KORU_JANK` is a log that
/// can be attached to a bug report, so a keystroke is `key` whatever letter it
/// was — the pressed/released split is kept because a stall on release is a
/// different bug from a stall on press, and neither says what was typed.
///
/// `&'static str` rather than a formatted name because [`timing::Watch`] holds
/// one and the whole module is built to allocate nothing while switched off.
fn stall_name(event: &WindowEvent) -> &'static str {
    match event {
        WindowEvent::KeyboardInput { event, .. } => match event.state {
            ElementState::Pressed => "key press",
            ElementState::Released => "key release",
        },
        WindowEvent::MouseInput { state, .. } => match state {
            ElementState::Pressed => "mouse press",
            ElementState::Released => "mouse release",
        },
        WindowEvent::CursorMoved { .. } => "pointer move",
        WindowEvent::MouseWheel { .. } => "scroll",
        WindowEvent::RedrawRequested => "redraw",
        WindowEvent::Resized(_) => "resize",
        WindowEvent::ScaleFactorChanged { .. } => "dpi change",
        WindowEvent::Focused(_) => "focus",
        WindowEvent::ThemeChanged(_) => "theme change",
        WindowEvent::DroppedFile(_) => "file drop",
        WindowEvent::CloseRequested => "close",
        WindowEvent::Ime(_) => "ime",
        WindowEvent::ModifiersChanged(_) => "modifiers",
        _ => "window event",
    }
}

/// Ctrl+letter arrives as an ASCII control code on Windows — Ctrl+N is `0x0E`,
/// not `Character("n")`. Both forms are normalised so the two window-level
/// chords work on every platform.
///
/// **This was a second copy**, with a comment saying the crate could not borrow
/// the editor's without depending on the editor. That was true, and stopped
/// being true when `ctrl_letter` moved to `koru-ui` — a widget crate this one
/// may link — so the copy is gone and every chord table in the tree now matches
/// through one function. The copy had also drifted: it read `text` first and
/// knew nothing about punctuation, so a press reported *only* as
/// `Character("\u{0b}")` with no text answered `'\u{0b}'` rather than `'k'`.
pub(crate) use koru_ui::keys::ctrl_letter;

/// What `Ctrl+Alt+digit` arrives as on Windows when the layout's AltGr table
/// has no entry for that digit — the US one included: `ToUnicodeEx` has
/// nothing to say about the combination, so winit reports `Key::Unidentified`
/// with no text, and a chord written on the logical key (`Ctrl+Alt+1` is a
/// document's Heading 1) never matches. The physical key still names the
/// digit, so the logical one is rebuilt from it. Pure so it stays testable;
/// the one line that reads it off the `KeyEvent` is not — the same trade the
/// Ctrl+Tab arm below makes.
fn physical_digit(physical: PhysicalKey) -> Option<Key> {
    let digit = match physical {
        PhysicalKey::Code(KeyCode::Digit0) => "0",
        PhysicalKey::Code(KeyCode::Digit1) => "1",
        PhysicalKey::Code(KeyCode::Digit2) => "2",
        PhysicalKey::Code(KeyCode::Digit3) => "3",
        PhysicalKey::Code(KeyCode::Digit4) => "4",
        PhysicalKey::Code(KeyCode::Digit5) => "5",
        PhysicalKey::Code(KeyCode::Digit6) => "6",
        PhysicalKey::Code(KeyCode::Digit7) => "7",
        PhysicalKey::Code(KeyCode::Digit8) => "8",
        PhysicalKey::Code(KeyCode::Digit9) => "9",
        _ => return None,
    };
    Some(Key::Character(digit.into()))
}

/// The view chords' digit, from the physical key, on a Mac: ⌃⌘1–6
/// (`koru_keys::VIEW`). With ⌃ held AppKit may report a control character
/// or a layout's own symbol rather than the digit, and the number row's
/// physical keys are the identity the chord means on every layout. Off macOS
/// the view modifier is Alt alone, whose digit arrives intact.
fn view_digit(physical: PhysicalKey, mods: ModifiersState) -> Option<Key> {
    (cfg!(target_os = "macos") && mods == koru_keys::VIEW).then_some(())?;
    physical_digit(physical)
}

fn alt_digit(logical: &Key, physical: PhysicalKey) -> Option<Key> {
    let Key::Unidentified(_) = logical else {
        return None;
    };
    physical_digit(physical)
}

/// The same repair for `Ctrl+Alt+`**letter**, which the container binds once —
/// `Ctrl+Alt+W` opens workspaces and `Ctrl+Alt+P` opens profiles.
///
/// **This is the reason a letter can be bound there at all.** Every previous
/// attempt at a `Ctrl+Alt+letter` chord in this tree simply did not fire and was
/// moved to `Ctrl+Shift` (see `page_menu::WIDTHS`); the digits worked only
/// because [`alt_digit`] was rebuilding them. Rebuilt **only** over
/// `Key::Unidentified`, so a layout whose AltGr table does name the key keeps
/// its own character — the chord does not fire there, and taking a real
/// character away to make it would be the worse trade.
fn alt_letter(logical: &Key, physical: PhysicalKey) -> Option<Key> {
    let Key::Unidentified(_) = logical else {
        return None;
    };
    let letter = match physical {
        PhysicalKey::Code(KeyCode::KeyW) => "w",
        PhysicalKey::Code(KeyCode::KeyP) => "p",
        // The context column's own two: step its sections, and copy what the
        // Cloud tab is showing.
        PhysicalKey::Code(KeyCode::KeyJ) => "j",
        PhysicalKey::Code(KeyCode::KeyK) => "k",
        _ => return None,
    };
    Some(Key::Character(letter.into()))
}

/// `Ctrl+Shift+7/8` names list commands by the number-row keys, but the logical
/// key is commonly `&`/`*` after Shift has been applied (and varies by layout).
/// At command time the physical digit is the stable identity, just as it is for
/// the unidentified Ctrl+Alt heading digits repaired by [`alt_digit`].
fn shifted_digit(physical: PhysicalKey, mods: ModifiersState) -> Option<Key> {
    (mods.control_key() || mods.super_key())
        .then_some(())
        .filter(|_| mods.shift_key() && !mods.alt_key())?;
    physical_digit(physical)
}

/// Recover editing keys from their physical identity while a command modifier
/// is held. Windows can expose Ctrl+Backspace as a control character instead
/// of `NamedKey::Backspace`; a one-line field cannot infer the intended edit
/// from that character alone. The shell still has the physical key, so it
/// restores the unambiguous logical key before any view sees the event.
fn modified_editing_key(physical: PhysicalKey, mods: ModifiersState) -> Option<Key> {
    if !mods.control_key() && !mods.super_key() {
        return None;
    }
    match physical {
        PhysicalKey::Code(KeyCode::Backspace) => Some(Key::Named(NamedKey::Backspace)),
        PhysicalKey::Code(KeyCode::Delete) => Some(Key::Named(NamedKey::Delete)),
        _ => None,
    }
}

/// Whether a key press arriving at a window may be dispatched at all.
///
/// A window that is not focused has no keyboard, so a press delivered to one
/// is the trailing edge of an OS window-switching chord: Windows hands
/// Alt+Tab's Tab to the window that just *lost* focus, after the focus event
/// has cleared the modifier bookkeeping — dispatched, it reaches the editor
/// as a bare Tab and indents the paragraph under the caret. An unknown window
/// (`None`) is not gated here; the dispatch path already returns for it.
fn drop_unfocused_key_press(pane_focused: Option<bool>) -> bool {
    pane_focused == Some(false)
}

/// Ctrl/Shift/Alt/Super: holding them auto-repeats, and a repeat is not a new
/// chord. Dispatching each one is how a Ctrl+click's held Ctrl flooded the
/// log and kept `down_keys` from noticing the key had already come up.
fn is_modifier_key(physical: PhysicalKey, logical: &Key) -> bool {
    matches!(
        physical,
        PhysicalKey::Code(
            KeyCode::ControlLeft
                | KeyCode::ControlRight
                | KeyCode::ShiftLeft
                | KeyCode::ShiftRight
                | KeyCode::AltLeft
                | KeyCode::AltRight
                | KeyCode::SuperLeft
                | KeyCode::SuperRight
        )
    ) || matches!(
        logical,
        Key::Named(
            NamedKey::Control
                | NamedKey::Shift
                | NamedKey::Alt
                | NamedKey::Super
                | NamedKey::Meta
                | NamedKey::Hyper
        )
    )
}

/// `/dictate` has no physical release, so its unmodified Enter is a finish
/// gesture. Kept as a value-level decision so the input loop and its tests do
/// not grow two descriptions of which key is consumed.
fn finishes_commanded_dictation(commanded: bool, key: &Key, mods: ModifiersState) -> bool {
    commanded && mods.is_empty() && matches!(key, Key::Named(NamedKey::Enter))
}

/// Snap a zoom factor to the 0.1 grid and clamp to the bounds. Snapping stops
/// float drift accumulating across many in/out cycles, so the indicator keeps
/// reading 120% rather than drifting to 119.99999%.
/// A store client on this channel, at `root` when one was named.
///
/// Free rather than a method because three worker threads need it and cannot
/// borrow `self` — the import inspect, the import build and the export each
/// clone the root out of the app before spawning. See [`App::store_client`] for
/// why a bare `Client::open` must not appear beside these.
fn client_at(
    root: Option<&std::path::Path>,
) -> Result<koru_api_client::Client, koru_store_core::errors::Error> {
    client_at_segment(koru_workspace::fromstore::segment(), root)
}

/// [`client_at`] for a caller that already owns the segment string — the export
/// thread, which captures it rather than calling into `fromstore` off the main
/// thread.
fn client_at_segment(
    segment: &str,
    root: Option<&std::path::Path>,
) -> Result<koru_api_client::Client, koru_store_core::errors::Error> {
    match root {
        Some(root) => koru_api_client::Client::open_at(segment, root),
        None => koru_api_client::Client::open(segment),
    }
}

/// Lazily-opened system-document destination for an import worker. Keeping the
/// session behind the format-neutral `TagSink` means the Evernote crate never
/// sees Store or UI types, while accepted source commits still reuse the same
/// replay/canonicalization path as an ordinary workspace.
struct StoreTagSink {
    segment: String,
    root: Option<PathBuf>,
    workspace: koru_store_core::ids::WorkspaceId,
    session: std::sync::Mutex<Option<koru_workspace::system::ImportSystemSession>>,
}

impl StoreTagSink {
    fn new(
        segment: String,
        root: Option<PathBuf>,
        workspace: koru_store_core::ids::WorkspaceId,
    ) -> Self {
        Self {
            segment,
            root,
            workspace,
            session: std::sync::Mutex::new(None),
        }
    }
}

impl koru_import::TagSink for StoreTagSink {
    fn snapshot(&self) -> Result<Option<Vec<u8>>, koru_import::ImportError> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| koru_import::ImportError::new("the import tag session stopped"))?;
        if session.is_none() {
            *session = Some(
                koru_workspace::system::ImportSystemSession::open(
                    &self.segment,
                    self.root.as_deref(),
                    self.workspace,
                )
                .map_err(koru_import::ImportError::new)?,
            );
        }
        session
            .as_ref()
            .expect("the import system session was opened")
            .snapshot()
            .map_err(koru_import::ImportError::new)
    }

    fn publish(
        &self,
        snapshot: &[u8],
        register_system: bool,
    ) -> Result<(), koru_import::ImportError> {
        let mut session = self
            .session
            .lock()
            .map_err(|_| koru_import::ImportError::new("the import tag session stopped"))?;
        if session.is_none() {
            *session = Some(
                koru_workspace::system::ImportSystemSession::open(
                    &self.segment,
                    self.root.as_deref(),
                    self.workspace,
                )
                .map_err(koru_import::ImportError::new)?,
            );
        }
        session
            .as_mut()
            .expect("the import system session was opened")
            .publish_staged(snapshot, register_system)
            .map_err(koru_import::ImportError::new)
    }
}

fn clamp_zoom(z: f32) -> f32 {
    theme::clamp_zoom(z)
}

/// How many **documents** a workspace holds, from the Store's per-kind counts:
/// its pages, and its canvases when this build shows them (`canvas`, from
/// `koru_features::canvas`).
///
/// A canvas is a document to the person counting — it is made from the `+`
/// menu, opened as a tab and renamed like a page — so the manager's column
/// counts it with them rather than dropping it on the floor for carrying a
/// token the Store does not interpret. A build that hides canvases
/// (`ENABLE_CANVAS` off) does not count what it gives no way to see or open
/// (decided 2026-10-10). Files, notes and the
/// hidden kinds are not documents here, as before.
fn document_count(kinds: &std::collections::BTreeMap<String, u64>, canvas: bool) -> u64 {
    let canvases = if canvas {
        &[koru_core::explore::KIND_CANVAS][..]
    } else {
        &[]
    };
    std::iter::once(&koru_core::explore::KIND_DOCUMENT)
        .chain(canvases)
        .filter_map(|kind| kinds.get(*kind))
        .sum()
}

/// How often the periodic geometry capture runs. See its call site in
/// `about_to_wait`.
const PERSIST_EVERY: Duration = Duration::from_secs(1);
/// How often the idle half of the content-burst boundary is asked (ADR-273).
///
/// The same throttle as [`PERSIST_EVERY`] and for the same reason: `about_to_wait`
/// fires several times a second forever, because the caret blinks, and anything
/// on that path is paid for as long as the app is open. One second is plenty for
/// a five-second quiet period, and — this is the part worth keeping — it is a
/// throttle on an *existing* wake rather than a clock of its own. Nothing in
/// this feature schedules anything; `ContentBursts::quiet` is handed the time
/// this loop already had.
const BURST_TICK_EVERY: Duration = Duration::from_secs(1);
/// A stream of `Moved` events is a live native drag. Expensive window-system
/// queries wait until the stream has gone quiet instead of running per pixel.
const WINDOW_MOVE_SETTLE: Duration = Duration::from_millis(75);

pub(crate) const CARET_BLINK: Duration = Duration::from_millis(530);

/// Solid for the first half of each blink period counted from `origin`, which
/// a caller resets on every edit so the caret never blinks out from under the
/// keystroke just made. One formula for the document caret and first run's.
pub(crate) fn caret_phase_on(origin: Instant) -> bool {
    let period = CARET_BLINK.as_millis() * 2;
    origin.elapsed().as_millis() % period < CARET_BLINK.as_millis()
}

/// When the caret blinking from `origin` next changes phase.
pub(crate) fn next_caret_flip(origin: Instant, now: Instant) -> Instant {
    let half = CARET_BLINK.as_millis();
    let elapsed = now.saturating_duration_since(origin).as_millis();
    origin + Duration::from_millis(((elapsed / half + 1) * half) as u64)
}
/// Two clicks this close together, at roughly the same spot, select a word.
pub(crate) const DOUBLE_CLICK: Duration = Duration::from_millis(400);

// The display zoom bounds and step live beside the `theme::zoom()` global they
// are about — `theme::ZOOM_MIN`/`ZOOM_MAX`/`ZOOM_STEP`. They were three
// literals here and three more in `koru_app_shell::state`; the window menu's
// zoom row would have been a fourth copy, which is what finally moved them.

/// How often to wake while a view reports itself animating.
///
/// Faster than any display refresh on purpose: the loop is unsynced
/// ([`PRESENT_MODE`]), so this is a *floor* on latency rather than a frame
/// rate. It applies only to panes that are actually animating, and only until
/// they stop.
const ANIMATION_FRAME: Duration = Duration::from_millis(8);

/// How often a hidden Koru process drains agent requests.
///
/// The agent reader does not push into this winit loop, so a hidden process
/// needs a short wake to receive the restore chosen from the agent's tray.
const HIDDEN_POLL: Duration = Duration::from_millis(120);
/// Keep the accepted Stop state visible long enough to be understood, even if
/// the worker reaches its next cancellation checkpoint immediately.
const IMPORT_CANCEL_HOLD: Duration = Duration::from_secs(3);

/// How often the pane's switcher asks its worker to refresh the Store profile
/// catalog and republishes the workspace list.
///
/// What it is watching for is another *process* — a second Koru window adding a
/// workspace or renaming a profile — so it is matched to
/// `koru_workspace::profile::POLL`, which polls the profile document for the
/// same class of change. It used to run on every wake, which meant a
/// Store request and two `Vec` clones per pane three times a second for news
/// that arrives once a session, if ever.
/// Something happened off the event loop thread.
///
/// **The variants carry no work.** All this does is end the wait: winit runs
/// `about_to_wait` after every batch of events, including a user one, and that
/// is where `workspace.tick()` already lives. A payload here would be a second
/// path into work that has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wake {
    /// A store session has something to hand over.
    Store,
    /// A legacy import worker has inspection or completion to hand over.
    Import,
    /// A delegation folder check has an answer for its requesting window.
    DelegateFolder,
    /// A session-recording start or finalize worker has an answer
    /// (SESSIONRECORD). The session itself travels on the channel; this only
    /// ends the wait.
    Recording,
    /// The Store profile catalog worker has a fresh result to hand over.
    Profiles,
    /// The binding-marker writer finished a marker: the loop can now ask the
    /// Agent to bind, or tell the person the intent could not be recorded.
    ProfileBindings,
    /// The shared clipboard worker normalized a bitmap or captured a file list.
    Clipboard,
    /// The Koru Agent queued a restore or search target for this process.
    Agent,
    /// The taskbar activation listener set a verb flag (new / open / search)
    /// that the loop must drain.
    Taskbar,
    /// The settings authority answered — the startup read, or another window's
    /// change. Without this an idle window would sit on stale settings until
    /// somebody moved the pointer.
    Settings,
    /// AppKit delivered one or more LaunchServices URLs to the retained
    /// application delegate.
    ExternalUrl,
    /// The voice worker's reader thread queued a transcript. **The reason
    /// dictation needs no timer**: a partial arrives several times a second
    /// and a loop that polled for one would wake an idle window forever.
    Voice,
    /// Identity-free onboarding worker or Agent state changed.
    Onboarding,
    /// The spell worker finished a batch of checks. The results are already on
    /// each asking view's own reply channel; `maintain` drains them.
    Spelling,
    /// The diagram worker laid a `mermaid` block out. The drawing is already
    /// on the asking view's reply channel; `maintain` lands it and asks for
    /// the redraw. Carries nothing, like `Spelling`.
    Diagram,
    /// The session backend queued a `BackendEvent` for a session tab. Queue
    /// first, wake second — the event is already on the owning view's queue and
    /// `maintain` drains it; nothing on the loop thread touches the session
    /// socket.
    Session,
    /// A native child surface (a WebView) queued an event on its owning
    /// view's channel; the next maintenance pass drains it.
    Surface,
    /// A native menu row was chosen; its id is queued in `menus`.
    Menu,
    /// A screen reader attached, detached, or asked a window to do something.
    ///
    /// **Carries nothing**, deliberately. The request is already on the
    /// owning window's own mailbox and is drained on the loop thread with the
    /// generation checks that decide whether it still means anything; a
    /// payload here would be a second route to the same work and would be
    /// answered on whichever thread the platform chose. See
    /// `koru_window::accessibility`.
    Accessibility,
}

#[derive(Debug, PartialEq, Eq)]
struct PendingReminderStartup {
    accepted: bool,
    waiters: Vec<ReminderStartupRequestId>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ReminderStartupQueue {
    pending: Option<PendingReminderStartup>,
    queued: Option<ReminderStartupRequest>,
    superseded: Vec<ReminderStartupRequestId>,
}

struct ReminderStartupCompletion {
    waiters: Vec<ReminderStartupRequestId>,
    superseded: Vec<ReminderStartupRequestId>,
    next: Option<ReminderStartupRequest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReminderStartupEnqueue {
    Start,
    Coalesced,
    Queued,
}

impl ReminderStartupQueue {
    fn enqueue(&mut self, request: ReminderStartupRequest) -> ReminderStartupEnqueue {
        match self.pending.as_mut() {
            None => {
                self.pending = Some(PendingReminderStartup {
                    accepted: request.accepted,
                    waiters: vec![request.id],
                });
                ReminderStartupEnqueue::Start
            }
            Some(current) if current.accepted == request.accepted => {
                // The newest answer matches the in-flight one, so it cancels
                // any opposite answer that was queued between them.
                if let Some(superseded) = self.queued.take() {
                    self.superseded.push(superseded.id);
                }
                current.waiters.push(request.id);
                ReminderStartupEnqueue::Coalesced
            }
            Some(_) => {
                if let Some(superseded) = self.queued.replace(request) {
                    self.superseded.push(superseded.id);
                }
                ReminderStartupEnqueue::Queued
            }
        }
    }

    fn finish(&mut self) -> ReminderStartupCompletion {
        ReminderStartupCompletion {
            waiters: self
                .pending
                .take()
                .map(|pending| pending.waiters)
                .unwrap_or_default(),
            superseded: std::mem::take(&mut self.superseded),
            next: self.queued.take(),
        }
    }
}

fn reminder_startup_facts(
    settings: &koru_settings::GlobalSettings,
) -> koru_window::ReminderStartupFacts {
    koru_window::ReminderStartupFacts {
        start_on_login: settings.start_on_login,
        explained: settings.reminder_start_on_login_explained,
    }
}

fn reminder_startup_patch(accepted: bool) -> koru_settings::GlobalPatch {
    koru_settings::GlobalPatch {
        // Declining records only that the explanation was answered. It does
        // not turn off a startup choice made elsewhere.
        start_on_login: accepted.then_some(true),
        reminder_start_on_login_explained: Some(true),
        ..Default::default()
    }
}

struct InspectedImport {
    prepared: Box<dyn koru_import::PreparedImport>,
    identity_conflict: bool,
}

enum ImportMessage {
    ContentProgress {
        stage: koru_import::ImportStage,
        completed: usize,
        total: Option<usize>,
    },
    Inspected(Result<InspectedImport, String>),
    Imported(Result<koru_store_core::model::Workspace, String>),
    ContentImported(Result<ContentImportSummary, koru_import::ImportError>),
    Exported {
        id: WorkspaceId,
        name: String,
        destination: PathBuf,
        result: Result<(), String>,
    },
    /// One or more documents written through a format exporter.
    DocumentsExported {
        count: usize,
        /// First title, for the singular toast.
        name: String,
        destination: PathBuf,
        remembered_dir: Option<PathBuf>,
        result: Result<(), String>,
        format: &'static str,
        ids: Vec<koru_core::DocumentId>,
    },
    WorkspaceDeleted {
        id: WorkspaceId,
        name: String,
        was_current: bool,
        result: Result<koru_store_core::model::Workspace, (koru_store_core::Code, String)>,
    },
    FileSaved {
        name: String,
        destination: PathBuf,
        result: Result<(), String>,
    },
    FileOpened {
        name: String,
        result: Result<(), String>,
    },
    FilesSaved {
        count: usize,
        destination: PathBuf,
        result: Result<(), String>,
    },
    FilesOpened {
        count: usize,
        result: Result<(), String>,
    },
    /// A file's bytes were fetched from a peer, or the attempt ended. Carries the
    /// document so the row that was pressed can stop saying it is fetching.
    FileDownloaded {
        id: koru_core::DocumentId,
        name: String,
        result: Result<(), String>,
    },
    /// Which of this workspace's files are held locally, as of a sweep.
    ///
    /// A whole answer rather than a delta: the sweep is cheap, runs only when the
    /// manifest moves or a fetch lands, and a delta would have to be reconciled
    /// against a list that another window may have changed underneath it.
    FileLocality {
        workspace: koru_core::WorkspaceId,
        remote: Vec<koru_core::DocumentId>,
    },
}

struct ClipboardImageResult {
    targets: Vec<(WindowId, PasteTarget)>,
    result: Result<koru_clipboard::ImagePayload, koru_clipboard::Error>,
}

/// One cross-workspace move handed to the single transfer worker.
///
/// The worker is deliberately serial: the Store mutation is ordered, and two
/// concurrent moves from the same manifest would only contend on its global
/// transaction while making completion order ambiguous to the window.
struct TransferJob {
    origin: WindowId,
    client: koru_api_client::Client,
    id: koru_core::DocumentId,
    source: WorkspaceId,
    target: WorkspaceId,
    parent: Option<koru_core::DocumentId>,
    index: usize,
    follow: bool,
}

struct TransferResult {
    origin: WindowId,
    id: koru_core::DocumentId,
    source: WorkspaceId,
    target: WorkspaceId,
    follow: bool,
    result: Result<Vec<koru_store_core::ids::DocumentId>, String>,
}

/// One selection move into a document this window holds no replica of.
///
/// The cuttings are copies taken at request time, so the worker owns
/// everything it writes and the source document stays untouched until the
/// completion drain hears the write was durable.
struct SelectionMoveJob {
    origin: WindowId,
    segment: String,
    root: Option<std::path::PathBuf>,
    client: koru_api_client::Client,
    source: WorkspaceId,
    target: WorkspaceId,
    document: koru_core::DocumentId,
    node: Option<koru_core::NodeId>,
    title: String,
    cuttings: Vec<(koru_core::schema::Cutting, Option<koru_core::ListStyle>)>,
    blobs: Vec<koru_store_core::model::BlobHash>,
    inline: Option<(Vec<koru_core::TextRun>, Vec<koru_core::node::Attr>)>,
}

struct SelectionMoveResult {
    origin: WindowId,
    document: koru_core::DocumentId,
    node: Option<koru_core::NodeId>,
    title: String,
    result: Result<(), String>,
}

/// The list style of the `ListBlock` nearest above `id`, if any — what a
/// moved list item has to carry, because the selection never includes the
/// container that holds the style.
fn enclosing_list_style(
    ws: &koru_core::schema::WorkspaceDoc,
    id: koru_core::NodeId,
) -> Option<koru_core::ListStyle> {
    let mut at = id;
    while let Ok(koru_core::schema::Parent::Node(parent)) = ws.parent(at) {
        if let Ok(node) = ws.read(parent)
            && node.kind == koru_core::Kind::ListBlock
        {
            return node.meta.list_style;
        }
        at = parent;
    }
    None
}

/// Drop the empty paragraph `create_document_after` seeds once moved content
/// has made it redundant — but only while it is genuinely empty and last, so
/// nothing typed or planted can ever be trimmed by mistake.
fn trim_trailing_seed(ws: &mut koru_core::schema::WorkspaceDoc) {
    let Some(root) = koru_workspace::document_root(ws) else {
        return;
    };
    let Ok(children) = ws.children(koru_core::schema::Parent::Node(root)) else {
        return;
    };
    let Some(&last) = children.last().filter(|_| children.len() > 1) else {
        return;
    };
    let empty = ws.read(last).is_ok_and(|n| {
        n.content.as_text().is_some_and(str::is_empty) && n.content.attrs().is_empty()
    });
    let childless = ws
        .children(koru_core::schema::Parent::Node(last))
        .is_ok_and(|c| c.is_empty());
    if empty && childless {
        let _ = ws.delete(last);
    }
}

struct ContentImportSummary {
    root: ContentImportRoot,
    files: Vec<(koru_core::DocumentId, String)>,
    imported: usize,
    already_known: usize,
    had_failures: bool,
    /// Only populated when even the FAILED child could not be stored.
    warnings: Vec<String>,
}

/// Report progress to the visible import toast and existing non-Evernote
/// diagnostics. Evernote updates at note boundaries, so its routine progress
/// is deliberately not an unconditional per-note log.
struct ContentImportProgress {
    format: &'static str,
    tx: mpsc::Sender<ImportMessage>,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
    cancellation: Arc<koru_import::CancellationToken>,
}

impl koru_import::ImportProgress for ContentImportProgress {
    fn update(&self, stage: koru_import::ImportStage, completed: usize, total: Option<usize>) {
        if self.format != "evernote" {
            let total_label = total
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".into());
            println!(
                "[Koru][import] format={} stage={stage:?} completed={completed} total={total_label}",
                self.format
            );
        }
        let _ = self.tx.send(ImportMessage::ContentProgress {
            stage,
            completed,
            total,
        });
        let _ = self.proxy.send_event(Wake::Import);
    }

    fn cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    fn cancel(&self) {
        self.cancellation.cancel();
    }
}

#[derive(Clone, Copy)]
enum ContentImportRoot {
    Notebook(koru_core::DocumentId),
    Document(koru_core::DocumentId),
    File(koru_core::DocumentId),
}

impl ContentImportRoot {
    fn id(self) -> koru_core::DocumentId {
        match self {
            Self::Notebook(id) | Self::Document(id) | Self::File(id) => id,
        }
    }

    fn notice_target(self) -> crate::ImportNoticeTarget {
        match self {
            Self::Notebook(id) => crate::ImportNoticeTarget::Notebook(id),
            Self::Document(id) => crate::ImportNoticeTarget::Document(id),
            Self::File(id) => crate::ImportNoticeTarget::File(id),
        }
    }
}

/// Choose the user-visible root without mistaking a dependency for the thing
/// being imported. OneNote deliberately emits attachments first so their bytes
/// exist before note pills refer to them; those files are parentless manifest
/// entries, but they are not the notebook root and FAILED must never land under
/// one of them.
fn content_import_root(
    format: &str,
    kind: &str,
    parent: Option<koru_store_core::ids::DocumentId>,
    id: koru_core::DocumentId,
) -> Option<ContentImportRoot> {
    if parent.is_some() {
        return None;
    }
    let roots_import =
        crate::formats::import_descriptor(format).is_some_and(|descriptor| descriptor.roots_import);
    if roots_import {
        return (kind != koru_core::explore::KIND_FILE).then_some(ContentImportRoot::Notebook(id));
    }
    Some(if kind == koru_core::explore::KIND_FILE {
        ContentImportRoot::File(id)
    } else {
        ContentImportRoot::Document(id)
    })
}

fn snapshot_file_locator(snapshot: &[u8]) -> Option<(String, koru_core::NodeId)> {
    let doc = koru_core::WorkspaceDoc::from_snapshot(snapshot).ok()?;
    let hash = doc.blob_hashes().ok()?.into_iter().next()?;
    let node = doc
        .children(koru_core::schema::Parent::Root)
        .ok()?
        .into_iter()
        .next()?;
    Some((hash, node))
}

#[derive(Clone)]
struct KnownFile {
    document: koru_core::DocumentId,
    node: koru_core::NodeId,
    title: String,
}

/// Live file entries already using each content-addressed blob. The blob store
/// itself deduplicates bytes, but this index prevents a second visible file row
/// from being created for those same bytes.
fn known_workspace_files(
    client: &koru_api_client::Client,
    workspace: koru_store_core::ids::WorkspaceId,
) -> Result<std::collections::HashMap<String, KnownFile>, String> {
    let mut known = std::collections::HashMap::new();
    let entries = client.documents(workspace).map_err(|error| {
        format!("could not check whether selected files are already known: {error}")
    })?;
    for entry in entries
        .into_iter()
        .filter(|entry| entry.kind == koru_core::explore::KIND_FILE && entry.trashed_at.is_none())
    {
        let snapshot = client.snapshot(workspace, entry.id).map_err(|error| {
            format!(
                "could not check existing file {:?} for duplicates: {error}",
                entry.title
            )
        })?;
        let Some((hash, node)) = snapshot_file_locator(&snapshot) else {
            continue;
        };
        known.entry(hash).or_insert_with(|| KnownFile {
            document: koru_core::DocumentId::from_uuid(entry.id.as_uuid()),
            node,
            title: entry.title,
        });
    }
    Ok(known)
}

/// Redirect attachment attributes when a content import reuses a file already
/// present in the workspace. The visible file entry and the pill are one
/// decision: skipping the duplicate row without rewriting the pill would leave
/// it pointing at an id that was never stored.
fn remap_imported_file_references(
    snapshot: &[u8],
    remap: &std::collections::HashMap<koru_core::DocumentId, KnownFile>,
) -> Result<Vec<u8>, String> {
    use koru_core::ids::DocRef;
    use koru_core::node::AttrValue;

    if remap.is_empty() {
        return Ok(snapshot.to_vec());
    }
    let mut doc = koru_core::WorkspaceDoc::from_snapshot(snapshot).map_err(|error| {
        format!("could not open imported document for file deduplication: {error}")
    })?;
    for (node, _) in doc.walk_all().map_err(|error| error.to_string())? {
        let attrs = doc
            .read(node)
            .map_err(|error| error.to_string())?
            .content
            .attrs()
            .to_vec();
        for mut attr in attrs {
            let AttrValue::Ref(mut reference) = attr.value else {
                continue;
            };
            let Some(DocRef::Document(document)) = reference.home else {
                continue;
            };
            let Some(known) = remap.get(&document) else {
                continue;
            };
            reference.node = known.node;
            reference.home = Some(DocRef::Document(known.document));
            attr.value = AttrValue::Ref(reference);
            doc.set_attr(node, &attr)
                .map_err(|error| error.to_string())?;
        }
    }
    doc.export_snapshot().map_err(|error| error.to_string())
}

/// Build the visible child that preserves partial-import diagnostics beside the
/// content they describe. The Store write stays in the caller so this helper is
/// deterministic and directly testable.
const DOCUMENT_CREATE_TRASHED_AT: &str = "document-create-trashed-at";

fn preview_contains_trashed(nodes: &[koru_import::PreviewNode]) -> bool {
    nodes
        .iter()
        .any(|node| node.trashed || preview_contains_trashed(&node.children))
}

fn after_deleted_import_capability_check<T>(
    has_deleted: bool,
    capabilities: &[String],
    operation: impl FnOnce() -> Result<T, koru_import::ImportError>,
) -> Result<T, koru_import::ImportError> {
    if has_deleted
        && !capabilities
            .iter()
            .any(|capability| capability == DOCUMENT_CREATE_TRASHED_AT)
    {
        return Err(koru_import::ImportError::coded(
            koru_import::ImportErrorCode::StoreUpdateRequired,
        ));
    }
    operation()
}

fn import_error_key(code: koru_import::ImportErrorCode) -> koru_i18n::Key {
    match code {
        koru_import::ImportErrorCode::StoreUpdateRequired => {
            koru_i18n::Key::ImportStoreUpdateRequired
        }
        koru_import::ImportErrorCode::NotConfigured => koru_i18n::Key::NotSetUpInThisBuild,
    }
}

fn rendered_import_warning(lang: koru_i18n::Lang, warning: &koru_import::ImportWarning) -> String {
    let Some(key) = warning.code.and_then(crate::formats::import_warning_key) else {
        return warning.message.clone();
    };
    let values: Vec<&str> = warning.arguments.iter().map(String::as_str).collect();
    koru_i18n::fill(koru_i18n::t(lang, key), &values)
}

fn failed_import_document(
    workspace: koru_store_core::ids::WorkspaceId,
    parent: koru_store_core::ids::DocumentId,
    source: &std::path::Path,
    intro: &str,
    messages: &[String],
) -> Result<koru_import::ImportedDocument, String> {
    use koru_core::node::Node;
    use koru_core::schema::Parent;

    let mut doc = koru_core::WorkspaceDoc::create(
        koru_core::WorkspaceId::from_uuid(workspace.as_uuid()),
        "FAILED",
    )
    .map_err(|error| error.to_string())?;
    let root = doc
        .insert(
            Parent::Root,
            Node::document("FAILED").map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
    doc.insert(
        Parent::Node(root),
        Node::paragraph(intro).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    for message in messages {
        for line in message
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            doc.insert(
                Parent::Node(root),
                Node::paragraph(line).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(koru_import::ImportedDocument {
        id: koru_store_core::ids::DocumentId::new(),
        source: source.to_path_buf(),
        title: "FAILED".into(),
        kind: koru_core::explore::KIND_DOCUMENT.into(),
        mime: None,
        parent: Some(parent),
        trashed: false,
        trashed_at: None,
        snapshot: doc.export_snapshot().map_err(|error| error.to_string())?,
    })
}

fn failed_import_documents(
    workspace: koru_store_core::ids::WorkspaceId,
    fallback_parent: koru_store_core::ids::DocumentId,
    source_roots: &std::collections::BTreeMap<PathBuf, koru_core::DocumentId>,
    warning_groups: std::collections::BTreeMap<PathBuf, Vec<String>>,
    lang: koru_i18n::Lang,
) -> Vec<Result<koru_import::ImportedDocument, String>> {
    let intro = koru_i18n::t(lang, koru_i18n::Key::ImportFailedIntro);
    warning_groups
        .into_iter()
        .map(|(source, messages)| {
            let parent = source_roots
                .get(&source)
                .copied()
                .map(|id| koru_store_core::ids::DocumentId::from_uuid(id.as_uuid()))
                .unwrap_or(fallback_parent);
            failed_import_document(workspace, parent, &source, intro, &messages)
        })
        .collect()
}

/// Fetch the bytes behind an opaque workspace file. Both Save as and Open in
/// app use this exact path so neither can drift into interpreting file entries
/// differently.
fn workspace_file_bytes(
    segment: &str,
    root: Option<&std::path::Path>,
    workspace: koru_store_core::ids::WorkspaceId,
    file: koru_store_core::ids::DocumentId,
) -> Result<Vec<u8>, String> {
    let client = client_at_segment(segment, root).map_err(|e| e.to_string())?;
    let snapshot = client
        .snapshot(workspace, file)
        .map_err(|e| e.to_string())?;
    let doc =
        koru_core::schema::WorkspaceDoc::from_snapshot(&snapshot).map_err(|e| e.to_string())?;
    let root_id = doc
        .children(koru_core::schema::Parent::Root)
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "the file entry has no root".to_string())?;
    let node = doc.read(root_id).map_err(|e| e.to_string())?;
    let koru_core::node::Content::Blob { hash, .. } = node.content else {
        return Err("the file entry does not contain blob data".into());
    };
    let hash = koru_store_core::model::BlobHash::parse(&hash.0).map_err(|e| e.to_string())?;
    client.get_blob(workspace, &hash).map_err(|e| e.to_string())
}

/// Turn a hydration failure into the sentence a person needs.
///
/// The codes reaching here come from the replication coordinator through two
/// layers of `StorageFailed`, so without this the whole explanation a user gets
/// is `storage_failed: storage_failed: blob hydration failed: …`. Only the cases
/// somebody can *act* on are named; the rest keep the generic line and put the
/// code in the details, where it is diagnostic rather than advice.
fn download_failure_hint(error: &str) -> String {
    let waiting = waiting_device_names(error);
    if error.contains("blob_waiting_for_peer") {
        match waiting {
            Some(names) => format!(
                "The device holding this file ({names}) is offline. It will be fetched when it comes online."
            ),
            None => "The device holding this file is offline. It will be fetched when that device comes online.".to_string(),
        }
    } else if error.contains("blob_too_large") {
        "This file is larger than syncing between devices currently supports.".to_string()
    } else if error.contains("no_linked_devices") {
        "Link another device to download files that were added elsewhere.".to_string()
    } else if error.contains("blob_not_found") {
        "No connected device has this file's contents yet.".to_string()
    } else if error.contains("blob_checksum_mismatch") {
        "The bytes that arrived did not match what this file says it is.".to_string()
    } else {
        "The workspace was not changed.".to_string()
    }
}

/// The devices a parked hydration is waiting on, joined for a sentence.
///
/// The daemon sends them as structured `waiting_on` display data, and a caller
/// holding the typed `Error` should read `Error::waiting_on` instead — the
/// image loader does. This parser is for the paths that cross a stringly
/// boundary, where `Display` renders the same names as a `\u{1f}`-joined
/// suffix. `None` means "we don't know which device" — the sentence then stays
/// generic rather than guessing.
fn waiting_device_names(error: &str) -> Option<String> {
    let suffix = error.split_once(" waiting_on=")?.1;
    let names: Vec<&str> = suffix
        .split('\u{1f}')
        .filter(|name| !name.is_empty())
        .collect();
    (!names.is_empty()).then(|| names.join(", "))
}

/// True when a fetch failed only because the file's holder is offline — the
/// one failure that deserves a quiet toast rather than an error dialog: the
/// hydration stays registered and completes by itself when that device
/// connects, so there is nothing wrong and nothing to act on but wait.
fn blob_is_waiting_for_a_device(error: &str) -> bool {
    error.contains("blob_waiting_for_peer")
}

/// Which of these files this device does not hold the bytes for.
///
/// One `snapshot` per file to learn its blob hash, then one listing of the local
/// blob store — never `get_blob`, which would *fetch* the very thing being asked
/// about and turn a sweep into a download of the whole workspace.
///
/// A file whose snapshot cannot be read is reported as present. Being wrong that
/// way costs one honest error if it is clicked; being wrong the other way paints
/// a placeholder over a file that is sitting right there.
fn workspace_remote_files(
    segment: &str,
    root: Option<&std::path::Path>,
    workspace: koru_store_core::ids::WorkspaceId,
    files: &[koru_core::DocumentId],
) -> Result<Vec<koru_core::DocumentId>, String> {
    let client = client_at_segment(segment, root).map_err(|e| e.to_string())?;
    let local: std::collections::HashSet<String> = client
        .blobs(workspace)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|blob| blob.hash.as_str().to_owned())
        .collect();
    let mut remote = Vec::new();
    for file in files {
        let store_id = koru_store_core::ids::DocumentId::from_uuid(file.as_uuid());
        let Ok(snapshot) = client.snapshot(workspace, store_id) else {
            continue;
        };
        let Some(hash) = snapshot_blob_hash(&snapshot) else {
            continue;
        };
        if !local.contains(&hash) {
            remote.push(*file);
        }
    }
    Ok(remote)
}

/// The content hash a file document's root node points at.
fn snapshot_blob_hash(snapshot: &[u8]) -> Option<String> {
    let doc = koru_core::schema::WorkspaceDoc::from_snapshot(snapshot).ok()?;
    let root = doc
        .children(koru_core::schema::Parent::Root)
        .ok()?
        .into_iter()
        .next()?;
    match doc.read(root).ok()?.content {
        koru_core::node::Content::Blob { hash, .. } => Some(hash.0),
        _ => None,
    }
}

/// One stable cache slot per file id. Prefixing the cleaned leaf avoids Windows
/// device names while retaining the extension the OS uses to choose an app.
fn temporary_open_path(id: koru_store_core::ids::DocumentId, name: &str) -> PathBuf {
    let leaf = name
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("file");
    let cleaned: String = leaf
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                '_'
            } else {
                c
            }
        })
        .take(180)
        .collect();
    let cleaned = if cleaned.trim_matches(['.', ' ']).is_empty() {
        "file"
    } else {
        cleaned.trim_end_matches(['.', ' '])
    };
    std::env::temp_dir()
        .join("koru-open")
        .join(id.to_storage())
        .join(format!("koru-{cleaned}"))
}

/// Open a workspace file's bytes: a shared link goes to the desktop URL
/// handler; anything else is written to the temp cache and opened by type.
fn open_file_bytes(
    id: koru_store_core::ids::DocumentId,
    name: &str,
    mime: Option<&str>,
    bytes: &[u8],
) -> Result<(), String> {
    match crate::url_file::open_target(name, mime, bytes) {
        crate::url_file::OpenTarget::Url(url) => {
            koru_window::open_uri(&url).map_err(|e| e.to_string())
        }
        crate::url_file::OpenTarget::InvalidLink => {
            Err("This link has no URL to open.".to_string())
        }
        crate::url_file::OpenTarget::File => {
            let destination = temporary_open_path(id, name);
            let parent = destination
                .parent()
                .ok_or_else(|| "the temporary file has no parent".to_string())?;
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            std::fs::write(&destination, bytes).map_err(|e| e.to_string())?;
            koru_window::open_uri(destination.to_string_lossy().as_ref()).map_err(|e| e.to_string())
        }
    }
}

/// Add one OS file-drop event to its window's pending batch.
///
/// Winit reports a multi-file drop as one `DroppedFile` event per path.  The
/// events arrive together before `about_to_wait`, so collecting them here and
/// starting the importer there preserves the user's single batch.  Starting on
/// the first event would set `import_busy` and silently discard every remaining
/// path.
fn queue_content_drop<K: Eq>(
    pending: &mut VecDeque<(K, &'static str, Vec<PathBuf>)>,
    source: K,
    format: &'static str,
    path: PathBuf,
) {
    if let Some((_, _, paths)) = pending
        .iter_mut()
        .find(|(id, queued_format, _)| id == &source && *queued_format == format)
    {
        paths.push(path);
    } else {
        pending.push_back((source, format, vec![path]));
    }
}

/// Import format selected by an externally dropped path. The mapping lives in
/// `crate::formats` with the rest of the registry knowledge; this alias keeps
/// the call sites short.
fn dropped_content_format(path: &Path) -> Option<&'static str> {
    crate::formats::dropped_content_format(path)
}

fn dropped_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff"
            )
        })
}

const CONTEXT_POLL: Duration = Duration::from_secs(2);

/// How often to re-check the OS light/dark while preference is System.
/// `ThemeChanged` is not always delivered (first flip after launch has been
/// missed); this is the belt for that. Cheap: one registry read.
const OS_THEME_POLL: Duration = Duration::from_millis(500);

/// **Latency over tearing** — see [ADR-26](../../../docs/decisions/README.md).
///
/// Vsync makes a finished frame wait for the next refresh, which is a whole
/// frame between a keystroke and seeing it. An editor is judged on exactly that
/// delay, and the thing that tears — a page of static text — is the thing that
/// tears least visibly. `AutoNoVsync` takes immediate if the driver has it and
/// falls back through mailbox to vsync, so this is a preference, not a demand.
const PRESENT_MODE: wgpu::PresentMode = wgpu::PresentMode::AutoNoVsync;

/// The window state a recording size lock was taken from, restored by
/// [`WindowRequest::UnlockSizeAndRestore`]. A maximised window restores its
/// maximised *state* rather than a rectangle.
#[derive(Clone, Copy)]
struct SavedGeometry {
    position: Option<winit::dpi::PhysicalPosition<i32>>,
    inner_size: winit::dpi::PhysicalSize<u32>,
    maximised: bool,
}

/// One window: its GPU surface, the view inside it, and the pointer state that
/// belongs to *that* window rather than to the app.
///
/// Everything about the document is deliberately absent — it lives once, in
/// [`Workspace`], and every pane borrows it. That is what lets two windows show
/// the same document and see each other's typing.
struct Pane<V> {
    inspector: Option<crate::inspector::Inspector>,
    inspector_window: Option<inspector_window::NativeInspector>,
    inspector_geometry: Option<crate::inspector::Geometry>,
    window: Arc<Window>,
    /// What the titlebar currently says, so it is only set when it changes.
    title: String,
    surface: RenderSurface<'static>,
    view: V,
    cursor: (f32, f32),
    /// The shape currently set on the window, so it is only set when it
    /// changes rather than on every move the pointer makes.
    cursor_icon: CursorIcon,
    /// Which edge or corner the pointer is on, if any — the window's own resize
    /// border, which an undecorated window has to provide for itself. See
    /// [`koru_window::resize`].
    resize_edge: Option<ResizeDirection>,
    /// Whether the window last filled the screen. `None` until it is first
    /// asked, so the initial state is always pushed rather than assumed.
    maximized: Option<bool>,
    /// Which of this window's edges are against the screen's, so the
    /// compositor's border on them is off-screen and the shell draws its own.
    /// See [`koru_window::hairline`].
    ///
    /// Cached rather than asked, because answering costs three user32 calls —
    /// it is recomputed on a resize or a move, never in a draw.
    flush: hairline::Edges,
    /// Most recent native move event. While this is recent, defer both the
    /// flush-edge OS queries and periodic geometry persistence.
    moved_at: Option<Instant>,
    /// Whether this window is kept above the others. Per-window: each pane owns
    /// its own pin, so a popped-out document can float above an unpinned main
    /// window, or vice versa.
    pinned: bool,
    /// Whether resize affordances are limited to height changes.
    fixed_width: bool,
    /// The window is held at an exact recording size
    /// ([`WindowRequest::PlaceAndLockExact`]): the resize border, snap rows
    /// and maximise are refused until [`WindowRequest::UnlockSizeAndRestore`].
    size_locked: bool,
    /// The geometry a size lock was taken from, to restore on unlock.
    saved_geometry: Option<SavedGeometry>,
    /// A live capture session on this window (SESSIONRECORD), parked between
    /// the start worker's `Started` and the stop's finalize worker. `Some`
    /// only while a recording is actually producing frames.
    // Read only by the Windows capture workers (M31).
    #[cfg_attr(not(windows), allow(dead_code))]
    recording: Option<crate::app::recording::Live>,
    last_click: Option<(Instant, f32, f32)>,
    /// Whether this pane believes a left press is still down.
    ///
    /// **macOS does not reliably deliver `mouseUp:`.** A window drag consumes
    /// it outright (`performWindowDragWithEvent:` runs its own event loop), and
    /// `KORU_INPUT_DEBUG` on a real session showed four presses and no releases
    /// at all. A gesture that never ends leaves the grab cursor stuck and makes
    /// every later click read as the continuation of a drag that finished
    /// minutes ago — so the shell tracks the press itself and closes it when
    /// the OS says the button is no longer down. See [`press_ended_unseen`].
    pressed: bool,
    /// Whether anything has asked for a frame since the last one was presented.
    ///
    /// **macOS sends `RedrawRequested` on its own, tens of times a second**, and
    /// nothing in the winit API asks it to stop: an idle window measured 480
    /// frames against fewer than 60 requests from this app. Drawing each one
    /// cost a whole CPU core and a quarter of the GPU.
    ///
    /// So the "only draw when something changed" rule in `AGENTS.md` — which
    /// Windows gets for free, because `WM_PAINT` arrives only after an
    /// invalidation — is enforced here instead. A `Cell` because
    /// `request_redraw` takes `&self`.
    needs_frame: std::cell::Cell<bool>,
    /// Whether this window still owes itself a `set_maximized(true)` when it
    /// is first revealed.
    ///
    /// **Deferred off the attributes deliberately.** Every window is created
    /// hidden and shown only after its first frame has been presented — and
    /// winit's `with_maximized(true)` on a hidden window *shows it*:
    /// `set_maximized` runs after `CreateWindowExW` and its `apply_diff` calls
    /// `ShowWindow(SW_MAXIMIZE)`, which would put the blank window this hides
    /// straight back on screen. A `Cell` because reveal takes `&self` panes.
    maximize_on_reveal: std::cell::Cell<bool>,
    /// A window drag this shell is running itself — `None` when none is.
    ///
    /// **macOS delivers nothing at all during a drag on a borderless window:**
    /// no `mouseDragged:`, no `mouseUp:`. So neither `drag_window`
    /// (`performWindowDragWithEvent:`, which reports success and moves nothing)
    /// nor a `CursorMoved` handler can work — there are no events to handle.
    ///
    /// What *does* run is `about_to_wait`, on the caret's clock. So the drag is
    /// **polled**: the pointer's position is read from the OS each tick and the
    /// window placed absolutely from where the gesture began. Nothing here
    /// depends on an event arriving.
    drag: Option<WindowDrag>,
    /// Where a press began, when one is in flight: the pointer on screen (in
    /// logical points) and the same instant's position inside the window.
    ///
    /// **macOS sends no `mouseDragged:` on our windows**, so a press-and-drag
    /// gesture *inside* a view — dragging a document in the explore tree,
    /// carrying a tab, extending a selection — never sees the pointer move and
    /// hangs in its start state. The window drag has the same cause and the
    /// same cure: poll the pointer and synthesise the move. Recording both
    /// halves at press time means the local position can be reconstructed from
    /// the screen delta alone, with no window origin or screen height to get
    /// wrong.
    press_origin: Option<((f64, f64), (f32, f32))>,
    /// A dedicated search only dismisses on blur after the OS has focused it
    /// once. Some compositors report an initial `Focused(false)` while a new
    /// window is still being activated; treating that as click-away would make
    /// the global shortcut flash and immediately disappear.
    focused_once: bool,
    /// What the last `Focused` event said. Gates key-press dispatch: Windows
    /// delivers the trailing key of a window-switching chord — Alt+Tab's Tab —
    /// *after* the focus-loss event, when the modifier bookkeeping has already
    /// been cleared, so it arrives looking like a bare key. A window without
    /// focus has no keyboard; the press is not ours.
    focused: bool,
    /// Physical keys whose original press belonged to the window that handed
    /// focus here. Repeats are consumed until release (or a safety timeout).
    quarantine: koru_window::KeyQuarantine,
    /// This window's native accessibility provider and its semantic identity.
    ///
    /// **A field rather than an `Option` set up later**, because the adapter
    /// can only be installed while the window is still hidden and every
    /// `set_visible(true)` in this file acts on a `Pane` that already exists.
    /// Making it part of the value is what turns "register before the first
    /// reveal" from a rule somebody has to remember into one that has nowhere
    /// left to go wrong. See `crate::accessibility`.
    access: crate::accessibility::PaneAccessibility,
}

impl<V> Pane<V> {
    fn close_inspector(&mut self) {
        if let Some(mut native) = self.inspector_window.take() {
            self.inspector_geometry = Some(native.remembered_geometry());
        }
        self.inspector.take();
        self.access.invalidate_structure();
    }

    fn scale(&self) -> f64 {
        self.window.scale_factor()
    }

    /// The border under a logical point, in this window's current state.
    fn resize_edge_at(&self, x: f32, y: f32) -> Option<ResizeDirection> {
        // **The one gate.** winit cannot perform a resize drag on macOS, so
        // there is no band there — not for the press, and not for the cursor.
        // Asked here rather than in `resize::edge` so the geometry stays pure
        // and stays tested on every platform. See `resize`'s module docs.
        if !resize::is_ours() {
            return None;
        }
        // A window locked at its recording size has no resize border.
        if self.size_locked {
            return None;
        }
        let s = self.scale() as f32;
        let size = self.window.inner_size();
        let edge = resize::edge(
            size.width as f32 / s,
            size.height as f32 / s,
            x,
            y,
            self.window.is_maximized(),
        )?;
        if !self.fixed_width {
            return Some(edge);
        }
        match edge {
            ResizeDirection::North | ResizeDirection::NorthEast | ResizeDirection::NorthWest => {
                Some(ResizeDirection::North)
            }
            ResizeDirection::South | ResizeDirection::SouthEast | ResizeDirection::SouthWest => {
                Some(ResizeDirection::South)
            }
            ResizeDirection::East | ResizeDirection::West => None,
        }
    }
}

/// A window drag the shell is running itself.
///
/// **Why this is polled and not driven by events.** On a borderless macOS
/// window, pressing the mouse and dragging delivers *nothing*: no
/// `mouseDragged:`, no `mouseUp:`. `KORU_INPUT_DEBUG` on a real session showed a
/// press, then silence until the button came back up. So there is no event to
/// hang a drag on, and `performWindowDragWithEvent:` — which winit's
/// `drag_window` calls, and which reports success — moves nothing either.
///
/// What does run is `about_to_wait`, on the caret's clock. So the pointer is
/// read from the OS each tick, and the window placed **absolutely** from where
/// the gesture began. Nothing here waits for an event.
#[derive(Copy, Clone, Debug)]
pub struct WindowDrag {
    /// Pointer position on screen when the drag began, in logical points.
    start_pointer: (f64, f64),
    /// The window's outer position when the drag began, in physical pixels.
    start_window: (i32, i32),
    scale: f64,
}

/// Where the pointer is on screen right now, in logical points.
///
/// **Screen coordinates, and macOS puts the origin at the bottom left** — Y
/// grows upward, the opposite of a window position. [`WindowDrag::position_for`]
/// is where that is undone.
#[cfg(target_os = "macos")]
fn pointer_on_screen() -> Option<(f64, f64)> {
    let p = objc2_app_kit::NSEvent::mouseLocation();
    Some((p.x, p.y))
}

#[cfg(not(target_os = "macos"))]
fn pointer_on_screen() -> Option<(f64, f64)> {
    None
}

impl WindowDrag {
    /// Record where everything is, so the drag can be computed absolutely.
    pub fn begin(window: &Window) -> Option<Self> {
        Some(Self {
            start_pointer: pointer_on_screen()?,
            start_window: {
                let p = window.outer_position().ok()?;
                (p.x, p.y)
            },
            scale: window.scale_factor(),
        })
    }

    /// Where the window should be, given where the pointer is now.
    ///
    /// **Absolute, never incremental.** The window's position is computed from
    /// the total travel since the gesture began, so a missed tick cannot
    /// accumulate error and the cursor stays welded to the spot it grabbed.
    /// Adding a per-tick delta instead would drift, and drift in a drag is what
    /// makes a window slide out from under the pointer.
    ///
    /// Pure, so the coordinate flip below is testable without a window — the
    /// same split `snap::half` and `resize::edge` use.
    fn position_for(&self, pointer: (f64, f64)) -> (i32, i32) {
        let dx = pointer.0 - self.start_pointer.0;
        // **Flipped.** Screen Y grows upward on macOS; a window's Y grows
        // downward. Without the negation the window runs away from the pointer
        // vertically while tracking it perfectly horizontally.
        let dy = -(pointer.1 - self.start_pointer.1);
        (
            self.start_window.0 + (dx * self.scale).round() as i32,
            self.start_window.1 + (dy * self.scale).round() as i32,
        )
    }
}

/// Has the left button been released without the window being told?
///
/// **macOS does not guarantee a `mouseUp:`.** `performWindowDragWithEvent:` —
/// what `Window::drag_window` calls — runs its own event loop for the whole
/// gesture and consumes the release; a real session logged four presses and no
/// releases at all. winit registers `mouseUp:` correctly, so this is AppKit
/// declining to route it rather than a winit defect, and nothing in the winit
/// API reports it.
///
/// So the shell asks the OS directly. `NSEvent::pressedMouseButtons` is a
/// *global* bitmask of what is physically held right now, needs no event, and
/// is cheap enough for the pointer-move path.
///
/// `true` means: this pane thinks a press is in flight, and the button is not
/// actually down — so the gesture must be ended now.
///
/// Windows normally delivers the release, but the local-search trace found the
/// same observable failure there: a click ended physically while the field
/// kept extending its selection on later pointer moves. `GetAsyncKeyState`
/// gives the same non-guess answer there. Other platforms retain `false`.
#[cfg(any(windows, target_os = "macos", test))]
fn physically_ended(believed_pressed: bool, physically_pressed: bool) -> bool {
    believed_pressed && !physically_pressed
}

#[cfg(target_os = "macos")]
fn press_ended_unseen(believed_pressed: bool) -> bool {
    if !believed_pressed {
        return false;
    }
    // A class method reading global input state — no receiver and no pointers,
    // which is why objc2 exposes it as safe.
    let held = objc2_app_kit::NSEvent::pressedMouseButtons();
    // Bit 0 is the left button.
    physically_ended(believed_pressed, held & 1 != 0)
}

#[cfg(target_os = "windows")]
fn press_ended_unseen(believed_pressed: bool) -> bool {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};

    if !believed_pressed {
        return false;
    }
    let held = (unsafe { GetAsyncKeyState(VK_LBUTTON as i32) } as u16 & 0x8000) != 0;
    physically_ended(believed_pressed, held)
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn press_ended_unseen(_believed_pressed: bool) -> bool {
    false
}

impl<V: View> Pane<V> {
    /// Set the pointer shape, if it moved onto something different.
    ///
    /// The resize border wins over whatever the view would say, the same way
    /// the scrollbar gutter wins over the row scrolled behind it: it is the
    /// outermost thing, and a view that drew an I-beam within a pixel of the
    /// window's edge would make the border unfindable.
    fn sync_cursor_icon(&mut self) {
        let icon = if self
            .inspector
            .as_ref()
            .is_some_and(crate::inspector::Inspector::wants_arrow_cursor)
        {
            CursorIcon::Default
        } else {
            match self.resize_edge {
                Some(dir) => CursorIcon::from(dir),
                None => self.view.cursor_icon(),
            }
        };
        if self.cursor_icon != icon {
            self.cursor_icon = icon;
            self.window.set_cursor(icon);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProfileBindingReconcile {
    AlreadyBound,
    Conflict,
    WaitForExactServerProfile,
    Bind,
}

fn clear_failed_connected_profile_start<T>(pending: &mut Option<T>) -> bool {
    pending.take().is_some()
}

fn connected_profile_is_open(
    current: Option<koru_core::ProfileId>,
    expected: koru_core::ProfileId,
) -> bool {
    current == Some(expected)
}

fn profile_binding_reconcile_action(
    marker: &crate::binding_markers::PendingProfileAccountBinding,
    local_account: Option<&str>,
    profiles: &[koru_agent_wire::AccountProfileRow],
) -> ProfileBindingReconcile {
    let stored = local_account.and_then(|value| value.strip_prefix("koru:"));
    if stored == Some(marker.new_identity_id.as_str()) {
        return ProfileBindingReconcile::AlreadyBound;
    }
    if stored != marker.expected_identity_id.as_deref() {
        return ProfileBindingReconcile::Conflict;
    }
    if profiles.iter().any(|row| {
        row.profile_id == marker.profile.to_storage()
            && row.koru_identity_id == marker.new_identity_id
    }) {
        ProfileBindingReconcile::Bind
    } else {
        ProfileBindingReconcile::WaitForExactServerProfile
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectedProfileCreationStage {
    CreatingLocally,
    AwaitingHostedProfile(koru_core::ProfileId),
    AwaitingAccountReturn(koru_core::ProfileId),
    AwaitingLocalBinding(koru_core::ProfileId),
}

impl ConnectedProfileCreationStage {
    fn after_local_creation(self, profile: koru_core::ProfileId) -> Self {
        match self {
            Self::CreatingLocally => Self::AwaitingHostedProfile(profile),
            later => later,
        }
    }

    fn after_hosted_ack(self, profile: koru_core::ProfileId) -> Self {
        match self {
            Self::AwaitingHostedProfile(expected) | Self::AwaitingAccountReturn(expected)
                if expected == profile =>
            {
                Self::AwaitingLocalBinding(profile)
            }
            other => other,
        }
    }

    fn completes_after_local_ack(self, profile: koru_core::ProfileId) -> bool {
        matches!(self, Self::AwaitingLocalBinding(expected) if expected == profile)
    }

    fn waiting_on_agent(self, agent_available: bool) -> Option<koru_core::ProfileId> {
        if agent_available {
            return None;
        }
        match self {
            Self::AwaitingHostedProfile(profile) | Self::AwaitingLocalBinding(profile) => {
                Some(profile)
            }
            Self::CreatingLocally | Self::AwaitingAccountReturn(_) => None,
        }
    }
}

struct PendingConnectedProfileCreation {
    owner_scope: u64,
    submission: Option<crate::account_ui::CommandSubmissionId>,
    generation: Option<u64>,
    source: WindowId,
    owner_display_name: String,
    profile_name: String,
    workspace_name: String,
    connect_account: bool,
    stage: ConnectedProfileCreationStage,
}

impl PendingConnectedProfileCreation {
    fn retry_command(&self, profile: koru_core::ProfileId) -> koru_agent_wire::AccountCommand {
        koru_agent_wire::AccountCommand::CreateProfile {
            profile_id: profile.to_storage(),
            owner_display_name: self.owner_display_name.clone(),
            name: self.profile_name.clone(),
            locale: Some("en".into()),
        }
    }
    fn accepts_admission(&self, admission: crate::account_ui::CommandAdmission) -> bool {
        self.submission == Some(admission.submission) && self.owner_scope == admission.owner_scope
    }
}

struct App<H: Host> {
    /// What makes a view. The one question the loop cannot answer itself.
    host: H,
    context: RenderContext,
    /// One vello renderer per **device**, shared by every window on it.
    ///
    /// `Renderer::new` compiles every pipeline vello has and there is no
    /// pipeline cache on DX12, so one per window meant paying that again for
    /// each window opened — measured at 7.6 s for a search window in a debug
    /// build. Nothing in a `Renderer` belongs to a surface.
    renderers: HashMap<usize, Renderer>,
    /// Every open window. Keyed by id because that is what winit hands back
    /// with each event.
    panes: HashMap<WindowId, Pane<H::V>>,
    /// The window currently previewing a tab dragged out of **another**
    /// window's strip, so the preview is cleared when the drag moves on. The
    /// pointer keeps its capture with the source for the whole gesture, so
    /// this is the shell's own memory — no event is ever addressed to the
    /// target.
    foreign_hover: Option<WindowId>,
    /// The macOS menu bar, once there is an `NSApplication` to hang it on.
    ///
    /// `None` on every other platform, and `None` here until the first window
    /// exists — AppKit has no application object before that.
    menubar: Option<crate::menubar::MenuBar>,
    /// When the periodic [`Self::persist_if_changed`] last ran.
    last_persist: Option<Instant>,
    /// When the idle content-burst boundary last ran. See [`BURST_TICK_EVERY`].
    last_burst_tick: Option<Instant>,
    /// Whether the application has been observed actually hidden since the last
    /// hide was asked for. Arms the unhide check — see its use in
    /// `about_to_wait`.
    seen_hidden: bool,
    /// Whether every window is hidden and recoverable through Koru Agent.
    ///
    /// **Not the same as having no windows.** The panes are all still here,
    /// holding their tabs and their scroll — hiding is `set_visible(false)`, not
    /// a close — which is what makes coming back instant and lossless.
    in_tray: bool,
    /// Set for the span of [`Self::close_for_update`]: the last window then
    /// quits like any other instead of asking "keep Koru ready?" or hiding to
    /// the tray. The agent is what keeps search ready through an update, and
    /// it reopens these windows afterwards — a window that hid instead of
    /// leaving kept the agent waiting its full deadline and the update never
    /// happened. The unsaved-work question is untouched; that refusal is the
    /// one the design wants.
    closing_for_update: bool,
    reminder_startup_facts: Option<koru_window::ReminderStartupFacts>,
    reminder_startup_reply:
        Option<mpsc::Receiver<koru_api_client::PatchReply<koru_settings::GlobalSettings>>>,
    reminder_startup_queue: ReminderStartupQueue,
    /// **The `global-search` row**, shared with the Agent's overlay. This
    /// window changes the height and never the position, and takes the row's
    /// current position back off the disk before it writes — see
    /// `SearchGeometry::reload_position`.
    search_geometry: koru_window::SearchGeometry,
    /// **The settings authority, off the event thread.** Everything the
    /// registry owns is read and written through here; nothing in this process
    /// opens `preferences.conf` any more. See [`crate::preferences`].
    settings_link: koru_api_client::GlobalLink,
    /// Whether [`Self::settings_link`] has delivered a section yet. The first
    /// one finishes what `restore` could only do against the registry's
    /// defaults — see [`View::adopt_startup_preferences`].
    settings_heard: bool,
    background_start: bool,
    /// Keys this Koru process has seen pressed and not yet released. They are
    /// copied into a destination pane before a search-result focus handoff.
    down_keys: HashSet<PhysicalKey>,

    /// **One dictation per process, however many windows are open.** There is
    /// one microphone, and the worker refuses a second session anyway — so
    /// holding the controller per pane would only mean discovering that later
    /// and less clearly.
    dictation: Dictation,
    /// **The physical key that began the hold**, captured at the press.
    ///
    /// A release is matched against this and never against a chord. Half a
    /// gesture is exactly where a layout can change under you — an input
    /// source switched mid-hold, a dead key, `Ctrl+I` arriving as
    /// `NamedKey::Tab` — and any of those would leave the logical key of the
    /// release naming something the press never said. `physical_key` is the
    /// one thing that cannot move, and it is only visible here, which is the
    /// same reason `Ctrl+Tab` is resolved in this file.
    /// **First use**: the consent step, the record it writes, and the download
    /// it authorises. One field because they are one situation — see
    /// [`crate::voice_install::VoiceSetup`].
    voice: crate::voice_install::VoiceSetup,
    dictation_key: Option<PhysicalKey>,
    /// Whether the live session began from `/dictate`, which has no key release
    /// of its own. Plain Enter finishes this kind of capture and is consumed;
    /// shortcut and toggle sessions keep the ordinary interruption rule.
    dictation_commanded: bool,
    /// Which window is dictating. What its actions are routed to, and what a
    /// focus loss is compared against.
    dictation_pane: Option<WindowId>,
    /// Agent-minted correlation for the one global chord currently routed
    /// into this process's existing local dictation controller.
    agent_dictation_route: Option<u64>,
    /// Last content-free lifecycle value sent for that route. Maintenance may
    /// drain the controller often; unchanged Listening is not another event.
    agent_dictation_last_state: Option<crate::AgentDictationState>,
    /// **Which window was last handed a listening status**, so the `None` that
    /// ends the session can be delivered to it.
    ///
    /// Not the same field as [`Self::dictation_pane`], and that is the point:
    /// `service_dictation` clears *that* one the moment a commit closes the
    /// composition, and a surface told nothing more would keep drawing a meter
    /// for a dictation that had finished — which is exactly the way
    /// `View::animating` stops being honest.
    dictation_shown: Option<WindowId>,

    /// Whether the "this workspace has stopped saving" report has been made.
    /// Latched so it is said once, and cleared when a save lands again — an
    /// unplugged drive that comes back and goes again reports both times.
    reported_write_failure: bool,

    /// How a socket thread wakes this loop. Kept so a workspace opened later —
    /// a switch, a profile change — inherits the same one.
    store_waker: koru_workspace::store::link::Waker,
    /// Hands out the window and view numbers a screen reader's locators are
    /// built from. Owned here rather than a process global, so two windows in
    /// one process cannot be handed the same number and two tests cannot share
    /// a counter.
    slots: crate::accessibility::Slots,
    wake_proxy: winit::event_loop::EventLoopProxy<Wake>,
    import_tx: mpsc::Sender<ImportMessage>,
    import_rx: mpsc::Receiver<ImportMessage>,
    delegate_folder_check: Option<(WindowId, mpsc::Receiver<crate::DelegateFolder>)>,
    /// Session-recording workers' answers (SESSIONRECORD). One channel serves
    /// the start and the finalize worker alike; the [`Wake::Recording`] user
    /// event ends the wait and the maintenance pass drains it.
    // Read only by the Windows capture workers (M31).
    #[cfg_attr(not(windows), allow(dead_code))]
    recording_tx: mpsc::Sender<crate::app::recording::Answer>,
    #[cfg_attr(not(windows), allow(dead_code))]
    recording_rx: mpsc::Receiver<crate::app::recording::Answer>,
    transfer_tx: mpsc::Sender<TransferJob>,
    transfer_rx: mpsc::Receiver<TransferResult>,
    selection_move_tx: mpsc::Sender<SelectionMoveJob>,
    selection_move_rx: mpsc::Receiver<SelectionMoveResult>,
    profile_cascade_tx: mpsc::Sender<ProfileCascadeCompletion>,
    profile_cascade_rx: mpsc::Receiver<ProfileCascadeCompletion>,
    profile_cascades_in_flight: HashSet<koru_core::ProfileId>,
    profile_removal_tx: mpsc::Sender<ProfileRemovalCleanup>,
    profile_removal_rx: mpsc::Receiver<ProfileRemovalCleanup>,
    profile_removals_in_flight: HashSet<koru_core::ProfileId>,
    hosted_profile_removals: HashMap<koru_core::ProfileId, HostedProfileRemovalSource>,
    profile_offline_tx: mpsc::Sender<ProfileOfflineCompletion>,
    profile_offline_rx: mpsc::Receiver<ProfileOfflineCompletion>,
    profile_offline_in_flight: HashSet<koru_core::ProfileId>,
    profile_bindings_in_flight: HashSet<koru_core::ProfileId>,
    connected_profile_creation_tx: mpsc::Sender<OnboardingCreation>,
    connected_profile_creation_rx: mpsc::Receiver<OnboardingCreation>,
    pending_connected_profile_creation: Option<PendingConnectedProfileCreation>,
    embedded_profile_restore_tx: mpsc::Sender<EmbeddedProfileRestore>,
    embedded_profile_restore_rx: mpsc::Receiver<EmbeddedProfileRestore>,
    /// Target documents with a selection move in flight, so a double-commit in
    /// the picker cannot write the same content twice.
    selection_moves_in_flight: HashSet<koru_core::DocumentId>,
    /// Exact routes currently queued or running. A second identical gesture is
    /// stale input; the reverse route remains distinct so Undo can queue behind
    /// the original move on the serial worker.
    transfers_in_flight: HashSet<(koru_core::DocumentId, WorkspaceId, WorkspaceId)>,
    /// Off-loop image loading: worker pool, in-flight dedupe, and the bounded
    /// set of known misses retried when a device reconnects.
    image_loader: crate::images::ImageLoader,
    clipboard_image_tx: mpsc::Sender<ClipboardImageResult>,
    clipboard_image_rx: mpsc::Receiver<ClipboardImageResult>,
    clipboard_image_busy: bool,
    pending_clipboard_image_pastes: VecDeque<Vec<(WindowId, PasteTarget)>>,
    import_busy: bool,
    import_source: Option<WindowId>,
    /// The running content import's format token, for the completion event —
    /// the summary the worker sends back does not carry it.
    import_format: Option<&'static str>,
    /// The accepted workspace-import preview's document count, for the same
    /// reason: the apply's result message carries the workspace alone.
    import_workspace_count: Option<usize>,
    /// Cooperative cancellation for the active content import. Parsing code
    /// checks it at safe boundaries; Store writes check it before every entry.
    import_cancel: Option<Arc<koru_import::CancellationToken>>,
    /// Stop feedback remains visible for three seconds even when the worker
    /// unwinds sooner. The worker remains the authority for when another import
    /// may begin.
    import_cancel_hide_due: Option<Instant>,
    import_cancel_worker_done: bool,
    pending_import: Option<InspectedImport>,
    /// Workspaces with a durable soft-delete request running off the UI thread.
    /// Prevents a double click from issuing competing Store mutations.
    deleting_workspaces: HashSet<WorkspaceId>,
    /// `/attach` waits for the generic file importer to publish its file entry.
    pending_attachment: Option<(WindowId, NodeId)>,
    /// Importable paths coalesced from winit's one-event-per-file drop stream.
    /// Batches wait here while any other import is active rather than being
    /// silently discarded by `import_busy`.
    pending_content_drops: VecDeque<(WindowId, &'static str, Vec<PathBuf>)>,
    /// Somebody shared into Koru and the inbox has not been looked at since.
    /// Set at startup and on every activation; cleared only by a drain that
    /// found the inbox empty, so a share that arrives while the importer is
    /// busy is not lost between two focus events (ADR-416).
    share_drain_due: bool,
    /// The inbox entry the running content import came from. **The entry is
    /// removed on success and kept on failure** — it is the only copy of what
    /// was shared, and the next activation is a free retry. `import_busy`
    /// makes one at a time true, which is why this is one slot and not a map.
    share_import_in_flight: Option<koru_share_inbox::Entry>,
    /// A coalesced, off-event-loop refresh of the Store profile catalog.

    /// Documents whose last tab has closed but which were still the active one
    /// when the pane said so. Retried by [`opened_document`], the only moment
    /// one of them stops being active.
    ///
    /// [`opened_document`]: Self::opened_document
    releasing: Vec<koru_core::DocumentId>,

    /// The window whose geometry is remembered — the first one opened.
    ///
    /// Not the one whose closing exits: the app runs until the *last* window
    /// closes. `None` once it has been closed, which leaves the remembered
    /// geometry at whatever it last was rather than adopting a document
    /// window's shape in its place.
    primary: Option<WindowId>,
    scene: Scene,
    /// The document, its persistence and its undo history — shared by every
    /// view, because all three belong to the document rather than to a view.
    runtime: ProfileRuntime,
    /// Content-addressed blob store for images. `None` for an in-memory
    /// workspace, where images cannot be stored.
    /// Decoded images keyed by blob hash, filled by the shell and read by views
    /// at draw time. The bytes never enter the CRDT (ADR-12).
    images: ImageCache,
    mods: ModifiersState,
    blink_origin: Instant,
    /// Which half of the blink the last frame showed, so an idle loop can tell
    /// whether waking up has anything to draw.
    caret_phase: Option<bool>,
    /// Where a frame's time went. Inert unless `KORU_FRAME_STATS` is set.
    frames: framestats::Frames,
    /// Window geometry and toggles, restored at startup and written on exit.
    /// What this process last wrote to each of the three settings files, so an
    /// idle tick writes none of them when nothing in that file moved.
    /// Display zoom, the source of truth. Mirrored into `theme::zoom()` for the
    /// draw side. Applied as a multiplier on the root transform (`scale * zoom`),
    /// with the viewport and pointer divided by `zoom` so the views still
    /// believe they live in unscaled logical pixels — the same trick already
    /// used for the OS DPI `scale`.
    zoom: f32,
    /// The first window's view, built before there is a window to put it in.
    pending: Option<H::V>,
    /// What the titlebar says, minus the peer suffix.
    name: String,
    /// Last theme pushed to the OS titlebar, so it is only set on change.
    titlebar_dark: Option<bool>,
    /// Which update file this instance owns, and which settings file. Two
    /// names, because they are two things — see [`session::Instance`].
    instance: session::Instance,
    /// The profile this window belongs to: who you are, and which workspaces
    /// you own. One per process, like the workspace — a second profile is a
    /// second window, which is a second process.
    /// Store-backed rows shown by the profile switcher. No disk registry rows.
    /// Profile-scoped IPC inbox for search results opened by another Koru
    /// process on this machine. Unlike the legacy taskbar pipe, its messages
    /// carry workspace and lifecycle routing as well as the node.
    /// Which workspace is open.
    /// False only for the welcome state after the last workspace is deleted.
    /// `current` then identifies an in-memory placeholder and must never be
    /// persisted or registered as Store content.
    /// Requests pushed in by the Koru Agent rather than by a pane — a result
    /// chosen in the machine-wide overlay. Drained by
    /// [`serve_one_pane_request`](Self::serve_one_pane_request).
    from_the_agent: Vec<PaneRequest>,
    last_agent_pins: Vec<(String, koru_core::DocumentId, koru_core::WorkspaceId, bool)>,
    /// Where this run's content lives. Decided before the app exists — see
    /// [`crate::startup`] — and held because the settings file lives in it.
    /// `--root`, when one was named: the public Gateway rendezvous to use
    /// instead of the channel's shared one. Gateway derives the Store root.
    ///
    /// **Held rather than re-read from `std::env::args` at each use.** Not
    /// because re-reading would disagree — argv does not change — but because
    /// the three worker threads that need it (import inspect, import build,
    /// export) clone it before they spawn, and one field is the only way to see
    /// at a glance that every store call in this window shares a root.
    gateway_root: Option<PathBuf>,
    /// Whether the set of blobs the document references may have changed since
    /// [`decode_missing_images`] last looked.
    ///
    /// Set by anything that can add an image node — a local edit, a peer's, a
    /// paste, a workspace swap — and by construction, since the first pass has
    /// to happen. Everything else is a wake with nothing new in it, and this is
    /// what stops those costing a walk of the document.
    ///
    /// [`decode_missing_images`]: Self::decode_missing_images
    images_dirty: bool,
    /// The active document whose blob references were last scanned.
    ///
    /// Tabs are window-scoped and can stay active while Explore shows another
    /// workspace. Keeping the scope makes a tab change re-scan its document and
    /// prevents decoded bytes from one workspace masking a missing local copy
    /// in another.
    image_scope: Option<(WorkspaceId, koru_core::DocumentId)>,
    /// The workspace tag table as last read from the system document —
    /// pushed into every pane so attribute chips and the tag chooser resolve
    /// ids without a view ever reaching the store. Refreshed when
    /// `Workspace::take_tags_changed` says the table moved.
    tag_table: Vec<koru_core::system::TagEntry>,
    /// The workspace **list table**, read and pushed on the same turn and by
    /// the same rule — the tasks rail resolves a `koru.list` value to a name
    /// and a swatch without ever reaching the store, because names are the
    /// workspace's and only counts come off the wire.
    list_table: Vec<koru_core::system::ListEntry>,
    /// When [`poll_pane_context`] last requested the Store catalog. `None` until
    /// the first poll, so a window shows its cached switcher rows immediately.
    ///
    /// [`poll_pane_context`]: Self::poll_pane_context
    last_context_poll: Option<Instant>,
    /// Last time we compared paint to `system_theme` while on System preference.
    last_os_theme_poll: Option<Instant>,
    /// What the accumulated [`timing`] spans are measuring, and from when.
    ///
    /// Drained by the next `present()`, because the honest end of "opening a
    /// document" is the frame reaching the glass rather than the last function
    /// returning. A workspace switch re-arms it, so the same machinery measures
    /// both — hence the `Instant`: a cold launch counts from process start, a
    /// switch from the moment it was asked for.
    pending_report: Option<(String, Instant)>,
    /// The "this installation is being updated" guard, held for the life of the
    /// process. **Still the shell's** even though updating is the agent's now:
    /// what it protects is *this* process starting on files another process is
    /// in the middle of replacing, which is a fact about this executable.
    ///
    /// **Never read, and that is the whole of it** — it is a file lock, and the
    /// lock is the value being alive. It used to be read as well, for
    /// `other_instances` when this process installed its own update; the agent
    /// counts instances now. Dropping the field would release the lease.
    #[allow(dead_code)]
    update_lease: Option<koru_update::InstanceLease>,
    health_signaled: bool,
}

/// The live identity-free shell. It owns only process/window lifetime and the
/// services needed to remain a Koru window; no Profile/Workspace value can be
/// reached from this type.
struct OnboardingLive {
    window: Arc<Window>,
    surface: RenderSurface<'static>,
}

/// A creation worker may return to the editable form only while no Store write
/// has been acknowledged. Once a Profile exists, the worker either reopens
/// that exact durable state or asks the event-loop thread to stop with the
/// ordinary fatal dialog; it never submits Profile creation again.
enum OnboardingCreation {
    Opened(koru_workspace::fromstore::OpenedFromStore),
    Retryable { message: String },
    RestoreFailed { message: String },
    CommittedFailure { message: String },
}

struct EmbeddedProfileRestore {
    source: WindowId,
    result: OnboardingCreation,
}

struct OnboardingApp<H: Host> {
    host: H,
    name: String,
    profile_dir: PathBuf,
    gateway_root: Option<PathBuf>,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
    live: Option<OnboardingLive>,
    /// **This window's native accessibility provider and its semantic
    /// identity.**
    ///
    /// `Option` because the window is: the provider is installed inside
    /// [`OnboardingApp::open`], between `create_window` and the first frame
    /// that reveals it, and it goes with the window. See
    /// `koru_window::accessibility` for why that order is a platform trap
    /// rather than a preference, and `crate::onboarding::accessibility` for
    /// what gets published.
    access: Option<crate::accessibility::PaneAccessibility>,
    /// Hands out this process's window and view numbers, and is **carried into
    /// [`App`]** when a Profile is created — so the workspace window that
    /// replaces this one cannot be handed the number this one used. A slot is
    /// handed out once and never reused; that is what lets a request for a
    /// window that is gone be refused rather than routed.
    slots: crate::accessibility::Slots,
    /// **The F12 UI inspector over the first-run window, its native panel and
    /// the panel's remembered geometry.** The same tool the workspace window
    /// gets: `OnboardingApp` and `App` share the [`crate::inspector`]
    /// machinery and the [`inspector_window`] panel, but each keeps its own
    /// glue — they have no common window collection, the same reason
    /// `seat_accessibility` is deliberately a second call site.
    inspector: Option<crate::inspector::Inspector>,
    inspector_window: Option<inspector_window::NativeInspector>,
    inspector_geometry: Option<crate::inspector::Geometry>,
    /// Tracked from `WindowEvent::Focused`, as `Pane::focused` is: the F12/F11
    /// chords are only this window's to answer while it has focus.
    focused: bool,
    gpu: Option<GpuSource>,
    context: Option<RenderContext>,
    renderers: HashMap<usize, Renderer>,
    scene: Scene,
    view: crate::onboarding::OnboardingView,
    pointer: (f32, f32),
    mods: ModifiersState,
    last_caption_click: Option<(Instant, f32, f32)>,
    creation_tx: mpsc::Sender<OnboardingCreation>,
    creation_rx: mpsc::Receiver<OnboardingCreation>,
    first_minutes: Option<(String, String, String)>,
    connect_created_profile: bool,
    pending_connected_open: Option<koru_workspace::fromstore::OpenedFromStore>,
    awaiting_hosted_profile: bool,
    connected_owner_scope: u64,
    connected_submission: Option<crate::account_ui::CommandSubmissionId>,
    connected_generation: Option<u64>,
    connected_paused: bool,
    awaiting_local_binding: bool,
    /// Markers for bindings asked during onboarding; see `binding_markers`.
    pending_bindings: crate::binding_markers::PendingBindings,
    /// A signed-in notification is level-triggered: every account command
    /// publishes the current state again. Without this edge guard, completing
    /// `ListProfiles` immediately requested another list and its response reset
    /// an in-progress Profile form back to step one.
    signed_in_profiles_requested: bool,
    /// The installation must remain protected while onboarding is open too.
    #[allow(dead_code)]
    update_lease: Option<koru_update::InstanceLease>,
}

fn needs_onboarding_window(has_window: bool) -> bool {
    !has_window
}

fn take_signed_in_profile_request(
    requested: &mut bool,
    stage: koru_agent_wire::AccountStage,
) -> bool {
    if stage != koru_agent_wire::AccountStage::SignedIn {
        *requested = false;
        return false;
    }
    if *requested {
        return false;
    }
    *requested = true;
    true
}

fn take_created_profile_connection(connect_created_profile: &mut bool) -> bool {
    std::mem::take(connect_created_profile)
}

fn live_profile_ids_for_settings(
    launch: &koru_workspace::fromstore::StoreLaunch,
) -> HashSet<koru_core::ProfileId> {
    match launch {
        koru_workspace::fromstore::StoreLaunch::NeedsProfile => HashSet::new(),
        koru_workspace::fromstore::StoreLaunch::Open(opened) => {
            opened.profiles.iter().map(|profile| profile.id).collect()
        }
    }
}

#[derive(Clone, Copy)]
enum OnboardingCheckpoint {
    BeforeProfile,
    Profile {
        profile: koru_core::ProfileId,
    },
    Workspace {
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
    },
}

impl OnboardingCheckpoint {
    /// Keep the retry decision tied to the durable write boundary. The
    /// committed closure receives only Store-acknowledged ids.
    fn resolve<T>(
        self,
        retryable: impl FnOnce() -> T,
        committed: impl FnOnce(koru_core::ProfileId, Option<koru_core::WorkspaceId>) -> T,
    ) -> T {
        match self {
            Self::BeforeProfile => retryable(),
            Self::Profile { profile } => committed(profile, None),
            Self::Workspace { profile, workspace } => committed(profile, Some(workspace)),
        }
    }
}

/// The one dispatch boundary between identity-free shell lifetime and ordinary
/// editing. `App` remains an open-only owner of a non-optional
/// [`ProfileRuntime`], so onboarding cannot compile a path to editing state.
/// **Both variants are boxed, and must stay boxed.** `App` is ~83 KB and the
/// onboarding hand-over moves the whole state by value through several frames
/// of the main thread (`mem::replace`, `finish_profile_creation`,
/// `new_with_ack`) while the first `MainView` (~75 KB) is being built. Inline,
/// those copies overflowed the main thread's 1 MB Windows stack the moment
/// *Finish* was pressed: the Profile was saved, and the window simply vanished
/// with `0xc00000fd`. Boxed, the hand-over moves two pointers.
enum ContentState<H: Host> {
    Onboarding(Box<OnboardingApp<H>>),
    Open(Box<App<H>>),
    /// Transient owner while a durable onboarding acknowledgement is installed.
    Changing,
}

impl<H: Host> ContentState<H> {
    fn should_stop_the_store_on_exit(&self) -> bool {
        match self {
            Self::Onboarding(app) => !app.host.agent_available(),
            Self::Open(app) => app.should_stop_the_store_on_exit(),
            Self::Changing => false,
        }
    }

    fn gateway_root(&self) -> Option<&Path> {
        match self {
            Self::Onboarding(app) => app.gateway_root.as_deref(),
            Self::Open(app) => app.gateway_root.as_deref(),
            Self::Changing => None,
        }
    }

    /// Install a creation worker's strictly reopened durable state. Retryable
    /// and post-commit failures are handled before this consumes onboarding.
    fn finish_profile_creation(
        self,
        opened: koru_workspace::fromstore::OpenedFromStore,
    ) -> Option<Self> {
        let Self::Onboarding(app) = self else {
            return Some(self);
        };
        let OnboardingApp {
            host,
            name,
            profile_dir,
            proxy,
            gpu,
            context,
            renderers,
            live,
            access,
            slots,
            update_lease,
            first_minutes,
            ..
        } = *app;
        // The window is going, and so is its provider. Anything a client had
        // queued for it is refused here rather than surviving into a window
        // that never published it — see `crate::accessibility::lifecycle`.
        if let Some(mut access) = access {
            access.retire();
        }
        drop(live);
        let gpu = match context {
            Some(context) => GpuSource::Ready(Warmed {
                context,
                renderer: renderers.into_iter().next(),
            }),
            None => gpu.expect("onboarding retains warmup until its first window"),
        };
        match App::new_with_ack(
            host,
            &name,
            profile_dir,
            gpu,
            proxy,
            Some(opened),
            update_lease,
            slots,
        ) {
            Some(mut next) => {
                if let (Self::Open(app), Some((owner, profile, workspace))) =
                    (&mut next, first_minutes)
                    && let Some(view) = app.pending.as_mut()
                {
                    let _ = view.show_first_minutes(&owner, &profile, &workspace);
                }
                Some(next)
            }
            // Startup has already shown the concrete fatal error. This is not a
            // failed Profile submission: the durable Profile remains in Store
            // and the next launch opens it normally.
            None => None,
        }
    }
}

impl<H: Host> ApplicationHandler<Wake> for ContentState<H> {
    fn user_event(&mut self, event_loop: &ActiveEventLoop, wake: Wake) {
        let created = match self {
            Self::Onboarding(app) => {
                app.user_event(event_loop, wake);
                app.take_creation()
            }
            Self::Open(app) => {
                app.user_event(event_loop, wake);
                None
            }
            Self::Changing => None,
        };
        let Some(result) = created else { return };
        match result {
            OnboardingCreation::Opened(opened) => {
                if let Self::Onboarding(app) = self
                    && take_created_profile_connection(&mut app.connect_created_profile)
                {
                    if app.host.agent_available() {
                        app.begin_connected_profile(opened);
                        app.request_redraw();
                        return;
                    }
                }
                let state = std::mem::replace(self, Self::Changing);
                match state.finish_profile_creation(opened) {
                    Some(next) => {
                        *self = next;
                        // The event loop was already resumed while onboarding
                        // owned this process. The newly installed open App will
                        // not receive another startup callback, so hand it the
                        // lifecycle event that creates, reveals and registers
                        // its first workspace window now.
                        self.resumed(event_loop);
                    }
                    None => event_loop.exit(),
                }
            }
            OnboardingCreation::Retryable { message } => {
                if let Self::Onboarding(app) = self {
                    app.view.creation_failed(message);
                    app.request_redraw();
                }
            }
            OnboardingCreation::RestoreFailed { message } => {
                if let Self::Onboarding(app) = self {
                    app.view.restore_failed(message);
                    app.request_redraw();
                }
            }
            OnboardingCreation::CommittedFailure { message } => {
                // The Profile has already been acknowledged. Staying on the
                // creation form could duplicate it; the next ordinary launch
                // will discover the durable catalog state instead.
                startup::fatal(&message);
                event_loop.exit();
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        match self {
            Self::Onboarding(app) => app.resumed(event_loop),
            Self::Open(app) => app.resumed(event_loop),
            Self::Changing => {}
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        match self {
            Self::Onboarding(app) => app.window_event(event_loop, id, event),
            Self::Open(app) => app.window_event(event_loop, id, event),
            Self::Changing => {}
        }
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        match self {
            Self::Onboarding(_) => {}
            Self::Open(app) => app.suspended(event_loop),
            Self::Changing => {}
        }
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        match self {
            Self::Onboarding(_) => {
                crate::events_host::session_ended_and_flush(std::time::Duration::from_secs(2))
            }
            Self::Open(app) => app.exiting(event_loop),
            Self::Changing => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self {
            Self::Onboarding(app) => app.about_to_wait(event_loop),
            Self::Open(app) => app.about_to_wait(event_loop),
            Self::Changing => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }
}

fn roadmap_template_files(slug: &str) -> koru_store_core::Result<Vec<PathBuf>> {
    if slug.is_empty() || !slug.bytes().all(|byte| byte.is_ascii_lowercase()) {
        return Err(koru_store_core::Error::new(
            koru_store_core::Code::InvalidRequest,
            "invalid Workspace template name",
        ));
    }
    let mut roots = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(beside) = exe.parent()
    {
        roots.push(beside.join("docs/templates/roadmap"));
        roots.push(beside.join("../Resources/docs/templates/roadmap"));
    }
    if cfg!(debug_assertions) {
        roots.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/templates"));
    }
    let directory = roots
        .into_iter()
        .map(|root| root.join(slug))
        .find(|candidate| candidate.is_dir())
        .ok_or_else(|| {
            koru_store_core::Error::new(
                koru_store_core::Code::InvalidRequest,
                format!("packaged Workspace template {slug} is missing"),
            )
        })?;
    let mut files = std::fs::read_dir(&directory)
        .map_err(|error| {
            koru_store_core::Error::new(
                koru_store_core::Code::Internal,
                format!("could not read {}: {error}", directory.display()),
            )
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("md"))
        .collect::<Vec<_>>();
    files.sort();
    if files.is_empty() {
        return Err(koru_store_core::Error::new(
            koru_store_core::Code::InvalidRequest,
            format!("packaged Workspace template {slug} contains no Markdown files"),
        ));
    }
    Ok(files)
}

fn retryable_template_failure(template: &str, error: impl std::fmt::Display) -> OnboardingCreation {
    OnboardingCreation::Retryable {
        message: format!(
            "This Workspace template isn't available\nKoru couldn't read the packaged files for {template}. Choose another template or repair this Koru installation. ({error})"
        ),
    }
}

fn retryable_profile_failure(error: impl std::fmt::Display) -> OnboardingCreation {
    OnboardingCreation::Retryable {
        message: format!("Koru couldn't save this Profile safely: {error}"),
    }
}

fn resolve_onboarding_failure(
    checkpoint: OnboardingCheckpoint,
    root: Option<&Path>,
    error: impl std::fmt::Display,
) -> OnboardingCreation {
    let message = error.to_string();
    checkpoint.resolve(
        || retryable_profile_failure(&message),
        |profile, workspace| reconcile_onboarding_commit(root, profile, workspace, &message),
    )
}
/// Resolve a failure after Profile acknowledgement by reopening the exact
/// durable ids. Success enters that state (possibly with fewer starter
/// documents); failure is terminal because showing Create again could mint a
/// duplicate Profile.
fn reconcile_onboarding_commit(
    root: Option<&Path>,
    profile: koru_core::ProfileId,
    workspace: Option<koru_core::WorkspaceId>,
    cause: impl std::fmt::Display,
) -> OnboardingCreation {
    let reopened = koru_workspace::fromstore::open_from_store_profile_preferring_empty(
        koru_workspace::fromstore::segment(),
        root,
        profile,
        workspace,
    );
    match reopened {
        Ok(koru_workspace::fromstore::StoreLaunch::Open(opened)) => {
            OnboardingCreation::Opened(opened)
        }
        Ok(koru_workspace::fromstore::StoreLaunch::NeedsProfile) => {
            OnboardingCreation::CommittedFailure {
                message: format!(
                    "Koru saved the new Profile but could not reopen it safely. Restart Koru to recover the saved Profile.\n\nOriginal error: {cause}\nRecovery error: the acknowledged Profile was absent"
                ),
            }
        }
        Err(recovery) => OnboardingCreation::CommittedFailure {
            message: format!(
                "Koru saved the new Profile but could not reopen it safely. Restart Koru to recover the saved Profile.\n\nOriginal error: {cause}\nRecovery error: {recovery}"
            ),
        },
    }
}

fn create_onboarding_profile(
    root: Option<&Path>,
    profile_name: &str,
    workspace_name: &str,
    template: &str,
    profile_icon: Option<&str>,
    profile_color: Option<&str>,
    workspace_icon: Option<&str>,
    workspace_color: Option<&str>,
) -> OnboardingCreation {
    use koru_import::Importer;

    let files = match roadmap_template_files(template) {
        Ok(files) => files,
        Err(error) => return retryable_template_failure(template, error),
    };
    // `inspect` reads and parses every packaged file before the first durable
    // write. A malformed template disables only that choice and cannot leave a
    // half-created Profile.
    let prepared = match koru_import_markdown::MarkdownImporter.inspect(
        koru_import::ImportSource::Files(files),
        &koru_import::NoProgress,
    ) {
        Ok(prepared) => prepared,
        Err(error) => return retryable_template_failure(template, error),
    };

    let segment = koru_workspace::fromstore::segment();
    let profile = match koru_workspace::fromstore::create_local_profile_with_appearance(
        segment,
        root,
        profile_name,
        profile_icon,
        profile_color,
    ) {
        Ok(profile) => profile,
        Err(error) => {
            return resolve_onboarding_failure(OnboardingCheckpoint::BeforeProfile, root, error);
        }
    };
    let workspace =
        match koru_workspace::fromstore::create_workspace(segment, root, profile, workspace_name) {
            Ok(workspace) => workspace,
            Err(error) => {
                return resolve_onboarding_failure(
                    OnboardingCheckpoint::Profile { profile },
                    root,
                    error,
                );
            }
        };
    if let Err(error) = koru_workspace::fromstore::set_workspace_appearance(
        segment,
        root,
        workspace,
        workspace_icon,
        workspace_color,
        false,
        false,
    ) {
        return resolve_onboarding_failure(
            OnboardingCheckpoint::Workspace { profile, workspace },
            root,
            error,
        );
    }
    let client = match client_at(root) {
        Ok(client) => client,
        Err(error) => {
            return resolve_onboarding_failure(
                OnboardingCheckpoint::Workspace { profile, workspace },
                root,
                error,
            );
        }
    };
    let store_profile = koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid());
    let store_workspace = koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid());
    let blobs = StoreBlobSink {
        client: client.clone(),
        workspace: store_workspace,
    };
    let occupied_titles = std::collections::BTreeSet::new();
    let context = koru_import::ImportContext {
        target_profile: Some(store_profile),
        target_workspace: Some(store_workspace),
        occupied_titles: &occupied_titles,
        blobs: &blobs,
        tags: &koru_import::NoTagSink,
    };
    let product = match prepared.build(
        koru_import::ImportOptions {
            identity: koru_import::IdentityMode::Copy,
            accept_warnings: true,
        },
        &context,
        &koru_import::NoProgress,
    ) {
        Ok(product) => product,
        Err(error) => {
            return resolve_onboarding_failure(
                OnboardingCheckpoint::Workspace { profile, workspace },
                root,
                error,
            );
        }
    };
    let koru_import::ImportProduct::Documents(content) = product else {
        return resolve_onboarding_failure(
            OnboardingCheckpoint::Workspace { profile, workspace },
            root,
            "Workspace template produced the wrong import kind",
        );
    };
    for document in content.documents {
        if let Err(error) = client.import_document_at_with_mime(
            store_workspace,
            document.id,
            &document.title,
            &document.kind,
            document.mime.as_deref(),
            document.parent,
            document.snapshot,
        ) {
            return resolve_onboarding_failure(
                OnboardingCheckpoint::Workspace { profile, workspace },
                root,
                error,
            );
        }
    }
    match koru_workspace::fromstore::open_from_store_profile_preferring_empty(
        segment,
        root,
        profile,
        Some(workspace),
    ) {
        Ok(koru_workspace::fromstore::StoreLaunch::Open(opened)) => {
            OnboardingCreation::Opened(opened)
        }
        Ok(koru_workspace::fromstore::StoreLaunch::NeedsProfile) => {
            OnboardingCreation::CommittedFailure {
                message: "Koru saved the new Profile but could not reopen it safely. Restart Koru to recover the saved Profile."
                    .to_owned(),
            }
        }
        Err(error) => resolve_onboarding_failure(
            OnboardingCheckpoint::Workspace { profile, workspace },
            root,
            error,
        ),
    }
}

/// Which `profile.*` event changes what the Profile catalog would answer.
///
/// Matched by name rather than subscribed to individually because the events
/// carry no payload worth reading: every one of them means "read the profiles
/// again", and the id travels as an attr rather than in the envelope's scope
/// (a profile has no home there), so there is nothing here to route on.
/// `profile.dictionary.changed` is deliberately absent — it belongs to the
/// spelling overlay, and refetching the catalog for it is work nobody asked
/// for.
fn profile_catalog_event(name: &str) -> bool {
    matches!(
        name,
        "profile.created" | "profile.renamed" | "profile.appearance_changed"
    )
}

fn pane_profile_summary(
    mut live: koru_core::ProfileSummary,
    catalog: Option<&koru_core::ProfileSummary>,
) -> koru_core::ProfileSummary {
    if let Some(catalog) = catalog {
        live.icon = catalog.icon.clone();
        live.color = catalog.color.clone();
        // Account attribution belongs to Store, not the Profile CRDT summary.
        live.account = catalog.account.clone();
    }
    live.folder.clear();
    live
}

fn restore_onboarding_profile(root: Option<&Path>, raw_profile: &str) -> OnboardingCreation {
    let Some(profile) = koru_core::ProfileId::parse(raw_profile) else {
        return OnboardingCreation::RestoreFailed {
            message: "Koru couldn't identify that Profile. Nothing was changed.".into(),
        };
    };
    // Account provisioning deliberately creates the catalog row before the
    // Profile identity CRDT arrives. That row is already active and safe to
    // open as an empty Profile; asking the Store to "restore" it instead
    // validates bytes which, by definition, have not arrived yet. Open first.
    // Only an actually absent/removed row takes the retained-content restore
    // path below.
    match koru_workspace::fromstore::open_from_store_profile_preferring_empty(
        koru_workspace::fromstore::segment(),
        root,
        profile,
        None,
    ) {
        Ok(koru_workspace::fromstore::StoreLaunch::Open(opened)) => {
            return OnboardingCreation::Opened(opened);
        }
        Ok(koru_workspace::fromstore::StoreLaunch::NeedsProfile) => {}
        Err(error) if error.code() == koru_store_core::errors::Code::ProfileNotFound => {}
        Err(error) => {
            return OnboardingCreation::RestoreFailed {
                message: format!("Koru couldn't open that Profile safely: {error}"),
            };
        }
    }
    let client = match client_at(root) {
        Ok(client) => client,
        Err(error) => {
            return OnboardingCreation::RestoreFailed {
                message: format!("Koru couldn't reach the local Store: {error}"),
            };
        }
    };
    let store_profile = koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid());
    if let Err(error) = client.store().restore_profile_local(store_profile) {
        return OnboardingCreation::RestoreFailed {
            message: format!(
                "Koru couldn't restore the retained Profile safely. Nothing was changed: {error}"
            ),
        };
    }
    match koru_workspace::fromstore::open_from_store_profile_preferring_empty(
        koru_workspace::fromstore::segment(),
        root,
        profile,
        None,
    ) {
        Ok(koru_workspace::fromstore::StoreLaunch::Open(opened)) => {
            OnboardingCreation::Opened(opened)
        }
        Ok(koru_workspace::fromstore::StoreLaunch::NeedsProfile) => {
            OnboardingCreation::RestoreFailed {
                message: "Koru restored the Profile but could not open it yet. Try again or restart Koru."
                    .into(),
            }
        }
        Err(error) => OnboardingCreation::RestoreFailed {
            message: format!("Koru restored the Profile but could not open it: {error}"),
        },
    }
}
impl<H: Host> OnboardingApp<H> {
    fn request_signed_in_profiles_once(&mut self) {
        if take_signed_in_profile_request(
            &mut self.signed_in_profiles_requested,
            self.host.account_state().stage,
        ) {
            self.host.account_command_owned(
                self.view.account_scope(),
                koru_agent_wire::AccountCommand::ListProfileChoices,
            );
        }
    }

    fn open(&mut self, event_loop: &ActiveEventLoop) {
        if !needs_onboarding_window(self.live.is_some()) {
            return;
        }
        let warmed = self
            .gpu
            .take()
            .expect("onboarding warmup is consumed once")
            .take();
        self.context = Some(warmed.context);
        self.renderers = warmed.renderer.into_iter().collect();

        let mut attributes = Window::default_attributes()
            .with_title(self.name.clone())
            .with_visible(false)
            // The welcome split has to continue through the title area. On
            // macOS the native traffic-light titlebar remains platform-owned;
            // elsewhere onboarding draws the same compact direct controls as
            // the rest of Koru's custom-caption windows.
            .with_decorations(cfg!(target_os = "macos"))
            .with_inner_size(winit::dpi::LogicalSize::new(1000.0, 680.0))
            // The shared emoji picker is 420 px wide and about 590 px tall;
            // keep its 16 px modal gutter reachable without clipping.
            .with_min_inner_size(winit::dpi::LogicalSize::new(452.0, 640.0))
            .with_theme(window_theme_for_pref());
        if let Some(icon) = self.host.window_icon().or_else(app_icon) {
            attributes = attributes.with_window_icon(Some(icon));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                startup::fatal(&format!(
                    "Koru could not open its window:

{error}"
                ));
                event_loop.exit();
                return;
            }
        };
        self.view.set_custom_caption(!cfg!(target_os = "macos"));
        self.view.set_maximized(window.is_maximized());
        if cfg!(target_os = "macos") {
            frame::apply_caption(&window, theme::p().bg, theme::p().text);
        } else {
            frame::apply(&window, theme::p().panel_border, window.is_maximized());
        }
        let size = window.inner_size();
        let context = self.context.as_mut().expect("set above");
        let mut surface = match pollster::block_on(context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            PRESENT_MODE,
        )) {
            Ok(surface) => surface,
            Err(error) => {
                startup::fatal(&format!(
                    "Koru could not initialise its onboarding surface:

{error}"
                ));
                event_loop.exit();
                return;
            }
        };
        surface.config.desired_maximum_frame_latency = 1;
        context.set_present_mode(&mut surface, PRESENT_MODE);
        let dev_id = surface.dev_id;
        if let std::collections::hash_map::Entry::Vacant(entry) = self.renderers.entry(dev_id) {
            match koru_window::build_renderer(&context.devices[dev_id].device) {
                Ok(renderer) => {
                    entry.insert(renderer);
                }
                Err(message) => {
                    startup::fatal(&format!(
                        "Koru could not initialise its renderer: {message}"
                    ));
                    event_loop.exit();
                    return;
                }
            }
        }
        let scale = window.scale_factor() as f32;
        self.view
            .set_viewport(size.width as f32 / scale, size.height as f32 / scale);
        // **Before the first reveal, and this is the only reveal path.**
        // `Adapter::with_direct_handlers` panics on an already-visible window,
        // and the redraw below is what makes this one visible — the single
        // `set_visible(true)` this window has, guarded by `is_visible` inside
        // `redraw`. `open` itself runs once, behind `needs_onboarding_window`.
        // See `koru_window::accessibility`.
        self.access = Some(self.seat_accessibility(event_loop, &window));
        self.live = Some(OnboardingLive { window, surface });
        // A hidden window receives no WM_PAINT on Windows, so a requested
        // redraw would never deliver the first frame that makes it visible.
        // Paint synchronously, as `App::reveal_pane` does for workspace panes.
        self.redraw();
        if let Some(live) = &self.live {
            live.window.focus_window();
        }
        activate_app();
    }

    fn request_redraw(&self) {
        if let Some(live) = &self.live {
            live.window.request_redraw();
        }
    }

    /// The first-run window's half of the F12 inspector: the toggle itself,
    /// the F11 spacing rays, and the picking pass an open inspector makes over
    /// this window's events. The panel window's own events are
    /// [`OnboardingApp::native_inspector_event`]'s. One `Inspector` does both
    /// sides, exactly as in [`App::inspector_event`]; the zoom is 1.0 because
    /// the first-run window has no display zoom.
    fn inspector_event(&mut self, event_loop: &ActiveEventLoop, event: &WindowEvent) -> bool {
        if self.focused
            && let Some(changed) = self
                .inspector
                .as_mut()
                .and_then(|i| i.spacing_shortcut(event, self.mods))
        {
            if changed {
                self.request_redraw();
            }
            return true;
        }
        if let WindowEvent::KeyboardInput {
            event,
            is_synthetic,
            ..
        } = event
            && self.focused
            && koru_keys::registry::get(koru_keys::ShortcutId::ToggleUiInspector)
                .chord
                .matches(&event.logical_key, event.text.as_deref(), self.mods)
        {
            if crate::inspector::shortcut_pressed(event.state, event.repeat, *is_synthetic) {
                if let Some(inspector) = self.inspector.as_mut() {
                    if inspector.shortcut_requests_close() {
                        self.close_inspector();
                    }
                } else {
                    // The press the form is mid-way through is over: the
                    // inspector starts by consuming this window's input.
                    self.view.mouse_up();
                    self.open_inspector_window(event_loop);
                }
                if let Some(access) = self.access.as_mut() {
                    access.invalidate_structure();
                }
                self.sync_cursor_icon();
                self.request_redraw();
            }
            return true;
        }
        let Some(inspector) = self.inspector.as_mut() else {
            return false;
        };
        let scale = self
            .live
            .as_ref()
            .map_or(1.0, |live| live.window.scale_factor());
        let result = inspector.source_event(event, scale, 1.0);
        if result.structure_changed
            && let Some(access) = self.access.as_mut()
        {
            access.invalidate_structure();
        }
        if result.close {
            self.close_inspector();
        }
        self.sync_cursor_icon();
        if result.redraw {
            self.request_redraw();
        }
        result.consumed
    }

    /// Drop the inspector and its panel, keeping the panel's geometry for the
    /// next F12 this session. The panel's provider retires with its window.
    fn close_inspector(&mut self) {
        if let Some(mut native) = self.inspector_window.take() {
            self.inspector_geometry = Some(native.remembered_geometry());
        }
        self.inspector.take();
        if let Some(access) = self.access.as_mut() {
            access.invalidate_structure();
        }
        self.request_redraw();
    }

    /// Set the pointer shape: the system arrow while the inspector wants this
    /// window to pick from, whatever the form under it would say otherwise.
    fn sync_cursor_icon(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let icon = if self
            .inspector
            .as_ref()
            .is_some_and(crate::inspector::Inspector::wants_arrow_cursor)
        {
            CursorIcon::Default
        } else {
            self.view.cursor_icon()
        };
        live.window.set_cursor(icon);
    }

    fn settle_actions(&mut self) {
        for telemetry in self.view.take_telemetry() {
            crate::events_host::onboarding(telemetry);
        }
        while let Some(action) = self.view.take_action() {
            match action {
                crate::onboarding::Action::Account(command) => self.submit_account_command(command),
                crate::onboarding::Action::OpenCreatedProfile(_) => {
                    self.connected_paused = false;
                    self.finish_connected_profile();
                }
                crate::onboarding::Action::ConfirmAccountCreation => self.submit_account_command(
                    koru_agent_wire::AccountCommand::ConfirmAccountCreation,
                ),
                crate::onboarding::Action::CancelAccountCreation => self
                    .submit_account_command(koru_agent_wire::AccountCommand::CancelAccountCreation),
                crate::onboarding::Action::RetryAccount => {
                    self.submit_account_command(koru_agent_wire::AccountCommand::Retry)
                }
                crate::onboarding::Action::RetryEnrollment => self.host.account_command_owned(
                    self.view.account_scope(),
                    koru_agent_wire::AccountCommand::RetryEnrollment {
                        profile_id: self.view.selected_account_profile_id().map(str::to_owned),
                    },
                ),
                crate::onboarding::Action::StartGoogle => {
                    self.submit_account_command(koru_agent_wire::AccountCommand::StartGoogle {
                        locale: "en".into(),
                    })
                }
                crate::onboarding::Action::StartApple => {
                    self.submit_account_command(koru_agent_wire::AccountCommand::StartApple {
                        locale: "en".into(),
                    })
                }
                crate::onboarding::Action::ContinueWithEmail { address, password } => self
                    .submit_account_command(koru_agent_wire::AccountCommand::ContinueWithEmail {
                        locale: "en".into(),
                        address,
                        password,
                    }),
                crate::onboarding::Action::SubmitEmailCode { code } => {
                    self.submit_account_command(koru_agent_wire::AccountCommand::SubmitEmailCode {
                        code,
                    })
                }
                crate::onboarding::Action::SubmitEmailPassword { password } => self
                    .submit_account_command(koru_agent_wire::AccountCommand::SubmitEmailPassword {
                        password,
                    }),
                crate::onboarding::Action::ResendEmailCode => {
                    self.submit_account_command(koru_agent_wire::AccountCommand::ResendEmailCode)
                }
                crate::onboarding::Action::OpenUri(uri) => self.host.open_uri(uri),
                crate::onboarding::Action::SelectAccountProfiles { profile_ids } => self
                    .submit_account_command(koru_agent_wire::AccountCommand::SelectProfiles {
                        profile_ids,
                    }),
                crate::onboarding::Action::RestoreLocalProfile { profile_id } => {
                    let tx = self.creation_tx.clone();
                    let proxy = self.proxy.clone();
                    let root = self.gateway_root.clone();
                    let _ = std::thread::Builder::new()
                        .name("koru-onboarding-restore".into())
                        .spawn(move || {
                            let result = restore_onboarding_profile(root.as_deref(), &profile_id);
                            let _ = tx.send(result);
                            let _ = proxy.send_event(Wake::Onboarding);
                        });
                }
                crate::onboarding::Action::CancelGoogle => {
                    self.submit_account_command(koru_agent_wire::AccountCommand::Cancel {
                        generation: self.view.account_generation(),
                    })
                }
                crate::onboarding::Action::CancelEnrollment => self.host.account_command_owned(
                    self.view.account_scope(),
                    koru_agent_wire::AccountCommand::CancelEnrollment,
                ),
                crate::onboarding::Action::CreateLocalProfile {
                    owner_display_name,
                    name,
                    workspace_name,
                    template,
                    workspace_icon,
                    workspace_color,
                    profile_icon,
                    profile_color,
                    connect_account,
                } => {
                    self.connect_created_profile = connect_account;
                    self.connected_owner_scope = self.view.account_scope();
                    self.first_minutes =
                        Some((owner_display_name, name.clone(), workspace_name.clone()));
                    let tx = self.creation_tx.clone();
                    let proxy = self.proxy.clone();
                    let root = self.gateway_root.clone();
                    let _ = std::thread::Builder::new()
                        .name("koru-onboarding-create".into())
                        .spawn(move || {
                            let result = create_onboarding_profile(
                                root.as_deref(),
                                &name,
                                &workspace_name,
                                &template,
                                profile_icon.as_deref(),
                                profile_color.as_deref(),
                                workspace_icon.as_deref(),
                                workspace_color.as_deref(),
                            );
                            let _ = tx.send(result);
                            let _ = proxy.send_event(Wake::Onboarding);
                        });
                }
            }
        }
    }

    fn begin_connected_profile(&mut self, opened: koru_workspace::fromstore::OpenedFromStore) {
        let profile = opened
            .profile
            .id()
            .expect("a Store-opened Profile always carries its durable id");
        let name = self
            .first_minutes
            .as_ref()
            .map(|(_, profile, _)| profile.clone())
            .unwrap_or_else(|| "Personal".into());
        let owner_display_name = self
            .first_minutes
            .as_ref()
            .map(|(owner, _, _)| owner.clone())
            .unwrap_or_else(|| name.clone());
        self.pending_connected_open = Some(opened);
        self.connect_created_profile = false;
        self.awaiting_hosted_profile = true;
        self.submit_account_command(koru_agent_wire::AccountCommand::CreateProfile {
            profile_id: profile.to_storage(),
            owner_display_name,
            name,
            locale: Some("en".into()),
        });
    }

    fn finish_connected_profile(&mut self) {
        self.awaiting_hosted_profile = false;
        self.awaiting_local_binding = false;
        if self.view.account_scope() != self.connected_owner_scope {
            return;
        }
        if let Some(opened) = self.pending_connected_open.take() {
            let _ = self.creation_tx.send(OnboardingCreation::Opened(opened));
            let _ = self.proxy.send_event(Wake::Onboarding);
        }
    }

    fn poll_local_profile_binding(&mut self) {
        while let Some((raw_profile, result)) = self.host.take_local_profile_binding() {
            if !self.awaiting_local_binding {
                continue;
            }
            let expected = self
                .pending_connected_open
                .as_ref()
                .and_then(|opened| opened.profile.id())
                .map(|profile| profile.to_storage());
            if expected.as_deref() != Some(raw_profile.as_str()) {
                continue;
            }
            match result {
                Ok(
                    koru_agent_wire::LocalProfileAccountBindingOutcome::Bound
                    | koru_agent_wire::LocalProfileAccountBindingOutcome::AlreadyBound,
                ) => {
                    if let Some(profile) = koru_core::ProfileId::parse(&raw_profile) {
                        self.pending_bindings.forget(profile);
                    }
                    self.finish_connected_profile();
                }
                Err(code) => {
                    // The Store Profile/Workspace are already durable. A local
                    // CAS refusal never rolls them back or strands the person.
                    if code == "profile_account_conflict"
                        && let Some(profile) = koru_core::ProfileId::parse(&raw_profile)
                    {
                        self.pending_bindings.forget(profile);
                    }
                    self.finish_connected_profile();
                }
            }
        }
    }

    fn pause_created_profile_connection(&mut self, retry: koru_agent_wire::AccountCommand) {
        let Some(profile) = self
            .pending_connected_open
            .as_ref()
            .and_then(|opened| opened.profile.id())
        else {
            return;
        };
        self.awaiting_hosted_profile = false;
        self.connected_paused = true;
        self.view
            .connection_paused_scoped(self.connected_owner_scope, profile, retry);
    }
    fn created_profile_retry(&self) -> Option<koru_agent_wire::AccountCommand> {
        let profile = self.pending_connected_open.as_ref()?.profile.id()?;
        let (owner, name, _) = self.first_minutes.as_ref()?;
        Some(koru_agent_wire::AccountCommand::CreateProfile {
            profile_id: profile.to_storage(),
            owner_display_name: owner.clone(),
            name: name.clone(),
            locale: Some("en".into()),
        })
    }
    fn submit_account_command(&mut self, command: koru_agent_wire::AccountCommand) {
        if matches!(
            &command,
            koru_agent_wire::AccountCommand::CreateProfile { .. }
        ) && self.pending_connected_open.is_some()
        {
            if self.view.account_scope() != self.connected_owner_scope {
                self.pause_created_profile_connection(command);
                return;
            }
            let submission = crate::account_ui::CommandSubmissionId::next();
            self.connected_submission = Some(submission);
            self.connected_generation = None;
            self.connected_paused = false;
            self.awaiting_hosted_profile = true;
            self.host.account_command_tracked_owned(
                submission,
                self.connected_owner_scope,
                command,
            );
            return;
        }
        if let Some(submission) = self.view.command_submission(&command) {
            self.host
                .account_command_tracked_owned(submission, self.view.account_scope(), command);
        } else {
            self.host
                .account_command_owned(self.view.account_scope(), command);
        }
    }

    fn poll_account(&mut self) {
        self.poll_local_profile_binding();
        if (self.awaiting_hosted_profile || self.awaiting_local_binding)
            && !self.host.agent_available()
        {
            // Retain the created ID through uncertain admission. The recovery
            // form also offers opening the already durable local Profile.
            if self.awaiting_hosted_profile
                && let Some(retry) = self.created_profile_retry()
            {
                self.pause_created_profile_connection(retry);
            } else {
                self.finish_connected_profile();
            }
        }
        if self.host.take_account_dirty() {
            let state = self.host.account_state().clone();
            if self.view.select_account_context(&state) {
                self.signed_in_profiles_requested = false;
            }
            if state.stage == koru_agent_wire::AccountStage::SignedIn {
                self.view.account_creation_finished();
                self.request_signed_in_profiles_once();
            } else if matches!(
                state.stage,
                koru_agent_wire::AccountStage::Restoring
                    | koru_agent_wire::AccountStage::WaitingForBrowser
                    // The two stages that are the email journey's own
                    // screens. Without them here the worker would park at the
                    // code step and the form would never be told.
                    | koru_agent_wire::AccountStage::AwaitingEmailCode
                    | koru_agent_wire::AccountStage::AwaitingEmailPassword
                    | koru_agent_wire::AccountStage::AwaitingAccountCreation
                    | koru_agent_wire::AccountStage::Exchanging
                    | koru_agent_wire::AccountStage::Refreshing
                    | koru_agent_wire::AccountStage::Unavailable
                    | koru_agent_wire::AccountStage::Failed
            ) || state.enrollment != koru_agent_wire::EnrollmentState::Idle
            {
                self.view.show_account_state(&state);
            }
            if state.stage != koru_agent_wire::AccountStage::SignedIn {
                self.signed_in_profiles_requested = false;
            }
        }
        while let Some(admission) = self.host.take_account_command_admission() {
            self.view.account_command_admitted(admission);
            if self.connected_submission == Some(admission.submission)
                && self.connected_owner_scope == admission.owner_scope
            {
                self.connected_generation = Some(admission.generation);
            }
        }
        if self.pending_connected_open.is_some()
            && !self.awaiting_hosted_profile
            && !self.awaiting_local_binding
            && !self.connected_paused
            && self.view.account_scope() == self.connected_owner_scope
        {
            self.finish_connected_profile();
        }
        while let Some((owner_scope, generation, result)) = self.host.take_scoped_account_result() {
            if self.connected_owner_scope == owner_scope
                && self.connected_generation == Some(generation)
                && matches!(
                    &result,
                    koru_agent_wire::AccountOperationResult::Failed { .. }
                )
                && let Some(retry) = self.created_profile_retry()
            {
                self.pause_created_profile_connection(retry);
                // This independently admitted creation owns this failure. Its
                // local Profile/retry must settle before chooser correlation.
                continue;
            }
            if self.view.selection_result(owner_scope, generation, &result) {
                continue;
            }
            if owner_scope != self.view.account_scope()
                && !matches!(
                    &result,
                    koru_agent_wire::AccountOperationResult::Profile { .. }
                )
            {
                self.view.accept_parked_account_result(owner_scope, &result);
                continue;
            }
            match result {
                koru_agent_wire::AccountOperationResult::AccountCreationRequired => {
                    self.view
                        .account_creation_required(&self.host.account_state());
                }
                koru_agent_wire::AccountOperationResult::SignedIn => {
                    self.view.account_creation_finished();
                    self.request_signed_in_profiles_once();
                }
                koru_agent_wire::AccountOperationResult::Profiles { profiles } => {
                    self.view.show_account_profiles(profiles);
                }
                koru_agent_wire::AccountOperationResult::ProfileChoices {
                    display_name,
                    choices,
                } => {
                    self.view
                        .show_account_profile_choices(&display_name, choices);
                }
                koru_agent_wire::AccountOperationResult::Profile { profile } => {
                    let local_profile = self
                        .pending_connected_open
                        .as_ref()
                        .and_then(|opened| opened.profile.id());
                    if self.awaiting_hosted_profile
                        && self.connected_owner_scope == owner_scope
                        && local_profile
                            .is_some_and(|local| local.to_storage() == profile.profile_id)
                    {
                        self.awaiting_hosted_profile = false;
                        let local_profile = local_profile.expect("checked above");
                        let marker = crate::binding_markers::PendingProfileAccountBinding {
                            profile: local_profile,
                            expected_identity_id: None,
                            new_identity_id: profile.koru_identity_id.clone(),
                        };
                        // The Agent is asked once the marker is durable:
                        // `service_binding_markers` sends the bind when the
                        // writer answers.
                        self.awaiting_local_binding = true;
                        self.pending_bindings.remember(marker);
                    } else if owner_scope == self.view.account_scope() {
                        self.view.show_account_profiles(vec![profile]);
                    }
                }
                koru_agent_wire::AccountOperationResult::ProfilesSelected { profiles } => {
                    let profiles = crate::account_ui::selection_result_rows(
                        &self.host.account_state(),
                        generation,
                        profiles,
                    );
                    self.view.profiles_selected(profiles);
                }
                koru_agent_wire::AccountOperationResult::Cancelled => self.view.account_cancelled(),
                koru_agent_wire::AccountOperationResult::Failed { code } => {
                    let mut state = self.host.account_state().clone();
                    state.stage = koru_agent_wire::AccountStage::Failed;
                    state.failure_code = Some(code);
                    self.view.show_account_state(&state);
                }
                koru_agent_wire::AccountOperationResult::SignedInIdentity { .. }
                | koru_agent_wire::AccountOperationResult::ProfileAuthorizationRemoved { .. }
                | koru_agent_wire::AccountOperationResult::ProfileDeleted { .. }
                | koru_agent_wire::AccountOperationResult::ProfileRestored { .. }
                | koru_agent_wire::AccountOperationResult::ProfileLifecycle { .. }
                | koru_agent_wire::AccountOperationResult::ProfileLifecycleReconciliation {
                    ..
                }
                | koru_agent_wire::AccountOperationResult::AddGoogleCandidate { .. }
                | koru_agent_wire::AccountOperationResult::GoogleAuthorizationAdded { .. }
                | koru_agent_wire::AccountOperationResult::AddGoogleCancelled { .. }
                | koru_agent_wire::AccountOperationResult::SignedOut
                | koru_agent_wire::AccountOperationResult::AccountSelected { .. }
                | koru_agent_wire::AccountOperationResult::CloudSubscription { .. }
                | koru_agent_wire::AccountOperationResult::CloudSync { .. } => {}
            }
        }
        while let Some(failure) = self.host.take_account_command_failure() {
            if self.connected_submission == Some(failure.submission)
                && let Some(retry) = self.created_profile_retry()
            {
                self.pause_created_profile_connection(retry);
            }
            self.view.account_command_failed(failure);
        }
    }

    fn redraw(&mut self) {
        let (Some(context), Some(live)) = (self.context.as_mut(), self.live.as_mut()) else {
            return;
        };
        self.scene.reset();
        let scale = live.window.scale_factor();
        let inspection_root = Affine::scale(scale);
        if let Some(inspector) = self.inspector.as_mut() {
            inspector.constrain(
                live.surface.config.width as f32 / scale as f32,
                live.surface.config.height as f32 / scale as f32,
            );
        }
        // The inspector's measure of this frame, taken the same way
        // `App::redraw` takes it: the scope stays open across the view's draw
        // so instrumented descendants record into the active frame.
        let inspection_frame = self
            .inspector
            .as_mut()
            .filter(|inspector| inspector.capture_live())
            .map(|inspector| {
                inspector.frame += 1;
                let started = std::time::Instant::now();
                let viewport = vello::kurbo::Rect::new(
                    0.0,
                    0.0,
                    live.surface.config.width as f64 / scale,
                    live.surface.config.height as f64 / scale,
                );
                let frame =
                    koru_draw::inspect::Frame::begin(inspector.frame, inspection_root, viewport);
                let owner = koru_draw::inspect::scope(
                    "First-run window",
                    "container",
                    inspection_root,
                    viewport,
                    |record| {
                        record.property(
                            "layout",
                            "window / overlay",
                            "onboarding; only instrumented descendants appear",
                        );
                    },
                );
                (started, frame, owner)
            });
        self.view.draw(&mut self.scene, Affine::scale(scale));
        if let Some((started, frame, owner)) = inspection_frame {
            drop(owner);
            let snapshot = frame.finish();
            if let Some(inspector) = self.inspector.as_mut() {
                inspector.capture_micros = started.elapsed().as_micros();
                inspector.accept(snapshot);
            }
        }
        if let Some(inspector) = self.inspector.as_mut() {
            inspector.draw_overlay(&mut self.scene, inspection_root);
            if (inspector.capture_live() || inspector.dirty_semantics)
                && let Some(native) = self.inspector_window.as_mut()
            {
                native.invalidate();
                inspector.dirty_semantics = false;
            }
        }
        let (width, height) = (live.surface.config.width, live.surface.config.height);
        let device = &context.devices[live.surface.dev_id];
        let Some(renderer) = self.renderers.get_mut(&live.surface.dev_id) else {
            return;
        };
        if renderer
            .render_to_texture(
                &device.device,
                &device.queue,
                &self.scene,
                &live.surface.target_view,
                &vello::RenderParams {
                    base_color: theme::p().bg,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .is_err()
        {
            return;
        }
        let Ok(frame) = live.surface.surface.get_current_texture() else {
            return;
        };
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("onboarding blit"),
            });
        live.surface.blitter.copy(
            &device.device,
            &mut encoder,
            &live.surface.target_view,
            &frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        device.queue.submit([encoder.finish()]);
        frame.present();
        if !live.window.is_visible().unwrap_or(false) {
            live.window.set_visible(true);
            // The provider was installed in `open`; this only records that the
            // window has now been on screen.
            if let Some(access) = self.access.as_mut() {
                access.revealed();
            }
        }
    }

    fn take_creation(&self) -> Option<OnboardingCreation> {
        self.creation_rx.try_recv().ok()
    }

    /// Give the still-hidden first-run window its accessibility provider.
    ///
    /// The same shape as [`App::seat_accessibility`], and deliberately a second
    /// call site rather than a shared one: `OnboardingApp` and `App` have no
    /// common type, no common window collection and no common wake path, so the
    /// only thing they could share is these fifteen lines — and the audit test
    /// in `crate::accessibility::tests` counts creation sites against seat
    /// sites, so a third window cannot be added without answering the question.
    fn seat_accessibility(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Arc<Window>,
    ) -> crate::accessibility::PaneAccessibility {
        let slot = self.slots.window();
        let view = self.slots.view();
        let proxy = self.proxy.clone();
        let bridge = koru_window::WindowBridge::before_first_reveal(
            event_loop,
            window,
            // A screen reader's handlers run on whichever thread the platform
            // chose, and all they do is leave a note. This is how the loop
            // learns there is one to read.
            std::sync::Arc::new(move || {
                let _ = proxy.send_event(Wake::Accessibility);
            }),
        );
        if koru_env::switch(koru_env::name::KORU_ACCESSIBILITY_DEBUG) {
            eprintln!(
                "[koru:a11y] first-run window={} view={} provider={}",
                slot.raw(),
                view.raw(),
                bridge.is_some()
            );
        }
        crate::accessibility::PaneAccessibility::new(
            slot,
            view,
            bridge.map(|bridge| Box::new(bridge) as Box<dyn koru_window::AccessibilityProvider>),
        )
    }

    /// Everything the first-run window offers a screen reader right now.
    ///
    /// Only ever called when a client is attached *and* something changed — so
    /// on an ordinary session this never runs, which is what lets it ask the OS
    /// for the window's size and let the form measure text.
    ///
    /// `&mut self` where [`App::window_semantics`] is `&self`: the form's
    /// rectangles come back from the geometry functions drawing and
    /// hit-testing share, and those measure text through a `&mut TextEngine`.
    /// The seam that replaces the borrow is the controller itself — it owns no
    /// `Workspace`, no store session and no network handle, and every effect
    /// leaves it as a typed `onboarding::Action`.
    fn window_semantics(&mut self) -> Option<crate::accessibility::WindowSemantics> {
        let access = self.access.as_ref()?;
        let slot = access.slot();
        let view_slot = access.view_slot();
        let live = self.live.as_ref()?;
        let scale = live.window.scale_factor();
        let size = live.window.inner_size();
        let client = koru_draw::geometry::Rect::new(
            0.0,
            0.0,
            size.width as f32 / scale as f32,
            size.height as f32 / scale as f32,
        );
        let maximized = live.window.is_maximized();
        let label = self.name.clone();
        let mut children = crate::accessibility::window_commands(
            slot,
            maximized,
            // The form's own language, which is the one its labels are drawn
            // in — the same answer `crate::accessibility::ui_lang` gives, asked
            // once by the view that owns the strings.
            self.view.language(),
            // The window performs them; the form paints them, so the form is
            // asked where. Nothing here reconstructs the caption's arithmetic.
            self.view.window_command_bounds(),
        );
        children.extend(self.view.accessibility_semantics(slot, view_slot));
        Some(crate::accessibility::WindowSemantics {
            window: slot,
            label,
            scale,
            client,
            focus: self.view.accessibility_focus(slot, view_slot),
            children,
            // **A whole snapshot every time, and that is the point here.**
            // The first-run window is one small form at a time, so nothing is
            // saved by publishing a difference — and a whole snapshot
            // *replaces* the occurrence map rather than merging into it
            // (`PaneAccessibility::publish`), which is exactly the retirement
            // this screen's instance identity is for: when the screen changes,
            // the ids the previous one published stop resolving and a request
            // still queued against it is refused rather than routed onto the
            // control that replaced it. See ADR-341.
            scope: crate::accessibility::Scope::Whole,
            retired: Vec::new(),
        })
    }

    /// The loop's accessibility pass: drain, publish what is owed, and perform
    /// whatever is still about something that exists.
    ///
    /// The whole of it is skipped while nobody is listening: `listening` is
    /// false until a client has attached, so the form's revision is not even
    /// asked for.
    fn service_accessibility(&mut self, event_loop: &ActiveEventLoop) {
        let Some(access) = self.access.as_mut() else {
            return;
        };
        if access.listening() {
            let revision = self.view.semantics_revision();
            if let Some(access) = self.access.as_mut() {
                access.observe(revision);
            }
            if let Some(inspector) = self.inspector.as_mut() {
                inspector.observe_view_revision(revision);
            }
        }
        let wants = match self.access.as_mut() {
            Some(access) => access.poll(),
            None => return,
        };
        if wants.snapshot
            && let Some(semantics) = self.window_semantics()
            && let Some(access) = self.access.as_mut()
        {
            if access.publish(&semantics)
                && let Some(inspector) = self.inspector.as_mut()
            {
                inspector.observe_semantics(&semantics.children);
            }
        }
        let resolved = match self.access.as_mut() {
            Some(access) => access.resolve(),
            None => return,
        };
        if resolved.is_empty() {
            return;
        }
        let mut acted = false;
        for action in resolved {
            if self.perform_accessibility_action(event_loop, &action) {
                acted = true;
            }
            if self.live.is_none() {
                return;
            }
        }
        if acted {
            self.settle_actions();
            self.request_redraw();
        }
    }

    /// Do what a screen reader asked, through the owner that already does it.
    ///
    /// The split is the view slot and nothing else, exactly as in
    /// [`App::perform_accessibility_action`]: `ViewSlot::WINDOW` is the
    /// window's own chrome, which only the host performs; everything else was
    /// minted by the form and goes back to it.
    fn perform_accessibility_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        resolved: &crate::accessibility::Resolved,
    ) -> bool {
        use crate::accessibility::{ViewSlot, part};
        if resolved.occurrence.view != ViewSlot::WINDOW {
            let handled = self.view.accessibility_action(
                resolved.occurrence,
                resolved.action,
                resolved.data.as_ref(),
            );
            if !handled && koru_env::switch(koru_env::name::KORU_ACCESSIBILITY_DEBUG) {
                eprintln!(
                    "[koru:a11y] first-run declined {:?} on {}",
                    resolved.action,
                    resolved.occurrence.locator()
                );
            }
            if handled && let Some(access) = self.access.as_mut() {
                // The tree the client is reading has just changed underneath it.
                access.invalidate();
            }
            return handled;
        }
        // **A client sends what it likes, not what we advertised.** Windows
        // turns one Invoke into a `SetFocus` followed by the invoke whatever
        // the node offered, so the answer to an action nothing here performs is
        // to do nothing, visibly. See `guide/contributing/deps/ACCESSKIT.md`.
        if resolved.action != accesskit::Action::Click {
            return false;
        }
        let Some(window) = self.live.as_ref().map(|live| live.window.clone()) else {
            return false;
        };
        // The same three calls the caption buttons make.
        match resolved.occurrence.part {
            p if p == part::MINIMIZE => window.set_minimized(true),
            p if p == part::MAXIMIZE => {
                window.set_maximized(!window.is_maximized());
                let maximized = window.is_maximized();
                self.view.set_maximized(maximized);
                frame::apply(&window, theme::p().panel_border, maximized);
            }
            p if p == part::CLOSE => {
                self.retire_accessibility();
                self.live = None;
                event_loop.exit();
                return false;
            }
            _ => return false,
        }
        if let Some(access) = self.access.as_mut() {
            access.invalidate();
        }
        true
    }

    /// The window is going. Every queued request is refused from here on, and
    /// the adapter goes with the value.
    fn retire_accessibility(&mut self) {
        if let Some(access) = self.access.as_mut() {
            access.retire();
        }
    }
}

impl<H: Host> OnboardingApp<H> {
    /// The marker writer answered: ask the Agent to bind, or report that the
    /// intent could not be recorded — the failure the synchronous write used
    /// to raise, one tick later.
    fn service_binding_markers(&mut self) {
        for written in self.pending_bindings.drain() {
            match written.result {
                Ok(()) => {
                    if let Some(marker) = self.pending_bindings.get(written.profile).cloned() {
                        self.host.bind_local_profile_account(
                            marker.profile,
                            marker.expected_identity_id,
                            marker.new_identity_id,
                        );
                    }
                }
                Err(error) => {
                    self.awaiting_local_binding = false;
                    self.pending_bindings.forget(written.profile);
                    let _ = self
                        .creation_tx
                        .send(OnboardingCreation::CommittedFailure {
                            message: format!(
                                "Koru saved the new Profile but could not record its account connection safely. Restart Koru to recover it.\n\n{error}"
                            ),
                        });
                }
            }
        }
    }
}

impl<H: Host> ApplicationHandler<Wake> for OnboardingApp<H> {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self
            .host
            .account_command_deadline()
            .is_some_and(|due| due <= Instant::now())
        {
            self.poll_account();
            self.request_redraw();
        }
        // The loop's one accessibility pass, before it decides how long to
        // sleep: a client attaching wakes the loop, and this is what answers.
        self.service_accessibility(event_loop);
        self.service_inspector_accessibility();
        // A focused field's caret blinks; nothing else here keeps time, so
        // with no field focused (or the window in the background) the loop
        // still sleeps until something happens.
        if self.view.caret_needs_frame() {
            self.request_redraw();
        }
        let blink = self.view.caret_blink_deadline(Instant::now());
        let deadline = match (self.host.account_command_deadline(), blink) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        event_loop.set_control_flow(deadline.map_or(ControlFlow::Wait, ControlFlow::WaitUntil));
    }
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.open(event_loop);
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, wake: Wake) {
        self.service_binding_markers();
        self.poll_account();
        self.settle_actions();
        // An asynchronous change — an account answer landing, a binding
        // written — is what the inspector's refresh follows, the same
        // `host_changed` the workspace window's inspector gets on any wake
        // that is not the AT pass asking for a snapshot.
        if wake != Wake::Accessibility
            && let Some(inspector) = self.inspector.as_mut()
        {
            inspector.host_changed();
        }
        self.request_redraw();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        // The inspector panel is a second window with its own id; its events
        // are answered before the own-window check below would drop them.
        if self.native_inspector_event(id, &event) {
            return;
        }
        if self.live.as_ref().is_none_or(|live| live.window.id() != id) {
            return;
        }
        // **The adapter sees a window event before the application does.**
        // Documented on `Adapter::process_event`; empty on Windows, where the
        // provider is a subclass on the HWND, and used by the other adapters.
        // The call belongs here whether or not it does anything today.
        if let (Some(access), Some(live)) = (self.access.as_mut(), self.live.as_ref()) {
            access.process_event(&live.window, &event);
        }
        // F12, F11 and the picking pass an open inspector makes over this
        // window. Consumed here, before the form below can see them.
        if self.inspector_event(event_loop, &event) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.retire_accessibility();
                self.live = None;
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if let (Some(context), Some(live)) = (&mut self.context, &mut self.live) {
                    context.resize_surface(
                        &mut live.surface,
                        size.width.max(1),
                        size.height.max(1),
                    );
                    let scale = live.window.scale_factor() as f32;
                    self.view
                        .set_viewport(size.width as f32 / scale, size.height as f32 / scale);
                    let maximized = live.window.is_maximized();
                    if self.view.set_maximized(maximized) {
                        frame::apply(&live.window, theme::p().panel_border, maximized);
                    }
                    live.window.request_redraw();
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::CursorMoved { position, .. } => {
                let scale =
                    self.live
                        .as_ref()
                        .map_or(1.0, |live| live.window.scale_factor()) as f32;
                self.pointer = (position.x as f32 / scale, position.y as f32 / scale);
                let changed = self.view.mouse_move(self.pointer.0, self.pointer.1);
                if let Some(live) = &self.live {
                    live.window.set_cursor(self.view.cursor_icon());
                }
                if changed {
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(action) = self.view.caption_action_at(self.pointer.0, self.pointer.1) {
                    self.last_caption_click = None;
                    let Some(window) = self.live.as_ref().map(|live| live.window.clone()) else {
                        return;
                    };
                    match action {
                        koru_ui::caption::Action::Minimise => window.set_minimized(true),
                        koru_ui::caption::Action::ToggleMaximise => {
                            window.set_maximized(!window.is_maximized());
                            let maximized = window.is_maximized();
                            self.view.set_maximized(maximized);
                            frame::apply(&window, theme::p().panel_border, maximized);
                        }
                        koru_ui::caption::Action::Close => {
                            self.retire_accessibility();
                            self.live = None;
                            event_loop.exit();
                        }
                        koru_ui::caption::Action::WindowMenu => {}
                    }
                    self.request_redraw();
                    return;
                }
                if self
                    .view
                    .is_caption_drag_area(self.pointer.0, self.pointer.1)
                {
                    if let Some(live) = &self.live {
                        let now = Instant::now();
                        let double = self.last_caption_click.is_some_and(|(then, x, y)| {
                            now.duration_since(then) < DOUBLE_CLICK
                                && (x - self.pointer.0).abs() < 4.0
                                && (y - self.pointer.1).abs() < 4.0
                        });
                        self.last_caption_click = if double {
                            None
                        } else {
                            Some((now, self.pointer.0, self.pointer.1))
                        };
                        if double {
                            live.window.set_maximized(!live.window.is_maximized());
                            let maximized = live.window.is_maximized();
                            self.view.set_maximized(maximized);
                            frame::apply(&live.window, theme::p().panel_border, maximized);
                            self.request_redraw();
                        } else if !live.window.is_maximized() {
                            let _ = live.window.drag_window();
                        }
                    }
                    return;
                }
                if self.view.mouse_down(self.pointer.0, self.pointer.1) {
                    self.settle_actions();
                    self.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => self.view.mouse_up(),
            WindowEvent::MouseWheel { delta, .. } => {
                if self.view.scroll_by(delta) {
                    self.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.mods = modifiers.state(),
            // A modifier held on the way out is not replayed on the way back
            // in; without the clear it would silently gate the F12/F11 chords.
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.view.set_window_focused(focused);
                self.request_redraw();
                if !focused {
                    self.mods = ModifiersState::empty();
                }
            }
            WindowEvent::KeyboardInput { event, .. } if event.state.is_pressed() => {
                if self
                    .view
                    .key(&event.logical_key, event.text.as_deref(), self.mods)
                {
                    self.settle_actions();
                    self.request_redraw();
                }
            }
            _ => {}
        }
    }
}

fn process_started() -> Instant {
    static STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *STARTED.get_or_init(Instant::now)
}

fn log_device_ready(context: &RenderContext, dev_id: usize) {
    let info = context.devices[dev_id].adapter().get_info();
    koru_window::renderer::record_adapter(&info.name, &format!("{:?}", info.backend));
    let ms = process_started().elapsed().as_millis();
    tracing::info!(
        target: "koru::gpu",
        backend = ?info.backend,
        adapter = %info.name,
        "gpu device ready in {ms}ms"
    );
}

fn watch_device_errors(device: &wgpu::Device) {
    device.on_uncaptured_error(std::sync::Arc::new(|error| {
        tracing::error!(target: "koru::gpu", "wgpu uncaptured error: {error}");
    }));
}

struct Warmed {
    context: RenderContext,
    renderer: Option<(usize, Renderer)>,
}

struct GpuWarmup(Option<std::thread::JoinHandle<Warmed>>);

enum GpuSource {
    Warming(GpuWarmup),
    Ready(Warmed),
}
impl GpuSource {
    fn take(self) -> Warmed {
        match self {
            Self::Warming(warmup) => warmup.take(),
            Self::Ready(warmed) => warmed,
        }
    }
}

// **`Auto`, the instance and the fallback all live in `koru-window`.** They
// were a second copy here, and a copy is how `Auto` comes to mean DX12 in this
// process and something else in the agent's — which is the one thing a
// machine-local backend choice must not do.
use koru_window::{context_with, default_backends};

/// Build Vello's context on the backend this machine is configured for —
/// `WGPU_BACKEND` first (the diagnostic override), then the Settings tab's
/// choice in `renderer.conf`, then [`default_backends`]. A configured backend
/// that turns out to have no adapter is *fallen back from*, loudly, never
/// fatal — see the warm-up and `open_pane`.
fn render_context() -> RenderContext {
    context_with(koru_window::configured_backends().unwrap_or_else(default_backends))
}

impl GpuWarmup {
    /// Start the work. Call as early as possible — before choosing a folder,
    /// which may put a dialog in front of the user, and long before the load.
    fn start() -> Self {
        // A/B switch for measurement only: with it set, the device **and the
        // renderer** are made on the main thread in `resumed` exactly as they
        // were before, so the two can be compared on one build. Nothing in the
        // product reads it.
        if koru_env::switch(koru_env::name::KORU_NO_GPU_WARMUP) {
            return Self(None);
        }
        Self(
            std::thread::Builder::new()
                .name("koru-gpu-warmup".into())
                .spawn(|| {
                    let span = timing::Span::start("gpu warmup (off-thread)");
                    let mut context = render_context();
                    // `None`: there is no window to be compatible with yet.
                    let mut dev_id = pollster::block_on(context.device(None));
                    // A configured backend with no adapter degrades to the
                    // default — a wrong choice in `renderer.conf` (or a stale
                    // `WGPU_BACKEND`) must cost a log line, not the launch.
                    if dev_id.is_none() && koru_window::configured_backends().is_some() {
                        tracing::error!(
                            target: "koru::gpu",
                            "no adapter on the configured backend; falling back to the default"
                        );
                        eprintln!(
                            "[Koru][gpu] configured backend has no adapter; using the default"
                        );
                        context = context_with(default_backends());
                        dev_id = pollster::block_on(context.device(None));
                    }
                    span.stop();
                    let renderer = match dev_id {
                        // This used to pass through without a word, and the
                        // launch then died pages later at `create surface` —
                        // the seam has to say so itself.
                        None => {
                            tracing::error!(target: "koru::gpu", "gpu warmup found no usable adapter");
                            eprintln!("[Koru][gpu] no usable adapter found");
                            None
                        }
                        Some(dev_id) => {
                            log_device_ready(&context, dev_id);
                            watch_device_errors(&context.devices[dev_id].device);
                            let span = timing::Span::start("gpu renderer (off-thread)");
                            let built =
                                koru_window::build_renderer(&context.devices[dev_id].device);
                            span.stop();
                            // A failure was already said loudly by
                            // `build_renderer`; the main thread tries once more
                            // in `open_pane` and only then gives up.
                            built.ok().map(|renderer| (dev_id, renderer))
                        }
                    };
                    Warmed { context, renderer }
                })
                .ok(),
        )
    }

    /// Wait for it, or do the work here if the thread could not be started or
    /// panicked. Never a reason to fail to open a window.
    fn take(self) -> Warmed {
        match self.0.and_then(|h| h.join().ok()) {
            Some(warmed) => warmed,
            // The A/B switch, a thread that could not start, or — now that the
            // renderer build catches its own panics — a panic in wgpu itself.
            None => Warmed {
                context: render_context(),
                renderer: None,
            },
        }
    }
}

impl<H: Host> App<H> {
    fn window_title(&self, subject: &str) -> String {
        let profile = self.runtime.profile.name();
        profiled_window_title(subject, profile.as_deref(), &self.instance.slot)
    }

    fn app_window_title(&self) -> String {
        self.window_title(&self.name)
    }

    fn search_window_title(&self) -> String {
        self.window_title(&format!("{} Search", self.name))
    }

    /// `None` when the workspace could not be opened at all, in which case the
    /// user has already been told and the caller should exit.
    fn new(
        host: H,
        name: &str,
        profile_dir: PathBuf,
        gpu: GpuWarmup,
        proxy: winit::event_loop::EventLoopProxy<Wake>,
    ) -> Option<ContentState<H>> {
        Self::new_with_ack(
            host,
            name,
            profile_dir,
            GpuSource::Warming(gpu),
            proxy,
            None,
            None,
            crate::accessibility::Slots::default(),
        )
    }

    /// The shared startup/acknowledged-transition constructor. Supplying an
    /// acknowledged open skips the catalog decision but reuses the exact same
    /// non-optional Open App construction and restore path.
    fn new_with_ack(
        mut host: H,
        name: &str,
        profile_dir: PathBuf,
        gpu: GpuSource,
        proxy: winit::event_loop::EventLoopProxy<Wake>,
        acknowledged: Option<koru_workspace::fromstore::OpenedFromStore>,
        carried_update_lease: Option<koru_update::InstanceLease>,
        // **Carried, not re-created.** When onboarding hands this process over,
        // its window already used a number; starting again from zero would
        // hand the same one to the workspace window and break the invariant a
        // stale-request refusal rests on. See `crate::accessibility::identity`.
        slots: crate::accessibility::Slots,
    ) -> Option<ContentState<H>> {
        host.install_wake_proxy(proxy.clone());
        // muda delivers to its installed handler or receiver, never both.
        crate::menus::install(proxy.clone());
        // Before anything opens or binds: a copy running from the mounted dmg
        // (or App-Translocated) is an anonymous extra installation of this
        // channel, and letting it proceed is how two Korus end up competing
        // for one agent, gateway and store. `false` means the person chose
        // the installed copy, which is opening now.
        // A durable-creation transition remains in the already-admitted
        // process. Re-running the launch-only disk-image choice here could
        // interrupt an acknowledged in-process transition with a second
        // prompt after onboarding already owns the live window.
        if acknowledged.is_none() && !startup::continue_past_disk_image_check() {
            return None;
        }
        // The theme has to be set before the first layout is built, since
        // brushes are baked into parley layouts.
        // Everything before this point: process start, the command line, the
        // taskbar channel, and choosing a folder. Recorded as a span of its own
        // so `first frame` minus the rest is not left as a mystery.
        timing::record("...before the app", timing::started_at().elapsed());
        let instance = session::Instance::from_args();
        let geometry_slot = host
            .window_geometry_slot()
            .unwrap_or(&instance.slot)
            .to_owned();
        let span = timing::Span::start("settings");
        let pre_identity_settings = PreIdentitySettings::load(&profile_dir, &instance.slot);
        span.stop();
        let settings = pre_identity_settings.preferences();
        apply_pre_identity_preferences(&settings);
        theme_debug_startup();
        // **The switch says it is on, once, before anything can have happened.**
        // Without it, a log with no `served` line in it has two readings — the
        // sweep never ran, or the switch never took — and those need opposite
        // repairs. One line at startup tells them apart, and names the file so
        // the person who set the variable knows where the evidence is.
        tasks_debug_log(format_args!(
            "startup: task diagnostics on; appending to {}",
            std::env::temp_dir()
                .join("koru-tasks-debug.log")
                .to_string_lossy()
        ));
        let settings_link = koru_api_client::GlobalLink::global(
            koru_workspace::fromstore::segment().to_owned(),
            cli::root_from_args(),
        );
        {
            // The loop sleeps until something wakes it, so the link is given
            // the one call that makes it take another pass. `about_to_wait`
            // runs after every batch — a user event included — which is where
            // `poll_settings` already is.
            let proxy = proxy.clone();
            settings_link.on_update(move || {
                let _ = proxy.send_event(Wake::Settings);
            });
        }
        {
            let info = koru_channel::build_info();
            println!(
                "[Koru] release: {} {} (build {}, channel {}{})",
                info.version,
                info.build_label(),
                info.build_number,
                info.channel,
                info.released()
                    .map_or(String::new(), |at| format!(", released {at}")),
            );
        }
        // **The lease outlived the controller on purpose.** Nothing in this
        // process checks for or installs an update any more — the agent does —
        // but the swap still replaces the files this executable is running from,
        // and refusing to start halfway through that is the one thing only the
        // process being replaced can do. Gated on `updates_enabled` so a
        // development build, which can never be the subject of a swap, does not
        // acquire a lock or gain a way to fail at launch.
        let update_lease = if carried_update_lease.is_some() {
            carried_update_lease
        } else if koru_channel::build_info().updates_enabled() {
            let root = cli::root_from_args();
            let install = match std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
            {
                Some(install) => install,
                None => {
                    startup::fatal("Koru could not determine its installation directory.");
                    return None;
                }
            };
            match koru_update::UpdatePaths::for_install(root.as_deref(), &install) {
                Some(paths) => match koru_update::InstanceLease::acquire(&paths, &install) {
                    Ok(lease) => Some(lease),
                    Err(error) => {
                        startup::fatal(&format!(
                            "Koru cannot start while this installation is being updated.\n\n{error}"
                        ));
                        return None;
                    }
                },
                None if root.is_some() => None,
                None => {
                    startup::fatal("Koru could not determine where its update state lives.");
                    return None;
                }
            }
        } else {
            None
        };

        // There is deliberately no in-memory fall-back here. A workspace that
        // silently cannot save looks exactly like one that can, right up to the
        // moment it is closed — which is a whole day's typing, unrecoverably.
        let wanted = cli::workspace_from_args().or(pre_identity_settings.workspace_hint());
        // Store Profiles are authoritative. The `ProfileDoc` built below is a
        // transient view and therefore has no legacy folder session to open or
        // publish. Explicit folder import/open entry points remain available.
        let profile_session = ProfilePersistence::Store;

        // ---------------------------------------------------- the Koru Store
        //
        // Read once, here, and carried on the app from now on. See
        // [`Self::gateway_root`] for why it is not re-read at each use.
        let gateway_root = cli::root_from_args();

        // The Store catalog is the first content-bearing read. A plain launch
        // has not opened a profile folder, ProfileDoc or WorkspaceDoc above;
        // the empty-catalog arm can therefore enter onboarding without undoing
        // an earlier placeholder construction or legacy write.
        // **A store that will not answer is fatal, and deliberately so.** The
        // alternative — fall back to the folder — would give a window that looks
        // right and writes somewhere nobody asked for, which is the "no
        // in-memory fall-back" rule (AGENTS.md) broken in a new place. Better
        // one honest dialog than two sources of truth.
        let explicit_store_profile = cli::store_profile_from_args();
        let remembered_store_profile =
            explicit_store_profile.or_else(startup::selected_store_profile);
        let open_store = |selected: Option<koru_core::ProfileId>| {
            selected.map_or_else(
                || {
                    koru_workspace::fromstore::open_from_store_preferring_empty(
                        koru_workspace::fromstore::segment(),
                        gateway_root.as_deref(),
                        wanted,
                    )
                },
                |selected| {
                    koru_workspace::fromstore::open_from_store_profile_preferring_empty(
                        koru_workspace::fromstore::segment(),
                        gateway_root.as_deref(),
                        selected,
                        wanted,
                    )
                },
            )
        };
        // An explicit profile is strict. A remembered profile is a preference:
        // if that profile has since gone, fall back to the ordinary first-run
        // policy instead of making every future launch fail on a stale marker.
        // **"Not running" in the first seconds is "not yet", not "never".**
        // This window asked the agent to come up a moment ago and the agent
        // starts the store — but the agent *answering* and the store
        // *listening* are two moments, and on a cold first launch (Gatekeeper
        // scanning every fresh binary, the app translocated) the gap is widest.
        // Seen on a Mac nightly: the store bound 0.36 s after Koru started and
        // Koru had already dialed it once and put up the fatal dialog. So a
        // `store_unreachable` is retried for as long as `ensure_running` would
        // wait for a store it spawned itself; every other error stays fatal,
        // and so does this one once the deadline passes.
        let started = std::time::Instant::now();
        let open_store_patiently = |selected: Option<koru_core::ProfileId>| {
            let mut waited = false;
            loop {
                match open_store(selected) {
                    Err(error) if store_open_should_wait(error.code(), started.elapsed()) => {
                        // **Once, on the first retry.** Waiting for a store
                        // that is still binding is ordinary and says nothing;
                        // waiting *at all* is what distinguishes a launch that
                        // took ten seconds and then failed from one that never
                        // reached the store — and the ten-second version used
                        // to be a completely silent gap in the log.
                        if !waited {
                            waited = true;
                            tracing::warn!(
                                target: "koru::startup",
                                code = ?error.code(),
                                "the store is not answering yet; waiting up to {}s",
                                STORE_OPEN_PATIENCE.as_secs()
                            );
                        }
                        std::thread::sleep(STORE_OPEN_RETRY);
                    }
                    result => {
                        if waited {
                            tracing::info!(
                                target: "koru::startup",
                                waited_ms = started.elapsed().as_millis() as u64,
                                answered = result.is_ok(),
                                "the wait for the store ended"
                            );
                        }
                        return result;
                    }
                }
            }
        };
        let opened_from_store = match acknowledged {
            Some(opened) => Ok(koru_workspace::fromstore::StoreLaunch::Open(opened)),
            None => match open_store_patiently(remembered_store_profile) {
                Err(error)
                    if explicit_store_profile.is_none()
                        && remembered_store_profile.is_some()
                        && error.code() == koru_store_core::errors::Code::ProfileNotFound =>
                {
                    open_store_patiently(None)
                }
                result => result,
            },
        };
        if let Ok(launch) = &opened_from_store {
            // Reconcile even when the catalog is empty. A crash after the
            // Store's removal marker but before settings cleanup restarts as
            // `NeedsProfile`; skipping that state would preserve stale windows,
            // tabs, and the last-selected Profile forever.
            let live_profiles = live_profile_ids_for_settings(launch);
            let live_workspaces = match client_at(gateway_root.as_deref())
                .and_then(|client| client.workspaces_including_deleted())
            {
                Ok(workspaces) => workspaces
                    .into_iter()
                    .map(|workspace| WorkspaceId::from_uuid(workspace.id.as_uuid()))
                    .collect::<HashSet<_>>(),
                Err(error) => {
                    startup::fatal(&format!(
                        "Koru could not reconcile Profile settings safely:\n\n{error}"
                    ));
                    return None;
                }
            };
            if let Err(error) = crate::settings::reconcile_removed_profile_state(
                &profile_dir,
                gateway_root.as_deref(),
                &live_profiles,
                &live_workspaces,
            ) {
                startup::fatal(&format!(
                    "Koru could not reconcile Profile settings safely:\n\n{error}"
                ));
                return None;
            }
        }
        let (profile, profile_catalog, current, mut workspace, live_workspace) =
            // **The one they were last in**: `--workspace` if it was named,
            // otherwise what `Settings` remembered from the last switch. Without
            // it the store opens whichever workspace was *created* last, so
            // making one permanently changes what Koru opens on and switching
            // back to an older one is forgotten every time the window closes.
            match opened_from_store {
                Ok(koru_workspace::fromstore::StoreLaunch::Open(opened)) => (
                    opened.profile,
                    opened.profiles,
                    opened.current,
                    opened.workspace,
                    opened.has_workspace,
                ),
                Ok(koru_workspace::fromstore::StoreLaunch::NeedsProfile) => {
                    // The Store already answered. Start the event worker before
                    // first-run choices so their occurrence time is the click,
                    // not the later Profile acknowledgement.
                    ProfileRuntime::start_events(gateway_root.clone(), proxy.clone());
                    let (creation_tx, creation_rx) = mpsc::channel();
                    // Seeded before the first frame: a build with no Koru
                    // Cloud draws its welcome without the sign-in choices from
                    // the start, rather than for one frame with them.
                    let mut view = crate::onboarding::OnboardingView::default();
                    view.observe_account_projection(&host.account_state());
                    return Some(ContentState::Onboarding(Box::new(OnboardingApp {
                        host,
                        name: name.to_owned(),
                        gateway_root,
                        gpu: Some(gpu),
                        update_lease,
                        live: None,
                        access: None,
                        slots: crate::accessibility::Slots::default(),
                        inspector: None,
                        inspector_window: None,
                        inspector_geometry: None,
                        focused: true,
                        context: None,
                        renderers: HashMap::new(),
                        scene: Scene::new(),
                        view,
                        pointer: (0.0, 0.0),
                        mods: ModifiersState::empty(),
                        last_caption_click: None,
                        creation_tx,
                        creation_rx,
                        first_minutes: None,
                        connect_created_profile: false,
                        pending_connected_open: None,
                        awaiting_hosted_profile: false,
                        connected_owner_scope: 0,
                        connected_submission: None,
                        connected_generation: None,
                        connected_paused: false,
                        awaiting_local_binding: false,
                        pending_bindings: crate::binding_markers::PendingBindings::open(
                            profile_dir.clone(),
                            {
                                let proxy = proxy.clone();
                                move || {
                                    let _ = proxy.send_event(Wake::ProfileBindings);
                                }
                            },
                        ),
                        signed_in_profiles_requested: false,
                        profile_dir,
                        proxy,
                    })));
                }
                Err(e) => {
                    // When the gateway will not start, the usual reason is an
                    // older installation's store still holding this channel's
                    // data — macOS lets a running app's bundle be replaced,
                    // and the orphan keeps serving from nowhere. One `lsof`
                    // turns "nothing is listening" into a name
                    // (docs/architecture/api/MAC.md records the hour this once cost).
                    let squatter = (e.code()
                        == koru_store_core::errors::Code::StoreUnreachable)
                        .then(|| {
                            let root = gateway_root.clone().or_else(koru_channel::channel_root)?;
                            koru_api_discovery::diagnose_gateway_absence(
                                koru_workspace::fromstore::segment(),
                                &root,
                            )
                        })
                        .flatten()
                        .map(|hint| format!("\n\n{hint}"))
                        .unwrap_or_default();
                    startup::fatal(&format!(
                        "Koru could not reach the Koru Store.

{e}{squatter}

Nothing was written. The store starts on demand, so this usually means it could not start."
                    ));
                    return None;
                }
            };
        let Some(profile_id) = profile.id() else {
            startup::fatal("Koru opened a profile without an identity. Nothing was changed.");
            return None;
        };
        startup::remember_store_profile(profile_id);
        // The store answered, so the bus can be told the session began; every
        // later emit is one channel send, off this and every other loop.
        ProfileRuntime::start_events(gateway_root.clone(), proxy.clone());
        let profile_activation = crate::profile_activation::Inbox::bind(Some(profile_id));
        let channel_root = cli::root_from_args();
        // Geometry is keyed by profile id, which is only knowable now — hence
        // two steps. There is room for it: no window exists until `resumed`.
        let mut settings =
            pre_identity_settings.bind(profile_id, &geometry_slot, channel_root.as_deref());
        if let Some(size) = cli::window_size_from_args() {
            settings.apply_launch_window_size(size);
        }
        let written = Written::for_window(channel_root.as_deref(), profile_id, &geometry_slot);
        settings.remember_profile(profile_id);
        // **The strip belongs to the workspace that actually opened.** Usually
        // that is `settings.workspace` and `tabs=` already holds its row; but a
        // remembered workspace that has since been deleted falls back to the
        // newest, and reopening the deleted one's tabs would be a failed round
        // trip each. The launch and the switch take the same line.
        settings.adopt_tabs_of(current);
        if !live_workspace {
            settings.workspace = None;
        }
        // Who this window is, for anything it writes into a document — the trash
        // says "Moved to Trash by …" in whichever *other* window is looking. The
        // name lives in the profile document and the workspace document has no
        // pointer to it, so this is the one place the two are both in hand.
        workspace.set_actor(koru_workspace::actor_label(
            profile.name().as_deref(),
            profile.email().as_deref(),
            Some(instance.slot.as_str()),
        ));
        // **How a socket thread reaches this loop.** Built once and kept, because
        // every workspace opened later — a switch, another profile — has to
        // inherit the same one; a session without it would deliver a peer's edit
        // only when something unrelated happened to wake the window.
        //
        // `EventLoopProxy` is `Clone + Send + Sync`, so one serves every session
        // in the process. The closure is all `koru-workspace` ever sees of
        // winit, which is what keeps that crate free of it.
        let wake_proxy = proxy.clone();
        // The voice worker's reader thread gets its own handle for the same
        // reason the store's does: it is a thread with something to hand over
        // and no other way to say so.
        let voice_proxy = proxy.clone();
        let waker = koru_workspace::store::link::Waker::new(move || {
            // A send that fails means the loop has exited, which is not a fault
            // worth reporting from a socket thread on the way out.
            let _ = proxy.send_event(Wake::Store);
        });
        workspace.set_store_waker(waker.clone());

        // Built here so the first pane can adopt it, and so a workspace with no
        // storage still seeds itself.
        // Registration precedes `initial`, which is also the point at which the
        // full host starts scanner services. The live workspace must be in the
        // exclusion set before a scanner thread can observe the profile.
        // A first-run window reaches neither arm: with no profile there is no
        // live workspace either, so nothing here changes for a launch that has
        // one.
        if live_workspace {
            host.workspace_opened(profile_id, current);
            // **The agent learns what this window is showing here**, beside the
            // scanner registration and for the same reason: it has to be true
            // before anything can route to it. A window that said nothing is a
            // window a search result can only reach by launching a second one.
            let profile_name = profile.name().unwrap_or_default();
            let workspace_name = profile
                .workspace(current)
                .map(|workspace| workspace.name)
                .unwrap_or_default();
            host.agent_showing(profile_id, current, &profile_name, &workspace_name, &[]);
        }
        let span = timing::Span::start("first view");
        let mut view = host.initial(&mut workspace);
        span.stop();
        // The `&mut` borrow above has ended, so the document is available — and
        // it has to be: reopening a tab means building a view of a node.
        reopen_remembered_documents(&mut workspace, &settings);
        let span = timing::Span::start("restore tabs");
        view.restore(&workspace, &settings);
        span.stop();
        let (import_tx, import_rx) = mpsc::channel();
        let (recording_tx, recording_rx) = mpsc::channel();
        let (transfer_tx, transfer_jobs) = mpsc::channel::<TransferJob>();
        let (transfer_results, transfer_rx) = mpsc::channel();
        let transfer_proxy = wake_proxy.clone();
        let _ = std::thread::Builder::new()
            .name("koru-document-transfer".into())
            .spawn(move || {
                while let Ok(job) = transfer_jobs.recv() {
                    println!(
                        "[Koru][transfer] started document={} source={} target={}",
                        job.id.to_storage(),
                        job.source.to_storage(),
                        job.target.to_storage()
                    );
                    let source = koru_store_core::ids::WorkspaceId::from_uuid(job.source.as_uuid());
                    let target = koru_store_core::ids::WorkspaceId::from_uuid(job.target.as_uuid());
                    let document = koru_store_core::ids::DocumentId::from_uuid(job.id.as_uuid());
                    let result = match job.client.transfer_documents(
                        source,
                        target,
                        document,
                        job.parent.map(|parent| {
                            koru_store_core::ids::DocumentId::from_uuid(parent.as_uuid())
                        }),
                        job.index,
                    ) {
                        Ok(ids) => Ok(ids),
                        Err(error)
                            if error.code == koru_store_core::Code::DocumentNotFound
                                && job
                                    .client
                                    .locate_document(document)
                                    .is_ok_and(|location| location.workspace == target) =>
                        {
                            println!(
                                "[Koru][transfer] already applied document={} target={}",
                                job.id.to_storage(),
                                job.target.to_storage()
                            );
                            Ok(vec![document])
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    println!(
                        "[Koru][transfer] completed document={} source={} target={} result={}",
                        job.id.to_storage(),
                        job.source.to_storage(),
                        job.target.to_storage(),
                        if result.is_ok() { "ok" } else { "error" }
                    );
                    if transfer_results
                        .send(TransferResult {
                            origin: job.origin,
                            id: job.id,
                            source: job.source,
                            target: job.target,
                            follow: job.follow,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    let _ = transfer_proxy.send_event(Wake::Store);
                }
            });
        let (selection_move_tx, selection_move_jobs) = mpsc::channel::<SelectionMoveJob>();
        let (selection_move_results, selection_move_rx) = mpsc::channel();
        let selection_move_proxy = wake_proxy.clone();
        // Serial like the transfer worker, and for the same reason: completion
        // order must be unambiguous to the window that is waiting to write the
        // trail pill.
        let _ = std::thread::Builder::new()
            .name("koru-selection-move".into())
            .spawn(move || {
                while let Ok(job) = selection_move_jobs.recv() {
                    let result = (|| {
                        if job.source != job.target {
                            let source =
                                koru_store_core::ids::WorkspaceId::from_uuid(job.source.as_uuid());
                            let target =
                                koru_store_core::ids::WorkspaceId::from_uuid(job.target.as_uuid());
                            for blob in &job.blobs {
                                job.client.associate_blob(source, target, blob)?;
                            }
                        }
                        koru_workspace::plant_into_foreign_document(
                            &job.segment,
                            job.root.as_deref(),
                            job.target,
                            job.document,
                            job.node,
                            &job.cuttings,
                            job.inline
                                .as_ref()
                                .map(|(runs, attrs)| (runs.as_slice(), attrs.as_slice())),
                        )
                    })()
                    .map_err(|error: koru_store_core::Error| error.to_string());
                    if selection_move_results
                        .send(SelectionMoveResult {
                            origin: job.origin,
                            document: job.document,
                            node: job.node,
                            title: job.title,
                            result,
                        })
                        .is_err()
                    {
                        break;
                    }
                    let _ = selection_move_proxy.send_event(Wake::Store);
                }
            });
        // **The app creates the share inbox, not the extension.** The
        // extension resolves the channel root by asking which candidate
        // already holds a `share-inbox` — macOS keeps a sticky legacy XDG root
        // (`koru_channel::channel_root_of`), and an appex that guessed the
        // other one would stage into a directory nothing ever drains. Creating
        // it here is what makes the two answers agree. macOS only, because it
        // is the only platform with a share extension to write it; the drain
        // itself is unconditional, so the day Windows grows one this becomes a
        // one-line change rather than a bug hunt.
        if cfg!(target_os = "macos")
            && let Some(inbox) = koru_share_inbox::for_this_channel()
            && let Err(error) = inbox.ensure()
        {
            // Not fatal: no share sheet is a missing feature, and refusing to
            // open a window over one would be the wrong trade.
            println!("[Koru][share] could not create the share inbox: {error}");
        }
        let image_loader = crate::images::ImageLoader::spawn(&wake_proxy);
        crate::spelling::spawn(&wake_proxy, cli::root_from_args());
        crate::diagram::spawn(&wake_proxy);
        let (clipboard_image_tx, clipboard_image_rx) = mpsc::channel();
        let (profile_cascade_tx, profile_cascade_rx) = mpsc::channel();
        let (profile_removal_tx, profile_removal_rx) = mpsc::channel();
        let (profile_offline_tx, profile_offline_rx) = mpsc::channel();
        let (connected_profile_creation_tx, connected_profile_creation_rx) = mpsc::channel();
        let (embedded_profile_restore_tx, embedded_profile_restore_rx) = mpsc::channel();
        let (profile_refresh_tx, profile_refresh_requests) = mpsc::sync_channel(1);
        let (profile_refresh_results, profile_refresh_rx) = mpsc::channel();
        let profile_refresh_root = gateway_root.clone();
        let profile_refresh_proxy = wake_proxy.clone();
        let profile_event_requests = profile_refresh_tx.clone();
        let _ = std::thread::Builder::new()
            .name("koru-profile-catalog".into())
            .spawn(move || {
                let mut client = None;
                let mut profile_events = None;
                while profile_refresh_requests.recv().is_ok() {
                    let result = (|| {
                        if client.is_none() {
                            client = Some(client_at(profile_refresh_root.as_deref())?);
                        }
                        let client = client.as_ref().expect("just connected");
                        Ok(StoreCatalogRefresh {
                            profiles: koru_workspace::fromstore::profiles_with_client(client)?,
                            workspaces: client.workspaces_including_deleted()?,
                        })
                    })()
                    .map_err(|error: koru_store_core::Error| error.to_string());
                    // A Store restart invalidates its sessions. Reconnect on the
                    // next requested refresh; ordinary successful polls keep
                    // one client for the lifetime of this worker.
                    if result.is_err() {
                        client = None;
                        // The subscription died with its session. Dropping it
                        // here is what makes the next successful refresh
                        // install a new one; keeping the handle would leave
                        // this silently back on the poll alone.
                        profile_events = None;
                    }
                    // **A Profile change says when, rather than being waited
                    // for.** The catalog is otherwise re-read on
                    // [`CONTEXT_POLL`], which is right for a workspace this
                    // person just made here and wrong for a rename that arrived
                    // from a phone or the Cloud: the bus knows, so it asks.
                    // Installed on the client the refresh already holds, so
                    // there is no second connection and no second thread.
                    if profile_events.is_none()
                        && let Some(active) = client.as_ref()
                        && active.events().available()
                    {
                        let ask = profile_event_requests.clone();
                        profile_events = active
                            .events()
                            .on(
                                None,
                                koru_api_client::Filter::names("profile."),
                                move |event| {
                                    if profile_catalog_event(event.envelope.name.as_str()) {
                                        // Full means a refresh is already
                                        // queued, which is the answer this
                                        // would have asked for.
                                        let _ = ask.try_send(());
                                    }
                                },
                            )
                            .ok();
                    }
                    if profile_refresh_results.send(result).is_err() {
                        break;
                    }
                    let _ = profile_refresh_proxy.send_event(Wake::Profiles);
                }
            });
        // **Queue first, wake second** is the client's rule; installing the
        // wake here is the other half of it, and it is why a partial needs no
        // timer. A rooted worker is given the Store root inside the channel
        // root, so the client must derive its endpoint from that same path.
        let voice_root = cli::root_from_args();
        let mut dictation = Dictation::at(cli::store_root_from_args());
        dictation.client().install_wake(move || {
            let _ = voice_proxy.send_event(Wake::Voice);
        });
        if let Some(ensure) = host.voice_ensure() {
            dictation.client().install_ensure(ensure);
        }
        // **What somebody agreed to, read once at startup.** Without it
        // `press` refuses with `consent_required` and nothing is dialled --
        // which is the microphone half of the gate, and the reason this is not
        // an optional step somewhere later.
        dictation.set_consent(crate::voice_install::read_consent(voice_root.as_deref()));
        // Joined here, at the end of the load, because that is the whole
        // point: the adapter, the device and the renderer were being made on
        // another thread while the document was being read.
        let warmed = gpu.take();
        let pending_bindings =
            crate::binding_markers::PendingBindings::open(profile_dir.clone(), {
                let proxy = wake_proxy.clone();
                move || {
                    let _ = proxy.send_event(Wake::ProfileBindings);
                }
            });
        let runtime = ProfileRuntime::new(
            profile_dir,
            profile,
            profile_catalog,
            profile_session,
            profile_activation,
            workspace,
            current,
            live_workspace,
            settings,
            written,
            profile_refresh_tx,
            profile_refresh_rx,
            pending_bindings,
        );
        Some(ContentState::Open(Box::new(Self {
            menubar: None,
            last_persist: None,
            last_burst_tick: None,
            seen_hidden: false,
            host,
            name: name.to_string(),
            instance,
            gateway_root,
            runtime,
            store_waker: waker,
            slots,
            wake_proxy,
            import_tx,
            import_rx,
            delegate_folder_check: None,
            recording_tx,
            recording_rx,
            transfer_tx,
            transfer_rx,
            transfers_in_flight: HashSet::new(),
            selection_move_tx,
            selection_move_rx,
            profile_cascade_tx,
            profile_cascade_rx,
            profile_cascades_in_flight: HashSet::new(),
            profile_removal_tx,
            profile_removal_rx,
            profile_removals_in_flight: HashSet::new(),
            hosted_profile_removals: HashMap::new(),
            profile_offline_tx,
            profile_offline_rx,
            profile_offline_in_flight: HashSet::new(),
            profile_bindings_in_flight: HashSet::new(),
            connected_profile_creation_tx,
            connected_profile_creation_rx,
            pending_connected_profile_creation: None,
            embedded_profile_restore_tx,
            embedded_profile_restore_rx,
            selection_moves_in_flight: HashSet::new(),
            image_loader,
            clipboard_image_tx,
            clipboard_image_rx,
            clipboard_image_busy: false,
            pending_clipboard_image_pastes: VecDeque::new(),
            import_busy: false,
            import_source: None,
            import_format: None,
            import_workspace_count: None,
            import_cancel: None,
            import_cancel_hide_due: None,
            import_cancel_worker_done: false,
            pending_import: None,
            deleting_workspaces: HashSet::new(),
            pending_attachment: None,
            pending_content_drops: VecDeque::new(),
            // Due once at startup: the share that opened this window was
            // staged before the process existed, so nothing will focus-event
            // it into view.
            share_drain_due: true,
            share_import_in_flight: None,
            releasing: Vec::new(),
            images: ImageCache::new(),
            pending: Some(view),
            from_the_agent: Vec::new(),
            last_agent_pins: Vec::new(),
            titlebar_dark: None,
            zoom: theme::zoom(),
            context: warmed.context,
            renderers: warmed.renderer.into_iter().collect(),
            panes: HashMap::new(),
            foreign_hover: None,
            primary: None,
            images_dirty: true,
            image_scope: None,
            tag_table: Vec::new(),
            list_table: Vec::new(),
            last_context_poll: None,
            last_os_theme_poll: None,
            pending_report: Some(("cold start".into(), timing::started_at())),
            in_tray: false,
            closing_for_update: false,
            reminder_startup_facts: None,
            reminder_startup_reply: None,
            reminder_startup_queue: ReminderStartupQueue::default(),
            search_geometry: koru_window::SearchGeometry::open(koru_window::window_geometry_path(
                cli::root_from_args().as_deref(),
            )),
            settings_link,
            settings_heard: false,
            background_start: cli::background_from_args(),
            down_keys: HashSet::new(),
            dictation,
            voice: crate::voice_install::VoiceSetup::new(voice_root),
            dictation_key: None,
            dictation_commanded: false,
            dictation_pane: None,
            agent_dictation_route: None,
            agent_dictation_last_state: None,
            dictation_shown: None,
            reported_write_failure: false,
            scene: Scene::new(),
            mods: ModifiersState::empty(),
            blink_origin: Instant::now(),
            caret_phase: None,
            frames: framestats::Frames::default(),
            update_lease,
            health_signaled: false,
        })))
    }

    /// Move any window being dragged to where the pointer now is.
    ///
    /// Returns whether a drag is live, so the caller can keep the loop awake:
    /// this is polled rather than event-driven (see [`WindowDrag`]), and a
    /// gesture that only advances on the caret's blink is not a drag anybody
    /// would keep hold of.
    ///
    /// **The button is checked, not assumed.** A macOS drag ends with no
    /// `mouseUp:`, so the only honest end condition is the OS saying the button
    /// is no longer down — the same question [`press_ended_unseen`] asks.
    fn drive_window_drags(&mut self) -> bool {
        let dragging: Vec<WindowId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.drag.is_some())
            .map(|(id, _)| *id)
            .collect();
        if dragging.is_empty() {
            return false;
        }
        let Some(pointer) = pointer_on_screen() else {
            return false;
        };
        let mut live = false;
        for id in dragging {
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            // Let go of the window the moment the button is up. The press
            // itself is ended by `end_presses_the_os_dropped`, which runs on
            // the same tick.
            if press_ended_unseen(true) {
                pane.drag = None;
                continue;
            }
            if let Some(drag) = pane.drag {
                let (x, y) = drag.position_for(pointer);
                pane.window
                    .set_outer_position(winit::dpi::PhysicalPosition::new(x, y));
                live = true;
            }
        }
        live
    }

    /// Deliver pointer movement to a view during a press the OS is not
    /// reporting.
    ///
    /// Returns whether a press is in flight, so the caller keeps the loop hot —
    /// the same contract [`Self::drive_window_drags`] has, and for the same
    /// reason: there is no event to ride on.
    ///
    /// **Only where the OS is silent.** Off macOS `CursorMoved` arrives
    /// normally and synthesising a second stream of moves would double every
    /// drag.
    fn drive_view_drags(&mut self) -> bool {
        if !cfg!(target_os = "macos") {
            return false;
        }
        let Some(pointer) = pointer_on_screen() else {
            return false;
        };
        let pressing: Vec<WindowId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.pressed && p.drag.is_none() && p.press_origin.is_some())
            .map(|(id, _)| *id)
            .collect();
        let mut live = false;
        let zoom = self.zoom;
        for id in pressing {
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            live = true;
            let Some((start_screen, start_local)) = pane.press_origin else {
                continue;
            };
            // Screen Y grows upward on macOS; a window's grows downward — the
            // same flip `WindowDrag::position_for` makes, and for the same
            // reason.
            let x = start_local.0 + (pointer.0 - start_screen.0) as f32;
            let y = start_local.1 - (pointer.1 - start_screen.1) as f32;
            if (x - pane.cursor.0).abs() < 0.5 && (y - pane.cursor.1).abs() < 0.5 {
                continue;
            }
            pane.cursor = (x, y);
            let changed = pane.view.mouse_move(x / zoom, y / zoom);
            pane.sync_cursor_icon();
            if changed {
                self.request_redraw(id);
            }
        }
        live
    }

    /// End any press whose release the OS never delivered.
    ///
    /// Cheap by construction: panes that believe nothing is held are skipped
    /// before [`press_ended_unseen`] asks the OS anything. Linux is a constant
    /// `false`; macOS and Windows read the physical button because a release
    /// can fail to arrive (window-drag loop on macOS; Explorer Jump List COM
    /// on Windows).
    fn end_presses_the_os_dropped(&mut self) {
        let stuck: Vec<WindowId> = self
            .panes
            .iter()
            .filter(|(_, p)| press_ended_unseen(p.pressed))
            .map(|(id, _)| *id)
            .collect();
        for id in stuck {
            let Some(pane) = self.panes.get_mut(&id) else {
                continue;
            };
            pane.pressed = false;
            pane.drag = None;
            pane.press_origin = None;
            inputdebug::note("release never arrived -> ending the press (idle)");
            if pane.view.mouse_up(&mut self.runtime.workspace.ws) {
                self.runtime.workspace.touched();
                self.adopt_others(id);
            }
            self.request_redraw(id);
        }
    }

    fn service_menubar(&mut self, event_loop: &ActiveEventLoop) {
        // Drained into a list first: acting on a row needs `&mut self`, and
        // the queue itself is owned by the menu bar.
        let mut queued = Vec::new();
        for id in crate::menus::drain() {
            let id = id.0;
            if let Some(command) = self.menubar.as_ref().and_then(|bar| bar.command_for(&id)) {
                queued.push(command);
                continue;
            }
            let claimed: Vec<WindowId> = self
                .panes
                .iter_mut()
                .filter_map(|(pane_id, pane)| pane.view.popup_chosen(&id).then_some(*pane_id))
                .collect();
            for pane_id in claimed {
                self.request_redraw(pane_id);
            }
        }
        for command in queued {
            match command {
                // Through the ordinary exit, so `exiting` still saves.
                crate::menubar::Command::Quit => {
                    event_loop.exit();
                    return;
                }
                crate::menubar::Command::Native => {}
                crate::menubar::Command::Press(id) => {
                    let chord = koru_keys::registry::get(id).chord;
                    let (key, text) = match chord.on {
                        koru_keys::On::Named(n) => (Key::Named(n), None),
                        koru_keys::On::Letter(c) => {
                            (Key::Character(c.to_string().into()), Some(c.to_string()))
                        }
                    };
                    // The focused window, or the only one. A menu row acts on
                    // what you are looking at.
                    let Some(id) = self
                        .panes
                        .iter()
                        .find(|(_, p)| p.window.has_focus())
                        .map(|(id, _)| *id)
                        .or(self.primary)
                    else {
                        return;
                    };
                    // Exactly what a key press does, including the redraw and
                    // the relay to other windows on the same document.
                    if let Some(pane) = self.panes.get_mut(&id) {
                        pane.view.key(
                            &mut self.runtime.workspace.ws,
                            &key,
                            text.as_deref(),
                            chord.mods,
                        );
                    }
                    self.runtime.workspace.touched();
                    self.adopt_others(id);
                    self.wake(id);
                }
            }
        }
    }

    /// Drop physical modifier keys the OS now says are not held.
    ///
    /// `down_keys` supplements a late `ModifiersChanged`; it must never
    /// contradict a fresh one. See the call site for the stuck-Ctrl this fixes.
    /// Make `self.mods`, every view, and `down_keys` agree with `mods`.
    ///
    /// **`down_keys` must be forgotten here**, not only on `ModifiersChanged`.
    /// A Ctrl+click that opens a tab can lose the Ctrl key-up (Jump List COM
    /// eats it without a focus change). Repairing `self.mods` from the OS and
    /// then leaving ControlLeft in `down_keys` lets the next key press put
    /// Ctrl back via [`modifiers_with_pressed_keys`].
    fn adopt_effective_mods(&mut self, mods: ModifiersState) {
        self.forget_released_modifiers(mods);
        if mods == self.mods {
            return;
        }
        self.mods = mods;
        for pane in self.panes.values_mut() {
            pane.view.set_mods(mods);
        }
    }

    fn forget_released_modifiers(&mut self, mods: ModifiersState) {
        modifiers::forget_released(mods, &mut self.down_keys);
    }

    /// Every window draws its next frame. The flag is what `redraw` checks:
    /// a bare `request_redraw` is dropped on macOS, where `RedrawRequested`
    /// arrives whether anything changed or not.
    fn request_redraw_all(&self) {
        for pane in self.panes.values() {
            pane.needs_frame.set(true);
            pane.window.request_redraw();
        }
    }

    /// Tell the updater that the build it installed came up.
    ///
    /// **The shell still does this, even though the agent owns updating now.**
    /// An update installed by the *previous* build wrote a `request.json` naming
    /// `koru.exe` as its restart target, so that helper is watching for a token
    /// on *this* command line — and a token that never appears is what makes a
    /// bad install roll back. The old contract has to keep working across this
    /// change, and it does so for exactly as long as there are installations
    /// that were updated by a build which owned its own updater.
    ///
    /// Parsing the token stays here (that is where this binary's argument table
    /// is) and only the writing is shared, which is what
    /// `koru_update::UpdatePaths::signal_health` is.
    fn signal_update_health(&self) {
        let Some(token) = cli::update_health_token_from_args() else {
            return;
        };
        let root = cli::root_from_args();
        let install = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
        if let Some(paths) = install
            .as_deref()
            .and_then(|install| koru_update::UpdatePaths::for_install(root.as_deref(), install))
        {
            let _ = paths.signal_health(&token);
        }
    }

    /// Read, decode, store and hand an image file to the view. `convert` is the
    /// node a `/image` command should turn into a picture; `None` means a file
    /// dropped on the window, which becomes a brand-new image row.
    fn import_image(&mut self, id: WindowId, convert: Option<NodeId>, path: PathBuf) -> bool {
        let Some(img) = self.store_image_file(&path) else {
            return false;
        };
        let ws = &mut self.runtime.workspace.ws;
        let before = ws.version();
        if let Some(pane) = self.panes.get_mut(&id) {
            match convert {
                Some(node) => pane
                    .view
                    .insert_image(ws, node, &img.hash, &img.mime, img.width, img.height),
                None => pane
                    .view
                    .append_image(ws, &img.hash, &img.mime, img.width, img.height),
            }
        }
        let changed = ws.version() != before;
        self.request_redraw(id);
        changed
    }

    /// Read, decode and store one image file, returning the paste-ready block a
    /// view builds a row from. `None` if the file is not a recognised image, or
    /// there is no store (an in-memory workspace) to keep its bytes.
    fn store_image_file(&mut self, path: &std::path::Path) -> Option<PastedImage> {
        let bytes = std::fs::read(path).ok()?;
        let mut img = self.store_image_bytes(&bytes)?;
        // Name it after the file stem (e.g. "photo.png" → "photo") so the
        // explore column shows a real name, not "Image 1".
        img.name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string());
        Some(img)
    }

    /// Decode, store and cache one image's raw bytes: the shared work of
    /// `/image`, a drop, a paste, and an RTF picture pulled off the clipboard.
    /// The blob store keeps the bytes; the image cache keeps the decoded brush.
    /// `None` if the bytes are not a recognised image, or there is no store to
    /// keep them.
    fn store_image_bytes(&mut self, bytes: &[u8]) -> Option<PastedImage> {
        let (brush, w, h, mime) = match decode_image(bytes) {
            Some(d) => d,
            None => {
                clip_dbg(format!(
                    "store_image_bytes: decode_image none ({} bytes, head={:x?})",
                    bytes.len(),
                    bytes.get(..8).unwrap_or(&[])
                ));
                return None;
            }
        };
        let Some(blobs) = self.runtime.workspace.active_blobs() else {
            clip_dbg("store_image_bytes: the active document has no blob store");
            return None;
        };
        let hash = match blobs.put(bytes) {
            Ok(h) => h,
            Err(e) => {
                clip_dbg(format!("store_image_bytes: blobs.put err {e:?}"));
                return None;
            }
        };
        // The workspace that just wrote the bytes is, by definition, the one
        // associated with them — this put is the proof the cache records.
        self.images.insert(self.image_scope_id(), &hash.0, brush);
        // The single funnel every locally-added image passes through — a file
        // picked, a file dropped, a bitmap pasted, a picture pulled out of RTF
        // or HTML. Marking here rather than at each of those call sites is what
        // stops the next one being added without it.
        //
        // The brush is already in the cache above, so this pass will find
        // nothing to do; what it is for is the *node* that a view is about to
        // create referencing this hash, which a later scan has to see.
        self.images_dirty = true;
        Some(PastedImage {
            hash: hash.0,
            mime,
            width: Some(w),
            height: Some(h),
            name: None,
        })
    }

    /// Turn a worker's typed clipboard acquisition into document image blocks.
    /// Platform reads and RGBA→PNG normalization have already finished; only
    /// document-owned blob insertion and rich-format interpretation happen here.
    fn store_clipboard_images(
        &mut self,
        acquired: Result<koru_clipboard::ImagePayload, koru_clipboard::Error>,
    ) -> Vec<PastedImage> {
        let mut out = Vec::new();
        match acquired {
            Ok(koru_clipboard::ImagePayload::Files(paths)) => {
                clip_dbg(format!(
                    "read_clipboard_images: file_list_paths={}",
                    paths.len()
                ));
                for path in paths {
                    if let Some(image) = self.store_image_file(&path) {
                        out.push(image);
                    }
                }
                if !out.is_empty() {
                    clip_dbg(format!(
                        "read_clipboard_images: {} from file list",
                        out.len()
                    ));
                    return out;
                }
            }
            Ok(koru_clipboard::ImagePayload::Bitmap(bitmap)) => {
                if let Some(image) = self.store_image_bytes(&bitmap.bytes) {
                    clip_dbg(format!(
                        "read_clipboard_images: bitmap={}x{}",
                        bitmap.width, bitmap.height
                    ));
                    out.push(image);
                }
            }
            Ok(koru_clipboard::ImagePayload::Empty) => {
                clip_dbg("read_clipboard_images: no file list or bitmap")
            }
            Err(error) => clip_dbg(format!(
                "read_clipboard_images: acquisition failed code={:?}",
                error.code
            )),
        }
        // 3. CF_HTML data: images (Google Docs standalone image — no CF_DIB, only
        // a data: URI in HTML Format). This is the `Nothing` path (image-only
        // clipboard), so only pictures are taken, not text.
        if out.is_empty() {
            if let Some(html) = crate::clipboard_html::html_bytes() {
                clip_dbg(format!(
                    "read_clipboard_images: {} bytes of HTML Format (fallback)",
                    html.len()
                ));
                if let Ok(html) = std::str::from_utf8(&html) {
                    if let Some(raw) = crate::clipboard_html::extract(html) {
                        clip_dbg(format!(
                            "read_clipboard_images: CF_HTML fallback found {} block(s)",
                            raw.len()
                        ));
                        for block in raw {
                            if let crate::clipboard_rtf::RawBlock::Picture(p) = block {
                                match self.store_image_bytes(&p.bytes) {
                                    Some(mut img) => {
                                        if let Some(w) = p.width {
                                            img.width = Some(w);
                                        }
                                        if let Some(h) = p.height {
                                            img.height = Some(h);
                                        }
                                        img.name = p.name.clone();
                                        out.push(img);
                                    }
                                    None => clip_dbg(format!(
                                        "read_clipboard_images: CF_HTML image decode failed ({} bytes)",
                                        p.bytes.len()
                                    )),
                                }
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Start one acquisition at a time. Each queued gesture gets its own read,
    /// so changing the clipboard between two pastes preserves their order.
    fn start_clipboard_image_read(&mut self) {
        if self.clipboard_image_busy {
            return;
        }
        let Some(targets) = self.pending_clipboard_image_pastes.pop_front() else {
            return;
        };
        let tx = self.clipboard_image_tx.clone();
        let proxy = self.wake_proxy.clone();
        self.clipboard_image_busy = true;
        if let Err(error) = koru_clipboard::spawn_image_read(
            koru_clipboard::ImageLimits::default(),
            move |result| {
                let _ = tx.send(ClipboardImageResult { targets, result });
                let _ = proxy.send_event(Wake::Clipboard);
            },
        ) {
            self.clipboard_image_busy = false;
            clip_dbg(format!(
                "clipboard image worker could not start kind={:?}",
                error.kind()
            ));
        }
    }

    /// Apply completed acquisitions on the event thread. The result contains no
    /// borrowed OS storage and no raw RGBA, so this path never waits for PNG
    /// encoding or the clipboard owner.
    fn drain_clipboard_image_reads(&mut self) -> bool {
        let completed: Vec<_> = self.clipboard_image_rx.try_iter().collect();
        let mut pasted = false;
        for completed in completed {
            self.clipboard_image_busy = false;
            let images = self.store_clipboard_images(completed.result);
            for (id, target) in completed.targets {
                clip_dbg(format!(
                    "image_paste: anchor={:?} replacing={}",
                    target.anchor,
                    target.replacing.len()
                ));
                if images.is_empty() {
                    // Explicit Markdown and image-only HTML do not necessarily
                    // publish a bitmap. Their interpretation remains document
                    // policy and is the existing fallback.
                    let blocks = self.read_rich_blocks("");
                    if blocks.is_empty() {
                        clip_dbg("non_text_paste: no images or rich text found");
                        continue;
                    }
                    let ws = &mut self.runtime.workspace.ws;
                    if let Some(pane) = self.panes.get_mut(&id) {
                        pane.view.paste_rich(ws, Some(target), &blocks);
                        clip_dbg(format!(
                            "non_text_paste: paste_rich {} block(s)",
                            blocks.len()
                        ));
                        pasted = true;
                    }
                    self.request_redraw(id);
                    continue;
                }
                let ws = &mut self.runtime.workspace.ws;
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.view.paste_images(ws, Some(target), &images);
                    clip_dbg(format!(
                        "image_paste: paste_images {} image(s)",
                        images.len()
                    ));
                    pasted = true;
                }
                self.request_redraw(id);
            }
        }
        self.start_clipboard_image_read();
        pasted
    }

    /// Tag traffic between the views and the workspace **system document**
    /// (ADR-176's host seam): "Create tag" requests come up, the resolved
    /// table goes down. A view cannot see the system document, so this is
    /// where a fresh tag is minted and applied.
    ///
    /// **Lists ride the same turn**, because they are the same document and
    /// the same seam: the tasks rail's inline field sends a name and a swatch
    /// up, and the resolved table goes back down beside the tags. The one
    /// difference is that creating a list touches no node — a list is a filter
    /// (SPEC §8), so there is nothing to apply and nothing to checkpoint.
    fn service_tags(&mut self) -> bool {
        let requests: Vec<(Vec<NodeId>, String)> = self
            .panes
            .iter_mut()
            .filter_map(|(_, pane)| pane.view.take_wants_create_tag())
            .collect();
        // Workspace-dictionary words ride the same seam (D3): a view cannot
        // see the system document, so the word comes up and the refreshed
        // overlay goes down through `set_workspace_words` below.
        let words: Vec<(String, String)> = self
            .panes
            .iter_mut()
            .filter_map(|(_, pane)| pane.view.take_wants_workspace_word())
            .collect();
        for (language, word) in words {
            if self
                .runtime
                .workspace
                .add_workspace_dictionary_word(&language, &word)
            {
                crate::spelling::set_workspace_words(self.runtime.workspace.workspace_dictionary());
            }
        }
        let mut edited = false;
        for (nodes, name) in requests {
            if let Some(tag) = self.runtime.workspace.create_tag(&name) {
                // Applying is a document edit like any other: checkpointed so
                // it does not fold into typing, one step across every node.
                self.runtime.workspace.ws.checkpoint();
                let _ = self.runtime.workspace.ws.add_tag_to_all(&nodes, tag);
                self.runtime.workspace.touched();
                edited = true;
            }
        }
        // `/task_list`'s *New list*: the list is made, and the rows that
        // asked join it — made tasks first when they are not, the way a
        // chosen list joins in the editor, so the two doors are one step each
        // and one outcome. Attributed and attempted as the editor's own
        // `/task` would be.
        let joins: Vec<(Vec<NodeId>, String, Option<String>)> = self
            .panes
            .iter_mut()
            .filter_map(|(_, pane)| pane.view.take_wants_join_new_list())
            .collect();
        for (nodes, name, color) in joins {
            let Some(list) = self.runtime.workspace.create_list(&name, color.as_deref()) else {
                continue;
            };
            let mut host = crate::tasks::TaskHost::default();
            let ws = &mut self.runtime.workspace.ws;
            ws.checkpoint();
            let joined = koru_core::tasks::compose(ws, |ws| {
                for &node in &nodes {
                    let outcome = koru_core::tasks::ensure_task(
                        ws,
                        node,
                        koru_core::attributes::TaskReason::Explicit,
                    )?;
                    host.attempt(outcome.causation, &outcome.activity);
                }
                ws.set_attribute_on_all(
                    &nodes,
                    koru_core::attributes::KEY_LIST,
                    koru_core::node::AttrValue::Text(list.to_storage()),
                )
                .map(|_| ())
            });
            if joined.is_ok() {
                self.runtime.workspace.touched();
                edited = true;
            }
        }
        // The rail's "+ New list", committed. Nothing is applied to a node:
        // a list is a filter, so creating one only adds a row to the table,
        // and membership is set later on a task like any other attribute.
        let wanted: Vec<(String, Option<String>)> = self
            .panes
            .iter_mut()
            .filter_map(|(_, pane)| pane.view.take_wants_create_list())
            .collect();
        for (name, color) in wanted {
            self.runtime.workspace.create_list(&name, color.as_deref());
        }
        // ...and the rail's management of the lists that already exist. Renames
        // and recolours touch the registry alone; a delete tombstones the record
        // and then sweeps the memberships out of the document this window has
        // open, which is the one document a host can reach — see
        // `crate::tasks::serve_list_edit`, which owns that ordering so this
        // stays a sweep.
        let edits: Vec<crate::tasks::ListEdit> = self
            .panes
            .iter_mut()
            .flat_map(|(_, pane)| pane.view.take_wants_list_edits())
            .collect();
        for edit in edits {
            edited |= crate::tasks::serve_list_edit(&mut self.runtime.workspace, &edit);
        }
        // The Tasks surface's ticks, removals and captures. Collected first for
        // the borrow, then served one at a time — `crate::tasks::serve` owns the
        // ordering and the activity attempt, so this stays a sweep rather than a
        // second copy of ADR-273.
        let commands: Vec<crate::tasks::TaskCommand> = self
            .panes
            .iter_mut()
            .flat_map(|(_, pane)| pane.view.take_wants_task_commands())
            .collect();
        for command in commands {
            edited = true;
            let outcome = crate::tasks::serve(&mut self.runtime.workspace, &command);
            // **The switch says what it is documented to say.** `ENV.md` promises
            // `KORU_TASKS_DEBUG` reports "task command outcomes and the desktop
            // wrapper's coalesced counters", and until this line the only thing
            // that ever printed was the content-burst flush — so the one
            // question the switch exists to answer, *did the command run*, was
            // the one it could not. Closed tokens and counts only: the command's
            // shape, never a title, a date or a row's text.
            tasks_debug_log(format_args!(
                "served {} -> {} | {}",
                command.token(),
                match &outcome {
                    Ok(landed) if landed.published => "ok".to_owned(),
                    Ok(_) => "ok, unpublished".to_owned(),
                    Err(error) => format!("refused: {error}"),
                },
                crate::tasks::counters().describe()
            ));
            // **Both halves of the return channel**, to every pane: a refusal
            // gives a capture its typing back, and a receipt is what lets the
            // Tasks surfaces ask the read model again *because* the write
            // landed — after it, with its version in hand — rather than on
            // the turn the command was merely queued.
            match outcome {
                Ok(landed) => {
                    for pane in self.panes.values_mut() {
                        pane.view.task_command_landed(&landed);
                    }
                }
                Err(error) => {
                    for pane in self.panes.values_mut() {
                        pane.view.task_command_failed(&command, &error);
                    }
                }
            }
        }
        // **The delegation's crumb** (D7, D8). Same sweep and same reason as
        // the task commands above: the document a delegation came from may not
        // be the one on screen, so the surface asks and the shell writes.
        // A refusal is reported and nothing else: the run is real either way,
        // and the surface has already said so.
        let pills: Vec<crate::runpill::RunPill> = self
            .panes
            .iter_mut()
            .flat_map(|(_, pane)| pane.view.take_wants_run_pills())
            .collect();
        for pill in pills {
            edited = true;
            if let Err(error) = crate::runpill::serve(&mut self.runtime.workspace, &pill) {
                // Ids and a reason, never the objective or the document's text.
                tracing::warn!(
                    run = %pill.run.to_storage(),
                    document = %pill.document.to_storage(),
                    "the delegation pill could not be written: {error}"
                );
            }
        }
        let schedule_pills: Vec<crate::schedulepill::SchedulePill> = self
            .panes
            .iter_mut()
            .flat_map(|(_, pane)| pane.view.take_wants_schedule_pills())
            .collect();
        for pill in schedule_pills {
            edited = true;
            if let Err(error) = crate::schedulepill::serve(&mut self.runtime.workspace, &pill) {
                tracing::warn!(
                    schedule = %pill.schedule.to_storage(),
                    document = %pill.document.to_storage(),
                    "the schedule pill could not be written: {error}"
                );
            }
        }
        if self.runtime.workspace.take_tags_changed() {
            self.tag_table = self.runtime.workspace.tags_snapshot();
            // The flag really means "the system document changed", and the
            // workspace spelling dictionary lives there too (D3) — refresh
            // the overlay so a collaborator's word clears squiggles here.
            crate::spelling::set_workspace_words(self.runtime.workspace.workspace_dictionary());
        }
        if self.runtime.workspace.take_lists_changed() {
            self.list_table = self.runtime.workspace.lists_snapshot();
        }
        // Pushed every turn, compared inside the view: a pane or tab opened
        // since the last change still has to be given the table, and the
        // compare is a few small structs.
        let mut repaint = edited;
        // The workspace rides the same push for the same reason: a tab opened
        // since the last switch still has to be told, and the compare is one
        // `Option<Uuid>`.
        let workspace = self.runtime.workspace.current();
        for pane in self.panes.values_mut() {
            if pane.view.set_tag_table(&self.tag_table)
                | pane.view.set_list_table(&self.list_table)
                | pane.view.set_workspace(workspace)
            {
                pane.window.request_redraw();
                repaint = true;
            }
        }
        if edited {
            self.adopt_everywhere();
        }
        repaint
    }

    /// Open the file picker for any `/image` request, then decode anything a
    /// peer added that this window has not shown yet.
    /// `/image` requests (a blocking file dialog) and any images a peer added.
    /// Returns whether an image was newly decoded, so the loop can redraw.
    fn service_images(&mut self) -> bool {
        self.sync_image_scope();
        // Image insertion, image cut, and deferred rich/image paste all mutate
        // through this service rather than through the ordinary key/mouse paths
        // that call `Workspace::touched`. Compare once around the service so a
        // future image mutation cannot become local-only by omitting that call.
        let document_before = self.runtime.workspace.ws.version();
        let mut pasted = self.drain_clipboard_image_reads();
        // Attachment pill activations (open / reveal / save).
        let file_actions: Vec<(WindowId, koru_core::DocumentId, crate::FileAction)> = self
            .panes
            .iter_mut()
            .filter_map(|(id, pane)| {
                pane.view
                    .take_wants_file_action()
                    .map(|(file, action)| (*id, file, action))
            })
            .collect();
        if !file_actions.is_empty() {
            crate::attach_debug!(
                "shell service_images file_actions n={} (MainView should have converted these to PaneRequest and returned None)",
                file_actions.len()
            );
        }
        for (_id, file, action) in file_actions {
            crate::attach_debug!(
                "shell service_images leftover FileAction::{action:?} file={} — MainView did not intercept",
                file.to_storage()
            );
            match action {
                crate::FileAction::Open => {
                    self.open_file(file);
                }
                crate::FileAction::Save => {
                    self.save_file(file);
                }
                crate::FileAction::Reveal => {
                    // MainView intercepts Reveal; fallback open for bare views.
                    self.open_file(file);
                }
                crate::FileAction::Restore => {
                    // MainView intercepts Restore via PaneRequest; no-op here.
                }
            }
        }
        // *Save as…* on a drawn diagram. The container turns it into a
        // `PaneRequest::SaveDiagram` the way it does a file's Save; a bare
        // document window has no container, so what is left here is served
        // directly, against the window that asked.
        let diagram_saves: Vec<(WindowId, crate::SaveDiagram)> = self
            .panes
            .iter_mut()
            .filter_map(|(id, pane)| {
                pane.view
                    .take_wants_save_diagram_as(&self.runtime.workspace.ws)
                    .map(|request| (*id, request))
            })
            .collect();
        for (id, request) in diagram_saves {
            self.save_diagram(id, request);
        }
        // *Copy image* on a drawn diagram, the same way.
        let diagram_copies: Vec<(WindowId, crate::CopyDiagram)> = self
            .panes
            .iter_mut()
            .filter_map(|(id, pane)| {
                pane.view
                    .take_wants_copy_diagram()
                    .map(|request| (*id, request))
            })
            .collect();
        for (id, request) in diagram_copies {
            self.copy_diagram(id, &request);
        }
        // /attach and /image both open a native picker on the main thread,
        // matching each other: no parent window (a parented rfd dialog from
        // about_to_wait has failed silently on Windows), and only while no
        // other content import is already running.
        let attachments: Vec<(WindowId, NodeId)> = if self.import_busy {
            let skipped: Vec<_> = self
                .panes
                .iter_mut()
                .filter_map(|(_, pane)| pane.view.take_wants_attachment())
                .collect();
            if !skipped.is_empty() {
                crate::attach_debug!(
                    "shell service_images /attach DROPPED n={} because import_busy=true",
                    skipped.len()
                );
            }
            Vec::new()
        } else {
            self.panes
                .iter_mut()
                .filter_map(|(id, pane)| pane.view.take_wants_attachment().map(|node| (*id, node)))
                .collect()
        };
        for (id, node) in attachments {
            crate::attach_debug!(
                "shell service_images opening Attach picker for node={node:?} window={id:?}"
            );
            let paths = crate::dialogs::pick_open_files("Attach a file", &[], None, None);
            match &paths {
                Some(paths) => crate::attach_debug!(
                    "shell Attach picker chose n={} basenames={:?}",
                    paths.len(),
                    paths
                        .iter()
                        .map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .collect::<Vec<_>>()
                ),
                None => crate::attach_debug!("shell Attach picker cancelled or returned None"),
            }
            if let Some(paths) = paths {
                self.pending_attachment = Some((id, node));
                self.import_content(id, "files", crate::ImportSourceSelection::Files(paths));
            } else if let Some(pane) = self.panes.get_mut(&id) {
                pane.view
                    .attachment_cancelled(&mut self.runtime.workspace.ws, node);
            }
        }
        let requests: Vec<(WindowId, NodeId)> = self
            .panes
            .iter_mut()
            .filter_map(|(id, pane)| pane.view.take_wants_image().map(|n| (*id, n)))
            .collect();
        let n_image_req = requests.len();
        for (id, node) in requests {
            if let Some(path) = pick_image_file() {
                self.import_image(id, Some(node), path);
            }
        }
        // A Ctrl+C/X on an image puts it on the system clipboard by blob hash.
        // Cut defers deleting the row until after this write so OneNote and
        // friends see a real bitmap.
        let copies: Vec<(winit::window::WindowId, String)> = self
            .panes
            .iter_mut()
            .filter_map(|(id, pane)| pane.view.take_wants_copy_image().map(|hash| (*id, hash)))
            .collect();
        let n_copy = copies.len();
        for (wid, hash) in &copies {
            self.copy_image_hash(hash);
            if let Some(pane) = self.panes.get_mut(wid) {
                let _ = pane
                    .view
                    .apply_pending_image_cut(&mut self.runtime.workspace.ws);
            }
        }
        // A Ctrl+C on text or rows armed CF_HTML + plain. Write both in one
        // OpenClipboard session after embedding image blobs as data: URIs.
        let htmls: Vec<(String, String)> = self
            .panes
            .iter_mut()
            .filter_map(|(_, pane)| pane.view.take_wants_copy_html())
            .collect();
        for (html, plain) in htmls {
            let embedded = {
                let blobs = self.runtime.workspace.active_blobs();
                crate::clipboard_html::embed_koru_images(&html, |hash| {
                    let blobs = blobs.as_ref()?;
                    let bytes = blobs.get(&BlobHash(hash.to_owned())).ok()?;
                    Some(("image/png".into(), bytes))
                })
            };
            crate::clipboard_html::set_text_and_html(&plain, &embedded);
        }
        // A Ctrl+V that found a non-text clipboard. The shell owns the bytes,
        // so it reads them here and hands the view ready-made blocks to insert
        // at the block target the editor recorded.
        let pastes: Vec<(WindowId, PasteTarget)> = self
            .panes
            .iter_mut()
            .filter_map(|(id, pane)| pane.view.take_wants_paste_image().map(|n| (*id, n)))
            .collect();
        let rich: Vec<(WindowId, (PasteTarget, String))> = self
            .panes
            .iter_mut()
            .filter_map(|(id, pane)| pane.view.take_wants_paste_rich().map(|r| (*id, r)))
            .collect();
        if n_image_req + n_copy + pastes.len() + rich.len() > 0 {
            clip_dbg(format!(
                "service_images: drains /image={n_image_req} copy={n_copy} image_paste={} rich_paste={}",
                pastes.len(),
                rich.len()
            ));
        }
        if !pastes.is_empty() {
            self.pending_clipboard_image_pastes.push_back(pastes);
            self.start_clipboard_image_read();
        }
        // A Ctrl+V of *foreign text* in a document, deferred by the editor so the
        // shell can interleave any RTF pictures with it. The carried text is
        // used as-is for the no-picture fallback, so plain paste never depends on
        // a second clipboard read (which raced and dropped text last time).
        for (id, (target, text)) in rich {
            clip_dbg(format!(
                "rich_paste: anchor={:?} replacing={} carried_text={} bytes",
                target.anchor,
                target.replacing.len(),
                text.len()
            ));
            let mut blocks = self.read_rich_blocks(&text);
            let ws = &mut self.runtime.workspace.ws;
            if let Some(pane) = self.panes.get_mut(&id) {
                if blocks.is_empty() {
                    clip_dbg("rich_paste: no images found — pasting carried text");
                    if !text.trim().is_empty() {
                        pane.view.paste_text(ws, &text);
                        pasted = true;
                    }
                } else {
                    // If the blocks have no text (CF_HTML images-only, from a
                    // browser), prepend the carried text so what the editor
                    // deferred isn't lost.
                    if blocks.iter().all(|b| matches!(b, RichBlock::Image(_)))
                        && !text.trim().is_empty()
                    {
                        blocks.insert(
                            0,
                            RichBlock::Text(crate::RichTextBlock {
                                text: text.clone(),
                                list: None,
                                heading: None,
                                kind: crate::RichTextKind::Paragraph,
                                runs: Vec::new(),
                                links: Vec::new(),
                            }),
                        );
                    }
                    pane.view.paste_rich(ws, Some(target), &blocks);
                    clip_dbg(format!("rich_paste: paste_rich {} block(s)", blocks.len()));
                    pasted = true;
                }
            }
            self.request_redraw(id);
        }
        if self.runtime.workspace.ws.version() != document_before {
            self.runtime.workspace.touched();
            self.adopt_everywhere();
        }
        self.decode_missing_images() || pasted
    }

    /// Choose and decode the richest trustworthy clipboard representation.
    /// Explicit Markdown MIME wins; a conservative content gate covers
    /// producers that expose Markdown only as plain text. Otherwise Word's RTF
    /// and browser CF_HTML paths retain their existing precedence.
    fn read_rich_blocks(&mut self, carried_text: &str) -> Vec<RichBlock> {
        if clipboard_debug_on() {
            let names = crate::clipboard_markdown::format_names();
            clip_dbg(format!(
                "clipboard_formats: count={} names=[{}]",
                names.len(),
                names.join(", ")
            ));
        }

        if let Some((mime, markdown)) = crate::clipboard_markdown::explicit_markdown() {
            let blocks = crate::clipboard_markdown::parse(&markdown);
            clip_dbg(format!(
                "markdown_paste: source=mime mime={mime} bytes={} lines={} blocks={}",
                markdown.len(),
                markdown.lines().count(),
                blocks.len()
            ));
            if !blocks.is_empty() {
                return blocks;
            }
        } else if crate::clipboard_markdown::looks_like_markdown(carried_text) {
            let blocks = crate::clipboard_markdown::parse(carried_text);
            clip_dbg(format!(
                "markdown_paste: source=inferred_plain bytes={} lines={} blocks={}",
                carried_text.len(),
                carried_text.lines().count(),
                blocks.len()
            ));
            if !blocks.is_empty() {
                return blocks;
            }
        } else {
            clip_dbg(format!(
                "markdown_paste: source=none plain_bytes={} plain_lines={}",
                carried_text.len(),
                carried_text.lines().count()
            ));
        }

        // RTF (Word).
        if let Some(rtf) = crate::clipboard_rtf::rtf_bytes() {
            clip_dbg(format!("read_rich_blocks: {} bytes of RTF", rtf.len()));
            let Ok(rtf) = std::str::from_utf8(&rtf) else {
                clip_dbg("read_rich_blocks: RTF not utf-8");
                return Vec::new();
            };
            let count = |needle: &str| rtf.matches(needle).count();
            clip_dbg(format!(
                "read_rich_blocks: raw counts pict={} pngblip={} jpegblip={} emfblip={} wmetafile={} shppict={} nonshppict={}",
                count(r"\pict"),
                count(r"\pngblip"),
                count(r"\jpegblip"),
                count(r"\emfblip"),
                count(r"\wmetafile"),
                count(r"\shppict"),
                count(r"\nonshppict"),
            ));
            let mut out = Vec::new();
            if let Some(raw) = crate::clipboard_rtf::extract(rtf) {
                clip_dbg(format!(
                    "read_rich_blocks: RTF extract returned {} block(s)",
                    raw.len()
                ));
                for block in raw {
                    match block {
                        crate::clipboard_rtf::RawBlock::Text(t) => out.push(RichBlock::Text(t)),
                        crate::clipboard_rtf::RawBlock::Picture(p) => {
                            match self.store_image_bytes(&p.bytes) {
                                Some(mut img) => {
                                    // Use the source's display dimensions
                                    // (`\picwgoal`/`\pichgoal`) when available so
                                    // the image isn't stretched to full-res.
                                    if let Some(w) = p.width {
                                        img.width = Some(w);
                                    }
                                    if let Some(h) = p.height {
                                        img.height = Some(h);
                                    }
                                    img.name = p.name.clone();
                                    out.push(RichBlock::Image(img));
                                }
                                None => clip_dbg(format!(
                                    "read_rich_blocks: RTF picture decode failed ({} bytes, mime={})",
                                    p.bytes.len(),
                                    p.mime
                                )),
                            }
                        }
                    }
                }
            } else {
                clip_dbg("read_rich_blocks: RTF extract found no picture");
            }
            if !out.is_empty() {
                return out;
            }
        } else {
            clip_dbg("read_rich_blocks: no RTF format on clipboard");
        }
        // CF_HTML (browser data: URI images).
        if let Some(html) = crate::clipboard_html::html_bytes() {
            clip_dbg(format!(
                "read_rich_blocks: {} bytes of HTML Format",
                html.len()
            ));
            let Ok(html) = std::str::from_utf8(&html) else {
                return Vec::new();
            };
            // CF_HTML (browser / Google Docs) — interleaved text + data: images.
            if let Some(raw) = crate::clipboard_html::extract(html) {
                clip_dbg(format!(
                    "read_rich_blocks: CF_HTML extract returned {} block(s)",
                    raw.len()
                ));
                let mut out = Vec::new();
                for block in raw {
                    match block {
                        crate::clipboard_rtf::RawBlock::Text(t) => {
                            out.push(RichBlock::Text(t));
                        }
                        crate::clipboard_rtf::RawBlock::Picture(p) => {
                            match self.store_image_bytes(&p.bytes) {
                                Some(mut img) => {
                                    if let Some(w) = p.width {
                                        img.width = Some(w);
                                    }
                                    if let Some(h) = p.height {
                                        img.height = Some(h);
                                    }
                                    img.name = p.name.clone();
                                    out.push(RichBlock::Image(img));
                                }
                                None => clip_dbg(format!(
                                    "read_rich_blocks: CF_HTML image decode failed ({} bytes)",
                                    p.bytes.len()
                                )),
                            }
                        }
                    }
                }
                return out;
            }
        }
        clip_dbg("read_rich_blocks: no RTF or CF_HTML");
        Vec::new()
    }

    /// Copy decoded pixels for a blob hash to the system clipboard.
    fn copy_image_hash(&self, hash: &str) {
        let Some(brush) = self.images.get(hash) else {
            return;
        };
        let image = koru_clipboard::RgbaBitmap {
            width: brush.image.width.max(1) as usize,
            height: brush.image.height.max(1) as usize,
            bytes: brush.image.data.data().to_vec(),
        };
        let _ = koru_clipboard::system().set_bitmap(&image);
    }

    /// Decode any image whose bytes are present but not yet decoded here — on
    /// startup, after a peer added one, anything. Returns whether it decoded
    /// anything, so the caller can request a redraw.
    ///
    /// **Gated on the document having changed.** This runs from
    /// `about_to_wait`, which fires on every wake — at least three times a
    /// second forever, since the caret has to blink. Unconditionally it was a
    /// walk of both realms with a full `read` per node, so a large workspace was
    /// paying for a complete copy of itself several times a second while sitting
    /// idle. The scan is cheap now (`blob_hashes` reads one key per node), but
    /// asking a question whose answer cannot have changed is still the wrong
    /// shape, and the flag is what says so.
    /// The decoded cache's scope: the workspace whose association governs what
    /// may be drawn right now, as a plain integer.
    fn image_scope_id(&self) -> koru_draw::image::Scope {
        self.runtime
            .workspace
            .active_workspace()
            .unwrap_or(self.runtime.current)
            .as_uuid()
            .as_u128()
    }

    fn decode_missing_images(&mut self) -> bool {
        self.sync_image_scope();
        let dirty = self.images_dirty.then(|| {
            self.images_dirty = false;
            self.runtime
                .workspace
                .ws
                .blob_hashes()
                .unwrap_or_default()
                .into_iter()
                .filter(|hash| !self.images.contains(hash))
                .collect::<Vec<_>>()
        });
        let workspace = self
            .runtime
            .workspace
            .active_workspace()
            .unwrap_or(self.runtime.current);
        let blobs = self.runtime.workspace.active_blobs();
        self.image_loader
            .decode_missing(workspace, &mut self.images, dirty, blobs.as_ref())
    }

    /// Re-scope image work when the selected tab changes.
    ///
    /// A different document in the same workspace needs a fresh hash scan. A
    /// different workspace also needs an empty decoded cache: otherwise a hash
    /// decoded for one workspace can make another workspace look as though it
    /// durably owns bytes it does not have.
    fn sync_image_scope(&mut self) {
        let next = self.runtime.workspace.active().map(|document| {
            (
                self.runtime
                    .workspace
                    .active_workspace()
                    .unwrap_or(self.runtime.current),
                document,
            )
        });
        // **Unconditionally, before the early return.** The cache answers for
        // exactly one workspace's associations, and this is the one line that
        // says which — so there is no ordering in which a frame is drawn
        // against a scope that has moved on. It is a field write.
        self.images.set_scope(
            next.map(|scope| scope.0.as_uuid().as_u128())
                .unwrap_or(ImageCache::UNSCOPED),
        );
        if next == self.image_scope {
            return;
        }
        let workspace_changed = self.image_scope.map(|scope| scope.0) != next.map(|scope| scope.0);
        if workspace_changed {
            // **The pixels stay.** Emptying the cache was the safe answer while
            // bytes were filed per workspace and nothing here could tell
            // "decoded" from "allowed" apart; the scope above now does, so
            // coming back to a tab costs nothing instead of a full re-decode.
            // Only the in-flight loads are dropped, because their results name
            // the workspace that is no longer current.
            self.image_loader.clear();
        }
        self.image_scope = next;
        self.images_dirty = true;
    }

    /// Open a window showing `editor`, and return its id.
    fn open_pane(&mut self, event_loop: &ActiveEventLoop, mut view: H::V, title: &str) -> WindowId {
        // No OS titlebar: the view draws the caption row itself, so the tabs can
        // share it with the window buttons.
        //
        // This is cheaper than it looks on Windows. winit keeps `WS_CAPTION` and
        // `WS_SIZEBOX` on the window and removes the frame by answering
        // `WM_NCCALCSIZE` instead, so **aero snap, Win+Arrow, snap layouts and
        // the OS resize borders all still work** — the only things left to do
        // ourselves are dragging by the caption and the three buttons.
        //
        // The title is still set: it is what the taskbar and Alt+Tab show.
        let global_search = view.is_global_search();
        let preferred = view.preferred_window_size();
        let window_size = if global_search {
            (620.0, search_window_height(&self.search_geometry))
        } else if let Some((size, _)) = preferred {
            size
        } else {
            self.runtime.settings.size
        };
        let mut attrs = Window::default_attributes()
            .with_title(title.to_string())
            .with_window_icon(self.host.window_icon().or_else(app_icon))
            // **Hidden until the first frame is presented.** Shader compilation
            // and the first layout both happen after the window exists, and a
            // visible window during that interval is the blank rectangle the
            // launch used to show for seconds. `reveal_pane` draws into the
            // hidden surface and only then shows it — the same pattern
            // `koru-window`'s search panel has always used.
            .with_visible(false)
            .with_inner_size(winit::dpi::LogicalSize::new(window_size.0, window_size.1));
        if global_search {
            attrs = attrs.with_min_inner_size(winit::dpi::LogicalSize::new(
                620.0,
                desktop_preferences::GLOBAL_SEARCH_MIN_HEIGHT as f32,
            ));
        } else {
            let (min_w, min_h) = preferred.map_or(crate::settings::MIN_WINDOW_SIZE, |(_, min)| min);
            attrs = attrs.with_min_inner_size(winit::dpi::LogicalSize::new(min_w, min_h));
        }
        // **macOS keeps its titlebar; everywhere else we draw our own.**
        //
        // A borderless window on macOS is the wrong shape in four separate
        // ways, and one attribute fixes all of them: no traffic lights, square
        // corners where every other app is rounded, no resize border (winit
        // does not implement `drag_resize_window` there — see `resize`), and no
        // native drag. AppKit owns a ~28 px strip above our caption, which is
        // exactly the bar a Mac user expects to find them in.
        //
        // The title is hidden rather than shown: the tab strip below already
        // says what is open, and a second copy centred above it is noise. The
        // window still *has* the title — that is what Mission Control and the
        // Window menu read.
        //
        // **The search panel keeps the frame but loses the buttons.** It is
        // summoned by a chord and dismissed on blur, not a window you manage,
        // so traffic lights on it would offer to minimise and zoom something
        // with no business doing either. Going borderless to remove them was
        // the obvious move and the wrong one: a borderless `NSWindow` has
        // **square corners**, which is exactly what makes a floating panel look
        // unfinished beside every other Mac app. Keeping the `Titled` style
        // mask and hiding the buttons gets the platform's own corner radius and
        // shadow, with no furniture on it.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attrs = attrs
                .with_titlebar_transparent(false)
                .with_title_hidden(true);
            if global_search {
                attrs = attrs.with_titlebar_buttons_hidden(true);
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            attrs = attrs.with_decorations(false);
        }
        // Only the first window restores a remembered position; a second one
        // opening on top of it would be worse than letting the OS place it.
        // And a remembered position is only honoured where the desktop still
        // has it: one saved on a monitor that has since been unplugged, at a
        // resolution since dropped, describes nothing, and restoring it opened
        // the window off-screen where only the taskbar could see it.
        let first = self.panes.is_empty();
        let monitors: Vec<(i32, i32, i32, i32)> = event_loop
            .available_monitors()
            .map(|monitor| {
                let origin = monitor.position();
                let size = monitor.size();
                (
                    origin.x,
                    origin.y,
                    origin.x.saturating_add(size.width as i32),
                    origin.y.saturating_add(size.height as i32),
                )
            })
            .collect();
        let scale = event_loop
            .primary_monitor()
            .map_or(1.0, |monitor| monitor.scale_factor());
        let remembered =
            restorable_position(self.runtime.settings.pos, window_size, scale, &monitors);
        if let (Some((x, y)), true, false) = (remembered, first, global_search) {
            attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
        }
        let floating = view.floating_window();
        let fixed_width = view.fixed_width();
        if fixed_width {
            attrs =
                attrs.with_max_inner_size(winit::dpi::LogicalSize::new(window_size.0, 100_000.0));
        }
        if floating {
            attrs = attrs
                .with_decorations(false)
                .with_resizable(false)
                .with_window_level(winit::window::WindowLevel::AlwaysOnTop)
                .with_active(false);
            #[cfg(windows)]
            {
                use winit::platform::windows::WindowAttributesExtWindows;
                attrs = attrs.with_skip_taskbar(true);
            }
            let remembered = self.runtime.settings.floating_pos;
            let corner = remembered.or_else(|| {
                let monitor = self
                    .primary
                    .and_then(|id| self.panes.get(&id))
                    .and_then(|pane| pane.window.current_monitor())?;
                let pos = monitor.position();
                let size = monitor.size();
                let scale = monitor.scale_factor();
                let edge = (window_size.0 as f64 * scale).round() as i32;
                let margin = (24.0 * scale).round() as i32;
                Some((
                    pos.x + size.width as i32 - edge - margin,
                    pos.y + size.height as i32 - edge - margin - (48.0 * scale) as i32,
                ))
            });
            if let Some((x, y)) = corner {
                attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
            }
        }
        // `with_maximized(true)` would defeat `with_visible(false)` — see
        // `maximize_at_reveal`. Remembered on the pane, applied at reveal.
        let maximize_on_reveal =
            maximize_at_reveal(first, self.runtime.settings.maximized, global_search);
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        // **Here, and before anything can reveal it.** Every window this file
        // opens is created hidden and shown later — by `reveal_pane`, by a
        // restore from the tray, by search bringing one forward — and the
        // AccessKit adapter *panics* on a window that is already visible. This
        // is the one creation site those reveals all lead back to, so
        // installing the provider here covers every one of them by
        // construction rather than by auditing them one at a time. See
        // `koru_window::accessibility` for the trap itself.
        let access = self.seat_accessibility(event_loop, &window);
        // **Where this view lives**, told once and never again. An occurrence
        // is scoped by window and view, and a view does not choose either — so
        // it is handed the pair here rather than through `semantics`, which
        // stays the narrowest signature that can answer the question.
        view.seat_accessibility(access.slot(), access.view_slot());
        if global_search {
            let monitor = self
                .primary
                .and_then(|id| self.panes.get(&id))
                .and_then(|pane| pane.window.current_monitor());
            center_window(&window, monitor);
        }
        let id = window.id();
        // System → `None` so the OS can send `ThemeChanged`. Light/Dark lock
        // the chrome. Opening is not a "change", so this is set here rather
        // than only in `sync_window_theme`. Border/corners follow next.
        window.set_theme(window_theme_for_pref());
        // Flush is cached for the hairline; frame style (corners + shadow)
        // follows maximised only so a snap keeps its drop shadow.
        let flush = hairline::edges(&window);
        frame::apply(&window, theme::p().panel_border, window.is_maximized());
        let size = window.inner_size();

        let span = timing::Span::start("gpu surface");
        let mut surface = match pollster::block_on(self.context.create_surface(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            PRESENT_MODE,
        )) {
            Ok(surface) => surface,
            Err(error) => {
                // A configured backend degrades to the default before anything
                // is fatal; only the default failing too is the honest dialog —
                // which replaces what used to be a bare `expect` panic.
                //
                // **Only while this is the first window.** The fallback
                // replaces the whole `RenderContext`, and every open pane holds
                // a surface and a renderer belonging to the old one — swapping
                // it out from under them would leave windows that draw nothing.
                // A later window failing on a backend the first one is already
                // drawing with is not a backend problem anyway.
                if self.panes.is_empty() && koru_window::configured_backends().is_some() {
                    tracing::error!(
                        target: "koru::gpu",
                        "surface creation failed on the configured backend ({error}); \
                         retrying with the default"
                    );
                    eprintln!("[Koru][gpu] configured backend failed ({error}); using the default");
                    self.context = context_with(default_backends());
                    // The prebuilt renderer belongs to the abandoned device.
                    self.renderers.clear();
                    pollster::block_on(self.context.create_surface(
                        window.clone(),
                        size.width.max(1),
                        size.height.max(1),
                        PRESENT_MODE,
                    ))
                } else {
                    Err(error)
                }
                .unwrap_or_else(|error| {
                    crate::startup::fatal(&format!(
                        "Koru could not initialise a drawing surface: {error}"
                    ));
                    std::process::exit(1);
                })
            }
        };
        span.stop();
        // vello's helper hard-codes two, which lets the driver hold a finished
        // frame back while the next keystroke is already waiting behind it.
        // `set_present_mode` is the public way to re-apply the whole config.
        surface.config.desired_maximum_frame_latency = 1;
        self.context.set_present_mode(&mut surface, PRESENT_MODE);

        // **One renderer per *device*, not per window.** It was per window, on
        // the grounds that `Renderer` is cheap next to a window and tying it to
        // the surface removed a question. It is not cheap: `Renderer::new`
        // compiles every vello pipeline, there is no pipeline cache to be had on
        // DX12 (see below), and the cost is paid again for each window opened.
        // Measured in a real session as `maintenance.tray` blocking the main
        // thread for **7.6 seconds** — that is opening the global-search window
        // from the notification area, and it is the same bill for Ctrl+N and for
        // popping a tab out.
        //
        // Nothing about a `Renderer` belongs to a surface: `render_to_texture`
        // is handed the device, the queue, the scene and the target view. Two
        // surfaces on one device share one, and the map is keyed by `dev_id`
        // rather than assuming there is only ever one device.
        let dev_id = surface.dev_id;
        let prebuilt = self.renderers.contains_key(&dev_id);
        if !prebuilt {
            // The warm-up missed: the switch was set, the build failed there,
            // or this surface forced a second device (hybrid GPU). The second
            // device has no error watch yet either.
            watch_device_errors(&self.context.devices[dev_id].device);
            let span = timing::Span::start("gpu renderer");
            // `build_renderer` absorbs the naga shader-compile panic and
            // retries serially (see `koru_window::renderer`); an `Err` here
            // means even the serial attempt failed, and a window that cannot
            // draw is a launch that cannot continue — the one honest answer is
            // the dialog, not an uncaught panic.
            let renderer = match koru_window::build_renderer(&self.context.devices[dev_id].device) {
                Ok(renderer) => renderer,
                Err(message) => {
                    crate::startup::fatal(&format!(
                        "Koru could not initialise its renderer: {message}"
                    ));
                    std::process::exit(1);
                }
            };
            span.stop();
            self.renderers.insert(dev_id, renderer);
        }
        if first {
            // The launch line a slow-start report needs, so it is not gated on
            // a switch. "off-thread" is the overlapped path; "main thread"
            // means the warm-up missed and the compile was paid in full here.
            let ms = process_started().elapsed().as_millis();
            let built_on = if prebuilt {
                "off-thread"
            } else {
                "main thread"
            };
            tracing::info!(target: "koru::gpu", built_on, "gpu pipeline ready to render in {ms}ms");
            timing::record("gpu pipeline ready", process_started().elapsed());
        }

        let mut view = view;
        view.attach_surface_host(crate::native_surface::SurfaceHost::new(
            window.clone(),
            self.wake_proxy.clone(),
        ));
        self.panes.insert(
            id,
            Pane {
                inspector: None,
                inspector_window: None,
                inspector_geometry: None,
                window,
                title: title.to_string(),
                surface,
                view,
                cursor: (0.0, 0.0),
                cursor_icon: CursorIcon::Default,
                resize_edge: None,
                maximized: None,
                flush,
                moved_at: None,
                // Only the first window restores a remembered pin; a window
                // opened later (Ctrl+N, a popped-out document) starts unpinned,
                // the way it starts un-maximised.
                pinned: floating || (first && self.runtime.settings.pinned),
                fixed_width,
                size_locked: false,
                saved_geometry: None,
                recording: None,
                last_click: None,
                pressed: false,
                drag: None,
                press_origin: None,
                // The first frame is always owed.
                needs_frame: std::cell::Cell::new(true),
                maximize_on_reveal: std::cell::Cell::new(maximize_on_reveal),
                focused_once: false,
                focused: false,
                quarantine: Default::default(),
                access,
            },
        );
        // Everything a new window inherits from the app is applied here.
        self.sync_pinned(id);
        self.sync_viewport(id);
        id
    }

    /// Open a document in its own window, or focus the one already showing it.
    ///
    /// A document is an outline rooted at the document node, so this is the zoom
    /// A document opens in its own window, showing the same workspace through a
    /// `BuiltInDocument` instead of a `TaskOutline`.
    fn open_document(&mut self, event_loop: &ActiveEventLoop, doc: NodeId) {
        let _watch = timing::Watch::start("open_document");
        if let Some(pane) = self.panes.values().find(|p| {
            p.view.document_root() == Some(doc)
                || p.view
                    .tab_presence(koru_core::DocumentId::from_uuid(doc.as_uuid()))
                    != crate::TabPresence::Absent
        }) {
            pane.window.focus_window();
            return;
        }
        // A host with no document view declines, and nothing opens.
        let Some(mut view) = self.host.open_document(&self.runtime.workspace.ws, doc) else {
            return;
        };
        // A popped-out window inherits the remembered close choice, so one that
        // was told to remember skips its close dialog from the first close.
        view.set_popout_close(self.runtime.settings.popout_close);
        let document = koru_workspace::name_of(&self.runtime.workspace.ws, doc);
        let title = self.window_title(&document);
        let id = self.open_pane(event_loop, view, &title);
        self.reveal_pane(id);
        // **A detached document has to become the key window**, or it draws
        // perfectly and answers no keystroke — which is what it did. Every
        // other path that opens a window focuses it (`open_search`, and the
        // branch above for a document already open); this one did not, and
        // macOS does not hand a newly created window the keyboard by itself.
        if let Some(pane) = self.panes.get(&id) {
            pane.window.focus_window();
        }
        activate_app();
        self.request_redraw(id);
    }

    fn open_search(&mut self, event_loop: &ActiveEventLoop, trigger: Option<PhysicalKey>) {
        // Global search is its own surface. In particular, a hotkey received
        // while Koru is in the tray must not restore the ordinary workspace
        // windows behind it; the tray remains available after search closes.
        if let Some((id, window)) = self
            .panes
            .iter()
            .find(|(_, pane)| pane.view.is_global_search())
            .map(|(id, pane)| (*id, pane.window.clone()))
        {
            if let Some(trigger) = trigger
                && let Some(pane) = self.panes.get_mut(&id)
            {
                pane.quarantine.suppress(trigger, Instant::now());
            }
            let monitor = self
                .primary
                .and_then(|id| self.panes.get(&id))
                .and_then(|pane| pane.window.current_monitor());
            center_window(&window, monitor);
            window.set_visible(true);
            window.focus_window();
            // A global hotkey fires while another app is frontmost, so the
            // window has to arrive *in front* — on macOS that is a separate
            // act from focusing it. See `activate_app`.
            activate_app();
            return;
        }
        let Some(view) = self.host.open_search(&self.runtime.workspace.ws) else {
            return;
        };
        crate::events_host::search_opened(crate::events_host::SearchSurface::Window);
        let title = self.search_window_title();
        let id = self.open_pane(event_loop, view, &title);
        if let Some(trigger) = trigger
            && let Some(pane) = self.panes.get_mut(&id)
        {
            pane.quarantine.suppress(trigger, Instant::now());
        }
        // Windows are created hidden; the reveal draws the first frame into
        // the hidden surface and only then shows it.
        self.reveal_pane(id);
        if let Some(pane) = self.panes.get(&id) {
            pane.window.focus_window();
        }
        self.request_redraw(id);
    }

    fn open_feedback(&mut self, event_loop: &ActiveEventLoop) {
        if let Some((id, window)) = self
            .panes
            .iter()
            .find(|(_, pane)| pane.view.is_feedback_window())
            .map(|(id, pane)| (*id, pane.window.clone()))
        {
            window.set_visible(true);
            window.focus_window();
            activate_app();
            self.request_redraw(id);
            return;
        }
        for pane in self.panes.values_mut() {
            let _ = pane.view.close_feedback_tab();
        }
        let Some(view) = self.host.open_feedback(&self.runtime.workspace.ws) else {
            return;
        };
        let id = self.open_pane(event_loop, view, "Feedback");
        self.reveal_pane(id);
        if let Some(pane) = self.panes.get(&id) {
            pane.window.focus_window();
        }
        activate_app();
        self.request_redraw(id);
    }

    fn open_support(&mut self, event_loop: &ActiveEventLoop) {
        if let Some((id, window)) = self
            .panes
            .iter()
            .find(|(_, pane)| pane.view.is_support_window())
            .map(|(id, pane)| (*id, pane.window.clone()))
        {
            window.set_visible(true);
            window.focus_window();
            activate_app();
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.view.window_shown();
            }
            self.request_redraw(id);
            return;
        }
        let Some(view) = self.host.open_support(&self.runtime.workspace.ws) else {
            return;
        };
        let id = self.open_pane(event_loop, view, "Support");
        self.reveal_pane(id);
        if let Some(pane) = self.panes.get(&id) {
            pane.window.focus_window();
        }
        activate_app();
        self.request_redraw(id);
    }

    fn open_fab(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = self
            .panes
            .values()
            .find(|pane| pane.view.floating_window())
            .map(|pane| pane.window.clone())
        {
            window.set_visible(true);
            window.request_redraw();
            return;
        }
        let Some(view) = self.host.open_fab(&self.runtime.workspace.ws) else {
            return;
        };
        let id = self.open_pane(event_loop, view, "Support");
        self.reveal_pane(id);
        self.request_redraw(id);
    }

    fn notify_support_reply(&mut self) {
        let focused = self
            .panes
            .iter()
            .find(|(_, pane)| pane.window.has_focus())
            .map(|(id, _)| *id);
        if let Some(id) = focused
            && let Some(pane) = self.panes.get_mut(&id)
            && pane.view.show_support_reply()
        {
            self.request_redraw(id);
        }
    }

    fn drain_background_intents(&mut self, event_loop: &ActiveEventLoop) {
        let mut intents = Vec::new();
        for (id, pane) in self.panes.iter_mut() {
            while let Some(intent) = pane.view.take_background_intent() {
                intents.push((*id, intent));
                if intents.len() >= 64 {
                    break;
                }
            }
        }
        for (id, intent) in intents {
            match intent {
                crate::native_surface::BackgroundIntent::Close => self.close_pane(event_loop, id),
                crate::native_surface::BackgroundIntent::SetVisible(visible) => {
                    if let Some(pane) = self.panes.get(&id) {
                        pane.window.set_visible(visible);
                        if visible {
                            pane.window.request_redraw();
                        }
                    }
                }
                crate::native_surface::BackgroundIntent::OpenSupport => {
                    self.open_support(event_loop);
                }
                crate::native_surface::BackgroundIntent::ShowFab => self.open_fab(event_loop),
                crate::native_surface::BackgroundIntent::NotifyReply => {
                    self.notify_support_reply();
                }
            }
        }
    }

    fn background_maintain_all(&mut self, now: Instant, app_hidden: bool) -> Option<Instant> {
        let mut deadline: Option<Instant> = None;
        let mut changed = Vec::new();
        for (id, pane) in self.panes.iter_mut() {
            if pane.view.background_maintain(now, app_hidden) {
                changed.push(*id);
            }
            if let Some(due) = pane.view.background_deadline() {
                deadline = Some(deadline.map_or(due, |current| current.min(due)));
            }
        }
        for id in changed {
            self.request_redraw(id);
        }
        deadline
    }

    /// Keep every OS-visible title aligned with its authoritative metadata.
    /// Document and profile names can both arrive from another collaborating
    /// process, so this runs after both sessions have been polled.
    fn sync_titles(&mut self) {
        let profile = self.runtime.profile.name();
        let app_title = profiled_window_title(&self.name, profile.as_deref(), &self.instance.slot);
        let search_title = profiled_window_title(
            &format!("{} Search", self.name),
            profile.as_deref(),
            &self.instance.slot,
        );
        for pane in self.panes.values_mut() {
            let want = if pane.view.is_global_search() {
                search_title.clone()
            } else if let Some(root) = pane.view.document_root() {
                profiled_window_title(
                    &koru_workspace::name_of(&self.runtime.workspace.ws, root),
                    profile.as_deref(),
                    &self.instance.slot,
                )
            } else {
                app_title.clone()
            };
            if pane.title != want {
                pane.window.set_title(&want);
                pane.title = want;
                // The window's accessible name is its title, so a rename is
                // one of the few things this plan's semantic tree depends on.
                pane.access.invalidate();
            }
        }
    }

    /// Every window *except* `except` rebuilds from the document — the one that
    /// typed has already refreshed the row it edited, and rebuilding it would
    /// throw away the caret placement it just made.
    fn adopt_others(&mut self, except: WindowId) {
        for (id, pane) in self.panes.iter_mut() {
            if *id == except {
                continue;
            }
            pane.view.adopt(&self.runtime.workspace.ws);
            pane.window.request_redraw();
        }
    }

    /// A remote change touched the workspace, so every window has to rebuild
    /// from it. Rows are a cache; this is always the right answer.
    /// Push who and where we are into every pane's switcher.
    ///
    /// Cheap and idempotent: the view compares and reports whether anything
    /// changed, so a launch that has settled costs nothing and asks for no
    /// frame (ADR-26).
    /// The polling half of [`sync_pane_context`], on a clock rather than a
    /// frame.
    ///
    /// **Not on every wake.** The Store catalog is a socket round trip, and
    /// `about_to_wait` fires at least three
    /// times a second forever because the caret has to blink. What it is
    /// watching for is another *process* adding or renaming a profile or a
    /// workspace — exactly what `ProfileSession::tick` polls for beside it — so
    /// it gets [`CONTEXT_POLL`] rather than a frame.
    ///
    /// The two callers that act on a change *this* process just made call
    /// `sync_pane_context` directly, because for them the answer is already
    /// known to have changed and waiting up to two seconds to show it would be
    /// a visible lag.
    ///
    /// [`sync_pane_context`]: Self::sync_pane_context
    fn poll_pane_context(&mut self) -> bool {
        if self
            .last_context_poll
            .is_some_and(|t| t.elapsed() < CONTEXT_POLL)
        {
            return false;
        }
        self.last_context_poll = Some(Instant::now());
        let changed = self.sync_pane_context();
        self.runtime.request_catalog_refresh();
        changed
    }

    /// Adopt a Store profile catalog result without waiting on the event-loop
    /// thread. Only the newest queued result matters.
    fn drain_profile_catalog(&mut self) {
        if self.runtime.drain_catalog() {
            let current = self.runtime.current;
            let current_was_deleted = self.runtime.live_workspace
                && self.runtime.catalog_workspaces.iter().any(|workspace| {
                    workspace.id.as_uuid() == current.as_uuid() && workspace.deleted_at.is_some()
                });
            if current_was_deleted && let Err(error) = self.leave_current_workspace(current) {
                self.report(ErrorReport::new(
                    "Couldn't leave the deleted workspace",
                    error,
                ));
            }
            if let Some(workspace) = workspace_arrival::first_workspace_when_empty(
                self.runtime.live_workspace,
                &self.runtime.profile.workspaces(),
            ) {
                self.switch_workspace(workspace);
            }
            self.sync_pane_context();
        }
    }

    /// Record the one-time reminder startup answer through the one settings
    /// authority, without making the winit thread wait on it or the OS.
    fn answer_reminder_startup(&mut self, request: ReminderStartupRequest) {
        if self.reminder_startup_queue.enqueue(request) != ReminderStartupEnqueue::Start {
            return;
        }
        self.reminder_startup_reply = Some(self.settings_link.save_with_reply(
            reminder_startup_patch(request.accepted),
            |patch, section| patch.clone().apply(section),
        ));
    }

    fn complete_reminder_startup(
        &mut self,
        actual: koru_window::ReminderStartupFacts,
        error: Option<String>,
    ) {
        self.reminder_startup_facts = Some(actual);
        let completion = self.reminder_startup_queue.finish();
        for id in completion.waiters {
            let result = ReminderStartupResult {
                id,
                actual,
                error: error.clone(),
            };
            for pane in self.panes.values_mut() {
                if pane.view.reminder_startup_result(result.clone()) {
                    pane.window.request_redraw();
                }
            }
        }
        for id in completion.superseded {
            let result = ReminderStartupResult {
                id,
                actual,
                // This request did not fail: a newer answer made its own OS
                // write unnecessary. Return the authoritative facts from the
                // operation that won so every waiting pane converges without
                // presenting a persistence failure.
                error: None,
            };
            for pane in self.panes.values_mut() {
                if pane.view.reminder_startup_result(result.clone()) {
                    pane.window.request_redraw();
                }
            }
        }
        if let Some(next) = completion.next {
            self.answer_reminder_startup(next);
        }
    }

    fn complete_reminder_startup_reply(
        &mut self,
        reply: koru_api_client::PatchReply<koru_settings::GlobalSettings>,
    ) {
        match reply {
            koru_api_client::PatchReply::Applied(section) => {
                self.complete_reminder_startup(reminder_startup_facts(&section), None);
            }
            koru_api_client::PatchReply::Refused {
                authoritative,
                message,
            } => {
                let actual = authoritative
                    .as_ref()
                    .map(reminder_startup_facts)
                    .or(self.reminder_startup_facts)
                    .unwrap_or_default();
                self.complete_reminder_startup(actual, Some(message));
            }
        }
    }

    fn drain_reminder_startup(&mut self) {
        let Some(reply) = self.reminder_startup_reply.as_ref() else {
            return;
        };
        let completed = match reply.try_recv() {
            Ok(completed) => completed,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                let actual = self.reminder_startup_facts.unwrap_or_default();
                self.reminder_startup_reply = None;
                self.complete_reminder_startup(
                    actual,
                    Some("the settings worker stopped before saving reminder startup".into()),
                );
                return;
            }
        };
        self.reminder_startup_reply = None;
        self.complete_reminder_startup_reply(completed);
    }

    /// Lifecycle persistence is allowed to wait for the exact settings patch
    /// receipt. Ordinary event handling only tries the channel and never
    /// blocks.
    fn settle_reminder_startup_before_persist(&mut self) {
        while let Some(reply) = self.reminder_startup_reply.take() {
            match reply.recv() {
                Ok(completed) => self.complete_reminder_startup_reply(completed),
                Err(_) => {
                    let actual = self.reminder_startup_facts.unwrap_or_default();
                    self.complete_reminder_startup(
                        actual,
                        Some("the settings worker stopped before saving reminder startup".into()),
                    );
                }
            }
        }
    }

    fn sync_pane_context(&mut self) -> bool {
        // **Every profile on the machine, not just this one.** Handing the pane
        // only the open profile is what made its switcher unable to switch:
        // there was one row, and it named the profile you were already in.
        //
        // The worker refreshes this cache, so a profile added
        // by *another* window appears here without the two coordinating — the
        // same reason the workspace list is re-read every tick.
        let mut profiles = self.runtime.profile_catalog.clone();
        let open = pane_profile_summary(
            self.runtime.profile.summary(),
            profiles
                .iter()
                .find(|profile| self.runtime.profile.id() == Some(profile.id)),
        );
        // The open profile's own record is the live one — it may have been
        // renamed this session, and it is the only one with an avatar to draw.
        let seated = self.runtime.profile.id().is_some();
        match profiles.iter_mut().find(|p| p.id == open.id) {
            Some(row) => *row = open.clone(),
            // A refresh can race profile creation. Keep the current row usable
            // until the next successful Store catalog owns the list.
            None if seated => profiles.insert(0, open.clone()),
            // **First run has nobody to insert.** `summary()` fills an absent id
            // with the nil UUID and an absent name with the empty string, so
            // inserting it here would put a nameless persona in the switcher —
            // the invented identity opening unseated exists to avoid, arriving
            // one layer later. An empty list is the truth (S21).
            None => {}
        }
        let retained_profiles = profiles.clone();
        let active_account = self
            .host
            .account_state()
            .active_account_id
            .and_then(|id| koru_store_core::ProfileAccountBinding::parse_identity_id(&id).ok());
        profiles.retain(|profile| {
            koru_store_core::profile_account::LocalProfileAttribution::from_binding(
                profile.account.as_deref().unwrap_or(""),
            )
            .visible_for(active_account)
        });
        let hidden_profiles = retained_profiles.len() - profiles.len();
        let workspaces = self.runtime.profile.workspaces();
        // A local Welcome document has a placeholder UUID so it can use the
        // ordinary view machinery, but it is not a Store workspace. Preserve
        // that distinction across the pane boundary: document creation is not
        // a valid action until a real workspace exists.
        let current = self.runtime.live_workspace.then_some(self.runtime.current);
        let reminder_startup = self.reminder_startup_facts;
        let account_state = self.host.account_state();
        let mut changed = false;
        for pane in self.panes.values_mut() {
            if pane.view.set_account_state(account_state.clone()) {
                changed = true;
                pane.window.request_redraw();
            }
            if reminder_startup.is_some_and(|facts| pane.view.set_reminder_startup(facts)) {
                changed = true;
                pane.window.request_redraw();
            }
            if pane
                .view
                .set_pane_context(profiles.clone(), open.id, workspaces.clone(), current)
            {
                changed = true;
                pane.window.request_redraw();
            }
            if pane.view.set_retained_profiles(retained_profiles.clone())
                | pane.view.set_hidden_profile_count(hidden_profiles)
            {
                changed = true;
                pane.window.request_redraw();
            }
            if pane
                .view
                .set_profile_workspace_counts(self.runtime.active_workspace_counts())
            {
                changed = true;
                pane.window.request_redraw();
            }
            if pane
                .view
                .set_profile_workspaces(self.runtime.workspaces_by_profile())
            {
                changed = true;
                pane.window.request_redraw();
            }
        }
        // Prime a new or changed pane context immediately. Document changes do
        // not rely on this two-second poll: local manifest commands push their
        // entries directly, and `workspace.tick()` does so after remote changes.
        // An unchanged context therefore has no document work to do here.
        if changed {
            if let Some(current) = current {
                let workspace_name = workspaces
                    .iter()
                    .find(|workspace| workspace.id == current)
                    .map(|workspace| workspace.name.as_str())
                    .unwrap_or("");
                self.host.agent_showing(
                    open.id,
                    current,
                    &open.name,
                    workspace_name,
                    &self.pane_pins(),
                );
            }
            self.push_entries();
        }
        changed
    }

    /// Push what the workspace holds into every pane.
    ///
    /// Split out of [`sync_pane_context`](Self::sync_pane_context) because that
    /// one republishes the cached Store catalog and is rate-limited to
    /// [`CONTEXT_POLL`] for it. This reads only the manifest, already in memory,
    /// so it is cheap enough to call on every drained change — which is what it
    /// takes for a document created in another window to appear here without
    /// anyone clicking anything.
    fn push_entries(&mut self) -> bool {
        let (entries, trashed) = self.runtime.workspace.entries_and_trashed();
        // Which of them is open, from the workspace rather than left to the pane
        // to work out: a row is keyed by `DocumentId` and a tab's root is a
        // `NodeId` inside a document, and only this side holds both.
        let active = self.runtime.workspace.active();
        let mut changed = false;
        for pane in self.panes.values_mut() {
            if pane.view.set_entries(entries.clone(), active) {
                changed = true;
                pane.window.request_redraw();
            }
            if pane.view.set_trashed(trashed.clone()) {
                changed = true;
                pane.window.request_redraw();
            }
        }
        if changed {
            // The listing moved, so which of its files are held here may have
            // moved with it. Asking here rather than per frame is the whole
            // reason locality is pushed: the answer costs a Store call per file.
            self.refresh_file_locality();
        }
        changed
    }

    fn pane_pins(&self) -> Vec<(String, koru_core::DocumentId, koru_core::WorkspaceId, bool)> {
        let mut out = Vec::new();
        for pane in self.panes.values() {
            for row in pane.view.pinned_search_rows() {
                if !out.iter().any(|(_, id, _, _)| *id == row.1) {
                    out.push(row);
                }
            }
        }
        out
    }

    fn open_search_target_in_panes(
        &mut self,
        source: WindowId,
        document: Option<NodeId>,
        node: Option<NodeId>,
        trash_root: Option<NodeId>,
        match_text: Option<&str>,
        source_only: bool,
    ) -> bool {
        let started = Instant::now();
        tracing::info!(
            target: "koru::navigation",
            profile = %trace_profile(self.runtime.profile.id()),
            workspace = %self.runtime.current.to_storage(),
            document = %trace_node(document),
            node = %trace_node(node),
            in_tray = self.in_tray,
            panes = self.panes.len(),
            "Koru began placing a routed target in a pane"
        );
        // Opaque files live in the manifest but have no editable document to
        // open. Resolve the target after any workspace switch, while this
        // shell owns the authoritative listing, and let the container reveal
        // it in the Files navigator.
        let file = document.is_some_and(|document| {
            self.runtime.workspace.entries().iter().any(|entry| {
                entry.id.as_uuid() == document.as_uuid()
                    && entry.kind == koru_core::explore::KIND_FILE
            })
        });
        crate::search_debug!(
            "[Koru][search-open][pid={}] pane navigation begin: profile={} workspace={} source={source:?} node={} trash_root={} pane_count={}",
            std::process::id(),
            trace_profile(self.runtime.profile.id()),
            self.runtime.current.to_storage(),
            trace_node(node),
            trace_node(trash_root),
            self.panes.len(),
        );
        // An in-app modal opens into the window it covered. A dedicated search
        // declines the request, after which the primary container is preferred.
        // HashMap iteration is not a navigation policy, so build the order
        // explicitly — in `landing_order`, which is the whole of the policy and
        // is the only part of this function a test can reach.
        let document_id = document.map(|id| koru_core::DocumentId::from_uuid(id.as_uuid()));
        let known: Vec<_> = self
            .panes
            .iter()
            .map(|(id, pane)| (*id, document_id.map(|doc| pane.view.tab_presence(doc))))
            .collect();
        let targets = landing_order(source, self.primary, &known, source_only);
        for id in targets {
            let Some(pane) = self.panes.get_mut(&id) else {
                crate::search_debug!(
                    "[Koru][search-open][pid={}] pane navigation skipped missing pane: pane={id:?}",
                    std::process::id(),
                );
                continue;
            };
            let accepted = if file {
                document.is_some_and(|file| pane.view.open_file_search_target(file))
            } else {
                pane.view.open_search_target(
                    &self.runtime.workspace.ws,
                    document,
                    node,
                    trash_root,
                    match_text,
                )
            };
            if accepted {
                let window = pane.window.clone();
                pane.quarantine
                    .suppress_all(self.down_keys.iter().copied(), Instant::now());
                // Merely opening global search must leave the session in the
                // tray. Choosing a destination is the explicit point at which
                // the ordinary Koru session should return.
                let was_in_tray = self.in_tray;
                let visible_before = window.is_visible();
                let focused_before = window.has_focus();
                if was_in_tray {
                    // Show every pane, but do not focus the primary first. A
                    // result may belong to another pane, and Windows can spend
                    // the foreground permission on only that intermediate
                    // focus request and then reject the intended one below.
                    self.show_from_tray();
                }
                window.set_visible(true);
                window.focus_window();
                window.request_redraw();
                tracing::info!(
                    target: "koru::navigation",
                    profile = %trace_profile(self.runtime.profile.id()),
                    workspace = %self.runtime.current.to_storage(),
                    document = %trace_node(document),
                    node = %trace_node(node),
                    elapsed_ms = started.elapsed().as_millis(),
                    restored_from_tray = was_in_tray,
                    visible_after = ?window.is_visible(),
                    focused_after = window.has_focus(),
                    "Koru pane accepted and presented the routed target"
                );
                crate::search_debug!(
                    "[Koru][search-open][pid={}] pane navigation complete: profile={} workspace={} pane={id:?} node={} trash_root={} restored_from_tray={} visible_before={visible_before:?} visible_after={:?} focused_before={focused_before} focused_after={}",
                    std::process::id(),
                    trace_profile(self.runtime.profile.id()),
                    self.runtime.current.to_storage(),
                    trace_node(node),
                    trace_node(trash_root),
                    was_in_tray,
                    window.is_visible(),
                    window.has_focus(),
                );
                return true;
            }
            crate::search_debug!(
                "[Koru][search-open][pid={}] pane declined target: profile={} workspace={} pane={id:?} node={} trash_root={}",
                std::process::id(),
                trace_profile(self.runtime.profile.id()),
                self.runtime.current.to_storage(),
                trace_node(node),
                trace_node(trash_root),
            );
        }
        crate::search_debug!(
            "[Koru][search-open][pid={}] pane navigation failed: profile={} workspace={} node={} trash_root={}",
            std::process::id(),
            trace_profile(self.runtime.profile.id()),
            self.runtime.current.to_storage(),
            trace_node(node),
            trace_node(trash_root),
        );
        tracing::warn!(
            target: "koru::navigation",
            profile = %trace_profile(self.runtime.profile.id()),
            workspace = %self.runtime.current.to_storage(),
            document = %trace_node(document),
            node = %trace_node(node),
            elapsed_ms = started.elapsed().as_millis(),
            "no Koru pane accepted the routed target"
        );
        false
    }

    /// Open a search destination without replacing or focusing the source
    /// window. The current workspace can share this process and its live Loro
    /// document; another workspace/profile needs its own process because an
    /// `App` deliberately owns exactly one active workspace document.
    fn open_search_target_in_new_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        profile: koru_core::ProfileId,
        workspace: WorkspaceId,
        document: Option<NodeId>,
        node: Option<NodeId>,
        trash_root: Option<NodeId>,
        match_text: Option<&str>,
    ) {
        if let Some(document) = document {
            // `duplicate` opens a welcome tab in this process; a Store document
            // is not in the live `WorkspaceDoc`, so that window would stay on
            // welcome. A new process loads the document as its first page.
            startup::launch_target_new_window(
                &self.runtime.profile_dir,
                profile,
                workspace,
                Some(koru_core::DocumentId::from_uuid(document.as_uuid())),
                node,
                trash_root,
                match_text,
            );
            return;
        }
        if self.runtime.profile.id() == Some(profile) && self.runtime.current == workspace {
            let mut view = self.host.duplicate(&self.runtime.workspace.ws);
            if !view.open_search_target(
                &self.runtime.workspace.ws,
                document,
                node,
                trash_root,
                match_text,
            ) {
                self.report(ErrorReport::new(
                    "Couldn't open search result",
                    "That result is no longer present. Search was left open.",
                ));
                return;
            }
            let title = self.app_window_title();
            let id = self.open_pane(event_loop, view, &title);
            self.reveal_pane(id);
            if let Some(pane) = self.panes.get(&id) {
                pane.window.focus_window();
            }
            self.request_redraw(id);
            return;
        }

        startup::launch_target_new_window(
            &self.runtime.profile_dir,
            profile,
            workspace,
            document.map(|id| koru_core::DocumentId::from_uuid(id.as_uuid())),
            node,
            trash_root,
            match_text,
        );
    }

    fn open_background_document(
        &mut self,
        source: WindowId,
        document: koru_core::DocumentId,
        _node: Option<NodeId>,
    ) {
        let home = match self.runtime.workspace.ensure_document(document) {
            Ok(home) => home,
            Err(error) => {
                self.report(
                    ErrorReport::new(
                        "Couldn't open that document",
                        "The original link was kept, but its destination could not be found.",
                    )
                    .with_details(error),
                );
                return;
            }
        };
        let Some(doc) = self.runtime.workspace.document(document) else {
            return;
        };
        let opened = self
            .panes
            .get_mut(&source)
            .is_some_and(|pane| pane.view.open_background_tab(doc, document, Some(home)));
        if opened {
            self.invalidate_everywhere();
        }
    }

    /// Open every document as tabs in **one** new window (same process).
    fn open_documents_in_new_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        _source: WindowId,
        documents: Vec<koru_core::DocumentId>,
    ) {
        if documents.is_empty() {
            return;
        }
        let mut homes = Vec::new();
        for &document in &documents {
            match self.runtime.workspace.ensure_document(document) {
                Ok(home) => homes.push((document, home)),
                Err(error) => {
                    self.report(
                        ErrorReport::new(
                            "Couldn't open that document",
                            "The selection was kept, but one page could not be loaded.",
                        )
                        .with_details(error),
                    );
                    return;
                }
            }
        }
        let mut view = self.host.duplicate(&self.runtime.workspace.ws);
        for &(document, home) in &homes {
            let Some(doc) = self.runtime.workspace.document(document) else {
                continue;
            };
            let _ = view.open_background_tab(doc, document, Some(home));
        }
        // Land the window on the first selected page.
        if let Some(&(first, home)) = homes.first() {
            let _ = self.runtime.workspace.open_document_in(home, first);
        }
        let title = self.app_window_title();
        let id = self.open_pane(event_loop, view, &title);
        if let Some(pane) = self.panes.get_mut(&id) {
            let active = self.runtime.workspace.active();
            let _ = pane
                .view
                .show_active_document(&self.runtime.workspace.ws, active);
        }
        self.reveal_pane(id);
        if let Some(pane) = self.panes.get(&id) {
            pane.window.focus_window();
        }
        self.request_redraw(id);
    }

    /// Open a document under its home workspace **without** switching Explore.
    ///
    /// Tab selection uses this so a foreign tab shows its page while the active
    /// workspace stays put. Search, history and deep links still use Navigate.
    fn activate_document_in_place(
        &mut self,
        document: koru_core::DocumentId,
        workspace: Option<WorkspaceId>,
    ) {
        let home = workspace.unwrap_or(self.runtime.current);
        if self.runtime.workspace.active() == Some(document) {
            self.show_active_document();
            return;
        }
        let opened = {
            let _watch = timing::Watch::start("activate.open_document");
            self.runtime.workspace.open_document_in(home, document)
        };
        if let Err(error) = opened {
            self.report(
                ErrorReport::new(
                    "Couldn't open that document",
                    "The tab is still there, but its page could not be loaded.",
                )
                .with_details(error),
            );
            return;
        }
        self.opened_document(document);
        self.show_active_document();
        self.invalidate_everywhere();
    }

    /// Resolve stable identity, then perform the one routing policy used by
    /// pills, search, history, files and future external document URLs.
    fn navigate(
        &mut self,
        event_loop: &ActiveEventLoop,
        source: WindowId,
        target: crate::NavigationTarget,
        match_text: Option<String>,
        disposition: SearchTargetDisposition,
    ) {
        self.navigate_with_retry(event_loop, source, target, match_text, disposition, true);
    }

    fn navigate_with_retry(
        &mut self,
        event_loop: &ActiveEventLoop,
        source: WindowId,
        target: crate::NavigationTarget,
        match_text: Option<String>,
        disposition: SearchTargetDisposition,
        retry_moved_document: bool,
    ) {
        let started = Instant::now();
        match &target {
            crate::NavigationTarget::Workspace { profile, workspace } => tracing::info!(
                target: "koru::navigation",
                profile = %profile.to_storage(),
                workspace = %workspace.to_storage(),
                "Koru began resolving Agent pane navigation"
            ),
            crate::NavigationTarget::Document { document, node } => tracing::info!(
                target: "koru::navigation",
                document = %document.to_storage(),
                node = %trace_node(*node),
                "Koru began resolving Agent pane navigation"
            ),
        }
        let resolved = match target {
            crate::NavigationTarget::Workspace { profile, workspace } => {
                Ok((profile, workspace, None, None))
            }
            crate::NavigationTarget::Document { document, node } => self
                .runtime
                .workspace
                .store_client()
                .ok_or_else(|| {
                    koru_store_core::errors::Error::new(
                        koru_store_core::errors::Code::InvalidRequest,
                        "this workspace is not open on a store",
                    )
                })
                .and_then(|client| {
                    let _watch = timing::Watch::start("navigate.locate");
                    client.locate_document(koru_store_core::ids::DocumentId::from_uuid(
                        document.as_uuid(),
                    ))
                })
                .map(|location| {
                    (
                        koru_core::ProfileId::from_uuid(location.profile.as_uuid()),
                        WorkspaceId::from_uuid(location.workspace.as_uuid()),
                        Some(document),
                        node,
                    )
                }),
        };
        let (profile, workspace, document, node) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => {
                tracing::warn!(
                    target: "koru::navigation",
                    elapsed_ms = started.elapsed().as_millis(),
                    error = %error,
                    "Koru could not resolve the Agent navigation target in Store"
                );
                self.report_and_foreground(
                    source,
                    ErrorReport::new(
                        "Couldn't open that document",
                        "The original link was kept, but its destination could not be found.",
                    )
                    .with_details(error),
                );
                return;
            }
        };
        tracing::info!(
            target: "koru::navigation",
            profile = %profile.to_storage(),
            workspace = %workspace.to_storage(),
            document = document.map(|id| id.to_storage()).as_deref().unwrap_or("none"),
            node = %trace_node(node),
            resolve_elapsed_ms = started.elapsed().as_millis(),
            "Koru resolved the pane navigation location"
        );
        let document_node = document.map(|id| NodeId::from_uuid(id.as_uuid()));
        let current_profile = self.runtime.profile.id();
        crate::search_debug!(
            "[Koru][navigate][pid={}] resolved: profile={} workspace={} document={} node={} disposition={disposition:?}",
            std::process::id(),
            profile.to_storage(),
            workspace.to_storage(),
            document.map(|id| id.to_storage()).as_deref().unwrap_or("-"),
            trace_node(node),
        );
        if disposition == SearchTargetDisposition::NewWindow {
            tracing::info!(
                target: "koru::navigation",
                profile = %profile.to_storage(),
                workspace = %workspace.to_storage(),
                document = document.map(|id| id.to_storage()).as_deref().unwrap_or("none"),
                node = %trace_node(node),
                elapsed_ms = started.elapsed().as_millis(),
                "Koru handed the Agent navigation target to a new window"
            );
            self.open_search_target_in_new_window(
                event_loop,
                profile,
                workspace,
                document_node,
                node,
                None,
                match_text.as_deref(),
            );
            return;
        }
        if current_profile == Some(profile) {
            if self.runtime.current != workspace {
                self.switch_workspace(workspace);
            }
            if self.runtime.current == workspace {
                let (live_entries, trashed_entries) = self.runtime.workspace.entries_and_trashed();
                let document_is_file = document.is_some_and(|document| {
                    live_entries.iter().chain(&trashed_entries).any(|entry| {
                        entry.id == document && entry.kind == koru_core::explore::KIND_FILE
                    })
                });
                let document_is_trashed = document.is_some_and(|document| {
                    trashed_entries.iter().any(|entry| entry.id == document)
                });
                if let Some(document) = document
                    && !document_is_file
                    && !document_is_trashed
                    && self.runtime.workspace.active() != Some(document)
                {
                    let opened = {
                        let _watch = timing::Watch::start("navigate.open_document");
                        self.runtime.workspace.open_document(document)
                    };
                    if let Err(error) = opened {
                        if retry_moved_document
                            && let Some(client) = self.runtime.workspace.store_client()
                            && let Ok(location) = client.locate_document(
                                koru_store_core::ids::DocumentId::from_uuid(document.as_uuid()),
                            )
                        {
                            let retry_profile =
                                koru_core::ProfileId::from_uuid(location.profile.as_uuid());
                            let retry_workspace =
                                WorkspaceId::from_uuid(location.workspace.as_uuid());
                            if (retry_profile, retry_workspace) != (profile, workspace) {
                                tracing::info!(
                                    target: "koru::navigation",
                                    document = %document.to_storage(),
                                    from_profile = %profile.to_storage(),
                                    from_workspace = %workspace.to_storage(),
                                    to_profile = %retry_profile.to_storage(),
                                    to_workspace = %retry_workspace.to_storage(),
                                    elapsed_ms = started.elapsed().as_millis(),
                                    "Koru found that the routed document moved and is retrying"
                                );
                                self.navigate_with_retry(
                                    event_loop,
                                    source,
                                    crate::NavigationTarget::Document { document, node },
                                    match_text,
                                    disposition,
                                    false,
                                );
                                return;
                            }
                        }
                        tracing::warn!(
                            target: "koru::navigation",
                            profile = %profile.to_storage(),
                            workspace = %workspace.to_storage(),
                            document = %document.to_storage(),
                            elapsed_ms = started.elapsed().as_millis(),
                            error = %error,
                            "Koru located the routed document but could not load it from Store"
                        );
                        self.report_and_foreground(
                            source,
                            ErrorReport::new(
                                "Couldn't open that document",
                                "Its current location was found, but the document was not available there. The original link was kept.",
                            )
                            .with_details(error),
                        );
                        return;
                    }
                    tracing::info!(
                        target: "koru::navigation",
                        document = %document.to_storage(),
                        elapsed_ms = started.elapsed().as_millis(),
                        "Koru loaded the routed document from Store"
                    );
                    self.opened_document(document);
                    {
                        let _watch = timing::Watch::start("navigate.show_active");
                        self.show_active_document();
                    }
                    {
                        let _watch = timing::Watch::start("navigate.sync_context");
                        self.sync_pane_context();
                    }
                    {
                        let _watch = timing::Watch::start("navigate.invalidate");
                        self.invalidate_everywhere();
                    }
                }
                let opened = {
                    let _watch = timing::Watch::start("navigate.show");
                    self.open_search_target_in_panes(
                        source,
                        document_node,
                        node,
                        None,
                        match_text.as_deref(),
                        disposition == SearchTargetDisposition::SourceWindow,
                    )
                };
                crate::search_debug!(
                    "[Koru][navigate][pid={}] local route complete: document={} opened={opened}",
                    std::process::id(),
                    document.map(|id| id.to_storage()).as_deref().unwrap_or("-"),
                );
                if !opened {
                    if retry_moved_document
                        && let Some(document) = document
                        && let Some(client) = self.runtime.workspace.store_client()
                        && let Ok(location) = client.locate_document(
                            koru_store_core::ids::DocumentId::from_uuid(document.as_uuid()),
                        )
                    {
                        let retry_profile =
                            koru_core::ProfileId::from_uuid(location.profile.as_uuid());
                        let retry_workspace = WorkspaceId::from_uuid(location.workspace.as_uuid());
                        if (retry_profile, retry_workspace) != (profile, workspace) {
                            self.navigate_with_retry(
                                event_loop,
                                source,
                                crate::NavigationTarget::Document { document, node },
                                match_text,
                                disposition,
                                false,
                            );
                            return;
                        }
                    }
                    self.report_and_foreground(
                        source,
                        ErrorReport::new(
                            "Couldn't open that document",
                            "Its current location was found, but the document was not available there. The original link was kept.",
                        ),
                    );
                }
            } else {
                self.report_and_foreground(
                    source,
                    ErrorReport::new(
                        "Couldn't open that document",
                        "Koru could not switch to the document's current workspace. The original link was kept.",
                    ),
                );
            }
        } else if crate::profile_activation::signal(
            profile,
            crate::profile_activation::Target {
                workspace,
                document,
                node,
                match_text: match_text.clone(),
            },
        ) {
            // The receiving process re-enters this same resolver, so a move
            // racing the handoff cannot revive the stale workspace.
        } else {
            startup::launch_target_new_window(
                &self.runtime.profile_dir,
                profile,
                workspace,
                document,
                node,
                None,
                match_text.as_deref(),
            );
        }
    }

    fn queue_document_transfer(
        &mut self,
        origin: WindowId,
        id: koru_core::DocumentId,
        source: WorkspaceId,
        target: WorkspaceId,
        parent: Option<koru_core::DocumentId>,
        index: usize,
        follow: bool,
    ) {
        let key = (id, source, target);
        if !self.transfers_in_flight.insert(key) {
            println!(
                "[Koru][transfer] duplicate suppressed document={} source={} target={}",
                id.to_storage(),
                source.to_storage(),
                target.to_storage()
            );
            return;
        }
        let client = match self.runtime.workspace.prepare_document_transfer() {
            Ok(client) => client,
            Err(error) => {
                self.transfers_in_flight.remove(&key);
                for pane in self.panes.values_mut() {
                    let _ = pane.view.transfer_failed();
                }
                self.report_and_foreground(
                    origin,
                    ErrorReport::new("Couldn't move that document", "Nothing was changed.")
                        .with_details(error),
                );
                return;
            }
        };
        if let Err(error) = self.transfer_tx.send(TransferJob {
            origin,
            client,
            id,
            source,
            target,
            parent,
            index,
            follow,
        }) {
            self.transfers_in_flight.remove(&key);
            for pane in self.panes.values_mut() {
                let _ = pane.view.transfer_failed();
            }
            self.report_and_foreground(
                origin,
                ErrorReport::new("Couldn't move that document", "Nothing was changed.")
                    .with_details(error),
            );
        }
    }

    fn drain_transfer_results(&mut self) {
        while let Ok(completed) = self.transfer_rx.try_recv() {
            self.transfers_in_flight
                .remove(&(completed.id, completed.source, completed.target));
            match completed.result {
                Ok(ids) => {
                    self.runtime
                        .workspace
                        .finish_document_transfer(completed.source, &ids);
                    self.push_entries();
                    if completed.follow {
                        self.switch_workspace(completed.target);
                        if let Err(error) = self.runtime.workspace.open_document(completed.id) {
                            self.report_and_foreground(
                                completed.origin,
                                ErrorReport::new(
                                    "Moved, but couldn't open it there",
                                    "The document is in the other workspace.",
                                )
                                .with_details(error),
                            );
                        } else {
                            self.push_entries();
                        }
                    }
                    self.invalidate_everywhere();
                }
                Err(error) => {
                    for pane in self.panes.values_mut() {
                        let _ = pane.view.transfer_failed();
                    }
                    self.report_and_foreground(
                        completed.origin,
                        ErrorReport::new("Couldn't move that document", "Nothing was changed.")
                            .with_details(error),
                    );
                }
            }
        }
    }

    /// Consume complete search targets sent to this profile's machine-local
    /// activation pipe. The receiving process owns the profile document, so it
    /// can safely switch its one active workspace before opening the node.
    fn drain_profile_activations(&mut self, event_loop: &ActiveEventLoop) {
        loop {
            let Some(target) = self.runtime.profile_activation.take() else {
                break;
            };
            crate::search_debug!(
                "[Koru][navigate][pid={}] activation dequeued: profile={} current_workspace={} hinted_workspace={} document={} node={}",
                std::process::id(),
                trace_profile(self.runtime.profile.id()),
                self.runtime.current.to_storage(),
                target.workspace.to_storage(),
                target
                    .document
                    .map(|id| id.to_storage())
                    .as_deref()
                    .unwrap_or("-"),
                trace_node(target.node),
            );
            let source = self.primary.or_else(|| self.panes.keys().next().copied());
            if let Some(source) = source {
                let navigation = match target.document {
                    Some(document) => crate::NavigationTarget::Document {
                        document,
                        node: target.node,
                    },
                    None => {
                        let Some(profile) = self.runtime.profile.id() else {
                            continue;
                        };
                        crate::NavigationTarget::Workspace {
                            profile,
                            workspace: target.workspace,
                        }
                    }
                };
                self.navigate(
                    event_loop,
                    source,
                    navigation,
                    target.match_text,
                    SearchTargetDisposition::CurrentWindow,
                );
            } else {
                crate::search_debug!(
                    "[Koru][search-open][pid={}] activation failed: profile={} workspace={} no_pane=true",
                    std::process::id(),
                    trace_profile(self.runtime.profile.id()),
                    self.runtime.current.to_storage(),
                );
            }
        }
    }

    /// Perform whatever the pane's switcher asked for.
    ///
    /// Two mechanisms, and which one applies is not a style choice. A
    /// *workspace* is another document in the same folder, so it is swapped in
    /// place. A *profile* is another folder, and therefore another data
    /// directory, another profile document and another peer — so it is another
    /// process. Trying to hold two profiles in one `App` would turn every
    /// `&mut self.runtime.workspace.ws` in this file into "which one".
    /// Serve every request the panes have queued, not one per turn.
    ///
    /// **A gesture can ask for several things.** Six documents dragged to the
    /// Trash are six asks, and taking one per turn would serve them a frame
    /// apart at best — and, before the views queued them at all, dropped five of
    /// them on the floor. Each is served in order, and re-entering with the
    /// remainder is what keeps `open_search` and friends taking `event_loop`
    /// without this having to be recursive.
    fn drain_pane_requests(&mut self, event_loop: &ActiveEventLoop) {
        // Bounded so a request that somehow re-queues itself cannot hang the
        // window: what is left is still there on the next turn.
        for _ in 0..64 {
            if !self.serve_one_pane_request(event_loop) {
                return;
            }
        }
    }

    /// Service only requests delivered by the Agent.
    ///
    /// A hidden process returns from maintenance before ordinary pane work.
    /// Merely copying an Agent target into `from_the_agent` above that return
    /// therefore leaves it hidden forever: every later wake copies nothing and
    /// reaches the same return. Drain this queue while it is known to be the
    /// next source; once it is empty, leave ordinary pane requests for their
    /// usual maintenance phase.
    fn drain_agent_requests(&mut self, event_loop: &ActiveEventLoop) {
        for _ in 0..64 {
            if self.from_the_agent.is_empty() || !self.serve_one_pane_request(event_loop) {
                return;
            }
        }
    }

    /// One request, if any pane has one. `false` when none did.
    fn serve_one_pane_request(&mut self, event_loop: &ActiveEventLoop) -> bool {
        // **The agent's first**, and attributed to the primary pane. A request
        // that came from outside this process has no pane behind it, and
        // `source` is only ever used to decide which window a result lands in —
        // which for an agent-routed target is the one it chose this process
        // for. There is always a primary: `TabSet`'s rule is that the list is
        // never empty.
        //
        // **Popped only once there is a pane to attribute it to**, or a target
        // that arrived a moment before the first window would be taken off the
        // queue and dropped.
        let mut request = self
            .primary
            .or_else(|| self.panes.keys().next().copied())
            .and_then(|source| self.from_the_agent.pop().map(|r| (source, r)));
        if request.is_none() {
            for (id, pane) in self.panes.iter_mut() {
                if let Some(r) = pane.view.take_pane_request() {
                    request = Some((*id, r));
                    break;
                }
            }
        }
        let Some((source, request)) = request else {
            return false;
        };
        let _stall = timing::Watch::start(request.stall_name());
        match request {
            PaneRequest::Account(command) => self.host.account_command(command),
            PaneRequest::AccountOwned {
                owner_scope,
                command,
            } => self.submit_owned_account_command(owner_scope, command),
            PaneRequest::AccountTracked {
                submission,
                owner_scope,
                command,
            } => self
                .host
                .account_command_tracked_owned(submission, owner_scope, command),
            PaneRequest::CreateConnectedProfile { owner_scope, draft } => {
                if self.pending_connected_profile_creation.is_some() {
                    return true;
                }
                self.pending_connected_profile_creation = Some(PendingConnectedProfileCreation {
                    owner_scope,
                    submission: None,
                    generation: None,
                    source,
                    owner_display_name: draft.owner_display_name.clone(),
                    profile_name: draft.profile_name.clone(),
                    workspace_name: draft.workspace_name.clone(),
                    connect_account: draft.connect_account,
                    stage: ConnectedProfileCreationStage::CreatingLocally,
                });
                let tx = self.connected_profile_creation_tx.clone();
                let proxy = self.wake_proxy.clone();
                let root = self.gateway_root.clone();
                let started = std::thread::Builder::new()
                    .name("koru-connected-profile-create".into())
                    .spawn(move || {
                        let result = create_onboarding_profile(
                            root.as_deref(),
                            &draft.profile_name,
                            &draft.workspace_name,
                            &draft.template,
                            draft.profile_icon.as_deref(),
                            draft.profile_color.as_deref(),
                            draft.workspace_icon.as_deref(),
                            draft.workspace_color.as_deref(),
                        );
                        let _ = tx.send(result);
                        let _ = proxy.send_event(Wake::Onboarding);
                    });
                if let Err(error) = started {
                    let _ = clear_failed_connected_profile_start(
                        &mut self.pending_connected_profile_creation,
                    );
                    if let Some(pane) = self.panes.get_mut(&source) {
                        let _ = pane.view.connected_profile_creation_failed(&format!(
                            "Koru couldn't start Profile creation. Nothing was created. Try again. ({error})"
                        ));
                        pane.window.request_redraw();
                    }
                }
            }
            PaneRequest::RestoreLocalProfile { profile } => {
                let tx = self.embedded_profile_restore_tx.clone();
                let proxy = self.wake_proxy.clone();
                let root = self.gateway_root.clone();
                let started = std::thread::Builder::new()
                    .name("koru-embedded-profile-restore".into())
                    .spawn(move || {
                        let result =
                            restore_onboarding_profile(root.as_deref(), &profile.to_storage());
                        let _ = tx.send(EmbeddedProfileRestore { source, result });
                        let _ = proxy.send_event(Wake::Onboarding);
                    });
                if let Err(error) = started
                    && let Some(pane) = self.panes.get_mut(&source)
                {
                    let _ = pane.view.local_profile_restore_failed(&format!(
                        "Koru couldn't start Profile restoration. Nothing was changed. Try again. ({error})"
                    ));
                    pane.window.request_redraw();
                }
            }
            PaneRequest::RemoveLocalProfile {
                profile,
                delete_workspaces,
            } => {
                if delete_workspaces {
                    self.start_profile_cascade(profile);
                } else {
                    self.host.remove_local_profile(profile);
                }
            }
            PaneRequest::FinishHostedProfileDeletion {
                profile,
                discovered_after_offline,
            } => {
                self.hosted_profile_removals.insert(
                    profile,
                    if discovered_after_offline {
                        HostedProfileRemovalSource::Reconciliation
                    } else {
                        HostedProfileRemovalSource::Origin
                    },
                );
                self.start_profile_cascade(profile);
            }
            PaneRequest::ContinueDeletedProfileOffline {
                profile,
                expected_identity_id,
            } => self.start_continue_profile_offline(profile, expected_identity_id),
            PaneRequest::BindLocalProfileAccount {
                profile,
                expected_identity_id,
                new_identity_id,
            } => {
                let marker = crate::binding_markers::PendingProfileAccountBinding {
                    profile,
                    expected_identity_id: expected_identity_id.clone(),
                    new_identity_id: new_identity_id.clone(),
                };
                // Remembered first, asked once durable: `service_binding_markers`
                // sends the bind when the writer answers.
                self.profile_bindings_in_flight.insert(profile);
                self.runtime.pending_bindings.remember(marker);
            }
            // The same line Ctrl+= reaches: the window menu's stepper is the
            // chord with a glyph on it, not a second way to zoom.
            PaneRequest::Zoom(intent) => self.rezoom(match intent {
                ZoomIntent::Out => self.zoom - theme::ZOOM_STEP,
                ZoomIntent::In => self.zoom + theme::ZOOM_STEP,
                ZoomIntent::ActualSize => 1.0,
            }),
            PaneRequest::OpenSearch => self.open_search(event_loop, None),
            request @ (PaneRequest::OpenUri(_)
            | PaneRequest::OpenWebsite(_)
            | PaneRequest::OpenHelp) => {
                request.dispatch_system_uri(&self.host);
            }
            PaneRequest::OpenSupport => self.open_support(event_loop),
            PaneRequest::OpenChatLink { chat, block } => {
                self.host.open_chat_link(chat, block);
            }
            PaneRequest::Navigate {
                target,
                match_text,
                disposition,
            } => self.navigate(event_loop, source, target, match_text, disposition),
            PaneRequest::ActivateDocument {
                document,
                workspace,
            } => self.activate_document_in_place(document, workspace),
            PaneRequest::OpenBackground { document, node } => {
                self.open_background_document(source, document, node);
            }
            PaneRequest::OpenDocumentsInNewWindow { documents } => {
                self.open_documents_in_new_window(event_loop, source, documents);
            }
            PaneRequest::Switch(id) => self.switch_workspace(id),
            PaneRequest::OpenInNewWindow(id) => {
                startup::launch(&self.runtime.profile_dir, Some(id));
            }
            PaneRequest::NewWorkspace(name) => self.new_workspace(&name),
            PaneRequest::CreateDocument(title) => self.create_document(source, &title, false),
            PaneRequest::CreateCanvas(title) => self.create_document(source, &title, true),
            PaneRequest::NewWorkspaceIn(owner, name) => self.new_workspace_in(owner, &name),
            PaneRequest::CreateLinkedDocument(title) => self.create_linked_document(source, &title),
            PaneRequest::MoveSelectionToNewDocument {
                title,
                payload,
                follow,
                child,
            } => {
                self.move_selection_to_new_document(source, &title, &payload, follow, child);
            }
            PaneRequest::MoveSelectionToDocument {
                document,
                node,
                title,
                payload,
            } => self.move_selection_to_document(source, document, node, &title, &payload),
            PaneRequest::MoveDocument { id, parent, index } => {
                // **`Ok(false)` is not a failure.** It means the drop named a
                // document the manifest no longer holds where the pane thought
                // it was — a stale cached list, not a fault — and the
                // `push_entries` below is the whole of the right answer. Only a
                // real error gets a dialog; a modal for a gesture that merely
                // landed nowhere is how *"Couldn't move that document"* came to
                // appear for something nobody could reproduce.
                if let Err(e) = self.runtime.workspace.move_entry(id, parent, index) {
                    self.report(
                        ErrorReport::new("Couldn't move that document", "Nothing was changed.")
                            .with_details(e),
                    );
                }
                // The manifest just changed, in *this* process, so say so rather
                // than waiting for a drain to notice. Without it the drop looked
                // right only because `ExploreView`'s drag keeps its own row
                // order, and the next rebuild put it back where it was.
                self.push_entries();
                self.invalidate_everywhere();
            }
            PaneRequest::TransferDocument {
                id,
                source: source_workspace,
                target,
                parent,
                index,
                follow,
                path_label: _,
            } => self.queue_document_transfer(
                source,
                id,
                source_workspace,
                target,
                parent,
                index,
                follow,
            ),
            PaneRequest::ListWorkspaceDocuments(workspace) => {
                match self.runtime.workspace.list_workspace_documents(workspace) {
                    Ok(entries) => {
                        for pane in self.panes.values_mut() {
                            if pane
                                .view
                                .set_move_to_workspace_entries(workspace, entries.clone())
                            {
                                // redraw so expand/filter see the new rows
                            }
                        }
                        self.invalidate_everywhere();
                    }
                    Err(e) => {
                        eprintln!("warning: list workspace documents for move-to: {e}");
                    }
                }
            }
            PaneRequest::TrashDocuments(ids) => {
                let mut failed = None;
                for id in ids {
                    if let Err(e) = self.runtime.workspace.trash_document(id) {
                        failed.get_or_insert(e);
                    }
                }
                if let Some(e) = failed {
                    self.report(
                        ErrorReport::new("Couldn't move that to the trash", "Nothing was changed.")
                            .with_details(e),
                    );
                }
                // The library is shorter now, in this process, so say so rather
                // than waiting for a drain to notice.
                self.push_entries();
                self.invalidate_everywhere();
            }
            PaneRequest::RestoreDocuments(ids) => {
                crate::attach_debug!(
                    "shell drain PaneRequest::RestoreDocuments n={} ids={:?}",
                    ids.len(),
                    ids.iter().map(|i| i.to_storage()).collect::<Vec<_>>()
                );
                for id in ids {
                    if let Err(e) = self.runtime.workspace.restore_document(id) {
                        crate::attach_debug!(
                            "shell restore_document failed id={} err={e}",
                            id.to_storage()
                        );
                        self.report(
                            ErrorReport::new("Couldn't restore that", "Nothing was changed.")
                                .with_details(e),
                        );
                        break;
                    }
                }
                self.push_entries();
                self.invalidate_everywhere();
            }
            PaneRequest::PurgeDocuments(ids) => {
                let lang = koru_i18n::Lang::for_system(
                    koru_window::system_locale::system_locale().as_deref(),
                );
                if let Some(report) =
                    trash::purge_report(ids, lang, |id| self.runtime.workspace.purge_document(id))
                {
                    self.report(report);
                }
                self.push_entries();
                self.invalidate_everywhere();
            }
            PaneRequest::EmptyDocumentTrash => {
                if let Err(e) = self.runtime.workspace.empty_trashed_documents() {
                    self.report(
                        ErrorReport::new("Couldn't empty the Trash", "Nothing was changed.")
                            .with_details(e),
                    );
                }
                self.push_entries();
                self.invalidate_everywhere();
            }
            PaneRequest::CopyDocument { source, title } => {
                match self.runtime.workspace.copy_document(source, &title) {
                    Ok(copy) => {
                        self.opened_document(copy);
                        // **The listing before the announcement**, the order
                        // `create_document` keeps: the container reads a new
                        // document's kind from the listing when it builds its
                        // tab, and a copy of a canvas announced first would be
                        // built as a page.
                        self.push_entries();
                        self.show_active_document();
                        self.invalidate_everywhere();
                    }
                    // **Nothing was changed** is the load-bearing half: the
                    // document being copied is in the bin, so somebody reading
                    // this has to know their way out is still Restore.
                    Err(e) => self.report(
                        ErrorReport::new("Couldn't save a copy", "Nothing was changed.")
                            .with_details(e),
                    ),
                }
            }
            // No redraw and no context push: nothing anybody can see changed —
            // the tab is already gone from the strip that asked. What changed is
            // a thread and a socket, and those cost nothing to look at.
            PaneRequest::CloseDocument(id) => self.release_document(id),
            PaneRequest::SwitchProfile(id) => {
                self.switch_profile(id);
            }
            PaneRequest::SwitchToWorkspaceIn(profile, workspace) => {
                if self.runtime.profile.id() == Some(profile) {
                    self.switch_workspace(workspace);
                } else {
                    self.switch_profile_inner(profile, Some(workspace), true);
                }
            }
            PaneRequest::AddProfile => {
                if let Some(dir) = startup::ask_for_a_new_profile_folder() {
                    startup::launch(&dir, None);
                }
            }
            // The old in-window profile creator is retired; approved onboarding owns creation.
            PaneRequest::CreateStoreProfile => {}
            PaneRequest::OpenProfile => {
                if let Some(dir) = startup::ask_to_open_a_profile_folder() {
                    startup::launch(&dir, None);
                }
            }
            PaneRequest::RenameProfile(id, name) => self.rename_profile(id, &name),
            PaneRequest::SetProfileAppearance { id, icon, color } => {
                self.set_profile_appearance(id, icon.as_deref(), color.as_deref())
            }
            PaneRequest::RenameWorkspace(id, name) => self.rename_workspace(id, &name),
            PaneRequest::RefreshWorkspaceStats => self.push_workspace_stats(),
            PaneRequest::ImportWorkspace => self.import_workspace(source),
            PaneRequest::FinishWorkspaceImport(choice) => {
                self.finish_workspace_import(source, choice)
            }
            PaneRequest::SetWorkspaceIcon { id, icon, color } => {
                // One flush and one sidecar write for both fields: they are one
                // decision, and two writes would put a frame between them where
                // the switcher showed the new emoji on the old colour.
                self.set_workspace_flag(id, WorkspaceFlag::Appearance { icon, color });
            }
            PaneRequest::ImportContent {
                format,
                source: selection,
            } => {
                self.import_content(source, format, selection);
            }
            PaneRequest::ChooseDelegateFolder { title, start } => {
                self.choose_delegate_folder(source, &title, start.as_deref());
            }
            PaneRequest::ChooseRecordingPath {
                title,
                start,
                file_name,
            } => {
                self.choose_recording_path(source, &title, start.as_deref(), &file_name);
            }
            PaneRequest::ChooseAiChatFolder { title, start } => {
                // The picker is modal to the window that asked; the agent
                // itself says whether it can work there, so no disk probe.
                if let Some(pane) = self.panes.get(&source)
                    && let Some(folder) = crate::dialogs::pick_folder(
                        &title,
                        start.as_deref(),
                        Some(pane.window.as_ref()),
                    )
                    && let Some(pane) = self.panes.get_mut(&source)
                    && pane.view.ai_chat_folder_chosen(folder)
                {
                    pane.window.request_redraw();
                }
            }
            PaneRequest::ImportFiles(selection) => {
                self.import_content(source, "files", selection);
            }
            PaneRequest::CancelImport => self.cancel_content_import(),
            PaneRequest::VoiceConsent(accepted) => self.answer_voice_consent(accepted),
            PaneRequest::CancelVoiceDownload => self.stop_voice_download(),
            PaneRequest::SetUpVoice => self.offer_voice_model(source),
            PaneRequest::RemoveVoiceModels => self.remove_voice_models(),
            PaneRequest::AskVoiceDevices => self.ask_voice_devices(),
            PaneRequest::AnswerReminderStartup(request) => self.answer_reminder_startup(request),
            PaneRequest::ReminderSnooze(intent) => {
                if let Some(pane) = self.panes.get_mut(&source)
                    && pane.view.reminder_snooze(intent)
                {
                    pane.window.request_redraw();
                }
            }
            PaneRequest::SaveFile(id) => {
                crate::attach_debug!("shell drain PaneRequest::SaveFile id={}", id.to_storage());
                self.save_file(id);
            }
            PaneRequest::SaveFiles(ids) => self.save_files(ids),
            PaneRequest::SaveDiagram(request) => self.save_diagram(source, request),
            PaneRequest::CopyDiagram(request) => self.copy_diagram(source, &request),
            PaneRequest::OpenFile(id) => {
                crate::attach_debug!("shell drain PaneRequest::OpenFile id={}", id.to_storage());
                self.open_file(id);
            }
            PaneRequest::OpenFiles(ids) => self.open_files(ids),
            PaneRequest::OpenFileNote(id) => self.open_file_note(event_loop, source, id),
            PaneRequest::RenameFile(id, title) => {
                if let Err(error) = self.runtime.workspace.rename_document(id, &title) {
                    self.report(
                        ErrorReport::new("Couldn't rename that file", "Nothing was changed.")
                            .with_details(error),
                    );
                } else {
                    self.push_entries();
                    self.invalidate_everywhere();
                }
            }
            PaneRequest::DownloadFile(id) => self.download_file(id),
            PaneRequest::PinWorkspace(id, on) => {
                self.set_workspace_flag(id, WorkspaceFlag::Pinned(on));
            }
            PaneRequest::ReorderWorkspaces(ids) => self.reorder_workspaces(&ids),
            PaneRequest::ArchiveWorkspace(id, on) => {
                self.archive_workspace(id, on);
            }
            PaneRequest::RevealPath(dir) => self.reveal_path(&dir),
            // The same exit a removed last Profile takes: launch the successor,
            // then let this loop end.
            PaneRequest::Relaunch => {
                startup::launch(&self.runtime.profile_dir, None);
                self.primary = None;
                event_loop.exit();
            }
            PaneRequest::ExportWorkspace(id) => self.export_workspace(id),
            PaneRequest::ExportDocuments { ids, format } => {
                self.export_documents(ids, format);
            }
            PaneRequest::DeleteWorkspace(id) => self.delete_workspace(id),
            PaneRequest::RestoreWorkspace(id) => self.restore_workspace(id),
        }
        true
    }

    /// Pick/accept files, convert them off the event loop and atomically add
    /// each successful document to the current workspace.
    fn import_content(
        &mut self,
        source: WindowId,
        format: &'static str,
        selection: crate::ImportSourceSelection,
    ) {
        if self.import_busy {
            return;
        }
        let Some(workspace) = self.runtime.workspace.current() else {
            self.report(ErrorReport::new(
                "Nothing to import into",
                "Open a workspace first.",
            ));
            return;
        };
        let import_source = match selection {
            crate::ImportSourceSelection::Picker => {
                let Some(descriptor) = crate::formats::import_descriptor(format) else {
                    self.report(ErrorReport::new(
                        "Unknown import format",
                        format!("No importer is registered for \u{201c}{format}\u{201d}."),
                    ));
                    return;
                };
                match descriptor.source {
                    koru_import::SourceRequest::File { extensions }
                    | koru_import::SourceRequest::Files { extensions } => {
                        let filter = crate::dialogs::FileFilter {
                            name: descriptor.name,
                            extensions,
                        };
                        let lang = koru_i18n::Lang::for_system(
                            koru_window::system_locale::system_locale().as_deref(),
                        );
                        crate::dialogs::pick_open_files(
                            crate::formats::import_picker_title(format, lang)
                                .unwrap_or(descriptor.picker_title),
                            &[filter],
                            None,
                            self.primary_window(),
                        )
                        .map(koru_import::ImportSource::Files)
                    }
                    // Remote sources have no file dialog: the importer
                    // authenticates and opens its own picker on the worker.
                    koru_import::SourceRequest::Google => Some(koru_import::ImportSource::Google),
                    koru_import::SourceRequest::Directory => {
                        self.report(ErrorReport::new(
                            "Unknown import format",
                            format!(
                                "\u{201c}{}\u{201d} imports a folder, not content into this workspace.",
                                descriptor.name
                            ),
                        ));
                        return;
                    }
                }
            }
            crate::ImportSourceSelection::Files(paths) => {
                (!paths.is_empty()).then_some(koru_import::ImportSource::Files(paths))
            }
        };
        let Some(import_source) = import_source else {
            return;
        };
        self.begin_import_progress(
            source,
            "Import started. We’ll notify you when it’s ready.".into(),
        );
        self.import_busy = true;
        self.import_source = Some(source);
        self.import_format = Some(format);
        let cancellation = Arc::new(koru_import::CancellationToken::default());
        self.import_cancel = Some(Arc::clone(&cancellation));
        self.import_cancel_hide_due = None;
        self.import_cancel_worker_done = false;
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let oauth_credentials = self.host.oauth_credential_context();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let store_profile = self
            .runtime
            .profile
            .id()
            .map(|id| koru_store_core::ids::ProfileId::from_uuid(id.as_uuid()));
        let store_workspace = koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid());
        if let Err(error) = std::thread::Builder::new()
            .name("koru-import-content".into())
            .spawn(move || {
                match &import_source {
                    koru_import::ImportSource::Files(paths) => {
                        println!(
                            "[Koru][import] started format={format} workspace={} sources={}",
                            store_workspace.to_storage(),
                            paths.len()
                        );
                    }
                    koru_import::ImportSource::Directory(_) | koru_import::ImportSource::Google => {
                        println!(
                            "[Koru][import] started format={format} workspace={}",
                            store_workspace.to_storage()
                        );
                    }
                }
                let progress = ContentImportProgress {
                    format,
                    tx: tx.clone(),
                    proxy: proxy.clone(),
                    cancellation,
                };
                let result: Result<ContentImportSummary, koru_import::ImportError> = (|| {
                    let registry = koru_import::ImportRegistry::new(crate::formats::importers(oauth_credentials))
                        .map_err(|error| error.to_string())?;
                    let importer = registry
                        .get(format)
                        .ok_or_else(|| format!("no importer is registered for {format}"))?;
                    let prepared = importer
                        .inspect(
                            import_source,
                            &progress,
                        )
                        .map_err(|error| error.to_string())?;
                    if koru_import::ImportProgress::cancelled(&progress) {
                        return Err(koru_import::ImportError::cancelled());
                    }
                    let preview = prepared.preview();
                    let has_deleted = preview_contains_trashed(&preview.documents);
                    println!(
                        "[Koru][import] inspected format={format} documents={} nodes={} images={} warnings={}",
                        preview.document_count,
                        preview.node_count,
                        preview.image_count,
                        preview.warnings.len()
                    );
                    let client = client_at(root.as_deref()).map_err(|error| error.to_string())?;
                    let store_capabilities = client
                        .store()
                        .status()
                        .map_err(|error| error.to_string())?
                        .capabilities;
                    let occupied_titles: std::collections::BTreeSet<String> = client
                        .documents(store_workspace)
                        .map_err(|error| error.to_string())?
                        .into_iter()
                        .map(|entry| entry.title)
                        .collect();
                    let blobs = StoreBlobSink {
                        client: client.clone(),
                        workspace: store_workspace,
                    };
                    let tags = StoreTagSink::new(
                        segment,
                        root.clone(),
                        store_workspace,
                    );
                    let context = koru_import::ImportContext {
                        target_profile: store_profile,
                        target_workspace: Some(store_workspace),
                        occupied_titles: &occupied_titles,
                        blobs: &blobs,
                        tags: &tags,
                    };
                    let product = after_deleted_import_capability_check(
                        has_deleted,
                        &store_capabilities,
                        || prepared.build(
                            koru_import::ImportOptions {
                                identity: koru_import::IdentityMode::Copy,
                                accept_warnings: true,
                            },
                            &context,
                            &progress,
                        ),
                    )?;
                    if koru_import::ImportProgress::cancelled(&progress) {
                        return Err(koru_import::ImportError::cancelled());
                    }
                    let koru_import::ImportProduct::Documents(content) = product else {
                        return Err(
                            "that importer produced a workspace instead of documents".into()
                        );
                    };
                    println!(
                        "[Koru][import] converted format={format} documents={} warnings={}",
                        content.documents.len(),
                        content.warnings.len()
                    );
                    let mut succeeded = 0usize;
                    let mut already_known = 0usize;
                    let mut files = Vec::new();
                    let mut root = None;
                    let mut fallback_root = None;
                    let mut source_roots = std::collections::BTreeMap::new();
                    let dedups_known_files = crate::formats::import_descriptor(format)
                        .is_some_and(|descriptor| descriptor.dedups_known_files);
                    let mut known_files = if dedups_known_files {
                        known_workspace_files(&client, store_workspace)?
                    } else {
                        std::collections::HashMap::new()
                    };
                    let mut file_remap = std::collections::HashMap::new();
                    let lang = koru_i18n::Lang::for_system(
                        koru_window::system_locale::system_locale().as_deref(),
                    );
                    let mut warning_groups: std::collections::BTreeMap<PathBuf, Vec<String>> =
                        std::collections::BTreeMap::new();
                    for warning in content.warnings {
                        warning_groups
                            .entry(warning.source.clone())
                            .or_default()
                            .push(rendered_import_warning(lang, &warning));
                    }
                    for mut document in content.documents {
                        if koru_import::ImportProgress::cancelled(&progress) {
                            return Err(koru_import::ImportError::cancelled());
                        }
                        let parent = document.parent;
                        let kind = document.kind.clone();
                        let file_locator = (kind == koru_core::explore::KIND_FILE)
                            .then(|| snapshot_file_locator(&document.snapshot))
                            .flatten();
                        if let Some(known) = file_locator
                            .as_ref()
                            .and_then(|(hash, _)| known_files.get(hash))
                            .cloned()
                        {
                            already_known += 1;
                            let old = koru_core::DocumentId::from_uuid(document.id.as_uuid());
                            file_remap.insert(old, known.clone());
                            files.push((known.document, known.title.clone()));
                            if format == "files" {
                                root.get_or_insert(ContentImportRoot::File(known.document));
                            }
                            continue;
                        }
                        if kind != koru_core::explore::KIND_FILE && !file_remap.is_empty() {
                            document.snapshot = remap_imported_file_references(
                                &document.snapshot,
                                &file_remap,
                            )?;
                        }
                        let trashed_at = document.trashed.then(|| {
                            document
                                .trashed_at
                                .unwrap_or_else(koru_store_core::model::Timestamp::now)
                        });
                        match client.import_document_at_position_trashed(
                            store_workspace,
                            document.id,
                            &document.title,
                            &document.kind,
                            document.mime.as_deref(),
                            document.parent,
                            None,
                            trashed_at,
                            document.snapshot,
                        ) {
                            Ok(id) => {
                                succeeded += 1;
                                let id = koru_core::DocumentId::from_uuid(id.as_uuid());
                                let imported_root = if kind == koru_core::explore::KIND_FILE {
                                    ContentImportRoot::File(id)
                                } else if crate::formats::import_descriptor(format)
                                    .is_some_and(|descriptor| descriptor.roots_import)
                                {
                                    ContentImportRoot::Notebook(id)
                                } else {
                                    ContentImportRoot::Document(id)
                                };
                                fallback_root.get_or_insert(imported_root);
                                if root.is_none() {
                                    root = content_import_root(format, &kind, parent, id);
                                }
                                if parent.is_none() && kind != koru_core::explore::KIND_FILE {
                                    source_roots.entry(document.source).or_insert(id);
                                }
                                if kind == koru_core::explore::KIND_FILE {
                                    files.push((id, document.title));
                                    if let Some((hash, node)) = file_locator {
                                        known_files.insert(
                                            hash,
                                            KnownFile {
                                                document: id,
                                                node,
                                                title: files.last().unwrap().1.clone(),
                                            },
                                        );
                                    }
                                }
                            }
                            Err(error) => {
                                warning_groups
                                    .entry(document.source)
                                    .or_default()
                                    .push(error.to_string())
                            }
                        }
                    }
                    if succeeded == 0 && already_known == 0 {
                        let details = warning_groups
                            .values()
                            .flatten()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join("\n");
                        return Err(details.into());
                    }
                    let root = root.or(fallback_root).ok_or_else(|| {
                        "the importer completed without a root result".to_string()
                    })?;
                    let had_failures = !warning_groups.is_empty();
                    let mut warnings = Vec::new();
                    if had_failures {
                        if koru_import::ImportProgress::cancelled(&progress) {
                            return Err(koru_import::ImportError::cancelled());
                        }
                        let fallback_parent =
                            koru_store_core::ids::DocumentId::from_uuid(root.id().as_uuid());
                        for failed in failed_import_documents(
                                store_workspace,
                                fallback_parent,
                                &source_roots,
                                warning_groups,
                                lang,
                            ) {
                            match failed {
                            Ok(failed) => {
                                let source = failed.source.clone();
                                if let Err(error) = client.import_document_at_position(
                                    store_workspace,
                                    failed.id,
                                    &failed.title,
                                    &failed.kind,
                                    None,
                                    failed.parent,
                                    Some(0),
                                    failed.snapshot,
                                ) {
                                    warnings.push(format!(
                                        "{}: FAILED could not be created: {error}",
                                        source.display()
                                    ));
                                }
                            }
                            Err(error) => {
                                warnings.push(format!("FAILED could not be created: {error}"));
                            }
                            }
                        }
                    }
                    Ok(ContentImportSummary {
                        root,
                        files,
                        imported: succeeded,
                        already_known,
                        had_failures,
                        warnings,
                    })
                })();
                match &result {
                    Ok(summary) => println!(
                        "[Koru][import] completed format={format} root={} files={} imported={} already_known={} partial_failures={}",
                        summary.root.id().to_storage(),
                        summary.files.len(),
                        summary.imported,
                        summary.already_known,
                        summary.had_failures
                    ),
                    Err(_) => println!("[Koru][import] failed format={format}"),
                }
                let _ = tx.send(ImportMessage::ContentImported(result));
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.import_busy = false;
            self.import_source = None;
            self.import_cancel = None;
            self.import_cancel_hide_due = None;
            self.import_cancel_worker_done = false;
            self.clear_import_progress();
            self.report(
                ErrorReport::new("Couldn't start the import", "Nothing was imported.")
                    .with_details(error),
            );
        }
    }

    /// Start the oldest coalesced file-drop batch when the importer is free.
    /// Called from `about_to_wait`, after all currently queued window events
    /// have had a chance to join the batch and after completed imports clear
    /// `import_busy`.
    fn start_pending_content_drop(&mut self) {
        if self.import_busy {
            return;
        }
        let Some((source, format, paths)) = self.pending_content_drops.pop_front() else {
            return;
        };
        self.import_content(source, format, crate::ImportSourceSelection::Files(paths));
    }

    /// Import the oldest thing somebody shared into Koru, if anything is
    /// waiting and the importer is free.
    ///
    /// **The share extension is a second process and never opens the Store**
    /// ([ADR-136]/[ADR-416]), so it stages payloads into
    /// `<channel root>/share-inbox/` and this is the other half: activation is
    /// the earliest moment the app that *does* own the Store can act on them.
    /// Called from `about_to_wait` beside [`start_pending_content_drop`], for
    /// the same reason — the drop path is the template, down to serializing
    /// against `import_busy` rather than being silently dropped by it.
    ///
    /// Four things this deliberately does **not** do:
    ///
    /// - **It does not ask where to put it.** A share lands in the open
    ///   workspace, exactly as a dragged file does. The macOS extension has no
    ///   destination picker, so `manifest.workspaceId` is never set on this
    ///   platform; a picker is [TECHDEBT 76] and the field is already carried.
    /// - **It does not report an empty inbox, a missing one, or a closed
    ///   workspace.** This runs on *every* activation, so anything it says on
    ///   the ordinary path it says over and over. A share that cannot be
    ///   imported yet stays in the inbox and is retried, which is the same
    ///   contract iOS's postpone gives.
    /// - **It does not route by file type.** One entry is one import through
    ///   the generic file importer, because an entry is an arbitrary batch and
    ///   splitting it by extension would be several imports and several toasts
    ///   for one gesture.
    /// - **It does not remove the entry here.** That happens on the import's
    ///   result, and only on success.
    ///
    /// **The trigger's one boundary.** The flag is set by `Focused(true)`, so a
    /// share staged while Koru is *already* frontmost would wait for the next
    /// focus change. That cannot happen from a share sheet — the sheet belongs
    /// to the app being shared *from*, so Koru is by construction not frontmost
    /// when the extension runs, and activating it is what raises the event. It
    /// is written down because the day something else writes into this inbox,
    /// that assumption is the thing that stops being true. A timer is not the
    /// answer: nothing in this loop may walk anything on a clock.
    ///
    /// [ADR-136]: ../../../docs/decisions/ADR-126-150.md
    /// [ADR-416]: ../../../docs/decisions/ADR-416-macshare.md
    /// [TECHDEBT 76]: ../../../docs/contributing/techdebt/README.md
    fn start_share_drain(&mut self) {
        if !self.share_drain_due || self.import_busy || self.in_tray {
            return;
        }
        // No window is no place for the toast to land and no `WindowId` to
        // import from; stay due and try again when one exists. The focused
        // pane wins where there is one — the share brought a window to the
        // front, and that is the window whose toast the person is looking at.
        let source = self
            .panes
            .iter()
            .find(|(_, pane)| pane.focused)
            .map(|(id, _)| *id)
            .or_else(|| self.panes.keys().copied().next());
        let Some(source) = source else {
            return;
        };
        // No open workspace is not an error here the way it is for a menu
        // command: nobody asked for this import at this instant.
        if self.runtime.workspace.current().is_none() {
            return;
        }
        let mut entries = koru_share_inbox::drain_every_inbox();
        if entries.is_empty() {
            self.share_drain_due = false;
            return;
        }
        // One per pass; `about_to_wait` comes round again the moment the
        // import completes, and `share_drain_due` stays set until the inbox
        // reads empty. That keeps the whole queue moving without ever running
        // two imports at once.
        let entry = entries.remove(0);
        let paths = match entry.payloads() {
            Ok(payloads) => payloads
                .into_iter()
                .map(|(path, _)| path)
                .collect::<Vec<_>>(),
            Err(error) => {
                // A manifest this side refuses is not something a retry can
                // fix, and leaving it makes every activation trip over it.
                println!("[Koru][share] discarding an unusable entry: {error}");
                entry.discard();
                return;
            }
        };
        if paths.is_empty() {
            entry.discard();
            return;
        }
        println!(
            "[Koru][share] importing {} shared file(s) from {}",
            paths.len(),
            entry.id
        );
        self.share_import_in_flight = Some(entry);
        self.import_content(source, "files", crate::ImportSourceSelection::Files(paths));
        // `import_content` refuses while busy or without a workspace, both
        // already checked above — but if it ever declines for a reason this
        // does not know about, two things must not happen: the entry must not
        // be stranded as in-flight against an import that never runs, and this
        // must not become a hot loop re-reading the inbox on every wake. Stand
        // down until the next activation; the entry keeps, as it always does.
        if !self.import_busy {
            self.share_import_in_flight = None;
            self.share_drain_due = false;
        }
    }

    /// Inspect and convert an older single-document workspace without blocking
    /// the event loop or modifying the selected folder.
    fn import_workspace(&mut self, source: WindowId) {
        if self.import_busy {
            return;
        }
        let Some(folder) = crate::dialogs::pick_folder(
            "Choose an older Koru workspace",
            None,
            self.primary_window(),
        ) else {
            return;
        };
        self.import_busy = true;
        self.import_source = Some(source);
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-import-inspect".into())
            .spawn(move || {
                use koru_import::Importer;
                let importer = koru_import_legacy_workspace::LegacyWorkspaceImporter;
                let result = importer
                    .inspect(
                        koru_import::ImportSource::Directory(folder),
                        &koru_import::NoProgress,
                    )
                    .map_err(|e| e.to_string())
                    .and_then(|prepared| {
                        let client = client_at(root.as_deref()).map_err(|e| e.to_string())?;
                        let workspaces = client.workspaces().map_err(|e| e.to_string())?;
                        let identity_conflict =
                            prepared.preview().native_workspace_id.is_some_and(|id| {
                                workspaces.iter().any(|workspace| workspace.id == id)
                            });
                        Ok(InspectedImport {
                            prepared,
                            identity_conflict,
                        })
                    });
                let _ = tx.send(ImportMessage::Inspected(result));
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.import_busy = false;
            self.import_source = None;
            self.report(
                ErrorReport::new("Couldn't start the import", "Nothing was changed.")
                    .with_details(error),
            );
        }
    }

    fn handle_import_message(&mut self, message: ImportMessage) {
        match message {
            ImportMessage::ContentProgress {
                stage,
                completed,
                total,
            } => {
                if self.import_cancel_hide_due.is_none() {
                    self.update_content_import_progress(stage, completed, total);
                }
            }
            ImportMessage::Inspected(Err(error)) => {
                self.import_busy = false;
                self.import_source = None;
                self.report(
                    ErrorReport::new("That workspace could not be read", "Nothing was imported.")
                        .with_details(error),
                );
            }
            ImportMessage::Inspected(Ok(inspected)) => self.confirm_import(inspected),
            ImportMessage::Imported(Err(error)) => {
                self.import_busy = false;
                self.import_source = None;
                self.import_workspace_count = None;
                crate::events_host::import_completed(
                    "legacy_workspace",
                    0,
                    crate::events_host::ImportOutcome::Failed,
                );
                self.report(
                    ErrorReport::new(
                        "The workspace could not be imported",
                        "The old folder was not changed.",
                    )
                    .with_details(error),
                );
            }
            ImportMessage::Imported(Ok(workspace)) => {
                self.import_busy = false;
                self.import_source = None;
                crate::events_host::import_completed(
                    "legacy_workspace",
                    self.import_workspace_count.take().unwrap_or(0),
                    crate::events_host::ImportOutcome::Ok,
                );
                crate::events_host::workspace_created(
                    workspace.id.as_uuid(),
                    crate::events_host::WorkspaceSource::Import,
                );
                let id = koru_core::WorkspaceId::from_uuid(workspace.id.as_uuid());
                let _ = self.runtime.profile.ensure_workspace(
                    id,
                    &workspace.name,
                    workspace.created_at.millis(),
                );
                // Import already asked for confirmation before writing. Open
                // the result directly instead of raising a second native
                // dialog from a winit user-event callback. On Windows that
                // TaskDialog could be created hidden and spin the UI thread
                // forever while the import itself had completed successfully.
                self.switch_workspace(id);
            }
            ImportMessage::ContentImported(Err(error)) => {
                if self
                    .import_cancel
                    .as_ref()
                    .is_some_and(|token| token.is_cancelled())
                {
                    println!("[Koru][import] cancellation completed");
                    self.finish_pending_attachment(true);
                    self.import_cancel_worker_done = true;
                    if self.import_cancel_hide_due.is_none() {
                        // A cancel that did not come from the progress button
                        // (an importer ending its own flow — a closed picker)
                        // still has a progress toast up; clear it quietly.
                        self.clear_import_progress();
                        self.finish_content_import_cancellation();
                    }
                    return;
                }
                self.import_busy = false;
                self.import_source = None;
                self.import_cancel = None;
                self.import_cancel_hide_due = None;
                self.import_cancel_worker_done = false;
                self.finish_pending_attachment(true);
                self.clear_import_progress();
                // **A failed import keeps the share.** The inbox entry is the
                // only copy of what somebody shared; dropping it here would
                // turn a transient failure into lost content, which is the one
                // unrecoverable bug. It stays, and the next activation retries.
                self.share_import_in_flight = None;
                if let Some(format) = self.import_format.take() {
                    crate::events_host::import_completed(
                        format,
                        0,
                        crate::events_host::ImportOutcome::Failed,
                    );
                }
                let body = error
                    .code()
                    .map(import_error_key)
                    .map(|key| {
                        let lang = koru_i18n::Lang::for_system(
                            koru_window::system_locale::system_locale().as_deref(),
                        );
                        koru_i18n::t(lang, key)
                    })
                    .unwrap_or("No empty documents were left behind.");
                self.report(
                    ErrorReport::new("The content could not be imported", body)
                        .with_details(error),
                );
            }
            ImportMessage::ContentImported(Ok(summary)) => {
                if self
                    .import_cancel
                    .as_ref()
                    .is_some_and(|token| token.is_cancelled())
                {
                    println!("[Koru][import] cancellation completed after worker result");
                    self.finish_pending_attachment(true);
                    self.import_cancel_worker_done = true;
                    if self.import_cancel_hide_due.is_none() {
                        self.finish_content_import_cancellation();
                    }
                    return;
                }
                self.import_busy = false;
                self.import_source = None;
                self.import_cancel = None;
                self.import_cancel_hide_due = None;
                self.import_cancel_worker_done = false;
                self.clear_import_progress();
                // The bytes are in the Store, so the staged copy is spent.
                // This is the only place an entry is removed after an import,
                // and it is deliberately on the success arm alone.
                if let Some(entry) = self.share_import_in_flight.take() {
                    entry.discard();
                }
                let ContentImportSummary {
                    root,
                    files,
                    imported,
                    already_known,
                    had_failures,
                    warnings,
                } = summary;
                if let Some(format) = self.import_format.take() {
                    crate::events_host::import_completed(
                        format,
                        imported,
                        if had_failures {
                            crate::events_host::ImportOutcome::Partial
                        } else {
                            crate::events_host::ImportOutcome::Ok
                        },
                    );
                }
                self.push_entries();
                self.sync_pane_context();
                // The import wrote through its own store client, so the
                // manifest this window lists from would only meet the new
                // entries on the next sync turn — the listing would trail the
                // toast. Run the turn now, then push again with the entries
                // the first push could not yet see.
                if self.runtime.workspace.sync_manifest_now() {
                    let _ = self.runtime.workspace.take_entries_changed();
                    self.push_entries();
                }
                if let Some((window, node)) = self.pending_attachment.take()
                    && let Some(pane) = self.panes.get_mut(&window)
                {
                    crate::attach_debug!(
                        "shell import done → insert_attachment node={node:?} n_files={}",
                        files.len()
                    );
                    if files.is_empty() {
                        pane.view
                            .attachment_cancelled(&mut self.runtime.workspace.ws, node);
                    } else {
                        pane.view
                            .insert_attachment(&mut self.runtime.workspace.ws, node, &files);
                    }
                }
                self.invalidate_everywhere();
                let message = match (imported, already_known, had_failures) {
                    (0, 1, false) => "File is already known.".into(),
                    (0, known, false) if known > 1 => {
                        format!("{known} files are already known.")
                    }
                    (_, 0, false) => "Import succeeded.".into(),
                    (_, known, false) => format!(
                        "Import succeeded. {known} {} already known.",
                        if known == 1 { "file was" } else { "files were" }
                    ),
                    (_, 0, true) => {
                        "Import succeeded. See FAILED for items that need attention.".into()
                    }
                    (_, known, true) => format!(
                        "Import succeeded. {known} {} already known. See FAILED for items that need attention.",
                        if known == 1 { "file was" } else { "files were" }
                    ),
                };
                self.notice_import(
                    message,
                    root.notice_target(),
                );
                if !warnings.is_empty() {
                    self.report(
                        ErrorReport::new(
                            "Some imported content needs attention",
                            "The successful content was kept, but FAILED could not be created.",
                        )
                        .with_details(warnings.join("\n")),
                    );
                }
            }
            ImportMessage::Exported {
                id,
                name,
                destination,
                result,
            } => match result {
                Ok(()) => {
                    if let Some(dir) = destination.parent() {
                        self.remember_export_dir(dir.to_path_buf());
                    }
                    self.notice_reveal(
                        format!(
                            "\u{201c}{name}\u{201d} was exported to {}",
                            destination.display()
                        ),
                        destination,
                    );
                }
                Err(error) => self.report(
                    ErrorReport::new(
                        format!("Couldn't export \u{201c}{name}\u{201d}"),
                        format!(
                            "Koru could not write to {}, so no completed backup was left. The workspace was not changed.",
                            destination.parent().unwrap_or(&destination).display()
                        ),
                    )
                    .with_details(error)
                    .with_action(ErrorAction::Retry(PaneRequest::ExportWorkspace(id))),
                ),
            },
            ImportMessage::DocumentsExported {
                count,
                name,
                destination,
                remembered_dir,
                result,
                format,
                ids,
            } => match result {
                Ok(()) => {
                    if let Some(dir) = remembered_dir {
                        self.remember_export_dir(dir);
                    }
                    let msg = if count == 1 {
                        format!(
                            "\u{201c}{name}\u{201d} was exported to {}",
                            destination.display()
                        )
                    } else {
                        format!(
                            "{count} documents were exported to {}",
                            destination.display()
                        )
                    };
                    self.notice_reveal(msg, destination);
                }
                Err(error) => self.report(
                    ErrorReport::new(
                        if count == 1 {
                            format!("Couldn't export \u{201c}{name}\u{201d}")
                        } else {
                            format!("Couldn't export {count} documents")
                        },
                        "Nothing was changed in the workspace.".to_owned(),
                    )
                    .with_details(error)
                    .with_action(ErrorAction::Retry(PaneRequest::ExportDocuments {
                        ids,
                        format,
                    })),
                ),
            },
            ImportMessage::WorkspaceDeleted {
                id,
                name,
                was_current,
                result,
            } => {
                self.deleting_workspaces.remove(&id);
                match result {
                    Ok(workspace) => {
                        crate::events_host::workspace_deleted(id.as_uuid());
                        let _ = self.runtime.profile.set_workspace_deleted_at(
                            id,
                            workspace.deleted_at.map(|at| at.millis()),
                        );
                        let _ = self.runtime.profile_session.flush(&self.runtime.profile);
                        self.runtime.settings.workspace = self.runtime.live_workspace.then_some(self.runtime.current);
                        self.sync_pane_context();
                    }
                    Err((code, error)) => {
                        if was_current && (!self.runtime.live_workspace || self.runtime.current != id) {
                            self.switch_workspace(id);
                        }
                        let consequence = if code == koru_store_core::Code::WorkspaceInUse {
                            "The workspace is open in another window. Close that window and try again; nothing was deleted."
                        } else {
                            "Nothing was deleted."
                        };
                        self.report(
                            ErrorReport::new(
                                format!("Couldn't delete \u{201c}{name}\u{201d}"),
                                consequence,
                            )
                            .with_details(error),
                        );
                    }
                }
            }
            ImportMessage::FileSaved {
                name,
                destination,
                result,
            } => match result {
                Ok(()) => self.notice_reveal(format!("Saved \u{201c}{name}\u{201d}."), destination),
                Err(error) => self.report(
                    ErrorReport::new("The file could not be saved", "The workspace was not changed.")
                        .with_details(error),
                ),
            },
            ImportMessage::FileOpened { name, result } => match result {
                Ok(()) => self.notice(format!("Opened \u{201c}{name}\u{201d}.")),
                Err(error) if blob_is_waiting_for_a_device(&error) => match waiting_device_names(&error) {
                    Some(names) => self.notice(format!(
                        "\u{201c}{name}\u{201d} is on {names}, which isn't connected. \
                         It will be fetched when it comes online."
                    )),
                    None => self.notice(format!(
                        "\u{201c}{name}\u{201d} is on a device that isn't connected. \
                         It will be fetched when that device comes online."
                    )),
                },
                Err(error) => self.report(
                    ErrorReport::new(
                        "The file could not be opened",
                        "The workspace was not changed.",
                    )
                    .with_details(error),
                ),
            },
            ImportMessage::FilesSaved {
                count,
                destination,
                result,
            } => match result {
                Ok(()) => self.notice_reveal(
                    format!("Saved {count} files."),
                    destination,
                ),
                Err(error) => self.report(
                    ErrorReport::new(
                        "Some files could not be saved",
                        "The workspace was not changed.",
                    )
                    .with_details(error),
                ),
            },
            ImportMessage::FilesOpened { count, result } => match result {
                Ok(()) => self.notice(format!("Opened {count} files.")),
                Err(error) if blob_is_waiting_for_a_device(&error) => self.notice(
                    "Some of these files are on a device that isn't connected. \
                     They will be fetched when it comes online."
                        .to_string(),
                ),
                Err(error) => self.report(
                    ErrorReport::new(
                        "Some files could not be opened",
                        "The workspace was not changed.",
                    )
                    .with_details(error),
                ),
            },
            ImportMessage::FileDownloaded { id, name, result } => {
                for pane in self.panes.values_mut() {
                    pane.view.set_file_fetching(id, false);
                    pane.window.request_redraw();
                }
                match result {
                    // The sweep, not this message, is what turns the row into an
                    // ordinary one — so the list says what the Store says rather
                    // than what a worker believed a moment ago.
                    Ok(()) => {
                        self.notice(format!("Downloaded “{name}”."));
                        self.refresh_file_locality();
                    }
                    Err(error) if blob_is_waiting_for_a_device(&error) => {
                        match waiting_device_names(&error) {
                            Some(names) => self.notice(format!(
                                "\u{201c}{name}\u{201d} is on {names}, which isn't connected. \
                                 It will download when it comes online."
                            )),
                            None => self.notice(format!(
                                "\u{201c}{name}\u{201d} is on a device that isn't connected. \
                                 It will download when that device comes online."
                            )),
                        }
                    }
                    Err(error) => self.report(
                        ErrorReport::new(
                            "The file could not be downloaded",
                            download_failure_hint(&error),
                        )
                        .with_details(error),
                    ),
                }
            }
            ImportMessage::FileLocality { workspace, remote } => {
                self.apply_file_locality(workspace, remote)
            }
        }
    }

    fn remember_export_dir(&mut self, dir: PathBuf) {
        if self.runtime.settings.last_export_dir.as_ref() == Some(&dir) {
            return;
        }
        self.runtime.settings.last_export_dir = Some(dir);
        self.runtime.settings.save_all(
            &self.runtime.profile_dir,
            &self.instance.slot,
            &mut self.runtime.written,
        );
    }

    /// Parent window for native dialogs, when the primary pane still exists.
    fn primary_window(&self) -> Option<&winit::window::Window> {
        self.primary
            .and_then(|id| self.panes.get(&id))
            .map(|pane| pane.window.as_ref())
    }

    /// Save an opaque workspace file without opening it as an editor tab. The
    /// native picker stays on the event thread; snapshot and blob reads happen
    /// on a worker because an attachment may be gigabytes large.
    fn save_file(&mut self, id: koru_core::DocumentId) {
        crate::attach_debug!("shell save_file id={}", id.to_storage());
        let Some(workspace_id) = self.runtime.workspace.current() else {
            crate::attach_debug!("shell save_file abort: no current workspace");
            return;
        };
        let entries = self.runtime.workspace.entries();
        let name = entries
            .iter()
            .find(|entry| entry.id == id && entry.kind == koru_core::explore::KIND_FILE)
            .or_else(|| entries.iter().find(|entry| entry.id == id))
            .map(|entry| entry.title.clone());
        let Some(name) = name else {
            crate::attach_debug!(
                "shell save_file abort: id not in entries (n={})",
                entries.len()
            );
            self.report(ErrorReport::new(
                "File not found",
                "It may have been removed in another window.",
            ));
            return;
        };
        crate::attach_debug!("shell save_file opening picker name={name}");
        // No parent window — same reason as /attach and /image pickers.
        let Some(destination) = crate::dialogs::pick_save_file(
            "Save workspace file",
            &name,
            &[],
            self.runtime.settings.last_export_dir.as_deref(),
            None,
        ) else {
            crate::attach_debug!("shell save_file picker cancelled");
            return;
        };
        crate::attach_debug!(
            "shell save_file destination basename={:?}",
            destination
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        );
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let store_ws = koru_store_core::ids::WorkspaceId::from_uuid(workspace_id.as_uuid());
        let store_id = koru_store_core::ids::DocumentId::from_uuid(id.as_uuid());
        let worker_name = name.clone();
        let worker_destination = destination.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-save-file".into())
            .spawn(move || {
                let result = workspace_file_bytes(&segment, root.as_deref(), store_ws, store_id)
                    .and_then(|bytes| {
                        std::fs::write(&worker_destination, bytes).map_err(|e| e.to_string())
                    });
                let _ = tx.send(ImportMessage::FileSaved {
                    name: worker_name,
                    destination: worker_destination,
                    result,
                });
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.report(
                ErrorReport::new("Couldn't start saving the file", "Nothing was written.")
                    .with_details(error),
            );
        }
    }

    /// Save a drawn diagram as the picture on screen, as a PNG or an SVG.
    ///
    /// The picker stays on the event thread, like every save here. **The
    /// typed extension decides the format** (`crate::diagram::save_target`):
    /// the SVG comes from the painter's twin and needs no GPU; the PNG is
    /// rasterised at `SAVE_SCALE` on **this window's device, on the loop
    /// thread** — accepted for a one-off the person has just confirmed in a
    /// dialog, since a drawing-sized texture renders in a few milliseconds
    /// and the renderer it uses is the one the window already holds. The PNG
    /// encode and the atomic write go to the save worker, and the result is
    /// the same *Saved “name”.* notice, or the same error, a workspace file's
    /// save shows. The folder is remembered as every export's is.
    fn save_diagram(&mut self, source: WindowId, request: crate::SaveDiagram) {
        let Some(chosen) = crate::dialogs::pick_save_file(
            koru_i18n::t(koru_i18n::current(), koru_i18n::Key::MenuSaveAs),
            &crate::diagram::default_file_name(&request.title),
            crate::diagram::SAVE_FILTERS,
            self.runtime.settings.last_export_dir.as_deref(),
            None,
        ) else {
            return;
        };
        let (destination, format) = crate::diagram::save_target(chosen);
        if let Some(dir) = destination.parent() {
            self.remember_export_dir(dir.to_path_buf());
        }
        let name = crate::diagram::file_name(&destination);
        let payload = match format {
            crate::diagram::SaveFormat::Svg => Ok(crate::diagram::SavePayload::Svg(
                koru_draw::diagram::svg(&request.prepared, theme::p()),
            )),
            crate::diagram::SaveFormat::Png => self
                .raster_diagram(source, &request.prepared)
                .map(crate::diagram::SavePayload::Rgba),
        };
        let payload = match payload {
            Ok(payload) => payload,
            Err(error) => {
                // No adapter, a lost device, a texture too large: the
                // existing file-error notice, with the reason in its details.
                eprintln!("[Koru][diagram] save failed: {error}");
                self.handle_import_message(ImportMessage::FileSaved {
                    name,
                    destination,
                    result: Err(error),
                });
                return;
            }
        };
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let worker_destination = destination.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-save-file".into())
            .spawn(move || {
                let result = payload.encode().and_then(|bytes| {
                    koru_export_markdown::write_bytes_atomic(&worker_destination, &bytes)
                        .map_err(|error| error.to_string())
                });
                if let Err(error) = &result {
                    eprintln!("[Koru][diagram] save failed: {error}");
                }
                let _ = tx.send(ImportMessage::FileSaved {
                    name,
                    destination: worker_destination,
                    result,
                });
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.report(
                ErrorReport::new("Couldn't start saving the file", "Nothing was written.")
                    .with_details(error),
            );
        }
    }

    /// Put a picture of a drawn diagram on the system clipboard: the same
    /// 2× raster a saved PNG is, as a bitmap, so it pastes into a slide or a
    /// chat as crisply as the file would. No dialog and no notice — a copy
    /// is silent, as an image block's is; a raster that fails is logged and
    /// leaves the clipboard as it was.
    fn copy_diagram(&mut self, source: WindowId, request: &crate::CopyDiagram) {
        match self.raster_diagram(source, &request.prepared) {
            Ok(rgba) => {
                let image = koru_clipboard::RgbaBitmap {
                    width: rgba.width as usize,
                    height: rgba.height as usize,
                    bytes: rgba.bytes,
                };
                if let Err(error) = koru_clipboard::system().set_bitmap(&image) {
                    eprintln!("[Koru][diagram] copy failed: {error}");
                }
            }
            Err(error) => eprintln!("[Koru][diagram] copy failed: {error}"),
        }
    }

    /// The pixels of `prepared` at `SAVE_SCALE` over the page color, drawn
    /// with the renderer `source` draws with. A window whose device is gone
    /// renders through a fresh renderer on the context's device instead.
    fn raster_diagram(
        &mut self,
        source: WindowId,
        prepared: &koru_draw::diagram::PreparedDiagram,
    ) -> Result<koru_window::offscreen::Rgba, String> {
        let palette = theme::p();
        let scale = crate::diagram::SAVE_SCALE;
        let size = prepared.natural_size();
        let width = (size.width * scale).ceil() as u32;
        let height = (size.height * scale).ceil() as u32;
        let mut scene = Scene::new();
        koru_draw::diagram::paint(prepared, palette, Affine::scale(scale), 1.0, &mut scene);
        let dev_id = self.panes.get(&source).map(|pane| pane.surface.dev_id);
        let rendered = match dev_id.and_then(|dev_id| {
            self.renderers
                .get_mut(&dev_id)
                .map(|renderer| (dev_id, renderer))
        }) {
            Some((dev_id, renderer)) => koru_window::offscreen::raster_with(
                renderer,
                &self.context.devices[dev_id],
                &scene,
                width,
                height,
                palette.bg,
            ),
            None => {
                koru_window::offscreen::raster(&mut self.context, &scene, width, height, palette.bg)
            }
        };
        rendered.map_err(|error| error.to_string())
    }

    /// Save a selection into one folder, retaining each stored file name.
    fn save_files(&mut self, ids: Vec<koru_core::DocumentId>) {
        if ids.len() == 1 {
            self.save_file(ids[0]);
            return;
        }
        let Some(workspace_id) = self.runtime.workspace.current() else {
            return;
        };
        let entries = self.runtime.workspace.entries();
        let files: Vec<_> = ids
            .into_iter()
            .filter_map(|id| {
                entries
                    .iter()
                    .find(|entry| entry.id == id && entry.kind == koru_core::explore::KIND_FILE)
                    .map(|entry| (id, entry.title.clone()))
            })
            .collect();
        if files.is_empty() {
            self.report(ErrorReport::new(
                "Files not found",
                "They may have been removed in another window.",
            ));
            return;
        }
        let Some(destination) = crate::dialogs::pick_folder(
            "Save workspace files to folder",
            self.runtime.settings.last_export_dir.as_deref(),
            self.primary_window(),
        ) else {
            return;
        };
        self.remember_export_dir(destination.clone());
        let count = files.len();
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let store_ws = koru_store_core::ids::WorkspaceId::from_uuid(workspace_id.as_uuid());
        let worker_destination = destination.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-save-files".into())
            .spawn(move || {
                let mut failures = Vec::new();
                for (id, name) in files {
                    let store_id = koru_store_core::ids::DocumentId::from_uuid(id.as_uuid());
                    let safe_name = std::path::Path::new(&name)
                        .file_name()
                        .and_then(|name| name.to_str())
                        .filter(|name| !name.is_empty())
                        .unwrap_or("file");
                    let path = koru_export::unique_path(&worker_destination, safe_name);
                    let result =
                        workspace_file_bytes(&segment, root.as_deref(), store_ws, store_id)
                            .and_then(|bytes| {
                                std::fs::write(path, bytes).map_err(|e| e.to_string())
                            });
                    if let Err(error) = result {
                        failures.push(format!("{name}: {error}"));
                    }
                }
                let result = if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures.join("\n"))
                };
                let _ = tx.send(ImportMessage::FilesSaved {
                    count,
                    destination: worker_destination,
                    result,
                });
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.report(
                ErrorReport::new("Couldn't start saving the files", "Nothing was written.")
                    .with_details(error),
            );
        }
    }

    /// Open an opaque workspace file through its OS association. The Store read
    /// and disk write stay off the event thread; the cache keeps one bounded
    /// copy per stable file id rather than leaking a new temp file per click.
    fn open_file(&mut self, id: koru_core::DocumentId) {
        crate::attach_debug!("shell open_file id={}", id.to_storage());
        let Some(workspace_id) = self.runtime.workspace.current() else {
            crate::attach_debug!("shell open_file abort: no current workspace");
            return;
        };
        // Prefer a file-kind entry; fall back to any entry with this id so an
        // attachment whose manifest row is still settling can still open.
        let entries = self.runtime.workspace.entries();
        let listed = entries
            .iter()
            .find(|entry| entry.id == id && entry.kind == koru_core::explore::KIND_FILE)
            .or_else(|| entries.iter().find(|entry| entry.id == id));
        let Some((name, mime)) = listed.map(|entry| (entry.title.clone(), entry.mime.clone()))
        else {
            crate::attach_debug!(
                "shell open_file abort: id not in entries (n={})",
                entries.len()
            );
            self.report(ErrorReport::new(
                "File not found",
                "It may have been removed in another window.",
            ));
            return;
        };
        crate::attach_debug!(
            "shell open_file spawning worker name={name} workspace={}",
            workspace_id.to_storage()
        );
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let store_ws = koru_store_core::ids::WorkspaceId::from_uuid(workspace_id.as_uuid());
        let store_id = koru_store_core::ids::DocumentId::from_uuid(id.as_uuid());
        let worker_name = name.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-open-file".into())
            .spawn(move || {
                let result = (|| {
                    let bytes =
                        workspace_file_bytes(&segment, root.as_deref(), store_ws, store_id)?;
                    crate::attach_debug!(
                        "shell open_file worker got {} bytes for {worker_name}",
                        bytes.len()
                    );
                    open_file_bytes(store_id, &worker_name, mime.as_deref(), &bytes)
                })();
                if let Err(ref e) = result {
                    crate::attach_debug!("shell open_file worker failed: {e}");
                } else {
                    crate::attach_debug!("shell open_file worker ok for {worker_name}");
                }
                let _ = tx.send(ImportMessage::FileOpened {
                    name: worker_name,
                    result,
                });
                let _ = proxy.send_event(Wake::Import);
            })
        {
            crate::attach_debug!("shell open_file spawn failed: {error}");
            self.report(
                ErrorReport::new("Couldn't start opening the file", "Nothing was changed.")
                    .with_details(error),
            );
        }
    }

    /// Open the one note attached to a file, creating it first when the file
    /// has none. The store answers with the note's id either way
    /// (`ensure_file_note` is idempotent); opening it is the ordinary document
    /// navigation, with the note's empty document seeded on first open.
    fn open_file_note(
        &mut self,
        event_loop: &ActiveEventLoop,
        source: WindowId,
        id: koru_core::DocumentId,
    ) {
        match self.runtime.workspace.ensure_file_note(id) {
            Ok(note) => {
                // The manifest changed on the store a moment ago and this
                // window's copy arrived over its room; push what is known now
                // rather than wait for the next tick, so the row's note icon
                // appears with the tab rather than after it.
                self.push_entries();
                self.navigate(
                    event_loop,
                    source,
                    crate::NavigationTarget::Document {
                        document: note,
                        node: None,
                    },
                    None,
                    SearchTargetDisposition::CurrentWindow,
                );
            }
            Err(error) => {
                self.report(
                    ErrorReport::new("Couldn't open the note", "Nothing was changed.")
                        .with_details(error),
                );
            }
        }
    }

    /// Fetch one file's bytes from whichever peer has them.
    ///
    /// The whole transfer is one `get_blob`: the Store answers from disk when it
    /// can, and hydrates from a peer when it cannot. Nothing here needs to know
    /// which happened — that is the point of the call. It goes on a worker
    /// because the wait is unbounded by design: hydration parks without a timer
    /// until an authorized peer is reachable, and the event thread may not.
    fn download_file(&mut self, id: koru_core::DocumentId) {
        let Some(workspace_id) = self.runtime.workspace.current() else {
            return;
        };
        let name = self
            .runtime
            .workspace
            .entries()
            .iter()
            .find(|entry| entry.id == id && entry.kind == koru_core::explore::KIND_FILE)
            .map(|entry| entry.title.clone());
        let Some(name) = name else {
            self.report(ErrorReport::new(
                "File not found",
                "It may have been removed in another window.",
            ));
            return;
        };
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let store_ws = koru_store_core::ids::WorkspaceId::from_uuid(workspace_id.as_uuid());
        let store_id = koru_store_core::ids::DocumentId::from_uuid(id.as_uuid());
        let worker_name = name.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-download-file".into())
            .spawn(move || {
                let result = workspace_file_bytes(&segment, root.as_deref(), store_ws, store_id)
                    // The bytes are durable in the local Store by the time this
                    // returns, so they are deliberately dropped here rather than
                    // carried back to the event thread.
                    .map(|_| ());
                let _ = tx.send(ImportMessage::FileDownloaded {
                    id,
                    name: worker_name,
                    result,
                });
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.report(
                ErrorReport::new(
                    "Couldn't start downloading the file",
                    "Nothing was changed.",
                )
                .with_details(error),
            );
        }
    }

    /// Ask which of this workspace's files are not held on this device.
    ///
    /// Two reads: each file document's blob hash, and the local blob list. Both
    /// go on a worker — the first is one Store call per file — and the answer
    /// arrives as a message, so the draw path never asks this question.
    fn refresh_file_locality(&mut self) {
        let Some(workspace_id) = self.runtime.workspace.current() else {
            return;
        };
        let files: Vec<koru_core::DocumentId> = self
            .runtime
            .workspace
            .entries()
            .iter()
            .filter(|entry| {
                entry.kind == koru_core::explore::KIND_FILE && entry.trashed_at.is_none()
            })
            .map(|entry| entry.id)
            .collect();
        if files.is_empty() {
            self.apply_file_locality(workspace_id, Vec::new());
            return;
        }
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let store_ws = koru_store_core::ids::WorkspaceId::from_uuid(workspace_id.as_uuid());
        let _ = std::thread::Builder::new()
            .name("koru-file-locality".into())
            .spawn(move || {
                let Ok(remote) =
                    workspace_remote_files(&segment, root.as_deref(), store_ws, &files)
                else {
                    // A sweep that cannot run leaves the last answer standing.
                    // Guessing "everything is remote" would turn the whole list
                    // into placeholders because the Store was busy.
                    return;
                };
                let _ = tx.send(ImportMessage::FileLocality {
                    workspace: workspace_id,
                    remote,
                });
                let _ = proxy.send_event(Wake::Import);
            });
    }

    /// Hand a locality answer to every pane.
    ///
    /// A stale answer is dropped rather than shown: the sweep runs on a worker
    /// and the person may have switched workspaces while it was out, and there
    /// is no overlap between two workspaces' document ids to make a wrong answer
    /// merely partial.
    fn apply_file_locality(
        &mut self,
        workspace: koru_core::WorkspaceId,
        remote: Vec<koru_core::DocumentId>,
    ) {
        if self.runtime.workspace.current() != Some(workspace) {
            return;
        }
        let mut changed = false;
        for pane in self.panes.values_mut() {
            if pane.view.set_remote_files(remote.clone()) {
                changed = true;
                pane.window.request_redraw();
            }
        }
        if changed {
            self.invalidate_everywhere();
        }
    }

    /// Open every selected file through the associated OS application.
    fn open_files(&mut self, ids: Vec<koru_core::DocumentId>) {
        if ids.len() == 1 {
            self.open_file(ids[0]);
            return;
        }
        let Some(workspace_id) = self.runtime.workspace.current() else {
            return;
        };
        let entries = self.runtime.workspace.entries();
        let files: Vec<_> = ids
            .into_iter()
            .filter_map(|id| {
                entries
                    .iter()
                    .find(|entry| entry.id == id && entry.kind == koru_core::explore::KIND_FILE)
                    .map(|entry| (id, entry.title.clone(), entry.mime.clone()))
            })
            .collect();
        if files.is_empty() {
            self.report(ErrorReport::new(
                "Files not found",
                "They may have been removed in another window.",
            ));
            return;
        }
        let count = files.len();
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let store_ws = koru_store_core::ids::WorkspaceId::from_uuid(workspace_id.as_uuid());
        if let Err(error) = std::thread::Builder::new()
            .name("koru-open-files".into())
            .spawn(move || {
                let mut failures = Vec::new();
                for (id, name, mime) in files {
                    let store_id = koru_store_core::ids::DocumentId::from_uuid(id.as_uuid());
                    let result = (|| {
                        let bytes =
                            workspace_file_bytes(&segment, root.as_deref(), store_ws, store_id)?;
                        open_file_bytes(store_id, &name, mime.as_deref(), &bytes)
                    })();
                    if let Err(error) = result {
                        failures.push(format!("{name}: {error}"));
                    }
                }
                let result = if failures.is_empty() {
                    Ok(())
                } else {
                    Err(failures.join("\n"))
                };
                let _ = tx.send(ImportMessage::FilesOpened { count, result });
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.report(
                ErrorReport::new("Couldn't start opening the files", "Nothing was changed.")
                    .with_details(error),
            );
        }
    }

    /// Export selected documents through a registered format exporter.
    fn export_documents(&mut self, ids: Vec<koru_core::DocumentId>, format: &'static str) {
        if ids.is_empty() {
            return;
        }
        let Some(workspace_id) = self.runtime.workspace.current() else {
            self.report(ErrorReport::new(
                "Nothing to export",
                "Open a workspace first.",
            ));
            return;
        };

        let entries = self.runtime.workspace.entries();
        let titles: Vec<(koru_core::DocumentId, String)> = ids
            .iter()
            .map(|id| {
                let title = entries
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| e.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| "Untitled".into());
                (*id, title)
            })
            .collect();

        let registry = match koru_export::ExportRegistry::new(crate::formats::exporters()) {
            Ok(registry) => registry,
            Err(error) => {
                self.report(
                    ErrorReport::new("Couldn't start the export", "Nothing was written.")
                        .with_details(error),
                );
                return;
            }
        };
        let Some(exporter) = registry.into_exporter(format) else {
            self.report(ErrorReport::new(
                "Unknown export format",
                format!("No exporter is registered for \u{201c}{format}\u{201d}."),
            ));
            return;
        };

        let start = self.runtime.settings.last_export_dir.as_deref();
        let parent = self.primary_window();
        let extensions = exporter.descriptor().extensions;
        let destination = if titles.len() == 1 {
            let suggested = koru_export::suggested_file_name(&titles[0].1, extensions);
            crate::dialogs::pick_save_file(
                &format!("Export as {}", exporter.descriptor().name),
                &suggested,
                &[crate::dialogs::FileFilter {
                    name: exporter.descriptor().name,
                    extensions,
                }],
                start,
                parent,
            )
        } else {
            crate::dialogs::pick_folder("Export documents to folder", start, parent)
        };
        let Some(destination) = destination else {
            return;
        };

        let name = titles
            .first()
            .map(|(_, t)| t.clone())
            .unwrap_or_else(|| "document".into());
        let count = titles.len();
        // Prefer the live open document so typing that has not reached the
        // store yet still exports. Closed docs load from the store on the
        // worker. Snapshots are taken here so the worker never borrows `self`.
        let mut prepared: Vec<(koru_core::DocumentId, String, Option<Vec<u8>>)> =
            Vec::with_capacity(titles.len());
        for (id, title) in &titles {
            let live = self
                .runtime
                .workspace
                .document(*id)
                .and_then(|doc| doc.export_snapshot().ok());
            prepared.push((*id, title.clone(), live));
        }

        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let root = self.gateway_root.clone();
        let store_ws = koru_store_core::ids::WorkspaceId::from_uuid(workspace_id.as_uuid());
        let thread_dest = destination.clone();
        let thread_ids = ids.clone();

        if let Err(error) = std::thread::Builder::new()
            .name("koru-export-documents".into())
            .spawn(move || {
                let result = (|| {
                    let client =
                        client_at_segment(&segment, root.as_deref()).map_err(|e| e.to_string())?;
                    let mut documents = Vec::with_capacity(prepared.len());
                    for (id, title, live) in &prepared {
                        let bytes = match live {
                            Some(b) => b.clone(),
                            None => {
                                let store_doc =
                                    koru_store_core::ids::DocumentId::from_uuid(id.as_uuid());
                                client
                                    .snapshot(store_ws, store_doc)
                                    .map_err(|e| format!("{title}: {e}"))?
                            }
                        };
                        let doc = koru_core::schema::WorkspaceDoc::from_snapshot(&bytes)
                            .map_err(|e| format!("{title}: {e}"))?;
                        documents.push(koru_export::ExportDocument {
                            id: *id,
                            title: title.clone(),
                            doc,
                        });
                    }
                    let blobs = StoreBlobSource {
                        client,
                        workspace: store_ws,
                    };
                    let exported = exporter
                        .export(&documents, &thread_dest, &blobs, &koru_export::NoProgress)
                        .map_err(|e| e.to_string())?;
                    Ok::<_, String>(exported)
                })();
                let (remembered_dir, result) = match result {
                    Ok(exported) => (Some(exported.remembered_dir), Ok(())),
                    Err(e) => (None, Err(e)),
                };
                let _ = tx.send(ImportMessage::DocumentsExported {
                    count,
                    name,
                    destination: thread_dest,
                    remembered_dir,
                    result,
                    format,
                    ids: thread_ids,
                });
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.report(
                ErrorReport::new(
                    "Couldn't start the export",
                    "Nothing was written and the workspace was not changed.",
                )
                .with_details(error),
            );
        }
    }

    /// Drain work completed off the event-loop thread.
    ///
    /// `Wake::Import` only ends winit's wait. Dialogs and workspace swaps run
    /// here, from `about_to_wait`, alongside every other deferred UI action.
    fn drain_import_messages(&mut self) {
        let messages: Vec<_> = self.import_rx.try_iter().collect();
        for message in messages {
            self.handle_import_message(message);
        }
    }

    fn confirm_import(&mut self, inspected: InspectedImport) {
        let preview = inspected.prepared.preview();
        let mut rows = Vec::new();
        fn append(
            nodes: &[koru_import::PreviewNode],
            depth: usize,
            out: &mut Vec<crate::ImportPreviewRow>,
        ) {
            for node in nodes {
                out.push(crate::ImportPreviewRow {
                    depth,
                    title: node.title.clone(),
                    nodes: node.nodes,
                    trashed: node.trashed,
                });
                append(&node.children, depth + 1, out);
            }
        }
        append(&preview.documents, 0, &mut rows);
        let modal = crate::ImportPreview {
            workspace_name: preview.workspace_name.clone(),
            document_count: preview.document_count,
            node_count: preview.node_count,
            image_count: preview.image_count,
            warnings: preview.warnings.clone(),
            rows,
            identity_conflict: inspected.identity_conflict,
        };
        // For the completion event: the apply's result message carries the
        // workspace alone, and this is the last moment the count is in hand.
        self.import_workspace_count = Some(preview.document_count);
        let accepted = self
            .import_source
            .and_then(|id| self.panes.get_mut(&id))
            .is_some_and(|pane| {
                let accepted = pane.view.show_import_preview(modal);
                if accepted {
                    pane.window.request_redraw();
                }
                accepted
            });
        if !accepted {
            self.import_busy = false;
            self.import_source = None;
            self.report(ErrorReport::new(
                "The import preview could not be shown",
                "Nothing was imported.",
            ));
            return;
        }
        self.pending_import = Some(inspected);
    }

    fn finish_workspace_import(&mut self, source: WindowId, choice: crate::ImportChoice) {
        if self.import_source != Some(source) {
            return;
        }
        if choice == crate::ImportChoice::Cancel {
            self.pending_import = None;
            self.import_busy = false;
            self.import_source = None;
            return;
        }
        let Some(inspected) = self.pending_import.take() else {
            return;
        };
        let identity = match choice {
            crate::ImportChoice::KeepIdentity if !inspected.identity_conflict => {
                koru_import::IdentityMode::Keep
            }
            crate::ImportChoice::Copy => koru_import::IdentityMode::Copy,
            crate::ImportChoice::KeepIdentity | crate::ImportChoice::Cancel => {
                self.import_busy = false;
                self.import_source = None;
                return;
            }
        };
        let Some(owner) = self.runtime.profile.id() else {
            self.import_busy = false;
            self.import_source = None;
            self.report(ErrorReport::new(
                "The workspace could not be imported",
                "This profile has no identity.",
            ));
            return;
        };
        let owner = koru_store_core::ids::ProfileId::from_uuid(owner.as_uuid());
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-import-build".into())
            .spawn(move || {
                let result = inspected
                    .prepared
                    .build(
                        koru_import::ImportOptions {
                            identity,
                            accept_warnings: true,
                        },
                        &koru_import::ImportContext {
                            target_profile: None,
                            target_workspace: None,
                            occupied_titles: &std::collections::BTreeSet::new(),
                            blobs: &koru_import::NoBlobSink,
                            tags: &koru_import::NoTagSink,
                        },
                        &koru_import::NoProgress,
                    )
                    .map_err(|e| e.to_string())
                    .and_then(|product| {
                        let koru_import::ImportProduct::Workspace(bundle) = product else {
                            return Err("the legacy importer did not produce a workspace".into());
                        };
                        let client = client_at(root.as_deref()).map_err(|e| e.to_string())?;
                        let mode = match identity {
                            koru_import::IdentityMode::Keep => {
                                koru_store_core::bundle::ImportMode::Restore
                            }
                            koru_import::IdentityMode::Copy => {
                                koru_store_core::bundle::ImportMode::Duplicate
                            }
                        };
                        client
                            .import_workspace(&bundle, mode, owner)
                            .map_err(|e| e.to_string())
                    });
                let _ = tx.send(ImportMessage::Imported(result));
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.import_busy = false;
            self.import_source = None;
            self.report(
                ErrorReport::new("Couldn't start the import", "Nothing was changed.")
                    .with_details(error),
            );
        }
    }

    /// Ask the Store once for every workspace's indexed count and live bytes,
    /// then hand the answers to whichever pane wants them.
    ///
    /// **Only when asked, and one batch.** Closed workspaces are measured from
    /// their SQL projection without decoding a manifest or retaining it as
    /// open. The byte total is the deduplicated blob set referenced by live
    /// documents, so Trash and retained recovery data do not inflate it.
    fn push_workspace_stats(&mut self) {
        let now = koru_workspace::open::now_ms();
        let Some(profile) = self.runtime.profile.id() else {
            eprintln!("[Koru][workspace-usage] current profile has no Store id");
            return;
        };
        let store_profile = koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid());
        let usage = match self
            .store_client()
            .and_then(|client| client.workspace_usage(store_profile))
        {
            Ok(rows) => rows
                .into_iter()
                .map(|row| (row.workspace.as_uuid(), row))
                .collect::<HashMap<_, _>>(),
            Err(error) => {
                eprintln!("[Koru][workspace-usage] refresh failed: {error}");
                return;
            }
        };
        let stats: Vec<WorkspaceStat> = self
            .runtime
            .profile
            .workspaces()
            .into_iter()
            .filter_map(|w| {
                let measured = usage.get(&w.id.as_uuid())?;
                let dir = self
                    .runtime
                    .profile_dir
                    .join("workspaces")
                    .join(w.id.to_storage());
                // Only the open workspace can say anything about sync — it is
                // the one with a live `DocStore`. Every other row is `Local`,
                // which is the truth rather than a guess.
                let sync = if w.id == self.runtime.current {
                    match &self.runtime.workspace.session {
                        Some(s) if !s.damaged().is_empty() => SyncState::Damaged,
                        Some(s) if s.peer_live() => SyncState::Live,
                        _ => SyncState::Local,
                    }
                } else if self.workspace_is_live_elsewhere(w.id) {
                    SyncState::Live
                } else {
                    SyncState::Local
                };
                Some(WorkspaceStat {
                    id: w.id,
                    live_bytes: measured.stored_bytes,
                    // Store backups are whole-Store snapshots rather than a
                    // per-workspace folder, so no honest row-level split exists.
                    backup_bytes: 0,
                    docs: i64::try_from(document_count(&measured.kinds, koru_features::canvas()))
                        .ok(),
                    files: i64::try_from(
                        measured
                            .kinds
                            .get(koru_core::explore::KIND_FILE)
                            .copied()
                            .unwrap_or(0),
                    )
                    .ok(),
                    sync,
                    last_opened_ms: self.runtime.settings.opened_in(w.id),
                    now_ms: Some(now),
                    folder: dir.display().to_string(),
                    search: self
                        .runtime
                        .profile
                        .id()
                        .and_then(|profile| self.host.workspace_search_stat(profile, w.id)),
                })
            })
            .collect();
        for pane in self.panes.values_mut() {
            if pane.view.set_workspace_stats(stats.clone()) {
                pane.window.request_redraw();
            }
        }
    }

    /// Say so, **once**, if this workspace has stopped saving.
    ///
    /// The failure this closes is the quiet one: a workspace whose folder has
    /// gone — unplugged, renamed, or moved out from under this window by a
    /// delete in another — goes on accepting keystrokes and writing nothing,
    /// and says so only on a stderr nobody is reading. There is no way to
    /// recover automatically, so the honest thing is to say it plainly and let
    /// somebody decide.
    ///
    /// Latched so it is said once rather than every tick; cleared if a save
    /// lands again, so an unplugged drive that comes back reports again if it
    /// goes a second time.
    /// Say when work this machine had kept could not be put back.
    ///
    /// **Never silent, and never fatal.** The buffer is still on the disk — the
    /// point of refusing to import a delta that would not apply is that it stays
    /// recoverable — so this is a warning about content that is *not* in the
    /// window rather than a failure of the window. Somebody who is told can go
    /// and get it; somebody who is not will assume they never typed it.
    fn report_replay_trouble(&mut self) {
        let trouble = self.runtime.workspace.take_replay_trouble();
        if trouble.is_empty() {
            return;
        }
        self.report(
            ErrorReport::new(
                "Some changes saved on this computer could not be restored",
                "They were kept on this computer while the Koru Store was unavailable, and the \
                 store's copy of the document does not reach back far enough to accept them. \
                 Nothing was deleted — they are still here, and they are not in this window.",
            )
            .with_details(trouble.join("\n")),
        );
    }

    fn report_write_failure(&mut self) {
        match self.runtime.workspace.write_failed() {
            Some(e) if !self.reported_write_failure => {
                self.reported_write_failure = true;
                let details = e.to_string();
                let dir = self
                    .runtime
                    .workspace
                    .session
                    .as_ref()
                    .map(|s| s.root().to_path_buf());
                let mut report = ErrorReport::new(
                    "Koru has stopped saving this workspace",
                    "Its folder cannot be written to. Everything you have typed is still here \
                     in this window, but it is not being saved — copy anything you cannot lose \
                     somewhere else before closing it.",
                )
                .with_details(details);
                if let Some(dir) = dir {
                    report = report.with_action(ErrorAction::Reveal(dir));
                }
                self.report(report);
            }
            None => self.reported_write_failure = false,
            _ => {}
        }
    }

    /// Put an error in front of the user.
    ///
    /// Through the pane when there is one, because it can say more and offer a
    /// way out; a native dialog only when there is not — which today means a
    /// pane whose view does not implement `report_error`. **Never an
    /// `eprintln!`**: there is no console under `windows_subsystem`, so that is
    /// the same as saying nothing.
    fn report(&mut self, error: ErrorReport) {
        for pane in self.panes.values_mut() {
            if pane.view.report_error(error.clone()) {
                pane.window.request_redraw();
                return;
            }
        }
        startup::warn(&format!("{}\n\n{}", error.title, error.body));
    }

    /// Put an error in the pane that initiated an external route and bring that
    /// pane to the foreground.
    ///
    /// Navigation can arrive from Koru Agent while the workspace window is
    /// hidden or behind another application. Merely attaching a modal to that
    /// window leaves the person looking at the agent with no explanation. A
    /// dedicated search pane may decline rich errors, so the primary container
    /// is the explicit second choice rather than HashMap iteration order.
    fn report_and_foreground(&mut self, preferred: WindowId, error: ErrorReport) {
        let mut targets = vec![preferred];
        if let Some(primary) = self.primary
            && !targets.contains(&primary)
        {
            targets.push(primary);
        }
        let others: Vec<_> = self
            .panes
            .keys()
            .copied()
            .filter(|id| !targets.contains(id))
            .collect();
        targets.extend(others);
        for id in targets {
            let accepted = self
                .panes
                .get_mut(&id)
                .is_some_and(|pane| pane.view.report_error(error.clone()));
            if accepted {
                self.foreground_pane(id);
                return;
            }
        }
        startup::warn(&format!("{}\n\n{}", error.title, error.body));
    }

    fn foreground_pane(&mut self, id: WindowId) {
        if self.in_tray {
            self.show_from_tray();
        }
        if let Some(pane) = self.panes.get(&id) {
            pane.window.set_visible(true);
            pane.window.focus_window();
            pane.window.request_redraw();
        }
        activate_app();
    }

    fn notice(&mut self, message: String) {
        for pane in self.panes.values_mut() {
            if pane.view.show_notice(message.clone()) {
                pane.window.request_redraw();
                return;
            }
        }
    }

    fn begin_import_progress(&mut self, source: WindowId, message: String) {
        if let Some(pane) = self.panes.get_mut(&source)
            && pane
                .view
                .show_import_progress(message.clone(), crate::ImportNoticeProgress::Indeterminate)
        {
            pane.window.request_redraw();
            return;
        }
        self.notice(message);
    }

    fn cancel_content_import(&mut self) {
        let Some(token) = self.import_cancel.as_ref() else {
            return;
        };
        if token.is_cancelled() {
            return;
        }
        token.cancel();
        self.import_cancel_hide_due = Some(Instant::now() + IMPORT_CANCEL_HOLD);
        println!("[Koru][import] cancellation requested");
        if let Some(source) = self.import_source
            && let Some(pane) = self.panes.get_mut(&source)
            && pane
                .view
                .cancel_import_progress("Importing content\u{2026} Canceling".into())
        {
            pane.window.request_redraw();
        }
    }

    fn maintain_content_import_cancellation(&mut self, now: Instant) {
        let Some(due) = self.import_cancel_hide_due else {
            return;
        };
        if now < due {
            return;
        }
        self.import_cancel_hide_due = None;
        self.clear_import_progress();
        if self.import_cancel_worker_done {
            self.finish_content_import_cancellation();
        }
    }

    fn finish_content_import_cancellation(&mut self) {
        self.import_busy = false;
        self.import_source = None;
        self.import_cancel = None;
        self.import_cancel_worker_done = false;
        self.finish_pending_attachment(true);
    }

    /// Clear a pending `/attach`. When `cancelled`, restore the trigger row so
    /// the literal `/attach` text is not left behind.
    fn finish_pending_attachment(&mut self, cancelled: bool) {
        let Some((window, node)) = self.pending_attachment.take() else {
            return;
        };
        if cancelled && let Some(pane) = self.panes.get_mut(&window) {
            pane.view
                .attachment_cancelled(&mut self.runtime.workspace.ws, node);
        }
    }

    fn update_content_import_progress(
        &mut self,
        stage: koru_import::ImportStage,
        completed: usize,
        total: Option<usize>,
    ) {
        let Some(source) = self.import_source else {
            return;
        };
        let stage = match stage {
            koru_import::ImportStage::Inspecting => "Inspecting",
            koru_import::ImportStage::Converting => "Converting",
            koru_import::ImportStage::ReadingMedia => "Reading media",
            koru_import::ImportStage::BuildingBundle => "Finishing",
        };
        let (message, progress) = match total.filter(|total| *total > 0) {
            Some(total) => {
                let fraction = completed.min(total) as f32 / total as f32;
                let percent = (fraction * 100.0).round() as u8;
                (
                    format!("Importing content… {stage} {percent}%"),
                    crate::ImportNoticeProgress::Determinate(fraction),
                )
            }
            None => (
                format!("Importing content… {stage}"),
                crate::ImportNoticeProgress::Indeterminate,
            ),
        };
        if let Some(pane) = self.panes.get_mut(&source)
            && pane.view.update_import_progress(message, progress)
        {
            pane.window.request_redraw();
        }
    }

    fn clear_import_progress(&mut self) {
        for pane in self.panes.values_mut() {
            if pane.view.clear_import_progress() {
                pane.window.request_redraw();
            }
        }
    }

    fn notice_import(&mut self, message: String, target: crate::ImportNoticeTarget) {
        for pane in self.panes.values_mut() {
            if pane.view.show_import_notice(message.clone(), target) {
                pane.window.request_redraw();
                return;
            }
        }
    }

    /// Notice with a **Show** action that reveals `path` in Explorer / Finder.
    /// Falls back to a plain notice where the platform cannot reveal a path.
    fn notice_reveal(&mut self, message: String, path: PathBuf) {
        if !crate::reveal::is_supported() {
            self.notice(message);
            return;
        }
        for pane in self.panes.values_mut() {
            if pane.view.show_notice_reveal(message.clone(), path.clone()) {
                pane.window.request_redraw();
                return;
            }
        }
    }

    /// Move this window away before its current workspace becomes hidden.
    /// With no neighbour, the app remains useful in its local welcome state.
    fn leave_current_workspace(&mut self, id: WorkspaceId) -> Result<bool, String> {
        if !self.runtime.live_workspace || self.runtime.current != id {
            return Ok(false);
        }
        if let Some(next) = self.runtime.replacement_workspace(id) {
            self.switch_workspace(next);
            if self.runtime.live_workspace && self.runtime.current == id {
                return Err("Koru could not open the replacement workspace".into());
            }
        } else {
            self.show_empty_workspace()?;
        }
        Ok(true)
    }

    /// Give a workspace another name.
    ///
    /// **The profile entry is the authority, and the document follows it.**
    /// `Session::open` passes a name in but only uses it on `Opened::Fresh`, so
    /// a profile-side rename is not clobbered on the next open — and the
    /// workspace document's own `meta.name` is reconciled from the entry when
    /// that workspace is opened (see `switch_workspace` and `open_profile`).
    /// Here the document is written **only when it is the one this window has
    /// open**, because renaming any other would mean opening a second `Session`
    /// on a `snapshot.loro` another process may be writing.
    /// Tell every pane to show whatever document is now active.
    ///
    /// The shell owns which document is open; a *tab* is the view's. Swapping
    /// one without this changed the sidebar and left the window on the previous
    /// document, which reads as the click having done nothing.
    fn show_active_document(&mut self) {
        // Handed over with the document, not left to arrive separately: a pane
        // deciding whether this document is already in a tab needs to know
        // *which* document it is, and it used to be told afterwards.
        let active = self.runtime.workspace.active();
        let ws = &self.runtime.workspace.ws;
        for pane in self.panes.iter_mut() {
            let _ = pane.1.view.show_active_document(ws, active);
        }
    }

    /// Let go of a document no tab is showing any more.
    ///
    /// **A tab is what keeps a document open**, and the workspace has no way to
    /// know one closed. Under the store an open document is a `LoroDoc`, a room
    /// and a socket thread, so never doing this leaks one of each per document
    /// ever opened — restoring ten tabs and switching workspace twice would be
    /// thirty threads.
    ///
    /// The *active* document is refused, because something has to be under the
    /// caret. It is not dropped on the floor: it is remembered and retried after
    /// the next open, which is the only moment it stops being active.
    fn release_document(&mut self, id: koru_core::DocumentId) {
        if !self.runtime.workspace.close_document(id) && self.runtime.workspace.active() == Some(id)
        {
            self.releasing.push(id);
        }
    }

    /// A document became the active one; retry whatever release was owed.
    ///
    /// **Reopening is what "still wanted" looks like**, so `id` leaves the owed
    /// list rather than being closed a moment later — a tab is showing it again.
    /// Everything else that is no longer active is let go here, which is the
    /// only moment it can be.
    fn opened_document(&mut self, id: koru_core::DocumentId) {
        self.releasing.retain(|&owed| owed != id);
        if self.releasing.is_empty() {
            return;
        }
        for owed in std::mem::take(&mut self.releasing) {
            if self.runtime.workspace.active() == Some(owed) {
                self.releasing.push(owed);
            } else {
                self.runtime.workspace.close_document(owed);
            }
        }
    }

    /// Create a document in the open workspace, and open it.
    ///
    /// **Below the page it was made from**, which is what *New document* means
    /// from inside one. The anchor is the workspace's active document — the one
    /// the tab that pressed Ctrl+T is over — and `None` (a welcome tab, an empty
    /// workspace) appends. This was lost in the store cutover: the folder-backed
    /// path went through `create_document_after` and the store-backed one
    /// registered a document with no position at all, so every new page landed
    /// at the bottom of the library.
    ///
    /// **`canvas` makes a canvas instead** (`PaneRequest::CreateCanvas`): the
    /// manifest kind `canvas`, seeded with its root alone, and placed at the
    /// workspace root rather than beside the active page — canvases are a flat
    /// list of their own. Everything after the creation is the same door: the
    /// listing is pushed before the document is announced, so the container
    /// already knows the new document is a canvas when it builds its tab.
    fn create_document(&mut self, source: WindowId, title: &str, canvas: bool) {
        let created = if canvas {
            self.runtime.workspace.create_canvas(title)
        } else {
            let after = self.runtime.workspace.active();
            self.runtime.workspace.create_document_after(title, after)
        };
        match created {
            Ok(id) => {
                if let Err(e) = self.runtime.workspace.open_document(id) {
                    self.report(
                        ErrorReport::new(
                            "Made the document, but couldn't open it",
                            "It is in the sidebar.",
                        )
                        .with_details(e),
                    );
                }
                self.opened_document(id);
                // Creation changed the manifest locally. `workspace.tick()`
                // reports imported changes, so waiting for it leaves Explore
                // and document-id routing on the old list until a workspace
                // switch rebuilds everything.
                self.push_entries();
                self.show_active_document();
                self.sync_pane_context();
                self.invalidate_everywhere();
            }
            Err(e) => {
                // The asking view holds the other half of this failure: a
                // draft's promotion latch. Told before the dialog so a person
                // who dismisses it and types again simply retries. A canvas is
                // made at once rather than promoted from a draft, so it holds
                // no latch to hand back.
                if !canvas && let Some(pane) = self.panes.get_mut(&source) {
                    pane.view.create_document_failed();
                }
                self.report(
                    ErrorReport::new("Couldn't create that document", "Nothing was changed.")
                        .with_details(e),
                );
            }
        }
    }

    /// Create from the entity picker, replace its `@query` with the real routed
    /// pill in the source document, then finish on the newly created page.
    fn create_linked_document(&mut self, source: WindowId, title: &str) {
        let origin = self.runtime.workspace.active();
        let profile = self.runtime.profile.id();
        let workspace = self.runtime.current;
        // Below the page whose text is about to link to it — the same rule
        // `create_document` follows, and here the anchor is not a guess: it is
        // the document the `@query` is in.
        match self.runtime.workspace.create_document_after(title, origin) {
            Ok(id) => {
                let mut linked = false;
                let mut link_error = None;
                if let (Some(origin), Some(profile)) = (origin, profile) {
                    match self.runtime.workspace.open_document(origin) {
                        Ok(()) => {
                            linked = self.panes.get_mut(&source).is_some_and(|pane| {
                                pane.view.complete_new_page_link(
                                    &mut self.runtime.workspace.ws,
                                    profile,
                                    workspace,
                                    id,
                                    title.to_owned(),
                                )
                            });
                            if linked {
                                self.runtime.workspace.touched();
                            }
                        }
                        Err(e) => link_error = Some(e),
                    }
                }
                if !linked {
                    let report = ErrorReport::new(
                        "Made the document, but couldn't add its link",
                        "The page is in the sidebar and the original text was left alone.",
                    );
                    self.report(match link_error {
                        Some(e) => report.with_details(e),
                        None => report,
                    });
                }
                if let Err(e) = self.runtime.workspace.open_document(id) {
                    self.report(
                        ErrorReport::new(
                            "Made the document, but couldn't open it",
                            "It is in the sidebar.",
                        )
                        .with_details(e),
                    );
                    self.push_entries();
                    self.sync_pane_context();
                    self.invalidate_everywhere();
                    return;
                }
                self.opened_document(id);
                self.push_entries();
                self.show_active_document();
                self.sync_pane_context();
                self.invalidate_everywhere();
            }
            Err(e) => self.report(
                ErrorReport::new(
                    "Couldn't create that document",
                    "The @ text was left alone.",
                )
                .with_details(e),
            ),
        }
    }

    /// The payload of a selection move, read out of the **active** document as
    /// cuttings the destination can plant. Taken before anything swaps the
    /// active document, which is the `copy_document` "cut first" lesson.
    #[allow(clippy::type_complexity)]
    fn selection_content(
        &self,
        payload: &MoveSelectionPayload,
    ) -> Result<
        (
            Vec<(koru_core::schema::Cutting, Option<koru_core::ListStyle>)>,
            Option<(Vec<koru_core::TextRun>, Vec<koru_core::node::Attr>)>,
        ),
        String,
    > {
        match payload {
            MoveSelectionPayload::Rows(ids) => {
                let mut cuttings = Vec::with_capacity(ids.len());
                for id in ids {
                    if self
                        .runtime
                        .workspace
                        .ws
                        .parent(*id)
                        .map_err(|e| e.to_string())?
                        == koru_core::schema::Parent::Root
                    {
                        return Err("a document title cannot be moved out of its document".into());
                    }
                    // A selection selects list *items*, never their invisible
                    // `ListBlock` container — carrying the enclosing style is
                    // what keeps a bullet a bullet at the destination.
                    let style = enclosing_list_style(&self.runtime.workspace.ws, *id);
                    cuttings.push((
                        self.runtime
                            .workspace
                            .ws
                            .cutting(*id)
                            .map_err(|e| e.to_string())?,
                        style,
                    ));
                }
                Ok((cuttings, None))
            }
            MoveSelectionPayload::Inline { runs, attrs } => {
                Ok((Vec::new(), Some((runs.clone(), attrs.clone()))))
            }
        }
    }

    /// Tell the requesting view to replace its selection with the trail pill —
    /// only ever called after the destination write succeeded, so a refusal
    /// (the pending move is gone) leaves the content duplicated, never lost.
    fn complete_selection_move_on(
        &mut self,
        origin: WindowId,
        document: koru_core::DocumentId,
        node: Option<koru_core::NodeId>,
        title: String,
    ) -> bool {
        let completed = self.panes.get_mut(&origin).is_some_and(|pane| {
            pane.view
                .complete_selection_move(&mut self.runtime.workspace.ws, document, node, title)
        });
        if completed {
            self.runtime.workspace.touched();
            self.invalidate_everywhere();
        } else {
            self.report_and_foreground(
                origin,
                ErrorReport::new(
                    "Moved a copy, but couldn't finish",
                    "The content reached the destination; the original was left in place.",
                ),
            );
        }
        completed
    }

    fn cancel_selection_move_on(&mut self, origin: WindowId) {
        if let Some(pane) = self.panes.get_mut(&origin) {
            let _ = pane.view.cancel_selection_move();
        }
    }

    /// Move the selection into a document that does not exist yet, then open
    /// it — `create_linked_document`'s dance with content in the middle.
    fn move_selection_to_new_document(
        &mut self,
        source: WindowId,
        title: &str,
        payload: &MoveSelectionPayload,
        follow: bool,
        child: bool,
    ) {
        let Some(origin) = self.runtime.workspace.active() else {
            self.cancel_selection_move_on(source);
            self.report(ErrorReport::new(
                "Couldn't move the selection",
                "No document is open here.",
            ));
            return;
        };
        let (cuttings, inline) = match self.selection_content(payload) {
            Ok(content) => content,
            Err(details) => {
                self.cancel_selection_move_on(source);
                self.report(
                    ErrorReport::new("Couldn't read the selection", "Nothing was moved.")
                        .with_details(details),
                );
                return;
            }
        };
        let created = if child {
            self.runtime.workspace.create_document_under(title, origin)
        } else {
            self.runtime
                .workspace
                .create_document_after(title, Some(origin))
        };
        let id = match created {
            Ok(id) => id,
            Err(e) => {
                self.cancel_selection_move_on(source);
                self.report(
                    ErrorReport::new("Couldn't create that document", "Nothing was moved.")
                        .with_details(e),
                );
                return;
            }
        };
        // The new document is now the active one; plant at its top and drop
        // the seeded empty paragraph the content just made redundant.
        let planted = self
            .runtime
            .workspace
            .plant_into_document(
                self.runtime.current,
                id,
                None,
                &cuttings,
                inline
                    .as_ref()
                    .map(|(runs, attrs)| (runs.as_slice(), attrs.as_slice())),
            )
            .is_ok();
        if planted {
            trim_trailing_seed(&mut self.runtime.workspace.ws);
        }
        let mut completed = false;
        if let Ok(()) = self.runtime.workspace.open_document(origin) {
            if planted {
                completed = self.panes.get_mut(&source).is_some_and(|pane| {
                    pane.view.complete_selection_move(
                        &mut self.runtime.workspace.ws,
                        id,
                        None,
                        title.to_owned(),
                    )
                });
                if completed {
                    self.runtime.workspace.touched();
                }
            } else {
                self.cancel_selection_move_on(source);
            }
        }
        if !planted {
            self.report(ErrorReport::new(
                "Made the document, but couldn't move the selection into it",
                "The page is in the sidebar and the original text was left alone.",
            ));
        } else if !completed {
            self.report(ErrorReport::new(
                "Moved a copy, but couldn't finish",
                "The content is on the new page; the original was left in place.",
            ));
        }
        // Ctrl+Enter: the move happened and the trail pill is in; the person
        // stays where the selection was. The new document is in the sidebar.
        if !follow {
            self.push_entries();
            self.show_active_document();
            self.sync_pane_context();
            self.invalidate_everywhere();
            return;
        }
        if let Err(e) = self.runtime.workspace.open_document(id) {
            self.report(
                ErrorReport::new(
                    "Made the document, but couldn't open it",
                    "It is in the sidebar.",
                )
                .with_details(e),
            );
            self.push_entries();
            self.sync_pane_context();
            self.invalidate_everywhere();
            return;
        }
        self.opened_document(id);
        self.push_entries();
        self.show_active_document();
        self.sync_pane_context();
        self.invalidate_everywhere();
    }

    /// Move the selection into an existing document — this window's replica
    /// when it holds one, and otherwise over the store, off the event loop,
    /// with the trail pill written only after the write is durable.
    fn move_selection_to_document(
        &mut self,
        source: WindowId,
        document: koru_core::DocumentId,
        node: Option<koru_core::NodeId>,
        title: &str,
        payload: &MoveSelectionPayload,
    ) {
        let (cuttings, inline) = match self.selection_content(payload) {
            Ok(content) => content,
            Err(details) => {
                self.cancel_selection_move_on(source);
                self.report(
                    ErrorReport::new("Couldn't read the selection", "Nothing was moved.")
                        .with_details(details),
                );
                return;
            }
        };
        let inline_ref = inline
            .as_ref()
            .map(|(runs, attrs)| (runs.as_slice(), attrs.as_slice()));
        // A replica this window holds — the active document or an open tab —
        // is written synchronously; both are cheap and need no join.
        if self.runtime.workspace.active() == Some(document)
            || self.runtime.workspace.document(document).is_some()
        {
            match self.runtime.workspace.plant_into_document(
                self.runtime.current,
                document,
                node,
                &cuttings,
                inline_ref,
            ) {
                Ok(()) => {
                    self.complete_selection_move_on(source, document, node, title.to_owned());
                }
                Err(e) => {
                    self.cancel_selection_move_on(source);
                    self.report(
                        ErrorReport::new(
                            "Couldn't move the selection",
                            "Nothing was removed from this page.",
                        )
                        .with_details(e),
                    );
                }
            }
            return;
        }
        // Anything else — same workspace but never opened, or another
        // workspace entirely — is located through the store and written by the
        // worker, because the durability wait can take seconds.
        let location = self
            .runtime
            .workspace
            .store_client()
            .ok_or_else(|| "this window is not reaching the store".to_string())
            .and_then(|client| {
                client
                    .locate_document(koru_store_core::ids::DocumentId::from_uuid(
                        document.as_uuid(),
                    ))
                    .map_err(|e| e.to_string())
            });
        let location = match location {
            Ok(location) => location,
            Err(details) => {
                self.cancel_selection_move_on(source);
                self.report(
                    ErrorReport::new(
                        "Couldn't find that document",
                        "Nothing was removed from this page.",
                    )
                    .with_details(details),
                );
                return;
            }
        };
        if Some(koru_core::ProfileId::from_uuid(location.profile.as_uuid()))
            != self.runtime.profile.id()
        {
            self.cancel_selection_move_on(source);
            self.report(ErrorReport::new(
                "Couldn't move the selection there",
                "That document belongs to another profile. Nothing was removed from this page.",
            ));
            return;
        }
        if !self.selection_moves_in_flight.insert(document) {
            return;
        }
        let blobs = cuttings
            .iter()
            .flat_map(|(cutting, _)| cutting.blob_hashes())
            .map(|hash| {
                koru_store_core::model::BlobHash::parse(&hash.0).map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>();
        let mut blobs = match blobs {
            Ok(blobs) => blobs,
            Err(details) => {
                self.selection_moves_in_flight.remove(&document);
                self.cancel_selection_move_on(source);
                self.report(
                    ErrorReport::new(
                        "Couldn't move the selection",
                        "Nothing was removed from this page.",
                    )
                    .with_details(details),
                );
                return;
            }
        };
        blobs.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        blobs.dedup();
        let Some(client) = self.runtime.workspace.store_client() else {
            self.selection_moves_in_flight.remove(&document);
            self.cancel_selection_move_on(source);
            self.report(ErrorReport::new(
                "Couldn't move the selection",
                "This window is not reaching the store.",
            ));
            return;
        };
        let job = SelectionMoveJob {
            origin: source,
            segment: koru_workspace::fromstore::segment().to_owned(),
            root: self.gateway_root.clone(),
            client,
            source: self.runtime.current,
            target: WorkspaceId::from_uuid(location.workspace.as_uuid()),
            document,
            node,
            title: title.to_owned(),
            cuttings,
            blobs,
            inline,
        };
        if let Err(error) = self.selection_move_tx.send(job) {
            self.selection_moves_in_flight.remove(&document);
            self.cancel_selection_move_on(source);
            self.report(
                ErrorReport::new(
                    "Couldn't move the selection",
                    "Nothing was removed from this page.",
                )
                .with_details(error),
            );
        }
    }

    fn drain_selection_moves(&mut self) {
        while let Ok(done) = self.selection_move_rx.try_recv() {
            self.selection_moves_in_flight.remove(&done.document);
            match done.result {
                Ok(()) => {
                    self.complete_selection_move_on(
                        done.origin,
                        done.document,
                        done.node,
                        done.title,
                    );
                }
                Err(details) => {
                    self.cancel_selection_move_on(done.origin);
                    self.report_and_foreground(
                        done.origin,
                        ErrorReport::new(
                            "Couldn't move the selection",
                            "Nothing was removed from this page (it may have been copied to the destination).",
                        )
                        .with_details(details),
                    );
                }
            }
        }
    }

    fn rename_workspace(&mut self, id: WorkspaceId, name: &str) {
        // **The store first, because it is the one that remembers.**
        //
        // `self.runtime.profile` is a view rebuilt from the store on every launch, so
        // renaming only there changed the switcher until the window closed and
        // then silently reverted — which is exactly what happened, and is the
        // shape of every bug this transitional `ProfileDoc` can produce.
        //
        // On failure nothing is renamed anywhere. Updating the view first and
        // the store second would leave the two disagreeing whenever the store
        // refused, and the disagreement would look like a successful rename.
        if let Err(e) = koru_workspace::fromstore::rename_workspace(
            koru_workspace::fromstore::segment(),
            self.gateway_root.as_deref(),
            id,
            name,
        ) {
            self.report(
                ErrorReport::new("Couldn't rename that workspace", "Nothing was changed.")
                    .with_details(e),
            );
            return;
        }
        // **A workspace of a profile this window has not opened** is written
        // to the Store and nowhere else: the open `ProfileDoc` does not list
        // it, and the catalog the Manage tab reads is refreshed from the Store.
        if self.runtime.profile.workspace(id).is_none() {
            self.runtime.request_catalog_refresh();
            return;
        }
        if let Err(e) = self.runtime.profile.rename_workspace(id, name) {
            self.report(
                ErrorReport::new("Couldn't rename that workspace", "Nothing was changed.")
                    .with_details(e),
            );
            return;
        }
        let _ = self.runtime.profile_session.flush(&self.runtime.profile);
        if id == self.runtime.current {
            let _ = self.runtime.workspace.ws.rename(name);
            self.runtime.workspace.touched();
        }
        self.write_workspace_conf(id);
        self.sync_pane_context();
        self.sync_titles();
    }

    fn rename_profile(&mut self, id: koru_core::ProfileId, name: &str) {
        if let Err(e) = koru_workspace::fromstore::rename_profile(
            koru_workspace::fromstore::segment(),
            self.gateway_root.as_deref(),
            id,
            name,
        ) {
            let report = if e.code() == koru_store_core::Code::ApiVersionUnsupported {
                ErrorReport::new(
                    "Restart Koru Store to edit profiles",
                    "The running Store predates profile editing. Restart it, then try again.",
                )
            } else {
                ErrorReport::new("Couldn't rename that profile", "Nothing was changed.")
            };
            self.report(report.with_details(e));
            return;
        }
        if let Some(row) = self
            .runtime
            .profile_catalog
            .iter_mut()
            .find(|row| row.id == id)
        {
            row.name = name.to_owned();
        }
        if self.runtime.profile.id() == Some(id) {
            if let Err(e) = self.runtime.profile.set_name(name) {
                self.report(
                    ErrorReport::new("Couldn't rename that profile", "Nothing was changed.")
                        .with_details(e),
                );
                return;
            }
        }
        self.sync_pane_context();
        self.sync_titles();
    }

    fn set_profile_appearance(
        &mut self,
        id: koru_core::ProfileId,
        icon: Option<&str>,
        color: Option<&str>,
    ) {
        if let Err(e) = koru_workspace::fromstore::set_profile_appearance(
            koru_workspace::fromstore::segment(),
            self.gateway_root.as_deref(),
            id,
            icon,
            color,
        ) {
            let report = if e.code() == koru_store_core::Code::ApiVersionUnsupported {
                ErrorReport::new(
                    "Restart Koru Store to edit profiles",
                    "The running Store predates profile editing. Restart it, then try again.",
                )
            } else {
                ErrorReport::new("Couldn't update that profile", "Nothing was changed.")
            };
            self.report(report.with_details(e));
            return;
        }
        if let Some(row) = self
            .runtime
            .profile_catalog
            .iter_mut()
            .find(|row| row.id == id)
        {
            row.icon = icon.map(str::to_owned);
            row.color = color.map(str::to_owned);
        }
        self.sync_pane_context();
    }

    /// Set one flag on a workspace entry, then write and republish.
    ///
    /// Flushed rather than left to the poll, and `sync_pane_context` called
    /// straight after: this process knows the list changed, so waiting the two
    /// seconds a peer would wait is a delay nobody chose.
    fn set_workspace_flag(&mut self, id: WorkspaceId, flag: WorkspaceFlag) -> bool {
        if let Some(workspace) =
            other_profile_workspace(&self.runtime.profile, &self.runtime.catalog_workspaces, id)
        {
            let values = flag.applied_to(workspace);
            return self.set_other_profile_workspace_flag(id, values);
        }
        if let Err(e) = flag.apply(&mut self.runtime.profile, id) {
            let name = self.runtime.workspace_name(id);
            self.report(
                ErrorReport::new(
                    format!("Couldn't update \u{201c}{name}\u{201d}"),
                    "Nothing was changed.",
                )
                .with_details(e),
            );
            return false;
        }
        let _ = self.runtime.profile_session.flush(&self.runtime.profile);
        self.write_workspace_conf(id);
        // **And the store, which is the only one of the four that persists.**
        //
        // The three writes above are the folder world: the `ProfileDoc` is a
        // *view* rebuilt from the store every launch, `profile_session` flushes
        // the folder profile document, and the sidecar sits beside a folder.
        // Store-backed, nothing reads any of them back — so an icon and a colour
        // appeared to be set, survived as long as the window did, and were gone
        // by the next one. A rename never had the bug because it goes through
        // `fromstore::rename_workspace` to the store.
        if let Some(entry) = self.runtime.profile.workspace(id) {
            let _ = koru_workspace::fromstore::set_workspace_appearance(
                koru_workspace::fromstore::segment(),
                self.gateway_root.as_deref(),
                id,
                entry.icon.as_deref(),
                entry.color.as_deref(),
                entry.pinned,
                entry.archived,
            );
        }
        self.sync_pane_context();
        true
    }

    /// Write a workspace of a profile this window has not opened: the Store
    /// is the only copy, so the whole appearance goes there and the catalog
    /// worker is asked for the result.
    fn set_other_profile_workspace_flag(
        &mut self,
        id: WorkspaceId,
        values: WorkspaceAppearance,
    ) -> bool {
        if let Err(e) = koru_workspace::fromstore::set_workspace_appearance(
            koru_workspace::fromstore::segment(),
            self.gateway_root.as_deref(),
            id,
            values.icon.as_deref(),
            values.color.as_deref(),
            values.pinned,
            values.archived,
        ) {
            self.report(
                ErrorReport::new(
                    format!("Couldn't update \u{201c}{}\u{201d}", values.name),
                    "Nothing was changed.",
                )
                .with_details(e),
            );
            return false;
        }
        self.runtime.request_catalog_refresh();
        true
    }

    fn reorder_workspaces(&mut self, ids: &[WorkspaceId]) {
        let Some(owner) = self.runtime.profile.id() else {
            return;
        };
        let store_owner = koru_store_core::ids::ProfileId::from_uuid(owner.as_uuid());
        let store_ids: Vec<_> = ids
            .iter()
            .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()))
            .collect();
        let result = koru_api_client::Client::open(koru_workspace::fromstore::segment())
            .and_then(|client| client.reorder_workspaces(store_owner, &store_ids));
        match result {
            Ok(workspaces) => {
                for workspace in workspaces {
                    let id = WorkspaceId::from_uuid(workspace.id.as_uuid());
                    let _ = self
                        .runtime
                        .profile
                        .set_workspace_order(id, workspace.position);
                }
                let _ = self.runtime.profile_session.flush(&self.runtime.profile);
                self.sync_pane_context();
            }
            Err(error) => {
                self.report(
                    ErrorReport::new("Couldn't reorder the workspaces", "Nothing was changed.")
                        .with_details(error),
                );
                self.sync_pane_context();
            }
        }
    }

    /// Archive from any row, including the workspace this window is showing.
    /// Moving first releases its live Store rooms before the workspace leaves
    /// ordinary navigation; a lone workspace lands on the local welcome state.
    fn archive_workspace(&mut self, id: WorkspaceId, archived: bool) {
        let moved = if archived {
            match self.leave_current_workspace(id) {
                Ok(moved) => moved,
                Err(error) => {
                    self.report(
                        ErrorReport::new(
                            format!(
                                "Couldn't archive \u{201c}{}\u{201d}",
                                self.runtime.workspace_name(id)
                            ),
                            "Nothing was changed.",
                        )
                        .with_details(error),
                    );
                    return;
                }
            }
        } else {
            false
        };

        if !self.set_workspace_flag(id, WorkspaceFlag::Archived(archived)) && moved {
            self.switch_workspace(id);
        }
    }

    /// A store client for **this window's root**.
    ///
    /// **Every store call in this file goes through here or through
    /// [`client_at`], and that is the point.** `Client::open` finds the
    /// channel's shared store; a window launched with `--root` that renamed a
    /// workspace through it would have renamed one in a different store, and the
    /// window would show the rename it made locally with nothing behind it. The
    /// manifest and the documents are already rooted, so a bare `Client::open`
    /// left anywhere here is a window with its two halves in two stores.
    fn store_client(&self) -> Result<koru_api_client::Client, koru_store_core::errors::Error> {
        client_at(self.gateway_root.as_deref())
    }

    fn restore_workspace(&mut self, id: WorkspaceId) {
        let name = self.runtime.workspace_name(id);
        let result = (|| {
            let client = self.store_client()?;
            client.restore_workspace(koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()))?;
            self.runtime
                .profile
                .set_workspace_deleted_at(id, None)
                .map_err(|error| {
                    koru_store_core::Error::new(
                        koru_store_core::Code::Internal,
                        format!("workspace view: {error}"),
                    )
                })?;
            Ok::<_, koru_store_core::Error>(())
        })();
        if let Err(error) = result {
            self.report(
                ErrorReport::new(
                    format!("Couldn't restore \u{201c}{name}\u{201d}"),
                    "Nothing was changed.",
                )
                .with_details(error),
            );
            return;
        }
        let _ = self.runtime.profile_session.flush(&self.runtime.profile);
        self.sync_pane_context();
    }

    /// Rewrite one workspace's sidecar from the entry, so a folder that is later
    /// moved carries the change with it.
    fn write_workspace_conf(&mut self, id: WorkspaceId) {
        if let Some(entry) = self.runtime.profile.workspace(id) {
            // The count is only knowable for the workspace this window has
            // open; for any other, whatever the conf already says is left
            // alone rather than replaced with a guess.
            let docs = (id == self.runtime.current)
                .then(|| koru_workspace::open::document_count(&self.runtime.workspace.ws))
                .flatten()
                .or_else(|| {
                    koru_docstore::profilefolder::peek_workspace(&self.runtime.profile_dir, id)
                        .and_then(|f| f.docs)
                });
            koru_workspace::open::write_workspace_sidecar(&self.runtime.profile_dir, &entry, docs);
        }
    }

    fn reveal_path(&mut self, dir: &std::path::Path) {
        if let Err(e) = reveal::reveal(dir) {
            self.report(
                ErrorReport::new(
                    "Couldn't show that folder",
                    format!("Koru could not open {}.", dir.display()),
                )
                .with_details(e),
            );
        }
    }

    /// Ask the Store for one complete workspace bundle and write it away from
    /// the event-loop thread.
    fn export_workspace(&mut self, id: WorkspaceId) {
        let name = self.runtime.workspace_name(id);
        let suggested = export::suggested_name(&name, &koru_docstore::backup::stamp_now());
        let Some(to) = crate::dialogs::pick_save_file(
            "Export a backup of this workspace",
            &suggested,
            &[crate::dialogs::FileFilter {
                name: "Koru workspace",
                extensions: &["koru"],
            }],
            self.runtime.settings.last_export_dir.as_deref(),
            self.primary_window(),
        ) else {
            return;
        };
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let segment = koru_workspace::fromstore::segment().to_owned();
        let root = self.gateway_root.clone();
        let thread_name = name.clone();
        let thread_to = to.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-export-workspace".into())
            .spawn(move || {
                let result = (|| {
                    let client =
                        client_at_segment(&segment, root.as_deref()).map_err(|e| e.to_string())?;
                    let bytes = client
                        .export_workspace(koru_store_core::ids::WorkspaceId::from_uuid(
                            id.as_uuid(),
                        ))
                        .map_err(|error| error.to_string())?;
                    export::write_bundle(&thread_to, &bytes).map_err(|error| error.to_string())
                })();
                let _ = tx.send(ImportMessage::Exported {
                    id,
                    name: thread_name,
                    destination: thread_to,
                    result,
                });
                let _ = proxy.send_event(Wake::Import);
            })
        {
            self.report(
                ErrorReport::new(
                    format!("Couldn't export \u{201c}{name}\u{201d}"),
                    format!(
                        "Koru could not start the export to {}. No backup was written and the workspace was not changed.",
                        to.display()
                    ),
                )
                .with_details(error),
            );
        }
    }

    /// A runtime dialog owned by the primary Koru window. Without an owner,
    /// Windows may place a second confirmation behind the app while disabling
    /// the visible window, which looks exactly like the UI has frozen.
    fn message_dialog(&self) -> rfd::MessageDialog {
        let dialog = rfd::MessageDialog::new();
        match self.primary.and_then(|id| self.panes.get(&id)) {
            Some(pane) => dialog.set_parent(pane.window.as_ref()),
            None => dialog,
        }
    }

    /// Soft-delete a Store workspace. Its documents and blobs stay intact for
    /// restore; only the Store may later purge the bytes after retention.
    fn delete_workspace(&mut self, id: WorkspaceId) {
        if self.deleting_workspaces.contains(&id) {
            return;
        }
        let name = self.runtime.workspace_name(id);
        let confirmed = self
            .message_dialog()
            .set_level(rfd::MessageLevel::Warning)
            .set_title("Delete this workspace?")
            .set_description(format!(
                "\u{201c}{name}\u{201d} will move to Deleted.\n\nYou can restore it for 30 days."
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show();
        if confirmed != rfd::MessageDialogResult::Yes {
            return;
        }

        let was_current = self.runtime.live_workspace && id == self.runtime.current;
        if was_current {
            if let Err(error) = self.leave_current_workspace(id) {
                self.report(
                    ErrorReport::new(
                        format!("Couldn't delete \u{201c}{name}\u{201d}"),
                        "Koru could not leave that workspace. Nothing was deleted.",
                    )
                    .with_details(error),
                );
                return;
            }
        }

        self.deleting_workspaces.insert(id);
        let tx = self.import_tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.gateway_root.clone();
        let thread_name = name.clone();
        let spawned = std::thread::Builder::new()
            .name("koru-delete-workspace".into())
            .spawn(move || {
                let store_id = koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid());
                let result = (|| {
                    let client = client_at(root.as_deref())?;
                    // Dropping the current workspace's socket sessions is
                    // asynchronous. Give those local rooms one second to
                    // drain before deciding another window still owns them.
                    for attempt in 0..=10 {
                        match client.delete_workspace(store_id) {
                            Err(error)
                                if was_current
                                    && error.code() == koru_store_core::Code::WorkspaceInUse
                                    && attempt < 10 =>
                            {
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            result => return result,
                        }
                    }
                    unreachable!()
                })()
                .map_err(|error| (error.code(), error.to_string()));
                let _ = tx.send(ImportMessage::WorkspaceDeleted {
                    id,
                    name: thread_name,
                    was_current,
                    result,
                });
                let _ = proxy.send_event(Wake::Import);
            });
        if let Err(error) = spawned {
            self.deleting_workspaces.remove(&id);
            if was_current && (!self.runtime.live_workspace || self.runtime.current != id) {
                self.switch_workspace(id);
            }
            self.report(
                ErrorReport::new(
                    format!("Couldn't delete \u{201c}{name}\u{201d}"),
                    "Koru could not start the delete operation. Nothing was deleted.",
                )
                .with_details(error),
            );
        }
    }

    /// Replace the active Store workspace with a local, unsaved welcome state.
    fn show_empty_workspace(&mut self) -> Result<(), String> {
        self.capture_geometry();
        self.runtime.settings.workspace = None;
        self.runtime.settings.save_all(
            &self.runtime.profile_dir,
            &self.instance.slot,
            &mut self.runtime.written,
        );
        let outgoing = self.runtime.current;
        let placeholder = WorkspaceId::new();
        let doc = koru_core::schema::WorkspaceDoc::create(placeholder, "Welcome")
            .map_err(|error| error.to_string())?;
        let mut workspace = Workspace::new(doc);
        workspace.set_actor(koru_workspace::actor_label(
            self.runtime.profile.name().as_deref(),
            self.runtime.profile.email().as_deref(),
            Some(self.instance.slot.as_str()),
        ));
        workspace.set_store_waker(self.store_waker.clone());
        self.runtime.workspace = workspace;
        self.runtime.current = placeholder;
        self.runtime.live_workspace = false;
        if let Some(profile_id) = self.runtime.profile.id() {
            self.host.workspace_closed(profile_id, outgoing);
        }
        self.releasing.clear();
        self.host.adopt_workspace(&mut self.runtime.workspace);
        self.show_active_document();
        self.image_scope = None;
        self.images = ImageCache::new();
        self.image_loader.clear();
        self.images_dirty = false;
        for pane in self.panes.values_mut() {
            pane.view.reseat(&self.runtime.workspace.ws);
            pane.view
                .restore(&self.runtime.workspace, &self.runtime.settings);
            pane.window.request_redraw();
        }
        self.sync_pane_context();
        self.sync_titles();
        Ok(())
    }

    /// Whether another instance has written to this workspace's update log in
    /// the last few seconds. Evidence, not a lock — but it is the same evidence
    /// `DocStore::peer_live` uses to decide somebody else is there.
    fn workspace_is_live_elsewhere(&self, id: WorkspaceId) -> bool {
        const LIVE_WITHIN: std::time::Duration = std::time::Duration::from_secs(20);
        let updates = self
            .runtime
            .profile_dir
            .join("workspaces")
            .join(id.to_storage())
            .join("updates");
        let Ok(entries) = std::fs::read_dir(updates) else {
            return false;
        };
        let mine = format!("peer-{}.loro", self.instance.peer);
        entries.flatten().any(|e| {
            if e.file_name() == mine.as_str() {
                return false;
            }
            e.metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.elapsed().ok())
                .is_some_and(|age| age < LIVE_WITHIN)
        })
    }

    /// Make the named workspace and switch to it. The window collects the name
    /// first, so cancelling the prompt leaves no placeholder catalog row.
    /// Create a workspace in `owner` and open it there, switching Profile when
    /// `owner` is not the open one. Within the open Profile this is exactly
    /// [`Self::new_workspace`].
    fn new_workspace_in(&mut self, owner: koru_core::ProfileId, name: &str) {
        if self.runtime.profile.id() == Some(owner) {
            self.new_workspace(name);
            return;
        }
        match koru_workspace::fromstore::create_workspace(
            koru_workspace::fromstore::segment(),
            self.gateway_root.as_deref(),
            owner,
            name,
        ) {
            Ok(id) => {
                crate::events_host::workspace_created(
                    id.as_uuid(),
                    crate::events_host::WorkspaceSource::Manual,
                );
                self.switch_profile_inner(owner, Some(id), true);
            }
            Err(error) => self.report(
                ErrorReport::new("Couldn't create a workspace", "Nothing was changed.")
                    .with_details(error.to_string()),
            ),
        }
    }

    fn new_workspace(&mut self, name: &str) {
        // **The store, not the profile folder.** `open::create_workspace`
        // registers a workspace in a `ProfileDoc`, which since the cutover is a
        // view rebuilt from the store every launch — so the workspace existed
        // only in this window. It showed in the switcher and every later call
        // naming it answered 404, because the store had never heard of it.
        let Some(owner) = self.runtime.profile.id() else {
            self.report(ErrorReport::new(
                "Couldn't create a workspace",
                "This profile has no identity.",
            ));
            return;
        };
        match koru_workspace::fromstore::create_workspace(
            koru_workspace::fromstore::segment(),
            self.gateway_root.as_deref(),
            owner,
            name,
        )
        .map_err(|e| e.to_string())
        .and_then(|id| {
            // Keep the view in step so the switcher lists it before the next
            // launch rebuilds the view from the store.
            self.runtime
                .profile
                .ensure_workspace(id, &name, koru_workspace::open::now_ms())
                .map(|_| id)
                .map_err(|e| e.to_string())
        }) {
            Ok(id) => {
                crate::events_host::workspace_created(
                    id.as_uuid(),
                    crate::events_host::WorkspaceSource::Manual,
                );
                self.switch_workspace(id)
            }
            Err(e) => startup::fatal(&format!(
                "Koru could not create a workspace:

{e}"
            )),
        }
    }

    fn reconcile_pending_profile_bindings(
        &mut self,
        profiles: &[koru_agent_wire::AccountProfileRow],
    ) {
        for marker in self.runtime.pending_bindings.all() {
            let Some(local) = self
                .runtime
                .profile_catalog
                .iter()
                .find(|row| row.id == marker.profile)
            else {
                self.runtime.pending_bindings.forget(marker.profile);
                continue;
            };
            match profile_binding_reconcile_action(&marker, local.account.as_deref(), profiles) {
                ProfileBindingReconcile::AlreadyBound => {
                    self.runtime.pending_bindings.forget(marker.profile);
                }
                ProfileBindingReconcile::Conflict => {
                    self.runtime.pending_bindings.forget(marker.profile);
                    for pane in self.panes.values_mut() {
                        let _ = pane.view.local_profile_account_bind_failed(
                            marker.profile,
                            "profile_account_conflict",
                        );
                    }
                }
                ProfileBindingReconcile::WaitForExactServerProfile => {}
                ProfileBindingReconcile::Bind => {
                    if self.profile_bindings_in_flight.insert(marker.profile) {
                        self.host.bind_local_profile_account(
                            marker.profile,
                            marker.expected_identity_id,
                            marker.new_identity_id,
                        );
                    }
                }
            }
        }
    }

    /// The marker writer answered. A durable marker is the moment the Agent is
    /// asked; a marker that could not be written is the moment the person is
    /// told — and the connected-creation flow pauses with its retry rather
    /// than the process ending.
    fn service_binding_markers(&mut self) {
        for written in self.runtime.pending_bindings.drain() {
            match written.result {
                Ok(()) => {
                    if let Some(marker) =
                        self.runtime.pending_bindings.get(written.profile).cloned()
                    {
                        self.host.bind_local_profile_account(
                            marker.profile,
                            marker.expected_identity_id,
                            marker.new_identity_id,
                        );
                    }
                }
                Err(error) => {
                    eprintln!(
                        "[Koru][profiles] could not record the Profile binding marker for {}: {error}",
                        written.profile.to_storage()
                    );
                    self.profile_bindings_in_flight.remove(&written.profile);
                    self.runtime.pending_bindings.forget(written.profile);
                    let connected_creation = self
                        .pending_connected_profile_creation
                        .as_ref()
                        .is_some_and(|pending| {
                            pending.stage.completes_after_local_ack(written.profile)
                        });
                    if connected_creation {
                        self.pause_connected_profile_creation(written.profile);
                    } else {
                        for pane in self.panes.values_mut() {
                            let _ = pane.view.local_profile_account_bind_failed(
                                written.profile,
                                "profile_binding_marker_failed",
                            );
                        }
                    }
                }
            }
        }
    }

    fn service_embedded_profile_restores(&mut self) {
        while let Ok(completion) = self.embedded_profile_restore_rx.try_recv() {
            let EmbeddedProfileRestore { source, result } = completion;
            match result {
                OnboardingCreation::Opened(opened) => {
                    let Some(profile) = opened.profile.id() else {
                        if let Some(pane) = self.panes.get_mut(&source) {
                            let _ = pane.view.local_profile_restore_failed(
                                "Koru restored a Profile without a durable identity. Restart Koru to recover it.",
                            );
                            pane.window.request_redraw();
                        }
                        continue;
                    };
                    // `restore_onboarding_profile` opened the exact durable
                    // Profile to prove it is usable. Release that read before
                    // the ordinary shell-owned switch opens its live session.
                    drop(opened);
                    self.switch_profile(profile);
                    if let Some(pane) = self.panes.get_mut(&source) {
                        if connected_profile_is_open(self.runtime.profile.id(), profile) {
                            let _ = pane.view.local_profile_restored(profile);
                        } else {
                            let _ = pane.view.local_profile_restore_failed(
                                "Koru restored the Profile but could not switch to it. Try again.",
                            );
                        }
                        pane.window.request_redraw();
                    }
                }
                OnboardingCreation::Retryable { message }
                | OnboardingCreation::RestoreFailed { message }
                | OnboardingCreation::CommittedFailure { message } => {
                    if let Some(pane) = self.panes.get_mut(&source) {
                        let _ = pane.view.local_profile_restore_failed(&message);
                        pane.window.request_redraw();
                    }
                }
            }
        }
    }

    fn service_connected_profile_creation(&mut self, event_loop: &ActiveEventLoop) {
        while let Ok(result) = self.connected_profile_creation_rx.try_recv() {
            match result {
                OnboardingCreation::Opened(opened) => {
                    let Some(profile) = opened.profile.id() else {
                        startup::fatal(
                            "Koru created a Profile without a durable identity. Restart Koru to recover it.",
                        );
                        event_loop.exit();
                        return;
                    };
                    let Some(pending) = self.pending_connected_profile_creation.as_mut() else {
                        continue;
                    };
                    pending.stage = pending.stage.after_local_creation(profile);
                    if pending.connect_account {
                        let owner = pending.owner_scope;
                        let command = pending.retry_command(profile);
                        if self.host.account_state().active_scope != owner {
                            self.pause_connected_profile_creation(profile);
                        } else {
                            self.submit_owned_account_command(owner, command);
                        }
                    } else {
                        self.finish_connected_profile_creation(profile, true);
                    }
                }
                OnboardingCreation::Retryable { message } => {
                    let source = self
                        .pending_connected_profile_creation
                        .take()
                        .map(|pending| pending.source);
                    if let Some(pane) = source.and_then(|source| self.panes.get_mut(&source)) {
                        let _ = pane.view.connected_profile_creation_failed(&message);
                        pane.window.request_redraw();
                    }
                }
                OnboardingCreation::RestoreFailed { message } => {
                    let source = self
                        .pending_connected_profile_creation
                        .take()
                        .map(|pending| pending.source);
                    if let Some(pane) = source.and_then(|source| self.panes.get_mut(&source)) {
                        let _ = pane.view.connected_profile_creation_failed(&message);
                        pane.window.request_redraw();
                    }
                }
                OnboardingCreation::CommittedFailure { message } => {
                    // A Profile already exists, so returning to the form could
                    // create a duplicate. The next launch discovers the exact
                    // durable ids and generic marker reconciliation continues.
                    startup::fatal(&message);
                    event_loop.exit();
                    return;
                }
            }
        }
    }

    fn submit_owned_account_command(
        &mut self,
        owner_scope: u64,
        command: koru_agent_wire::AccountCommand,
    ) {
        if let koru_agent_wire::AccountCommand::CreateProfile { profile_id, .. } = &command
            && let Some(pending) = self.pending_connected_profile_creation.as_mut()
            && pending.owner_scope == owner_scope
            && matches!(pending.stage, ConnectedProfileCreationStage::AwaitingHostedProfile(profile) | ConnectedProfileCreationStage::AwaitingAccountReturn(profile) if profile.to_storage()==*profile_id)
        {
            let submission = crate::account_ui::CommandSubmissionId::next();
            pending.submission = Some(submission);
            pending.generation = None;
            let profile = koru_core::ProfileId::parse(profile_id).expect("retained Profile");
            pending.stage = ConnectedProfileCreationStage::AwaitingHostedProfile(profile);
            self.host
                .account_command_tracked_owned(submission, owner_scope, command);
        } else {
            self.host.account_command_owned(owner_scope, command);
        }
    }

    fn pause_connected_profile_creation(&mut self, profile: koru_core::ProfileId) {
        let Some(pending) = self.pending_connected_profile_creation.as_mut() else {
            return;
        };
        pending.stage = ConnectedProfileCreationStage::AwaitingAccountReturn(profile);
        let retry = pending.retry_command(profile);
        if let Some(pane) = self.panes.get_mut(&pending.source) {
            pane.view
                .connected_profile_connection_paused(pending.owner_scope, profile, retry);
            pane.window.request_redraw();
        }
    }

    fn accept_connected_profile_hosted_ack(
        &mut self,
        owner_scope: u64,
        profile: &koru_agent_wire::AccountProfileRow,
    ) {
        let Some(pending) = self.pending_connected_profile_creation.as_mut() else {
            return;
        };
        if pending.owner_scope != owner_scope {
            return;
        }
        let Some(local_profile) = koru_core::ProfileId::parse(&profile.profile_id) else {
            return;
        };
        let next = pending.stage.after_hosted_ack(local_profile);
        if next == pending.stage {
            return;
        }
        let marker = crate::binding_markers::PendingProfileAccountBinding {
            profile: local_profile,
            expected_identity_id: None,
            new_identity_id: profile.koru_identity_id.clone(),
        };
        pending.stage = next;
        self.profile_bindings_in_flight.insert(local_profile);
        // Asked once the marker is durable; see `service_binding_markers`.
        self.runtime.pending_bindings.remember(marker);
    }

    fn finish_connected_profile_creation(
        &mut self,
        profile: koru_core::ProfileId,
        account_connected: bool,
    ) {
        let Some(pending) = self.pending_connected_profile_creation.as_ref() else {
            return;
        };
        let source = pending.source;
        let owner_display_name = pending.owner_display_name.clone();
        let profile_name = pending.profile_name.clone();
        let workspace_name = pending.workspace_name.clone();
        let connection_was_requested = pending.connect_account;
        if pending.owner_scope != self.host.account_state().active_scope {
            let owner = pending.owner_scope;
            if let Some(pane) = self.panes.get_mut(&source) {
                pane.view.connected_profile_ready(owner, profile);
                pane.window.request_redraw();
            }
            self.pending_connected_profile_creation = None;
            return;
        }
        self.switch_profile(profile);
        if !connected_profile_is_open(self.runtime.profile.id(), profile) {
            // The switch already surfaced its concrete error. Retain the exact
            // pending Profile so no later action can mistake the old Profile
            // for the destination or begin a duplicate.
            return;
        }
        self.pending_connected_profile_creation = None;
        let target = self
            .panes
            .contains_key(&source)
            .then_some(source)
            .or(self.primary);
        if let Some(pane) = target.and_then(|id| self.panes.get_mut(&id)) {
            let _ =
                pane.view
                    .show_first_minutes(&owner_display_name, &profile_name, &workspace_name);
            if connection_was_requested && !account_connected {
                let _ = pane.view.show_notice(
                    "The Profile was created and remains available offline, but Koru could not finish its account connection.".into(),
                );
            }
            pane.window.request_redraw();
        }
    }

    fn service_profile_bindings(&mut self) {
        let mut changed = false;
        let mut connected_completion = None;
        while let Some((raw_profile, outcome)) = self.host.take_local_profile_binding() {
            let Some(profile) = koru_core::ProfileId::parse(&raw_profile) else {
                continue;
            };
            self.profile_bindings_in_flight.remove(&profile);
            let clear_marker = outcome.is_ok()
                || matches!(&outcome, Err(code) if code == "profile_account_conflict");
            if clear_marker {
                if outcome.is_ok()
                    && let Some(marker) = self.runtime.pending_bindings.get(profile).cloned()
                    && let Some(local) = self
                        .runtime
                        .profile_catalog
                        .iter_mut()
                        .find(|row| row.id == profile)
                {
                    local.account = Some(format!("koru:{}", marker.new_identity_id));
                }
                self.runtime.pending_bindings.forget(profile);
            }
            if self
                .pending_connected_profile_creation
                .as_ref()
                .is_some_and(|pending| pending.stage.completes_after_local_ack(profile))
            {
                connected_completion = Some((profile, outcome.is_ok()));
            }
            for pane in self.panes.values_mut() {
                changed |= match &outcome {
                    Ok(_) => pane.view.local_profile_account_bound(profile),
                    Err(code) => pane.view.local_profile_account_bind_failed(profile, code),
                };
            }
        }
        if let Some((profile, account_connected)) = connected_completion {
            self.finish_connected_profile_creation(profile, account_connected);
            changed = true;
        }
        if changed {
            self.invalidate_everywhere();
        }
    }

    /// Replace the Store profile behind every pane without replacing the OS
    /// window. The incoming profile is opened completely before any outgoing
    /// state moves, so a Store or decode failure leaves this profile intact.
    fn switch_profile(&mut self, id: koru_core::ProfileId) {
        self.switch_profile_inner(id, None, true);
    }

    fn switch_profile_after_removal(&mut self, id: koru_core::ProfileId) -> bool {
        self.switch_profile_inner(id, None, false);
        self.runtime.profile_id() == id
    }

    /// `wanted` is the workspace to open in the incoming Profile; `None` keeps
    /// the remembered one. A stale id falls back the same way a remembered one
    /// does, so a row that vanished mid-click still lands somewhere real.
    fn switch_profile_inner(
        &mut self,
        id: koru_core::ProfileId,
        wanted: Option<koru_core::WorkspaceId>,
        persist_outgoing: bool,
    ) {
        if self.runtime.profile.id() == Some(id) {
            return;
        }
        let began = Instant::now();
        let opened = match koru_workspace::fromstore::open_from_store_profile_preferring_empty(
            koru_workspace::fromstore::segment(),
            self.gateway_root.as_deref(),
            id,
            wanted,
        ) {
            Ok(koru_workspace::fromstore::StoreLaunch::Open(opened)) => opened,
            Ok(koru_workspace::fromstore::StoreLaunch::NeedsProfile) => {
                self.report(ErrorReport::new(
                    "Couldn't switch profiles",
                    "The selected profile is no longer available. Nothing was changed.",
                ));
                return;
            }
            Err(error) => {
                self.report(
                    ErrorReport::new("Couldn't switch profiles", "Nothing was changed.")
                        .with_details(error),
                );
                return;
            }
        };
        let koru_workspace::fromstore::OpenedFromStore {
            profiles,
            profile,
            mut workspace,
            current,
            has_workspace,
        } = opened;
        let Some(incoming_profile) = profile.id() else {
            self.report(ErrorReport::new(
                "Couldn't switch profiles",
                "The selected profile has no identity. Nothing was changed.",
            ));
            return;
        };

        // File the outgoing strip while its views still name the outgoing
        // workspace. This is the same boundary `switch_workspace` uses.
        if persist_outgoing {
            self.capture_geometry();
            self.runtime.settings.save_all(
                &self.runtime.profile_dir,
                &self.instance.slot,
                &mut self.runtime.written,
            );
            self.runtime.workspace.save();
        }
        let outgoing_profile = self.runtime.profile.id();
        let outgoing_workspace = self.runtime.current;
        let outgoing_was_live = self.runtime.live_workspace;

        workspace.set_store_waker(self.store_waker.clone());
        workspace.set_actor(koru_workspace::actor_label(
            profile.name().as_deref(),
            profile.email().as_deref(),
            Some(self.instance.slot.as_str()),
        ));
        if has_workspace {
            self.host.workspace_opened(incoming_profile, current);
        }

        self.runtime.profile = profile;
        self.runtime.profile_catalog = profiles;
        self.runtime.workspace = workspace;
        self.runtime.current = current;
        self.runtime.live_workspace = has_workspace;
        self.runtime.rebind_activation(incoming_profile);
        startup::remember_store_profile(incoming_profile);
        self.runtime.settings.remember_profile(incoming_profile);

        if outgoing_was_live && let Some(outgoing_profile) = outgoing_profile {
            self.host
                .workspace_closed(outgoing_profile, outgoing_workspace);
        }

        self.runtime.settings.workspace = has_workspace.then_some(current);
        self.runtime.settings.adopt_tabs_of(current);
        if has_workspace {
            self.runtime
                .settings
                .set_opened_in(current, koru_workspace::open::now_ms());
            reopen_remembered_documents(&mut self.runtime.workspace, &self.runtime.settings);
        }
        self.releasing.clear();

        self.host.adopt_workspace(&mut self.runtime.workspace);
        self.images = ImageCache::new();
        self.image_loader.clear();
        self.image_scope = None;
        self.images_dirty = true;

        for pane in self.panes.values_mut() {
            pane.view.reseat(&self.runtime.workspace.ws);
            pane.view
                .restore(&self.runtime.workspace, &self.runtime.settings);
            pane.window.request_redraw();
        }
        self.show_active_document();
        self.sync_pane_context();
        self.sync_titles();
        crate::search_debug!(
            "[Koru][profile-switch][pid={}] complete: profile={} workspace={} elapsed_ms={}",
            std::process::id(),
            incoming_profile.to_storage(),
            current.to_storage(),
            began.elapsed().as_millis(),
        );
    }

    /// Close the open workspace and open another, in this window.
    ///
    /// **On failure nothing changes.** The tempting shape here — open the new
    /// one, and carry on with an empty document if it will not — is the old
    /// "no in-memory fall-back" bug wearing a new hat: a workspace that cannot
    /// save looks exactly like one that can until it is closed.
    fn switch_workspace(&mut self, id: koru_core::WorkspaceId) {
        if self.runtime.live_workspace && self.runtime.current == id {
            crate::search_debug!(
                "[Koru][workspace-switch][pid={}] skipped: profile={} workspace={} already_current=true",
                std::process::id(),
                trace_profile(self.runtime.profile.id()),
                id.to_storage(),
            );
            return;
        }
        // Re-armed here rather than after the swap, so the span covers the save
        // of the outgoing document too — that is part of what a switch costs.
        let switch_began = Instant::now();
        crate::search_debug!(
            "[Koru][workspace-switch][pid={}] begin: profile={} from={} to={}",
            std::process::id(),
            trace_profile(self.runtime.profile.id()),
            self.runtime.current.to_storage(),
            id.to_storage(),
        );
        // The outgoing workspace's tabs reach disk *before* anything moves.
        self.capture_geometry();
        if self.runtime.live_workspace {
            self.runtime.settings.workspace = Some(self.runtime.current);
        }
        self.runtime.settings.save_all(
            &self.runtime.profile_dir,
            &self.instance.slot,
            &mut self.runtime.written,
        );

        let name = self
            .runtime
            .profile
            .workspace(id)
            .map(|w| w.name)
            .unwrap_or_else(|| "Workspace".into());
        let profile_id = self.runtime.profile.id();
        if let Some(profile_id) = profile_id {
            // Keep both identities protected across the swap. On failure the
            // incoming reservation is released below; the outgoing live
            // document never becomes scan-eligible in between.
            self.host.workspace_opened(profile_id, id);
        }
        // **The store, not the folder.** `switch_to` opens a workspace *folder*
        // and clears the manifest on the way — so switching left the workspace
        // with no manifest, `entries()` fell back to projecting the active
        // document's tree, and Explore went empty for every workspace including
        // the one that had just been showing documents. Switching back did not
        // recover, because nothing put a manifest back.
        // Every document still open in the strip — Main and Pinned. A tab is
        // what keeps a room alive; switching Explore must not close pages the
        // strip is still showing.
        let keep = self.runtime.settings.open_document_ids();
        let switched = self
            .runtime
            .workspace
            .open_from_store_keeping(
                koru_workspace::fromstore::segment(),
                self.gateway_root.as_deref(),
                id,
                &keep,
            )
            .map_err(|e| e.to_string());
        // **Reinstalled, because the sessions are new ones.** `open_from_store`
        // joins a fresh manifest room and drops the old sessions with their
        // threads; a switch that forgot this would leave the window awake only
        // for its own keystrokes and deaf to the workspace it had just moved to.
        self.runtime
            .workspace
            .set_store_waker(self.store_waker.clone());
        // The switch is what the agent's routing keys on, so it is told before
        // anything can fail below: an entry naming the workspace this window
        // *left* is worse than none at all.
        if let Some(profile_id) = profile_id {
            let profile_name = self.runtime.profile.name().unwrap_or_default();
            self.host
                .agent_showing(profile_id, id, &profile_name, &name, &self.pane_pins());
        }
        if let Err(e) = switched {
            if let Some(profile_id) = profile_id {
                self.host.workspace_closed(profile_id, id);
            }
            crate::search_debug!(
                "[Koru][workspace-switch][pid={}] failed: profile={} from={} to={} error={e}",
                std::process::id(),
                trace_profile(self.runtime.profile.id()),
                self.runtime.current.to_storage(),
                id.to_storage(),
            );
            startup::fatal(&format!(
                "Koru could not open that workspace:

{e}

                 Nothing was written, and what you had open is still open."
            ));
            return;
        }
        let outgoing = self.runtime.current;
        let outgoing_was_live = self.runtime.live_workspace;
        self.runtime.current = id;
        self.runtime.live_workspace = true;
        if outgoing_was_live && let Some(profile_id) = profile_id {
            self.host.workspace_closed(profile_id, outgoing);
        }
        self.runtime.settings.workspace = Some(id);
        // The Main strip is window-scoped — do not replace it with a
        // per-workspace session list.
        // "Last opened" is **when you looked at it on this machine**, so it is a
        // settings key rather than a CRDT write a peer would overwrite with a
        // time from a different computer.
        self.runtime
            .settings
            .set_opened_in(id, koru_workspace::open::now_ms());
        // Nothing here is owed any more for rooms we did not keep.
        self.releasing.clear();
        // Re-open every document still named by the strip (rooms already in
        // `keep` are cheap swaps; anything missing is fetched once).
        //
        // **The expensive half of a switch, and the reason one is felt.** Each
        // remembered tab is a room joined, a snapshot fetched and a document
        // decoded, all on this thread — so the cost is per *tab*, not per
        // workspace.
        {
            let _phase = timing::Watch::start("switch.reopen_tabs");
            reopen_remembered_documents(&mut self.runtime.workspace, &self.runtime.settings);
        }
        // Prefer the document the strip is already showing over the library's
        // first entry — switching Explore must not hijack the active tab.
        if self.runtime.workspace.active().is_none() {
            let showing = self
                .runtime
                .settings
                .pinned_tab
                .and_then(|i| self.runtime.settings.pinned_tabs.get(i).map(|p| p.document))
                .or_else(|| {
                    self.runtime
                        .settings
                        .tabs
                        .get(self.runtime.settings.tab)
                        .and_then(|(w, _)| w.document_id())
                });
            if let Some(doc) = showing {
                let home = self
                    .runtime
                    .settings
                    .pinned_tabs
                    .iter()
                    .find(|p| p.document == doc)
                    .map(|p| p.workspace)
                    .or_else(|| {
                        self.runtime
                            .settings
                            .tabs
                            .iter()
                            .find_map(|(w, _)| match w {
                                crate::TabRef::Document {
                                    id: d,
                                    workspace: Some(ws),
                                } if *d == doc => Some(*ws),
                                _ => None,
                            })
                    })
                    .unwrap_or(id);
                let _ = self.runtime.workspace.open_document_in(home, doc);
            }
        }
        {
            let _phase = timing::Watch::start("switch.adopt_workspace");
            self.host.adopt_workspace(&mut self.runtime.workspace);
        }
        self.show_active_document();
        // **Push the new workspace's documents now**, rather than waiting for
        // `CONTEXT_POLL`. Two seconds of showing the *previous* workspace's
        // entries reads as the switch having listed the wrong thing — and it
        // did, for as long as it took the poll to come round. This is what the
        // "callers that act on a change this process just made" note on
        // `poll_pane_context` is about.
        self.sync_pane_context();
        // A blob hash cached from the other workspace is not *wrong* bytes — it
        // is content-addressed — but one object now serves every workspace
        // associated with it, so a hash referenced by a node here whose
        // workspace holds no record of it must draw as missing rather than
        // showing the picture. Showing content that is not in this workspace
        // is a correctness bug, not a cache miss. The cache's scope is what
        // enforces that; the decoded pixels themselves are kept, so switching
        // back is free.
        self.images.set_scope(id.as_uuid().as_u128());
        self.image_loader.clear();
        self.image_scope = None;
        // A different document with different pictures, and an empty cache to
        // decode them into.
        self.images_dirty = true;

        {
            // Live switch: reseat trackers, refresh Explore for the new
            // workspace, **leave the tab strip**. Cold restore still rebuilds
            // tabs via `View::restore` on launch.
            let _phase = timing::Watch::start("switch.restore_panes");
            for pane in self.panes.values_mut() {
                pane.view.reseat(&self.runtime.workspace.ws);
                pane.view
                    .on_workspace_switched(&self.runtime.workspace, &self.runtime.settings);
                pane.window.request_redraw();
            }
        }
        // Announce the active document again; cross-workspace history also
        // completes its deferred row/scroll restoration on this push.
        self.show_active_document();
        self.sync_pane_context();
        self.sync_titles();
        crate::search_debug!(
            "[Koru][workspace-switch][pid={}] complete: profile={} workspace={} elapsed_ms={}",
            std::process::id(),
            trace_profile(self.runtime.profile.id()),
            id.to_storage(),
            switch_began.elapsed().as_millis(),
        );
        if timing::enabled() {
            self.pending_report = Some((format!("switch to {name}"), switch_began));
        }
    }

    fn adopt_everywhere(&mut self) {
        for pane in self.panes.values_mut() {
            pane.view.adopt(&self.runtime.workspace.ws);
            pane.window.request_redraw();
        }
    }

    /// The theme is process-global and baked into every layout, so a flip in one
    /// window invalidates all of them.
    fn invalidate_everywhere(&mut self) {
        for pane in self.panes.values_mut() {
            pane.view.invalidate();
            pane.needs_frame.set(true);
            pane.window.request_redraw();
        }
    }

    /// Record the primary window's size and position so the next launch can
    /// restore them. Only the primary: `Settings` holds one geometry, and a
    /// document window's shape is not what you want back on the task outline.
    fn capture_geometry(&mut self) {
        // `settings.workspace` is the key per-document view prefs and collapsed
        // sets are filed under, and `upsert_doc_prefs` / `doc_prefs_for` no-op
        // without it. It is only set on a switch otherwise, so a session that
        // never switches would silently drop per-document colours — set it here
        // so every save records the workspace the capture is for.
        // A binned or archived workspace is not the one to reopen on, which is
        // what `live_workspace` answers.
        self.runtime.settings.workspace =
            self.runtime.live_workspace.then_some(self.runtime.current);
        // And the workspace you are *in* was, by definition, opened. Refreshed
        // during a session rather than only on a switch, so a session that never
        // switches still records that this one was looked at today.
        //
        // **Coarsely, and that is the whole point** — see
        // [`refresh_opened_in`](Settings::refresh_opened_in), which is where the
        // rule and its reasoning live. An exact stamp here is the idle-path bug
        // that rewrote three configuration files at caret-blink rate — measured
        // on macOS, where this loop runs ~140 times a second, as a whole CPU
        // core spent at idle; Windows only hid it by waking less often. Every
        // real switch still calls `set_opened_in`, which is what orders the
        // recents.
        if self.runtime.live_workspace {
            self.runtime
                .settings
                .refresh_opened_in(self.runtime.current, koru_workspace::open::now_ms());
        }
        if let Some(pane) = self.primary.and_then(|id| self.panes.get(&id)) {
            // A minimised window reports a sentinel position — (-32000,
            // -32000) on Windows — and a meaningless size. Saving either would
            // pin the next launch off-screen, so a minimised window keeps the
            // last good geometry it had while visible.
            // Maximised is remembered separately, and while maximised the
            // geometry is deliberately *not* updated: `pos`/`size` hold the
            // shape to un-maximise back to, and overwriting them with the
            // maximised bounds would leave the window nothing to restore to.
            let maximized = pane.window.is_maximized();
            self.runtime.settings.maximized = maximized;
            if pane.window.is_minimized() != Some(true) && !maximized {
                let scale = pane.window.scale_factor() as f32;
                let pos = pane.window.outer_position().ok();
                // `is_minimized` is `Option<bool>` and Windows can answer
                // `None` while already exposing its minimised sentinel rect.
                // Treat that rect as authoritative and keep the last visible
                // size as well as the last visible position.
                let sentinel = pos
                    .as_ref()
                    .is_some_and(|pos| !usable_window_position(pos.x, pos.y));
                if !sentinel {
                    let size = pane.window.inner_size();
                    if size.width > 0 && size.height > 0 {
                        self.runtime.settings.size =
                            (size.width as f32 / scale, size.height as f32 / scale);
                    }
                    if let Some(pos) = pos {
                        self.runtime.settings.pos = Some((pos.x, pos.y));
                    }
                }
            }
            pane.view.capture(&mut self.runtime.settings);
            // **The strip belongs to the workspace, not to the window.** Filed
            // on every capture rather than only on a switch, or a session that
            // never switched would leave the row for the workspace it is in
            // holding whatever was open the last time it *did*.
            if self.runtime.live_workspace {
                self.runtime.settings.remember_tabs_of(self.runtime.current);
            }
            // Pin is per-window, but only the primary window's geometry is
            // remembered — so its pin is the one that comes back on the next
            // launch, and a document window's pin is not.
            self.runtime.settings.pinned = pane.pinned;
        }
        self.capture_preferences();
    }

    /// Read the process globals back into the active Profile settings.
    ///
    /// This is the cheap half of geometry capture and is used by controls that
    /// must persist without asking the window server for geometry.
    ///
    /// **The inverse of [`apply_settings`].** A global that function writes
    /// and this does not read is a setting that resets to whatever the
    /// authority last had — the click moved the live value, then the next
    /// poll wrote the stored one back over it.
    fn capture_preferences(&mut self) {
        self.runtime.settings.theme = theme::theme_preference();
        self.runtime.settings.caret_color = theme::caret_color();
        self.runtime.settings.caret_width = theme::caret_width();
        self.runtime.settings.caret_height = theme::caret_height();
        self.runtime.settings.font = theme::ui_font();
        self.runtime.settings.body_font = theme::body_font();
        self.runtime.settings.headings_font = theme::headings_font();
        self.runtime.settings.code_font = theme::code_font();
        self.runtime.settings.density = theme::density();
        self.runtime.settings.view_mode = theme::default_view_mode();
        self.runtime.settings.canvas_bg = theme::default_canvas_bg();
        self.runtime.settings.dictation_language = crate::voice_prefs::language();
        self.runtime.settings.dictation_tier = crate::voice_prefs::tier();
        self.runtime.settings.dictation_toggle = crate::voice_prefs::toggle();
        self.runtime.settings.dictation_input = crate::voice_prefs::input();
        self.runtime.settings.spellcheck_enabled = crate::spelling::enabled();
        self.runtime.settings.spellcheck_language = crate::spelling::preferred();
        self.runtime.settings.zoom = self.zoom;
    }

    /// Close one window.
    ///
    /// Closing a window closes *that view*, whichever one it is. The app exits
    /// when the last window goes, not when the first one does: a document
    /// opened from the outline is a window in its own right, and the outline
    /// having spawned it is not a reason for it to take the document down.
    ///
    /// Reached two ways now — the OS close button, and our own once the caption
    /// row is ours to draw.
    /// Hide every Koru window while the agent remains its way back.
    ///
    /// **Returns whether it happened, and the caller must respect a `false`.**
    /// The agent connection is checked *first*; if it is absent, nothing is
    /// hidden and the press closes the window as it always did. An app with no
    /// window and no agent is a process nobody can see, nobody can reach and nobody can end
    /// short of Task Manager — while it still holds the workspace it opened.
    ///
    /// The session reaches disk on the way in. Being in the tray is a state a
    /// machine can be shut down from, and "I put it in the tray last night" must
    /// not be how a day's work is lost.
    fn hide_to_tray(&mut self) -> bool {
        if !HAS_NOTIFICATION_AREA || self.panes.is_empty() || !self.host.agent_available() {
            return false;
        }
        self.capture_geometry();
        self.runtime.settings.save_all(
            &self.runtime.profile_dir,
            &self.instance.slot,
            &mut self.runtime.written,
        );
        self.runtime.workspace.save();
        // **On macOS the application-level hide is the *whole* hide.** Ordering
        // each window out as well is what breaks the way back: macOS's unhide
        // — what a Dock click performs — restores the *app*, and the windows it
        // finds are still ordered out, so Koru comes forward with its menu bar
        // and nothing else on screen. Exactly what the bug report showed.
        if !cfg!(target_os = "macos") {
            for pane in self.panes.values() {
                pane.window.set_visible(false);
                if let Some(native) = pane.inspector_window.as_ref() {
                    native.set_visible(false);
                }
            }
        }
        hide_application(true);
        // Not hidden *yet*: `hide:` is asynchronous.
        self.seen_hidden = false;
        self.in_tray = true;
        true
    }

    /// Make every window available again without choosing which one is front.
    fn show_from_tray(&mut self) {
        self.in_tray = false;
        // Undo `hide_application`. Harmless when macOS already unhid us,
        // which is what a Dock click does by itself.
        hide_application(false);
        for pane in self.panes.values_mut() {
            if !pane.view.visible_after_restore() {
                continue;
            }
            pane.window.set_visible(true);
            pane.view.window_shown();
            if let Some(native) = pane.inspector_window.as_ref() {
                native.set_visible(true);
            }
            // Hidden windows stop being redrawn, so what comes back would
            // otherwise be whatever was on screen when it went — and after a
            // remote edit that is stale.
            pane.window.request_redraw();
        }
    }

    /// The pane an Agents ask lands in when no pane names a better one: the
    /// primary while it is a tab container, else the first container pane.
    /// `None` on a shell with only special windows (search, feedback), where
    /// the ask has no tab strip to land in and does nothing rather than
    /// opening a session into a surface that cannot show one.
    fn container_pane(&self) -> Option<WindowId> {
        self.primary
            .filter(|id| {
                self.panes
                    .get(id)
                    .is_some_and(|pane| pane.view.is_container())
            })
            .or_else(|| {
                self.panes
                    .iter()
                    .find(|(_, pane)| pane.view.is_container())
                    .map(|(id, _)| *id)
            })
    }

    /// Bring every window back, choosing the primary for a general restore.
    fn restore_from_tray(&mut self) {
        self.show_from_tray();
        // The one that is *the* window comes forward; the rest keep their order
        // behind it rather than each stealing focus in turn.
        if let Some(pane) = self.primary.and_then(|id| self.panes.get(&id)) {
            pane.window.focus_window();
        }
        // The *application* comes forward with the windows. On macOS that is a
        // separate act, and without it the windows return behind whatever is on
        // screen — see [`activate_app`].
        activate_app();
    }

    fn close_pane(&mut self, event_loop: &ActiveEventLoop, id: WindowId) {
        if self
            .panes
            .get(&id)
            .is_some_and(|pane| pane.view.is_global_search())
        {
            // Resize events only update the in-memory value. Persist once when
            // the ephemeral search closes instead of writing a row for every
            // pixel of a live resize.
            self.persist_search_height();
        }
        if self.primary == Some(id) {
            // Its shape is the one that is remembered, so record it while it is
            // still here to ask. Nothing is promoted in its place — a document
            // window's geometry is not what should come back on the outline next
            // launch, so with the primary gone the remembered geometry simply
            // stops moving.
            self.capture_geometry();
            self.primary = None;
        }
        // Before the removal: after it there is no state left to retire, and a
        // request still queued for this window would have nowhere to be
        // refused from. Retiring marks the window gone; the adapter goes with
        // the value a line later.
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.access.retire();
        }
        // A capture this window somehow still holds is finalized, never
        // dropped — see `retire_recording`. The view's close veto makes this
        // the unreachable half; it runs so it cannot be the unhandled half.
        self.retire_recording(id);
        // Retire its reply before the native window id can be reused.
        if self
            .delegate_folder_check
            .as_ref()
            .is_some_and(|(source, _)| *source == id)
        {
            self.delegate_folder_check = None;
        }
        self.panes.remove(&id);
        if self.panes.is_empty() {
            self.runtime.settings.save_all(
                &self.runtime.profile_dir,
                &self.instance.slot,
                &mut self.runtime.written,
            );
            self.runtime.workspace.save();
            event_loop.exit();
        }
    }

    /// The agent is about to replace the files this process is running from, so
    /// every window has to go.
    ///
    /// **The ordinary close, on every pane — not a new path.** `initiate_close`
    /// is what a ✕ runs, so a document with a close-choice dialog puts it up and
    /// a last window with work the store has not confirmed asks about it, and
    /// all of that comes for free. It also means the person can *refuse*: a
    /// pane still standing when the pass is over is somebody who pressed cancel,
    /// and the honest thing to tell the agent is that this build was declined
    /// rather than to close over the top of the answer.
    fn close_for_update(&mut self, event_loop: &ActiveEventLoop) {
        self.closing_for_update = true;
        let ids: Vec<WindowId> = self.panes.keys().copied().collect();
        let asked = ids.len();
        for id in ids {
            self.initiate_close(event_loop, id);
        }
        self.closing_for_update = false;
        let refused = !self.panes.is_empty();
        tracing::info!(
            target: "koru::update",
            asked,
            remaining = self.panes.len(),
            refused,
            "close-for-update finished"
        );
        if refused {
            self.host.update_close_refused();
        }
    }

    /// Begin closing a window, giving the view the chance to veto — a popped-out
    /// document does so to put up its close-choice dialog. The two close entry
    /// points (the ✕ the view drew, and the OS's `CloseRequested`) both come
    /// here, so the dialog is shown either way.
    fn initiate_close(&mut self, event_loop: &ActiveEventLoop, id: WindowId) {
        // Before the veto question, deliberately: whether the window survives
        // or not, nothing may commit into a document somebody is closing.
        if self.dictation_pane == Some(id) {
            self.interrupt_dictation(Interruption::WindowClosed);
        }
        let vetoed = self
            .panes
            .get_mut(&id)
            .is_some_and(|p| p.view.request_close());
        if vetoed {
            if self.closing_for_update {
                // Routine on an ordinary ✕ (a close-choice dialog), news
                // during an update: this pane will count as a refusal.
                tracing::info!(target: "koru::update", "close vetoed by the view");
            }
            self.request_redraw(id);
            return;
        }
        let closing_auxiliary = self
            .panes
            .get(&id)
            .is_some_and(|pane| pane.view.is_auxiliary());
        let documents = self
            .panes
            .values()
            .filter(|pane| !pane.view.is_auxiliary())
            .count();
        if !closing_auxiliary
            && last_window_close_asks_about_the_tray(documents, self.closing_for_update)
        {
            let search_only = self
                .panes
                .get(&id)
                .is_some_and(|pane| pane.view.is_global_search());
            if search_only {
                if self.hide_to_tray() {
                    return;
                }
            } else {
                use desktop_preferences::CloseBehavior;
                let choice = match self.runtime.settings.close_behavior {
                    CloseBehavior::Background => Some(true),
                    CloseBehavior::Quit => Some(false),
                    CloseBehavior::Ask => {
                        let answer = self
                            .message_dialog()
                            .set_title("Keep Koru ready for global search?")
                            .set_description(
                                "Yes keeps Koru in the notification area, so Ctrl+Shift+K remains ready.\n\nNo quits Koru completely. Cancel leaves this window open.",
                            )
                            .set_buttons(rfd::MessageButtons::YesNoCancel)
                            .show();
                        match answer {
                            rfd::MessageDialogResult::Yes => Some(true),
                            rfd::MessageDialogResult::No => Some(false),
                            _ => None,
                        }
                    }
                };
                let Some(background) = choice else { return };
                if self.runtime.settings.close_behavior == CloseBehavior::Ask {
                    let remember = self
                        .message_dialog()
                        .set_title("Remember this choice?")
                        .set_description("Koru will remember this choice on this computer.")
                        .set_buttons(rfd::MessageButtons::YesNo)
                        .show();
                    if remember == rfd::MessageDialogResult::Yes {
                        self.set_close_behavior(if background {
                            CloseBehavior::Background
                        } else {
                            CloseBehavior::Quit
                        });
                    }
                }
                if background && self.hide_to_tray() {
                    return;
                }
                // With no tray, "keep running in the background" is a quit too.
                if !background || !HAS_NOTIFICATION_AREA {
                    self.host.last_window_closing();
                }
            }
        }
        // **Only when this is the last window.** Closing one of several leaves
        // the process — and every session with it — running, so nothing is at
        // stake and a dialog would be noise. Going to the tray is the same:
        // still running, still owed, still fine.
        if !closing_auxiliary && documents == 1 && !self.may_close_with_unsent_work() {
            return;
        }
        self.close_pane_with_restore(event_loop, id);
    }

    /// Ask before closing with work the store has not confirmed.
    ///
    /// **Two wordings, because they are two different facts.** Work this
    /// machine buffered is safe and closing costs nothing; work that could not
    /// be written anywhere is the one case where closing loses something. One
    /// wording for both would teach somebody to click through the one that
    /// matters.
    ///
    /// Buffering runs *before* the question, so the answer is about what is
    /// still owed rather than what was owed a moment ago — and so that a person
    /// who quits without reading has already had their work written down.
    fn may_close_with_unsent_work(&mut self) -> bool {
        self.runtime.workspace.buffer_everything_owed();
        let unconfirmed = self.runtime.workspace.unconfirmed();
        if unconfirmed.is_empty() {
            return true;
        }
        // **An update does not ask about work that is already safe.** Buffering
        // has just run, so the ordinary case here is work written to this
        // machine and owed to a store that is about to be restarted *by this
        // very update* — the dialog's own answer, "will be sent when the store
        // is running again", is guaranteed a minute later. The person has
        // already said "update and restart", and asking them again, once per
        // window, behind whatever else is on screen, is what turned a consented
        // upgrade into a race nobody knew they were running (ADR-151).
        //
        // The other case is not friction and is still asked: work that could
        // reach neither the store nor this disk is genuinely lost by closing,
        // and agreeing to an upgrade is not agreeing to that.
        if self.closing_for_update && !unconfirmed.would_lose_work() {
            tracing::info!(
                target: "koru::update",
                "unsent-work prompt suppressed; nothing would be lost (ADR-151)"
            );
            return true;
        }
        if self.closing_for_update {
            // The one legitimate way an update stalls on purpose: a blocking
            // dialog about work that would genuinely be lost. Category only —
            // never what the work is.
            tracing::info!(
                target: "koru::update",
                would_lose_work = unconfirmed.would_lose_work(),
                "unsent-work prompt shown"
            );
        }

        let answer = match &unconfirmed.not_buffered {
            None => self
                .message_dialog()
                .set_title("The Koru Store has not confirmed everything yet")
                .set_description(format!(
                    "{} the store has not confirmed.\n\nThey are saved on this computer and \
                     will be sent when the store is running again.\n\nClose Koru?",
                    unconfirmed.describe(),
                ))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show(),
            Some(why) => self
                .message_dialog()
                .set_title("Closing now would lose changes")
                .set_description(format!(
                    "{} that could not be sent to the Koru Store, and could not be saved on \
                     this computer either.\n\n{why}\n\nClose Koru anyway?",
                    unconfirmed.describe(),
                ))
                .set_buttons(rfd::MessageButtons::YesNo)
                .show(),
        };
        let close = answer == rfd::MessageDialogResult::Yes;
        if self.closing_for_update {
            tracing::info!(target: "koru::update", close, "unsent-work prompt answered");
        }
        close
    }

    /// Close a window, and if it was a popped-out document that asked to restore
    /// itself as a tab, route that to a container first — before the pane that
    /// holds the request is gone.
    fn close_pane_with_restore(&mut self, event_loop: &ActiveEventLoop, id: WindowId) {
        let restore = self
            .panes
            .get_mut(&id)
            .and_then(|p| p.view.take_wants_restore_tab());
        // Drain a new remembered close choice before the pane goes, and persist
        // it so the next popped-out window opens already decided.
        let remembered = self
            .panes
            .get_mut(&id)
            .and_then(|p| p.view.take_popout_close_setting());
        if let Some(choice) = remembered {
            self.runtime.settings.popout_close = choice;
            self.runtime.settings.save_all(
                &self.runtime.profile_dir,
                &self.instance.slot,
                &mut self.runtime.written,
            );
        }
        if let Some(doc) = restore {
            self.restore_tab_in_container(doc);
        }
        self.close_pane(event_loop, id);
    }

    /// Open `doc` as a tab in a container window — the receiving end of a
    /// popped-out document's "restore as a tab". The primary window is the
    /// container (it is what the manager first opens); if it has been closed,
    /// the first remaining container window takes its place. If no container is
    /// left the restore is quietly dropped: the document stays in the workspace
    /// and is reopenable from the recents list.
    fn restore_tab_in_container(&mut self, doc: NodeId) {
        let target = self
            .primary
            .filter(|id| self.panes.get(id).is_some_and(|p| p.view.is_container()))
            .or_else(|| {
                self.panes
                    .iter()
                    .find(|(_, p)| p.view.is_container())
                    .map(|(id, _)| *id)
            });
        let Some(id) = target else {
            return;
        };
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.view.restore_tab(&self.runtime.workspace.ws, doc);
        pane.window.request_redraw();
    }

    /// Open any document tabs the container asked to pop out into their own
    /// windows. Drained between events rather than inside a press, because
    /// making a window is not time-critical the way a drag is.
    fn drain_popouts(&mut self, event_loop: &ActiveEventLoop) {
        let popouts: Vec<NodeId> = self
            .panes
            .iter_mut()
            .filter_map(|(_, p)| p.view.take_wants_popout())
            .collect();
        for doc in popouts {
            self.open_document(event_loop, doc);
        }
    }

    /// Open any session the container asked to move to a new window into its
    /// own in-process container window (Q-E7 P8). The same shape
    /// [`drain_popouts`] has for documents, and drained beside it.
    fn drain_session_popouts(&mut self, event_loop: &ActiveEventLoop) {
        let sessions: Vec<koru_session_protocol::SessionId> = self
            .panes
            .iter_mut()
            .filter_map(|(_, p)| p.view.take_wants_session_popout())
            .collect();
        for session in sessions {
            self.open_session_in_new_window(event_loop, session);
        }
    }

    /// A new container window seated on `session` — the receiving end of *Move
    /// to new window*. Mirrors [`open_documents_in_new_window`]: `duplicate` a
    /// container, seat the session in it, reveal and focus it. The new window's
    /// own session backend attaches to the living session by id.
    fn open_session_in_new_window(
        &mut self,
        event_loop: &ActiveEventLoop,
        session: koru_session_protocol::SessionId,
    ) {
        let mut view = self.host.duplicate(&self.runtime.workspace.ws);
        view.open_session_tab(session);
        let title = self.app_window_title();
        let id = self.open_pane(event_loop, view, &title);
        self.reveal_pane(id);
        if let Some(pane) = self.panes.get(&id) {
            pane.window.focus_window();
        }
        self.request_redraw(id);
    }

    /// Write settings only when they differ from what is already on disk, so
    /// this can safely be called every frame.
    fn persist_if_changed(&mut self) {
        self.capture_geometry();
        // **Per file, not per struct.** The `!=` this used to do compared the
        // whole of `Settings`, so any field that moved rewrote every file — and
        // one of them moved on every tick. See
        // [`save_all_changed`](Settings::save_all_changed).
        self.runtime.settings.save_all_changed(
            &self.runtime.profile_dir,
            &self.instance.slot,
            &mut self.runtime.written,
        );
        self.patch_settings();
    }

    /// **Send what this window changed, naming only the fields that moved.**
    ///
    /// Called from the maintenance tick *and* straight out of the press that
    /// changed a setting: a Settings surface with no Apply button has to save
    /// as you click, and a second later is a second in which the app can be
    /// closed. The patch is empty and costs nothing when nothing moved, which
    /// is what makes calling it from both places safe.
    fn patch_settings(&mut self) {
        let patch =
            crate::preferences::patch(self.settings_link.authoritative(), &self.runtime.settings);
        if !patch.is_empty() {
            self.settings_link
                .save(patch, |patch, section| patch.clone().apply(section));
        }
    }

    /// Queue the search window's height into the `global-search` row.
    ///
    /// **The Agent owns that row's position and this process never touches
    /// it**, so the row's current position is taken back off the disk first:
    /// otherwise this would write back whichever position it read at launch,
    /// and an overlay moved since would be silently put back. One read, when a
    /// window closes.
    fn persist_search_height(&mut self) {
        self.search_geometry.reload_position();
        self.search_geometry.persist();
    }

    /// Record what ✕ on the last window does, and say so at once.
    ///
    /// The live copy is what this process consults when the moment comes; the
    /// patch is what every other window and the next launch see. Both, because
    /// the effect of this setting is felt much later than it is chosen and
    /// "closing Koru stopped quitting" is otherwise a mystery with no record.
    fn set_close_behavior(&mut self, value: desktop_preferences::CloseBehavior) {
        if self.runtime.settings.close_behavior == value {
            return;
        }
        let previous = self.runtime.settings.close_behavior;
        self.runtime.settings.close_behavior = value;
        self.patch_settings();
        tracing::info!(
            target: "koru::settings",
            from = ?previous,
            to = ?value,
            "what closing the last window does has changed"
        );
    }

    /// **Take an authoritative section, if one has arrived.**
    ///
    /// Two things bring one: the bounded startup wait resolving, and another
    /// window committing a change — the section-invalidation fact, refetched.
    /// Both land here, and both travel the same path a click on the control
    /// takes: the globals move, then the theme is re-applied, which invalidates
    /// and repaints every window.
    fn poll_settings(&mut self) {
        let Some(section) = self.settings_link.poll() else {
            return;
        };
        // Noted before anything can return early: a first answer that moved
        // nothing still means the next one is a change, not a startup.
        let first_answer = !std::mem::replace(&mut self.settings_heard, true);
        let startup = reminder_startup_facts(&section);
        if self.reminder_startup_facts != Some(startup) {
            self.reminder_startup_facts = Some(startup);
            for pane in self.panes.values_mut() {
                if pane.view.set_reminder_startup(startup) {
                    pane.window.request_redraw();
                }
            }
        }
        if !crate::preferences::adopt(&section, &mut self.runtime.settings) {
            return;
        }
        // Zoom before the globals: `rezoom` compares against the live value and
        // is the one path that re-sizes every viewport.
        self.rezoom(self.runtime.settings.zoom);
        apply_settings(&self.runtime.settings);
        // The few values a view keeps a copy of. Without this the next capture
        // writes the stale copy straight back, and two windows ping-pong.
        let settings = self.runtime.settings.clone();
        for pane in self.panes.values_mut() {
            if first_answer {
                pane.view.adopt_startup_preferences(&settings);
            } else {
                pane.view.adopt_preferences(&settings);
            }
        }
        self.apply_theme_preference();
    }

    /// Pin/unpin one window above other windows. Per-window: only the window
    /// whose button or chord was used changes, so a reference document can float
    /// above an unpinned main window (or the reverse).
    fn toggle_pinned(&mut self, id: WindowId) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        pane.pinned = !pane.pinned;
        let pinned = pane.pinned;
        let level = if pinned {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        };
        pane.window.set_window_level(level);
        pane.view.set_pinned(pinned);
        // The primary window's pin is the one remembered; capture it the way
        // geometry is captured, rather than snapping settings to the pane that
        // happened to toggle.
        if self.primary == Some(id) {
            self.runtime.settings.pinned = pinned;
            self.runtime.settings.save_all(
                &self.runtime.profile_dir,
                &self.instance.slot,
                &mut self.runtime.written,
            );
        }
    }

    /// Apply a pane's own pin state to its window and view. Used when a window
    /// opens, so it inherits the level it was created with rather than the OS
    /// default.
    fn sync_pinned(&mut self, id: WindowId) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let level = if pane.pinned {
            WindowLevel::AlwaysOnTop
        } else {
            WindowLevel::Normal
        };
        pane.window.set_window_level(level);
        pane.view.set_pinned(pane.pinned);
    }

    /// Match window chrome to the theme preference. `None` when System so
    /// live OS changes keep arriving as `ThemeChanged`.
    fn sync_window_theme(&mut self) {
        let dark = theme::is_dark();
        // Always re-apply `set_theme`: System↔Light can keep the same paint
        // dark flag while the preferred theme must move between None and Some.
        self.titlebar_dark = Some(dark);
        let titlebar = window_theme_for_pref();
        let border = theme::p().panel_border;
        for pane in self.panes.values() {
            pane.window.set_theme(titlebar);
            if let Some(native) = pane.inspector_window.as_ref() {
                native.sync_theme();
            }
            // The border is a palette colour, so it flips with everything else.
            // The hairline flips with it for free — it is drawn each frame from
            // the live palette. Corners stay on maximised state, not flush.
            frame::apply(&pane.window, border, pane.window.is_maximized());
        }
    }

    /// Apply the current theme preference: resolve paint, chrome, invalidate
    /// if the palette moved.
    fn apply_theme_preference(&mut self) {
        let pref = theme::theme_preference();
        let probe = system_theme::probe();
        theme::set_system_dark(probe.apps_dark);
        let dark = match pref {
            theme::ThemePreference::System => probe.apps_dark,
            theme::ThemePreference::Light => false,
            theme::ThemePreference::Dark => true,
        };
        let was = theme::is_dark();
        let paint_moved = was != dark;
        system_theme::debug_log(format!(
            "apply pref={pref:?} was_dark={was} -> dark={dark} moved={paint_moved} panes={} probe={{apps_dark={}, apps_use_light={:?}, system_use_light={:?}, source={}}} window_theme={:?}",
            self.panes.len(),
            probe.apps_dark,
            probe.apps_use_light,
            probe.system_use_light,
            probe.source,
            window_theme_for_pref(),
        ));
        theme::set_dark(dark);
        self.sync_window_theme();
        // Always invalidate when paint moved — and always request redraw so a
        // ThemeChanged that races the first frame still lands on screen.
        if paint_moved {
            self.invalidate_everywhere();
        } else {
            self.request_redraw_all();
        }
    }

    /// OS light/dark changed. Only while preference is System.
    ///
    /// The event is a wake-up only: re-read `AppsUseLightTheme` (same as cold
    /// start). Do not trust the event's `Theme` value.
    fn on_os_theme_changed(&mut self, os_theme: Theme) {
        let pref = theme::theme_preference();
        let probe = system_theme::probe();
        if theme::set_system_dark(probe.apps_dark) {
            self.request_redraw_all();
        }
        let winit_says_dark = matches!(os_theme, Theme::Dark);
        system_theme::debug_log(format!(
            "ThemeChanged winit={os_theme:?} winit_dark={winit_says_dark} pref={pref:?} paint_now={} panes={} probe={{apps_dark={}, apps_use_light={:?}, system_use_light={:?}, source={}}} registry_vs_winit={}",
            theme::is_dark(),
            self.panes.len(),
            probe.apps_dark,
            probe.apps_use_light,
            probe.system_use_light,
            probe.source,
            if probe.apps_dark == winit_says_dark {
                "agree"
            } else {
                "DISAGREE"
            },
        ));
        if pref != theme::ThemePreference::System {
            system_theme::debug_log("ThemeChanged ignored (pref not System)");
            return;
        }
        self.apply_theme_preference();
    }

    /// Catch OS flips that never delivered `ThemeChanged` (seen on the first
    /// Personalisation change after launch).
    fn poll_os_theme(&mut self) {
        let now = Instant::now();
        if self
            .last_os_theme_poll
            .is_some_and(|t| now.duration_since(t) < OS_THEME_POLL)
        {
            return;
        }
        self.last_os_theme_poll = Some(now);
        let probe = system_theme::probe();
        if theme::set_system_dark(probe.apps_dark) {
            self.request_redraw_all();
        }
        // Embedded terminals can follow System while app chrome is explicitly
        // Light or Dark. Keep their cached OS value fresh without changing it.
        if theme::theme_preference() != theme::ThemePreference::System {
            return;
        }
        if probe.apps_dark == theme::is_dark() {
            return;
        }
        system_theme::debug_log(format!(
            "poll drift paint_now={} apps_dark={} apps_use_light={:?} system_use_light={:?} — applying",
            theme::is_dark(),
            probe.apps_dark,
            probe.apps_use_light,
            probe.system_use_light,
        ));
        self.apply_theme_preference();
    }

    /// Solid for the first half of each blink period. Reset on every edit, so
    /// the caret never blinks out from under the keystroke you just made.
    fn caret_on(&self) -> bool {
        caret_phase_on(self.blink_origin)
    }

    fn wake(&mut self, id: WindowId) {
        self.blink_origin = Instant::now();
        self.request_redraw(id);
    }

    fn request_redraw(&self, id: WindowId) {
        if let Some(pane) = self.panes.get(&id) {
            pane.needs_frame.set(true);
            pane.window.request_redraw();
        }
    }

    // ------------------------------------------------------ the cross-window tab drag
    //
    // A tab dragged out of a window's strip keeps its OS capture with the
    // source for the whole gesture, so every move — and the release — arrives
    // addressed to the *source* window, in its own coordinates. "Which window
    // is the pointer over" is therefore answered from geometry here, and the
    // target is driven through its `foreign_tab_*` methods, never through an
    // event it never receives.

    /// Which other container window a point dragged out of `source` is over,
    /// and the point in that window's view space. `x, y` are the source
    /// pane's logical cursor (physical ÷ scale), as `pane.cursor` keeps it.
    fn foreign_window_at(&self, source: WindowId, x: f32, y: f32) -> Option<(WindowId, f32, f32)> {
        let pane = self.panes.get(&source)?;
        let scale = pane.scale() as f64;
        let origin = pane.window.outer_position().ok()?;
        let global = (
            origin.x as f64 + x as f64 * scale,
            origin.y as f64 + y as f64 * scale,
        );
        self.panes
            .iter()
            .filter(|(id, pane)| **id != source && pane.view.is_container())
            .find_map(|(id, pane)| {
                window_view_point(
                    pane.window.outer_position().ok()?,
                    pane.window.inner_size(),
                    pane.scale() as f64,
                    self.zoom as f64,
                    global,
                )
                .map(|(bx, by)| (*id, bx, by))
            })
    }

    /// Push the source window's pop-out-marker gate, redrawing on a change:
    /// the marker promises "release pops out", which a landing in another
    /// window makes false.
    fn set_source_targeted(&mut self, source: WindowId, targeted: bool) {
        if self
            .panes
            .get_mut(&source)
            .is_some_and(|pane| pane.view.set_foreign_targeted(targeted))
        {
            self.request_redraw(source);
        }
    }

    /// Preview, in whichever other window the pointer is over, where the tab
    /// being dragged out of `source` would land. Runs on every move of the
    /// drag; the states it drives are the target's own split preview and slot
    /// marker, so the target draws exactly what a local drag would show.
    /// Whether a window's logical cursor is still over that window. Both
    /// halves of a cross-window drag — the hover and the release — answer
    /// "not ours" from this, so they cannot disagree about whose gesture it is.
    fn cursor_inside(cursor: (f32, f32), size: winit::dpi::PhysicalSize<u32>, scale: f32) -> bool {
        let (x, y) = cursor;
        x >= 0.0 && y >= 0.0 && x < size.width as f32 / scale && y < size.height as f32 / scale
    }

    fn hover_foreign_tab(&mut self, source: WindowId) {
        let target = {
            let Some(pane) = self.panes.get(&source) else {
                return;
            };
            let Some(doc) = pane.view.pane_drag_document() else {
                // No tab drag in flight — an Esc mid-drag lands here too, and
                // a preview another window is showing must go with the drag.
                self.clear_foreign_hover();
                self.set_source_targeted(source, false);
                return;
            };
            let (x, y) = pane.cursor;
            if Self::cursor_inside(pane.cursor, pane.window.inner_size(), pane.scale() as f32) {
                self.clear_foreign_hover();
                self.set_source_targeted(source, false);
                return;
            }
            self.foreign_window_at(source, x, y)
                .map(|(id, bx, by)| (id, bx, by, doc))
        };
        let Some((target, bx, by, doc)) = target else {
            self.clear_foreign_hover();
            self.set_source_targeted(source, false);
            return;
        };
        if self.foreign_hover != Some(target) {
            self.clear_foreign_hover();
        }
        let accepted = {
            let ws = &self.runtime.workspace.ws;
            self.panes
                .get_mut(&target)
                .is_some_and(|pane| pane.view.foreign_tab_hover(ws, doc, bx, by))
        };
        if accepted {
            self.foreign_hover = Some(target);
            self.request_redraw(target);
        }
        self.set_source_targeted(source, accepted);
    }

    /// Land a tab dragged out of `source`'s strip in the window under the
    /// release, if there is one and it accepts: the target opens the document
    /// where the drop point says, and the source commits its take without the
    /// pop-out an ordinary outside release asks for. Answers whether the drop
    /// happened — `false` leaves the release to the source's own `mouse_up`.
    fn drop_foreign_tab(&mut self, source: WindowId) -> bool {
        let Some((doc, x, y)) = self.panes.get(&source).and_then(|pane| {
            // The same gate the hover applies: a release over the source's
            // own window is its own gesture — a split, a slot, a pop-out —
            // even when another Koru window lies behind it. Without this a
            // torn tab dropped on the source's edge landed in the window
            // behind, which no preview had promised.
            if Self::cursor_inside(pane.cursor, pane.window.inner_size(), pane.scale() as f32) {
                return None;
            }
            pane.view
                .pane_drag_document()
                .map(|doc| (doc, pane.cursor.0, pane.cursor.1))
        }) else {
            return false;
        };
        let Some((target, bx, by)) = self.foreign_window_at(source, x, y) else {
            return false;
        };
        let landed = {
            let ws = &self.runtime.workspace.ws;
            self.panes
                .get_mut(&target)
                .is_some_and(|pane| pane.view.foreign_tab_drop(ws, doc, bx, by))
        };
        self.foreign_hover = None;
        // The gesture ends here either way: the gate goes back to honest.
        self.set_source_targeted(source, false);
        if !landed {
            return false;
        }
        if let Some(pane) = self.panes.get_mut(&source) {
            pane.view.complete_foreign_drop();
        }
        self.request_redraw(source);
        self.request_redraw(target);
        true
    }

    /// Clear the preview another window is showing for a drag, if any.
    fn clear_foreign_hover(&mut self) {
        let Some(id) = self.foreign_hover.take() else {
            return;
        };
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.view.foreign_tab_leave();
        }
        self.request_redraw(id);
    }

    /// Whether this process should take the store down as it goes.
    ///
    /// **Only when there is no agent.** With one running, the agent owns the
    /// store's lifetime and stops it when *it* quits; doing it from here too
    /// would be two parties racing over one daemon. Without one — because the
    /// person quit it — nobody else is left, and the store would outlive every
    /// visible thing.
    ///
    /// Whether any *other* window is still attached is deliberately not asked
    /// here: this process cannot see them, and the store can. See
    /// [`stop_the_store_if_nothing_else_wants_it`].
    fn should_stop_the_store_on_exit(&self) -> bool {
        stop_the_store_on_exit(
            self.host.agent_available(),
            koru_api_client::grants::agent_running(
                koru_workspace::fromstore::segment(),
                self.gateway_root.as_deref(),
            ),
        )
    }

    /// Give a freshly created, still-hidden window its accessibility provider.
    ///
    /// The window number minted here is what every locator that window ever
    /// publishes is built from, and it is never handed out twice — see
    /// `crate::accessibility::identity`.
    fn seat_accessibility(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Arc<Window>,
    ) -> crate::accessibility::PaneAccessibility {
        let slot = self.slots.window();
        let view = self.slots.view();
        let proxy = self.wake_proxy.clone();
        let bridge = koru_window::WindowBridge::before_first_reveal(
            event_loop,
            window,
            // A screen reader's handlers run on whichever thread the platform
            // chose, and all they do is leave a note. This is how the loop
            // learns there is one to read; the note itself never travels.
            std::sync::Arc::new(move || {
                let _ = proxy.send_event(Wake::Accessibility);
            }),
        );
        if koru_env::switch(koru_env::name::KORU_ACCESSIBILITY_DEBUG) {
            eprintln!(
                "[koru:a11y] window={} view={} provider={}",
                slot.raw(),
                view.raw(),
                bridge.is_some()
            );
        }
        crate::accessibility::PaneAccessibility::new(
            slot,
            view,
            bridge.map(|bridge| Box::new(bridge) as Box<dyn koru_window::AccessibilityProvider>),
        )
    }

    /// Everything one window offers a screen reader right now.
    ///
    /// Only ever called when a client is attached *and* something changed —
    /// see `crate::accessibility::lifecycle`. That is what lets it ask the OS
    /// for the window's size: on an ordinary session this never runs at all.
    /// `scope` is [`Poll::full`](crate::accessibility::Poll::full)'s answer,
    /// asked *before* the tree is prepared: a client that has just attached is
    /// owed a coherent whole window, and everything after that may be a
    /// difference. Guessing it afterwards would be a race with the adapter,
    /// which is the authority on when a client is really there.
    fn window_semantics(
        &self,
        id: WindowId,
        scope: crate::accessibility::Scope,
    ) -> Option<crate::accessibility::WindowSemantics> {
        let pane = self.panes.get(&id)?;
        let slot = pane.access.slot();
        let scale = pane.window.scale_factor();
        let size = pane.window.inner_size();
        let client = koru_draw::geometry::Rect::new(
            0.0,
            0.0,
            size.width as f32 / scale as f32,
            size.height as f32 / scale as f32,
        );
        let view_slot = pane.access.view_slot();
        // **The window's own commands only belong to a whole snapshot.** They
        // are three nodes whose rectangles move with the window, and a window
        // resize is not a difference this pass takes — the view folds the
        // viewport into its revision, so a resize republishes the view's tree,
        // and re-sending three caption buttons with it costs nothing. What a
        // difference must not do is re-send them *without* the root that names
        // them, which is why they are in the `Whole` arm rather than always.
        let mut children = if scope == crate::accessibility::Scope::Whole {
            crate::accessibility::window_commands(
                slot,
                pane.window.is_maximized(),
                crate::accessibility::ui_lang(),
                // The window performs them; the view paints them, so the view
                // is asked where. Nothing here reconstructs the caption's
                // arithmetic.
                pane.view.window_command_bounds(),
            )
        } else {
            Vec::new()
        };
        children.extend(pane.view.semantics(view_slot, scope));
        Some(crate::accessibility::WindowSemantics {
            window: slot,
            label: pane.title.clone(),
            scale,
            client,
            // **The view's own focus state, not a second one.** Asked through
            // the same borrow, in the same pass, as the tree above it — see
            // `View::accessibility_focus`. `None` still means the window
            // itself, which is what a view with nothing focused answers.
            focus: pane.view.accessibility_focus(view_slot),
            children,
            scope,
            // Drained in the same pass, from the same borrow. Empty for a whole
            // snapshot, which replaces the occurrence map rather than merging
            // into it. See `View::accessibility_retired`.
            retired: match scope {
                crate::accessibility::Scope::Whole => Vec::new(),
                crate::accessibility::Scope::Delta => pane.view.accessibility_retired(),
            },
        })
    }

    /// The loop's accessibility pass: drain, publish what is owed, and perform
    /// whatever is still about something that exists.
    fn service_accessibility(&mut self, event_loop: &ActiveEventLoop) {
        let ids: Vec<WindowId> = self.panes.keys().copied().collect();
        // **What the Store has confirmed, per document, and only when somebody
        // is reading it.** One version-vector comparison per open document —
        // cheap, not free, and `Workspace::unconfirmed`'s note about a healthy
        // window paying nothing for a state it is not in applies here word for
        // word. Computed once for every listening window rather than per
        // window: it is the same workspace behind all of them.
        //
        // Read *before* the loop below borrows `self.panes`, which is also why
        // it cannot live inside the `listening` arm where it is used.
        let durability = self
            .panes
            .values()
            .any(|pane| pane.access.listening())
            .then(|| self.runtime.workspace.durability());
        for id in ids {
            let wants = match self.panes.get_mut(&id) {
                Some(pane) => {
                    // **The view says when its own tree is stale**, and only
                    // while somebody is reading it. `listening` is what keeps
                    // an ordinary session at zero: with no screen reader
                    // attached the revision is never even asked for, so a view
                    // pays nothing for having one.
                    if pane.access.listening() {
                        // Before the revision, never after: the view folds
                        // what it was just told into the number that says its
                        // tree changed, so a durability transition that
                        // arrived after the fingerprint was taken would not be
                        // published until something else happened to move it.
                        if let Some(durability) = durability.as_ref() {
                            pane.view.set_durability(durability);
                        }
                        let revision = pane.view.semantics_revision();
                        pane.access.observe(revision);
                        if let Some(inspector) = pane.inspector.as_mut() {
                            inspector.observe_view_revision(revision);
                        }
                    }
                    pane.access.poll()
                }
                None => continue,
            };
            let scope = if wants.full {
                crate::accessibility::Scope::Whole
            } else {
                crate::accessibility::Scope::Delta
            };
            if wants.snapshot
                && let Some(semantics) = self.window_semantics(id, scope)
                && let Some(pane) = self.panes.get_mut(&id)
            {
                if pane.access.publish(&semantics) {
                    if let Some(inspector) = pane.inspector.as_mut() {
                        inspector.observe_semantics(&semantics.children);
                    }
                }
            }
            let resolved = match self.panes.get_mut(&id) {
                Some(pane) => pane.access.resolve(),
                None => continue,
            };
            for action in resolved {
                if !self.perform_accessibility_action(event_loop, id, &action) {
                    return;
                }
            }
        }
    }

    /// Do what a screen reader asked, through the owner that already does it.
    ///
    /// `false` when the window is gone as a result, which is the one case a
    /// caller must not keep walking past.
    fn perform_accessibility_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        resolved: &crate::accessibility::Resolved,
    ) -> bool {
        use crate::accessibility::{ViewSlot, part};
        // **A request that is not about the window is the view's.** The split
        // is the view slot and nothing else: `ViewSlot::WINDOW` is the window's
        // own chrome, which only the host can perform; everything else was
        // minted by a view and is routed back to it.
        if let Some(pane) = self.panes.get_mut(&id)
            && let Some(inspector) = pane.inspector.as_mut()
        {
            inspector.host_changed();
            inspector.focus = false;
        }
        if resolved.occurrence.view != ViewSlot::WINDOW {
            return self.perform_view_accessibility_action(event_loop, id, resolved);
        }
        // **A client sends what it likes, not what we advertised.** Windows
        // turns one `IUIAutomationInvokePattern::Invoke` into a `SetFocus`
        // followed by the invoke, and `accesskit_windows` forwards both
        // whether or not the node offered `Action::Focus` — an advertised
        // action set is a hint to a client, never a filter on what arrives.
        // So the answer to an action nothing here performs is to do nothing,
        // visibly. The window's own commands have no focus ring to light: the
        // focus a view *does* own is routed above. See `guide/contributing/deps/ACCESSKIT.md`.
        if resolved.action != accesskit::Action::Click {
            if koru_env::switch(koru_env::name::KORU_ACCESSIBILITY_DEBUG) {
                eprintln!(
                    "[koru:a11y] ignored {:?} on {}",
                    resolved.action,
                    resolved.occurrence.locator()
                );
            }
            return true;
        }
        let Some(window) = self.panes.get(&id).map(|pane| pane.window.clone()) else {
            return false;
        };
        // The same three calls the caption buttons make. An AT action is not a
        // second command path into the window: it is the same one, reached from
        // somewhere else.
        match resolved.occurrence.part {
            p if p == part::MINIMIZE => window.set_minimized(true),
            p if p == part::MAXIMIZE => window.set_maximized(!window.is_maximized()),
            p if p == part::CLOSE => {
                self.initiate_close(event_loop, id);
                return self.panes.contains_key(&id);
            }
            _ => {}
        }
        true
    }

    /// Hand a request to the view that minted the control, then settle what
    /// the view asked for as a result.
    ///
    /// **The settlement is the same one a keystroke gets**, and that is the
    /// point: the view performs an AT action through the function the keyboard
    /// already reaches, so it comes back wanting the same things — a document
    /// opened, a window minimised, the other windows told the document moved.
    /// Skipping any of them would make an AT action a second, quieter command
    /// path, which is exactly what §5 forbids.
    fn perform_view_accessibility_action(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        resolved: &crate::accessibility::Resolved,
    ) -> bool {
        let document_before = self.runtime.workspace.ws.version();
        let Some(pane) = self.panes.get_mut(&id) else {
            return false;
        };
        let handled = pane.view.accessibility_action(
            &mut self.runtime.workspace.ws,
            resolved.occurrence,
            resolved.action,
            resolved.data.as_ref(),
        );
        if !handled {
            if koru_env::switch(koru_env::name::KORU_ACCESSIBILITY_DEBUG) {
                eprintln!(
                    "[koru:a11y] view declined {:?} on {}",
                    resolved.action,
                    resolved.occurrence.locator()
                );
            }
            return true;
        }
        let open = pane.view.take_wants_open();
        let request = pane.view.take_window_request();
        if self.runtime.workspace.ws.version() != document_before {
            self.runtime.workspace.touched();
            // Another window looking at the same document has to see it too.
            self.adopt_others(id);
        }
        if let Some(doc) = open {
            self.open_document(event_loop, doc);
        }
        if let Some(request) = request
            && !self.perform_window_request(event_loop, id, request)
        {
            return false;
        }
        if self.panes.contains_key(&id) {
            self.wake(id);
            // The tree the client is reading has just changed underneath it.
            self.invalidate_accessibility(id);
        }
        true
    }

    /// Perform a [`WindowRequest`] a view raised outside a press.
    ///
    /// `false` when the window is gone as a result. [`WindowRequest::Drag`] is
    /// the one that cannot be honoured here: winit only moves a window while
    /// the button that asked is still down, and there is no button.
    fn perform_window_request(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        request: WindowRequest,
    ) -> bool {
        let Some((window, size_locked)) = self
            .panes
            .get(&id)
            .map(|pane| (pane.window.clone(), pane.size_locked))
        else {
            return false;
        };
        match request {
            WindowRequest::Drag => {}
            WindowRequest::Place(what) => {
                // Snap rows are part of the recording size lock.
                if !size_locked {
                    let d = crate::settings::DEFAULT_WINDOW_SIZE;
                    snap::place(&window, what, (d.0 as u32, d.1 as u32));
                }
            }
            WindowRequest::Focus => {
                window.set_minimized(false);
                window.focus_window();
                activate_app();
            }
            WindowRequest::Minimise => window.set_minimized(true),
            WindowRequest::ToggleMaximise => {
                if !size_locked {
                    window.set_maximized(!window.is_maximized());
                }
            }
            WindowRequest::TogglePin => self.toggle_pinned(id),
            WindowRequest::PlaceAndLockExact { width, height } => {
                self.place_and_lock_exact(id, width, height);
            }
            WindowRequest::LockCurrentSize => {
                let size = window.inner_size();
                self.place_and_lock_exact(id, size.width, size.height);
            }
            WindowRequest::UnlockSizeAndRestore => self.unlock_size_and_restore(id),
            // The capture itself is the shell's — the HWND is. Start spawns
            // a worker; stop hands the parked session to a finalize worker.
            WindowRequest::StartRecording(config) => self.start_recording(id, config),
            WindowRequest::StopRecording => self.stop_recording(id),
            WindowRequest::Close => {
                self.initiate_close(event_loop, id);
                return self.panes.contains_key(&id);
            }
            WindowRequest::HideToTray => {
                if !self.hide_to_tray() {
                    self.initiate_close(event_loop, id);
                }
                return self.panes.contains_key(&id);
            }
            WindowRequest::SetCloseBehavior(value) => self.set_close_behavior(value),
            WindowRequest::TestUpgrade => {
                match self.launch_test_updater() {
                    Ok(()) => self.initiate_close(event_loop, id),
                    Err(error) => self.report(ErrorReport::new("Test upgrade", error)),
                }
                return self.panes.contains_key(&id);
            }
        }
        true
    }

    /// Hold a window at an exact recording size — **physical** pixels, so a
    /// 1920×1080 preset means those pixels on any display scale.
    ///
    /// The current geometry is remembered for [`unlock_size_and_restore`];
    /// the minimum size is lifted so a dev size like 640×360 is not clamped
    /// back to it; and while the lock holds, `Pane::resize_edge_at`, the snap
    /// rows and maximise are all refused, so the person cannot trip the
    /// recorder's `capture-geometry-changed` failure.
    fn place_and_lock_exact(&mut self, id: WindowId, width: u32, height: u32) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        if pane.size_locked {
            return;
        }
        let window = pane.window.clone();
        let saved = SavedGeometry {
            position: window.outer_position().ok(),
            inner_size: window.inner_size(),
            maximised: window.is_maximized(),
        };
        window.set_min_inner_size(None::<winit::dpi::PhysicalSize<u32>>);
        snap::place_exact(&window, (width, height));
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.saved_geometry = Some(saved);
            pane.size_locked = true;
        }
    }

    /// Undo [`place_and_lock_exact`]: the remembered geometry — or the
    /// maximised state — returns, the view's minimum size is re-applied, and
    /// the resize border works again.
    fn unlock_size_and_restore(&mut self, id: WindowId) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        if !pane.size_locked {
            return;
        }
        let window = pane.window.clone();
        let (min_w, min_h) = pane
            .view
            .preferred_window_size()
            .map_or(crate::settings::MIN_WINDOW_SIZE, |(_, min)| min);
        window.set_min_inner_size(Some(winit::dpi::LogicalSize::new(min_w, min_h)));
        if let Some(saved) = pane.saved_geometry {
            if saved.maximised {
                window.set_maximized(true);
            } else {
                if window.is_maximized() {
                    window.set_maximized(false);
                }
                if let Some(position) = saved.position {
                    window.set_outer_position(position);
                }
                let _ = window.request_inner_size(saved.inner_size);
            }
        }
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.saved_geometry = None;
            pane.size_locked = false;
        }
    }

    /// Something a window's semantics depend on changed.
    ///
    /// One increment on the pane, and nothing else unless a screen reader is
    /// listening — which is what keeps this callable from the ordinary title,
    /// resize and maximise paths without putting work on them.
    fn invalidate_accessibility(&mut self, id: WindowId) {
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.access.invalidate();
        }
    }

    /// Draw a pane's first frame into its still-hidden window, then show it —
    /// maximised first when the session says so.
    ///
    /// The draw is direct rather than `request_redraw` because a hidden window
    /// receives no `WM_PAINT`: the request would never deliver and the window
    /// would never earn its reveal. Presenting to a hidden surface is the
    /// pattern `koru-window`'s search panel has always used. Calling this on
    /// an already-visible pane costs nothing: the frame guard skips the draw
    /// and the visibility calls are no-ops.
    fn reveal_pane(&mut self, id: WindowId) {
        self.redraw(id);
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        // The provider was installed at creation; this only records that the
        // window has been on screen, which is the boundary a later plan's
        // "was this ever visible" questions are asked against.
        pane.access.revealed();
        if pane.maximize_on_reveal.replace(false) {
            // `set_maximized` shows the window on Windows; the explicit
            // `set_visible` after it keeps winit's visibility flag in step —
            // the same "always tell winit" rule `koru_window::reveal`
            // records.
            pane.window.set_maximized(true);
        }
        pane.window.set_visible(true);
    }

    /// Tell a pane's view that the window filled the screen, or stopped, and
    /// restyle the frame while it has.
    ///
    /// There is no "maximised changed" event — it arrives as a resize like any
    /// other — so the last state is remembered and this only acts on a real
    /// change. Two things depend on it: the caption's maximise button becomes a
    /// restore button, and [`frame::apply`] squares corners and turns off the
    /// drop shadow (a mere snap must keep both — see [`Self::sync_flush`] for
    /// the hairline, which does follow flush edges).
    fn sync_maximized(&mut self, id: WindowId) {
        let border = theme::p().panel_border;
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let now = pane.window.is_maximized();
        if pane.maximized == Some(now) {
            return;
        }
        pane.maximized = Some(now);
        pane.view.set_maximized(now);
        // The maximise command's label is *Maximize* or *Restore*, and its
        // identity is neither — see `accessibility::window_commands`.
        pane.access.invalidate();
        frame::apply(&pane.window, border, now);
    }

    /// Recompute which of a window's edges are against the screen's, and say
    /// whether that moved.
    ///
    /// Edge-triggered for the same reason [`Self::sync_maximized`] is: there is
    /// no event for it, the answer is unchanged on the overwhelming majority of
    /// moves, and asking costs three user32 calls. Only the drawn hairline
    /// follows this answer — corners stay on maximised state so a snapped
    /// window keeps its drop shadow.
    fn sync_flush(&mut self, id: WindowId) -> bool {
        let Some(pane) = self.panes.get_mut(&id) else {
            return false;
        };
        let now = hairline::edges(&pane.window);
        if pane.flush == now {
            return false;
        }
        pane.flush = now;
        true
    }

    /// Settle native move streams without putting OS queries in the per-pixel
    /// event path. Returns true while at least one window is still moving.
    fn settle_window_moves(&mut self, now: Instant) -> bool {
        let settled: Vec<_> = self
            .panes
            .iter()
            .filter_map(|(id, pane)| {
                pane.moved_at
                    .filter(|at| now.saturating_duration_since(*at) >= WINDOW_MOVE_SETTLE)
                    .map(|_| *id)
            })
            .collect();
        for id in settled {
            if let Some(pane) = self.panes.get_mut(&id) {
                pane.moved_at = None;
            }
            if self.sync_flush(id) {
                self.request_redraw(id);
            }
        }
        self.panes.values().any(|pane| pane.moved_at.is_some())
    }

    fn sync_viewport(&mut self, id: WindowId) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let size = pane.window.inner_size();
        let scale = pane.window.scale_factor() as f32;
        if size.width > 0 && size.height > 0 {
            // Divide by zoom so a view lays out for the content it can actually
            // show: at 200% it reflows as if the window were half as wide, and
            // the root transform scales that layout back up to fill the window.
            pane.view.set_viewport(
                size.width as f32 / scale / self.zoom,
                size.height as f32 / scale / self.zoom,
            );
        }
        self.reconcile_surface(id);
    }

    fn reconcile_surface(&mut self, id: WindowId) {
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let inner = pane.window.inner_size();
        let geometry = crate::native_surface::WindowGeometry {
            zoom: self.zoom,
            scale: pane.window.scale_factor(),
            inner: (inner.width, inner.height),
            maximized: pane.maximized.unwrap_or_else(|| pane.window.is_maximized()),
            band_is_ours: koru_window::resize::is_ours(),
        };
        if let Some(error) = crate::native_surface::reconcile(&mut pane.view, geometry) {
            pane.view.native_surface_failed(error);
        }
    }

    /// Change the display zoom: clamp + snap, mirror into the `theme` global,
    /// then re-sync every window's viewport. A width change is what reflows the
    /// active tab of each window — once, the same path the F6 font change takes
    /// — so this is a discrete reflow per step, never a per-frame one. The zoom
    /// indicator arms itself in `MainView` by noticing the global moved, so it
    /// is not armed here.
    fn rezoom(&mut self, next: f32) {
        let z = clamp_zoom(next);
        if z == self.zoom {
            return;
        }
        self.zoom = z;
        theme::set_zoom(z);
        let ids: Vec<WindowId> = self.panes.keys().copied().collect();
        for id in &ids {
            self.sync_viewport(*id);
        }
        for id in ids {
            self.request_redraw(id);
        }
    }

    fn inspector_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: WindowId,
        event: &WindowEvent,
    ) -> bool {
        let Some(pane) = self.panes.get_mut(&id) else {
            return false;
        };
        if pane.focused
            && let Some(changed) = pane
                .inspector
                .as_mut()
                .and_then(|i| i.spacing_shortcut(event, self.mods))
        {
            if changed {
                self.request_redraw(id);
            }
            return true;
        }
        if let WindowEvent::KeyboardInput {
            event,
            is_synthetic,
            ..
        } = event
            && pane.focused
            && koru_keys::registry::get(koru_keys::ShortcutId::ToggleUiInspector)
                .chord
                .matches(&event.logical_key, event.text.as_deref(), self.mods)
        {
            if crate::inspector::shortcut_pressed(event.state, event.repeat, *is_synthetic) {
                if let Some(inspector) = pane.inspector.as_mut() {
                    if inspector.shortcut_requests_close() {
                        pane.close_inspector();
                    }
                } else {
                    pane.view.mouse_up(&mut self.runtime.workspace.ws);
                    pane.pressed = false;
                    self.open_inspector_window(event_loop, id);
                }
                let Some(pane) = self.panes.get_mut(&id) else {
                    return true;
                };
                pane.access.invalidate_structure();
                pane.sync_cursor_icon();
                self.request_redraw(id);
            }
            return true;
        }
        let scale = pane.scale();
        let Some(inspector) = pane.inspector.as_mut() else {
            return false;
        };
        let result = inspector.source_event(event, scale, self.zoom);
        if result.structure_changed {
            pane.access.invalidate_structure();
        }
        if result.close {
            pane.close_inspector();
        }
        pane.sync_cursor_icon();
        if result.redraw {
            self.request_redraw(id);
        }
        result.consumed
    }

    fn redraw(&mut self, id: WindowId) {
        // **Nothing asked for this frame, so do not draw one.**
        //
        // macOS delivers `RedrawRequested` continuously whether or not anything
        // changed — measured at ~44/s on an idle window against fewer than 60
        // requests from this app in eleven seconds — and re-rendering each one
        // costs a full CPU core. Windows gets this guard for free, because
        // `WM_PAINT` follows an invalidation; here it has to be explicit.
        //
        // The surface keeps its last contents, so skipping is a true no-op on
        // screen. Safe because every source of change already asks for its own
        // redraw, which is the rule `AGENTS.md` states — this makes the rule
        // load-bearing rather than advisory.
        if self
            .panes
            .get(&id)
            .is_some_and(|p| !p.needs_frame.replace(false))
        {
            return;
        }
        let _stall = timing::Watch::start("redraw");
        let caret_on = self.caret_on();
        let mut timing = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
        // Recorded here rather than where the timer runs, so it is always what
        // was last *drawn* — which is what makes "has anything changed" answerable.
        self.caret_phase = Some(caret_on);
        let Some(pane) = self.panes.get_mut(&id) else {
            return;
        };
        let scale = pane.scale();
        let cursor = pane.cursor;
        let t = framestats::Phase::start();
        // Clearing the scene is the frame's business, not a view's. A view may
        // be one region of a window rather than the whole of it, and one that
        // reset the scene itself would wipe whatever was drawn beside it.
        self.scene.reset();
        // Views draw in logical pixels; the root transform does the scaling, and
        // for a hosted view it will carry the offset too. `scale` is the OS DPI
        // factor and `zoom` the user's display zoom on top of it; the viewport
        // and pointer are pre-divided by `zoom`, so a view still lays out and
        // hit-tests in unscaled logical space and never sees the multiplier.
        let pointer = (cursor.0 / self.zoom, cursor.1 / self.zoom);
        let inspection_root = Affine::scale(scale * self.zoom as f64);
        if let Some(inspector) = pane.inspector.as_mut() {
            inspector.constrain(
                pane.surface.config.width as f32 / scale as f32 / self.zoom,
                pane.surface.config.height as f32 / scale as f32 / self.zoom,
            );
        }
        let inspection_frame = pane
            .inspector
            .as_mut()
            .filter(|inspector| inspector.capture_live())
            .map(|inspector| {
                inspector.frame += 1;
                let started = std::time::Instant::now();
                let viewport = vello::kurbo::Rect::new(
                    0.0,
                    0.0,
                    pane.surface.config.width as f64 / scale / self.zoom as f64,
                    pane.surface.config.height as f64 / scale / self.zoom as f64,
                );
                let frame =
                    koru_draw::inspect::Frame::begin(inspector.frame, inspection_root, viewport);
                let owner = koru_draw::inspect::scope(
                    "Main window",
                    "container",
                    inspection_root,
                    viewport,
                    |record| {
                        record.property(
                            "layout",
                            "window / overlay",
                            "workspace host; only instrumented descendants appear",
                        );
                    },
                );
                (started, frame, owner)
            });
        {
            let _phase = timing::Watch::start("redraw.draw");
            pane.view.draw(
                &mut self.scene,
                &self.runtime.workspace,
                &self.images,
                Affine::scale(scale * self.zoom as f64),
                caret_on,
                pointer,
            );
        }
        if let Some((started, frame, owner)) = inspection_frame {
            drop(owner);
            let snapshot = frame.finish();
            if let Some(inspector) = pane.inspector.as_mut() {
                inspector.capture_micros = started.elapsed().as_micros();
                inspector.accept(snapshot);
            }
        }
        if let Some(inspector) = pane.inspector.as_mut() {
            inspector.draw_overlay(&mut self.scene, inspection_root);
            if (inspector.capture_live() || inspector.dirty_semantics)
                && let Some(native) = pane.inspector_window.as_mut()
            {
                native.invalidate();
                inspector.dirty_semantics = false;
            }
        }
        timing.0 = t.stop();

        let (width, height) = (pane.surface.config.width, pane.surface.config.height);
        if width == 0 || height == 0 {
            return;
        }
        // The window's own outline, once the compositor's has gone off-screen.
        // After the view, because it is the outermost thing; in physical
        // pixels, because it has to be exactly one of them. `flush` is read,
        // never recomputed — see [`hairline::edges`].
        hairline::draw(
            &mut self.scene,
            width,
            height,
            pane.flush,
            theme::p().panel_border,
        );
        let device = &self.context.devices[pane.surface.dev_id];
        // Shared with every other window on this device — see `renderers`. A
        // window whose device has somehow gone has nothing to draw with, and
        // skipping the frame is the only honest answer.
        let Some(renderer) = self.renderers.get_mut(&pane.surface.dev_id) else {
            return;
        };

        let t = framestats::Phase::start();
        let rendered = {
            let _phase = timing::Watch::start("redraw.render");
            renderer.render_to_texture(
                &device.device,
                &device.queue,
                &self.scene,
                &pane.surface.target_view,
                &vello::RenderParams {
                    base_color: theme::p().bg,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
        };
        timing.1 = t.stop();
        if let Err(e) = rendered {
            eprintln!("vello render failed: {e:?}");
            return;
        }

        // **The number that matters for latency.** With a vsync'd surface and a
        // frame already in flight, this is where the thread blocks.
        let t = framestats::Phase::start();
        let acquired = {
            let _phase = timing::Watch::start("redraw.acquire");
            pane.surface.surface.get_current_texture()
        };
        timing.2 = t.stop();
        let frame = match acquired {
            Ok(f) => f,
            Err(e) => {
                eprintln!("surface texture unavailable: {e:?}");
                return;
            }
        };
        let _present_phase = timing::Watch::start("redraw.present");
        let mut encoder = device
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("blit"),
            });
        // vello 0.8 has no `render_to_surface`: it draws into an intermediate
        // texture and getting that on screen is the caller's job. `RenderSurface`
        // carries the blitter for exactly this.
        pane.surface.blitter.copy(
            &device.device,
            &mut encoder,
            &pane.surface.target_view,
            &frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        device.queue.submit([encoder.finish()]);
        let t = framestats::Phase::start();
        frame.present();
        self.frames.record(timing.0, timing.1, timing.2, t.stop());
        // The frame is on the glass, so whatever was being timed is now over.
        // Fully qualified: `timing` is a local tuple in this function.
        if let Some((what, since)) = self.pending_report.take() {
            koru_core::timing::record("first frame", since.elapsed());
            koru_core::timing::report(&what);
            // The third readiness mark, beside "gpu device ready" and
            // "gpu pipeline ready": what a slow-launch report is actually
            // about, so it is not gated on a switch. `pending_report` is
            // re-armed for workspace switches, whose first frame is not a
            // launch — only the cold start's is measured from process start.
            if what == "cold start" {
                let ms = process_started().elapsed().as_millis();
                tracing::info!(target: "koru::startup", "first frame presented in {ms}ms");
                // The machine, once — here because the window and its
                // monitors exist and have settled, and *after* the frame so
                // nothing about it is on the launch's critical path. What
                // this reads is cheap getters; the expensive half (registry,
                // disk, network) is collected on the events worker.
                crate::events_host::app_environment(Self::window_facts(&pane.window));
            }
        }
        // A view's cursor can depend on time — a press held past a threshold
        // reveals a grab hand without the pointer moving — so re-sync it on the
        // frame, not only on mouse move.
        pane.sync_cursor_icon();
        if !self.health_signaled {
            self.signal_update_health();
            self.health_signaled = true;
        }
    }

    /// What only this loop can say about the machine's displays and this
    /// window — the half of the environment snapshot that needs a `Window`,
    /// plus the keyboard layout, which macOS 26 only answers on the main
    /// thread (`environment::keyboard_layout` says what it does elsewhere).
    ///
    /// **Once per run, on the frame that has just been presented.** Every
    /// call here is one of the "cheap-looking getters" that are a round trip
    /// to the compositor on macOS (`capture_geometry` measured 3.5 ms for
    /// this family), which is exactly why it happens once after a frame and
    /// never on the idle path.
    /// Takes the window rather than looking it up: the caller is inside a
    /// mutable borrow of its own pane, and a second borrow of `self` to find
    /// the same window would not compile.
    fn window_facts(window: &winit::window::Window) -> crate::environment::WindowFacts {
        let scale = window.scale_factor();
        let inner = window.inner_size();
        let primary = window
            .current_monitor()
            .or_else(|| window.primary_monitor())
            .map(|monitor| monitor.size());
        crate::environment::WindowFacts {
            monitor_count: Some(window.available_monitors().count() as u64),
            display_width: primary.map(|size| u64::from(size.width)),
            display_height: primary.map(|size| u64::from(size.height)),
            display_scale_percent: Some((scale * 100.0).round() as u64),
            // Logical pixels: what the view lays out in, and comparable
            // across machines in a way physical pixels are not.
            window_width: Some((f64::from(inner.width) / scale.max(0.01)).round() as u64),
            window_height: Some((f64::from(inner.height) / scale.max(0.01)).round() as u64),
            dark: koru_draw::theme::is_dark(),
            keyboard_layout: crate::environment::keyboard_layout(),
        }
    }

    /// Start the same standalone helper a packaged update uses, but in its
    /// inert progress mode. A development run keeps all binaries beside one
    /// another in `target/debug`, so this deliberately does not invoke Cargo or
    /// risk compiling while the app is already closing.
    fn launch_test_updater(&self) -> Result<(), String> {
        let current = std::env::current_exe().map_err(|error| error.to_string())?;
        let helper = current
            .parent()
            .ok_or("the current executable has no directory")?
            .join(koru_update::UPDATER_EXE);
        if !helper.is_file() {
            return Err(format!(
                "{} is not built yet; run cargo build -p koru-updater once",
                helper.display()
            ));
        }
        std::process::Command::new(helper)
            .arg("--test-upgrade")
            .spawn()
            .map_err(|error| format!("could not start test updater: {error}"))?;
        Ok(())
    }
}

fn workspace_routed_press(
    current_route: Option<u64>,
    proposed_route: u64,
    stage: koru_dictation::Stage,
    active_key: Option<PhysicalKey>,
    proposed_key: PhysicalKey,
) -> koru_dictation::RoutedPress {
    koru_dictation::routed_press(
        current_route,
        proposed_route,
        stage,
        active_key == Some(proposed_key),
    )
}

fn clear_agent_route_on_disconnect(route: &mut Option<u64>, stage: koru_dictation::Stage) -> bool {
    route.take().is_some() && stage != koru_dictation::Stage::Idle
}

/// **Dictation, from the shell's side.**
///
/// The controller decides *what* the focused surface should do; a `View`
/// decides *where*. What is here is the third thing neither can do: the
/// gesture. **None of this block is reachable from a unit test** — a
/// `winit::KeyEvent` cannot be constructed and `App` needs a real window — so
/// what is here is kept to glue and everything decidable lives below it, in
/// `koru-dictation` and in the container's router (Q-V61).
///
/// A hold has a beginning and an end, and **the end is a key
/// release, which no `View` has ever seen** — the release arm of
/// `WindowEvent::KeyboardInput` updates `down_keys`, the modifiers and the
/// quarantine and then returns. That is the seam this block adds.
impl<H: Host> App<H> {
    fn agent_dictation_state(&self) -> crate::AgentDictationState {
        match self.dictation.stage() {
            koru_dictation::Stage::Idle => crate::AgentDictationState::Idle,
            koru_dictation::Stage::Live => {
                crate::AgentDictationState::Listening(self.dictation.mode())
            }
            koru_dictation::Stage::Finalising => crate::AgentDictationState::Finalising,
        }
    }

    fn report_agent_dictation_state(&mut self, refused: bool) {
        let Some(route) = self.agent_dictation_route else {
            return;
        };
        let state = if refused {
            crate::AgentDictationState::Refused
        } else {
            self.agent_dictation_state()
        };
        if self.agent_dictation_last_state == Some(state) {
            return;
        }
        self.agent_dictation_last_state = Some(state);
        self.host.agent_dictation_state(route, state);
        if matches!(
            state,
            crate::AgentDictationState::Idle | crate::AgentDictationState::Refused
        ) {
            self.agent_dictation_route = None;
            self.agent_dictation_last_state = None;
        }
    }

    fn routed_dictation_gesture(&mut self, route: u64, gesture: crate::AgentDictationGesture) {
        use crate::{AgentDictationGesture as Gesture, AgentDictationKey as Key};
        let physical = |key| {
            winit::keyboard::PhysicalKey::Code(match key {
                Key::D => winit::keyboard::KeyCode::KeyD,
                Key::M => winit::keyboard::KeyCode::KeyM,
            })
        };
        match gesture {
            Gesture::Press(key) => {
                let physical = physical(key);
                let decision = workspace_routed_press(
                    self.agent_dictation_route,
                    route,
                    self.dictation.stage(),
                    self.dictation_key,
                    physical,
                );
                match decision {
                    koru_dictation::RoutedPress::Ignore => return,
                    koru_dictation::RoutedPress::Refuse => {
                        self.host
                            .agent_dictation_state(route, crate::AgentDictationState::Refused);
                        return;
                    }
                    _ => {
                        if self.agent_dictation_route != Some(route) {
                            self.agent_dictation_last_state = None;
                        }
                        self.agent_dictation_route = Some(route);
                    }
                }
                // RegisterHotKey can coexist with a native key event on some
                // systems. If that event already began this exact hold, adopt
                // it instead of turning a toggle press into a second press.
                if decision == koru_dictation::RoutedPress::Adopt {
                    self.report_agent_dictation_state(false);
                    return;
                }
                let Some(id) = self
                    .panes
                    .iter()
                    .find_map(|(id, pane)| pane.window.has_focus().then_some(*id))
                else {
                    self.report_agent_dictation_state(true);
                    return;
                };
                self.dictation_press(id, physical);
                self.report_agent_dictation_state(!self.dictation.is_live());
            }
            Gesture::Release(key) => {
                if self.agent_dictation_route != Some(route) {
                    return;
                }
                self.finish_routed_dictation_release(physical(key));
                self.report_agent_dictation_state(false);
            }
            Gesture::Cancel(_) => {
                if self.agent_dictation_route != Some(route) {
                    return;
                }
                self.interrupt_dictation(Interruption::Suspended);
                self.report_agent_dictation_state(false);
            }
        }
    }

    fn finish_routed_dictation_release(&mut self, physical: PhysicalKey) {
        if self.dictation_key == Some(physical) {
            self.dictation_release();
        }
    }

    fn agent_disconnected(&mut self) {
        let had_route = self.agent_dictation_route.is_some();
        let cancel = clear_agent_route_on_disconnect(
            &mut self.agent_dictation_route,
            self.dictation.stage(),
        );
        if had_route {
            self.agent_dictation_last_state = None;
        }
        if cancel {
            // Clear first: servicing the cancellation must not report state
            // over the dead link. A native session, with no route, is untouched.
            self.interrupt_dictation(Interruption::Suspended);
        }
    }

    /// The chord that dictates. Read from the registry rather than written
    /// here, so `reference/SHORTCUTS.md`, the menu label and the
    /// collision test all describe the key this actually resolves.
    fn dictate_chord() -> koru_keys::Chord {
        koru_keys::registry::get(koru_keys::ShortcutId::Dictate).chord
    }

    /// **The hold began.** Routes `Begin` to the focused view and tells the
    /// controller whether it was taken.
    ///
    /// A view that refuses — a masked credential field, a read-only document,
    /// a welcome screen — ends the session before a word is heard, and the
    /// refusal is reported the way every other one is.
    fn dictation_press(&mut self, id: WindowId, physical: PhysicalKey) {
        self.begin_dictation(id, Some(physical));
    }

    /// **`/dictate` was chosen in this window.** The same beginning as the
    /// chord's, drained from the view after the menu closed.
    ///
    /// One function below both, rather than a second copy of the routing: the
    /// command exists to *teach* the shortcut, and two implementations of "how
    /// a dictation starts" would drift into two behaviours the row was supposed
    /// to be an alias for.
    ///
    /// **No key, so no release.** A hold ends when the key comes up; this one
    /// ends at the listening surface's microphone button
    /// ([`stop_dictation`](Self::stop_dictation)), at the chord in toggle mode,
    /// or at `HOLD_CAP`. That is the whole difference, and it is why the key is
    /// an `Option` rather than the two paths being separate.
    fn dictation_command(&mut self, id: WindowId) {
        self.begin_dictation(id, None);
    }

    fn begin_dictation(&mut self, id: WindowId, physical: Option<PhysicalKey>) {
        self.sync_dictation_mode();
        let Some(action) = self.dictation.press(Instant::now()) else {
            self.report_dictation_refusal();
            return;
        };
        let accepted = self
            .panes
            .get_mut(&id)
            .is_some_and(|pane| pane.view.dictate(&mut self.runtime.workspace.ws, &action));
        self.dictation.began(accepted);
        if accepted {
            self.dictation_key = physical;
            self.dictation_commanded = physical.is_none();
            self.dictation_pane = Some(id);
            self.wake(id);
        } else {
            self.dictation_key = None;
            self.dictation_commanded = false;
            self.dictation_pane = None;
            self.report_dictation_refusal();
        }
    }

    /// Ask every window whether its `/dictate` row was chosen, and begin one
    /// dictation if any did.
    ///
    /// **Drained from every pane, begun in one** — the same shape
    /// `take_wants_image` uses, and for the same reason: a flag left unread in
    /// a background window would fire the next time that window was looked at.
    /// There is one microphone, so a second window asking in the same pass is
    /// refused by the controller exactly as it would be a moment later.
    fn begin_commanded_dictations(&mut self) {
        let asked: Vec<WindowId> = self
            .panes
            .iter_mut()
            .filter_map(|(id, pane)| pane.view.take_wants_dictation().then_some(*id))
            .collect();
        for id in asked {
            self.dictation_command(id);
        }
    }

    /// **The hold ended.** Asks for the final result; it does not commit the
    /// latest partial because a key went up — the composition stays
    /// provisional until the matching `Final` arrives.
    fn dictation_release(&mut self) {
        self.dictation_key = None;
        if self.dictation.release(Instant::now()) {
            self.service_dictation();
        }
    }

    /// **The listening surface's own microphone button was pressed.**
    ///
    /// A *finish*, not an interruption: the button says "I have finished
    /// speaking", so the words are kept and the composition stays provisional
    /// until the matching `Final` arrives. It is the one press in the window
    /// that does not discard, which is why `View::dictation_hit` exists at all.
    fn stop_dictation(&mut self) {
        if self.dictation.stop(Instant::now()) {
            self.dictation_key = None;
            self.service_dictation();
        }
    }

    /// Throw an open dictation away. Every interruption ends here — a click, a
    /// keystroke, Escape, focus lost, the window closing, the machine
    /// sleeping.
    fn interrupt_dictation(&mut self, why: Interruption) {
        if !self.dictation.is_live() {
            return;
        }
        self.dictation.interrupt(why);
        self.dictation_key = None;
        self.dictation_commanded = false;
        self.service_dictation();
    }

    /// Drain the controller and route what it says into the window that is
    /// dictating.
    ///
    /// **Free when nothing is running**: `pump` on an idle controller drains an
    /// empty inbox and answers an empty vector, which is the bar anything in
    /// `about_to_wait` has to meet.
    /// **The gesture follows the person.**
    ///
    /// Read from `Settings` rather than held twice: the Settings surface
    /// writes `preferences.conf` and this is the one place the controller is
    /// told, so the two cannot drift and a change takes effect without a
    /// restart. Free — a `bool` compare — which is why it is asked on the
    /// paths that use it rather than pushed from wherever the preference moves.
    fn sync_dictation_mode(&mut self) {
        let mode = if crate::voice_prefs::toggle() {
            koru_dictation::Mode::Toggle
        } else {
            koru_dictation::Mode::PushToTalk
        };
        if self.dictation.mode() != mode {
            self.dictation.set_mode(mode);
        }
        // **And the language, for the same reason and in the same breath.**
        // Read from the global rather than held twice, so a change made in the
        // Settings section takes effect on the next press rather than on the
        // next launch.
        self.dictation.set_language(crate::voice_prefs::language());
        self.dictation.set_tier(crate::voice_prefs::tier());
        // **And the microphone, in the same breath and for the same reason.**
        // Resolved by the *worker* against what is plugged in -- a name that
        // is gone falls back to the system default rather than refusing, which
        // is `koru_voice::input`'s rule and is not repeated here.
        self.dictation.set_input(crate::voice_prefs::input());
    }

    fn service_dictation(&mut self) {
        self.sync_dictation_mode();
        let actions = self.dictation.pump(Instant::now());
        // **Before the early return**, or a refusal the controller recorded
        // without producing an action — a worker that would not start, a
        // surface that said no — would sit in the controller until something
        // else happened to drain it.
        self.report_dictation_refusal();
        if actions.is_empty() {
            self.show_dictation_status();
            self.report_agent_dictation_state(false);
            return;
        }
        let Some(id) = self.dictation_pane else {
            self.show_dictation_status();
            self.report_agent_dictation_state(false);
            return;
        };
        let mut changed = false;
        let mut closed = false;
        for action in &actions {
            closed |= action.closes();
            if let Some(pane) = self.panes.get_mut(&id) {
                changed |= pane.view.dictate(&mut self.runtime.workspace.ws, action);
            }
        }
        if closed {
            self.dictation_pane = None;
            self.dictation_key = None;
            self.dictation_commanded = false;
            // A commit is a real edit, so the document has to be published and
            // every other window on it has to hear about it.
            self.runtime.workspace.touched();
            self.adopt_others(id);
        }
        // **After the close above, not before.** The status the surface is
        // handed here is the one that says the session has ended, and reading
        // it while `dictation_pane` still named a window would leave a meter on
        // screen for a dictation that had already committed.
        self.show_dictation_status();
        if changed {
            self.request_redraw(id);
        }
        self.report_agent_dictation_state(false);
    }

    /// **Hand the listening surface what it draws**, and take it away again.
    ///
    /// Free when nothing is running and nothing was: both the current pane and
    /// the last one it was shown in are `None`, so this is two `Option` reads
    /// on an idle loop.
    fn show_dictation_status(&mut self) {
        let status = self.dictation.status();
        let Some(id) = self.dictation_pane.or(self.dictation_shown) else {
            return;
        };
        // Recorded before the push, so a window that closed mid-utterance does
        // not leave this pointing at a pane that is gone.
        self.dictation_shown = status.is_some().then_some(id);
        let changed = self
            .panes
            .get_mut(&id)
            .is_some_and(|pane| pane.view.dictation_status(status));
        if changed {
            self.request_redraw(id);
        }
    }

    /// Say what went wrong, once. A code, translated by the reporter — never
    /// the worker's own developer text, and never a transcript.
    ///
    /// The log line goes out whether or not there is a window to say it in: a
    /// refusal arriving while every pane is in the notification area is still
    /// the thing somebody will want to read afterwards.
    fn report_dictation_refusal(&mut self) {
        let Some(refusal) = self.dictation.take_refusal() else {
            return;
        };
        tracing::info!(
            target: "koru::voice",
            code = refusal.code.as_str(),
            surface = refusal.surface,
            "dictation refused"
        );
        // The window that asked, the one that was last listening, or — for a
        // refusal that arrived before any of that was recorded — the primary.
        let Some(id) = self
            .dictation_pane
            .or(self.dictation_shown)
            .or(self.primary)
        else {
            return;
        };
        // **Two refusals are not a sentence, they are a step.** Nobody has
        // agreed to voice input on this machine, or the bytes are not here —
        // and both of those are what the consent step exists to change. A
        // toast saying so would be a dead end: it names a state and offers no
        // way out of it.
        //
        // `surface` is what keeps this from firing on the wrong thing: a
        // masked credential field refuses with `WrongState` and the flag says
        // the refusal came from the *host* rather than from the worker. A
        // surface refusal is never a reason to offer a download.
        if !refusal.surface
            && matches!(
                refusal.code,
                koru_dictation::ErrorCode::ConsentRequired
                    | koru_dictation::ErrorCode::ModelMissing
            )
        {
            self.offer_voice_model(id);
            return;
        }
        let shown = self
            .panes
            .get_mut(&id)
            .is_some_and(|pane| pane.view.dictation_refused(refusal));
        if shown {
            self.request_redraw(id);
        }
    }
}

/// **First use: offering a model, recording the answer, and running the
/// download.**
///
/// The state and the thread bodies are [`crate::voice_install`]'s; what is here
/// is the routing into a window, and it is the shape a content import already
/// has — one worker, one `mpsc`, one drain in `about_to_wait`, and a persistent
/// progress toast whose Stop comes back as a `PaneRequest`.
impl<H: Host> App<H> {
    /// **Open the consent step**, once the catalog has said what there is to
    /// offer.
    ///
    /// Reaching a catalog is the world, so it happens on a thread and the step
    /// opens when the answer arrives. A second press while that is in flight —
    /// or while a download is already running — is ignored rather than queued:
    /// there is one microphone and one cache, and somebody who pressed twice
    /// meant it once.
    fn offer_voice_model(&mut self, source: WindowId) {
        // **Two silent returns used to live here**, and both of them made
        // *Install…* a button that did nothing at all -- which is
        // indistinguishable from a button that is broken. Neither state is
        // somebody's mistake, so neither is an error; both are a sentence.
        // Nothing about voice is still dialled, started or offered on an
        // installation with it switched off (Q-V44) -- what changed is that
        // the refusal is now visible.
        if let Some(line) =
            crate::voice_install::setup_refused(self.voice.busy(), koru_dictation::disabled())
        {
            tracing::info!(
                target: "koru::voice",
                busy = self.voice.busy(),
                disabled = koru_dictation::disabled(),
                "a voice set-up was not started"
            );
            self.notice(line.to_owned());
            return;
        }
        self.voice.offering = true;
        self.voice.source = Some(source);
        let tx = self.voice.tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.voice.root.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-voice-catalog".into())
            .spawn(move || {
                let chosen = crate::voice_install::resolve_offer(root);
                let _ = tx.send(crate::voice_install::VoiceMessage::Offered(Box::new(
                    chosen,
                )));
                let _ = proxy.send_event(Wake::Voice);
            })
        {
            self.voice.settle();
            tracing::warn!(
                target: "koru::voice",
                error = %error,
                "the voice catalog worker would not start"
            );
            self.notice(
                "The model catalog could not be read: a worker thread would not run.".to_owned(),
            );
        }
    }

    /// **The consent step was answered.**
    ///
    /// `false` leaves the record alone. Declining is not revoking: somebody who
    /// pressed Cancel has not said *never*, they have said *not now*, and
    /// writing a refusal would make the next press behave differently from the
    /// first for no reason they would recognise. Revoking is the settings
    /// section's, and it writes a recorded answer on purpose.
    fn answer_voice_consent(&mut self, accepted: bool) {
        let Some(entries) = self.voice.entries.take() else {
            self.voice.settle();
            return;
        };
        if !accepted {
            self.voice.settle();
            return;
        }
        // **Recorded before the download starts**, not after it succeeds. The
        // agreement is a fact about the answer somebody gave, and a download
        // that fails halfway must not leave them answering a question they
        // have already answered.
        let granted = crate::voice_install::record_consent(
            self.voice.root.as_deref(),
            entries
                .iter()
                .map(koru_voice_models::CatalogEntry::terms)
                .collect(),
        );
        self.dictation.set_consent(Some(granted.clone()));

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.voice.offering = false;
        self.voice.installing = true;
        self.voice.cancel = Some(cancel.clone());
        self.voice.cancel_hide_due = None;
        let tx = self.voice.tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.voice.root.clone();
        let started = std::thread::Builder::new()
            .name("koru-voice-install".into())
            .spawn(move || {
                let wake_proxy = proxy.clone();
                let wake = move || {
                    let _ = wake_proxy.send_event(Wake::Voice);
                };
                let done = crate::voice_install::run_install(
                    root, &entries, &granted, &tx, &cancel, &wake,
                );
                let _ = tx.send(crate::voice_install::VoiceMessage::Finished(Box::new(done)));
                let _ = proxy.send_event(Wake::Voice);
            });
        if let Err(error) = started {
            self.voice.settle();
            tracing::warn!(
                target: "koru::voice",
                error = %error,
                "the voice install worker would not start"
            );
            self.notice(
                "The speech model download could not be started: a worker thread would not run."
                    .to_owned(),
            );
            return;
        }
        let (line, fraction) = crate::voice_install::progress_line(
            crate::voice_install::lang(),
            koru_voice_models::InstallProgress {
                stage: koru_voice_models::InstallStage::Resolving,
                received: 0,
                total: 0,
            },
        );
        self.show_voice_download(line, fraction);
    }

    /// **Stop was pressed.** Cooperative: the flag is set, the action comes off
    /// the toast so it cannot be asked twice, the stage becomes *Canceling*,
    /// and the toast closes three seconds later — DESIGN's contract, and the
    /// same one the content import follows.
    fn stop_voice_download(&mut self) {
        let Some(cancel) = self.voice.cancel.as_ref() else {
            return;
        };
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        cancel.store(true, std::sync::atomic::Ordering::Release);
        self.voice.cancel_hide_due = Some(Instant::now() + crate::voice_install::CANCEL_HOLD);
        let line = koru_i18n::t(
            crate::voice_install::lang(),
            koru_i18n::Key::VoiceDownloadCanceling,
        );
        if let Some(source) = self.voice.source
            && let Some(pane) = self.panes.get_mut(&source)
            && pane.view.cancel_voice_download(line.to_owned())
        {
            pane.window.request_redraw();
        }
    }

    /// Close the accepted-Stop toast once it has been seen.
    fn maintain_voice_cancellation(&mut self, now: Instant) {
        let Some(due) = self.voice.cancel_hide_due else {
            return;
        };
        if now < due {
            return;
        }
        self.voice.cancel_hide_due = None;
        self.clear_voice_download();
    }

    /// Drain what the catalog and install workers handed back.
    ///
    /// **Free when nothing is running**: an empty `try_iter` on a channel
    /// nobody has sent on, which is the bar anything in `about_to_wait` has to
    /// meet.
    fn drain_voice_messages(&mut self) {
        let messages: Vec<_> = self.voice.rx.try_iter().collect();
        for message in messages {
            self.handle_voice_message(message);
        }
    }

    fn handle_voice_message(&mut self, message: crate::voice_install::VoiceMessage) {
        use crate::voice_install::VoiceMessage;
        match message {
            VoiceMessage::Offered(chosen) => match *chosen {
                Ok(entries) if entries.is_empty() => {
                    self.voice.settle();
                    self.voice_notice(koru_i18n::Key::VoiceDownloadReady);
                }
                Ok(entries) => {
                    let offer = crate::voice_install::VoiceOffer {
                        terms: entries
                            .iter()
                            .map(koru_voice_models::CatalogEntry::terms)
                            .collect(),
                    };
                    self.voice.entries = Some(entries);
                    // **Three failures used to share one sentence**, and the
                    // sentence named only one of them. Somebody who pressed
                    // Install while another question was on screen was told
                    // their window had closed, which is untrue and sends them
                    // somewhere useless. The cause is carried rather than
                    // collapsed, and the log says which of the three it was.
                    enum NotOffered {
                        NobodyAsked,
                        WindowGone,
                        ViewRefused,
                    }
                    let outcome = match self.voice.source {
                        None => Err(NotOffered::NobodyAsked),
                        Some(source) => match self.panes.get_mut(&source) {
                            None => Err(NotOffered::WindowGone),
                            Some(pane) => {
                                if pane.view.show_voice_consent(offer) {
                                    pane.window.request_redraw();
                                    Ok(())
                                } else {
                                    Err(NotOffered::ViewRefused)
                                }
                            }
                        },
                    };
                    let opened = outcome.is_ok();
                    if !opened {
                        // A view with no consent step is a view that cannot
                        // agree to anything, and starting a download from one
                        // would be the gate failing open. **It is also not
                        // silence any more**: the catalog was reached, an
                        // offer exists, and the window that asked has gone or
                        // cannot show it -- which somebody who pressed a
                        // button is owed a sentence about.
                        self.voice.settle();
                        tracing::info!(
                            target: "koru::voice",
                            cause = match outcome {
                                Err(NotOffered::NobodyAsked) => "no source window recorded",
                                Err(NotOffered::WindowGone) => "the source window is gone",
                                _ => "the view already has a question up",
                            },
                            panes = self.panes.len(),
                            "a voice offer could not be shown"
                        );
                        self.notice(
                            match outcome {
                                Err(NotOffered::ViewRefused) => concat!(
                                    "The speech model is ready to install, but another ",
                                    "question is already on screen. Answer that one, then ",
                                    "press Install again.",
                                ),
                                _ => concat!(
                                    "The speech model could not be offered: the window that ",
                                    "asked for it is no longer showing. Open Settings and ",
                                    "press Install again.",
                                ),
                            }
                            .to_owned(),
                        );
                    }
                }
                Err(error) => {
                    self.voice.settle();
                    tracing::info!(
                        target: "koru::voice",
                        code = error.code.as_str(),
                        detail = error.message.as_str(),
                        "no voice model can be offered"
                    );
                    // **One sentence per reason.** They used to be one
                    // sentence for all of them, which was true of exactly one
                    // -- see `voice_install::catalog_failure`.
                    self.notice(crate::voice_install::catalog_failure(&error));
                }
            },
            VoiceMessage::Progress {
                progress,
                model,
                models,
            } => {
                // A cancellation that has been accepted owns the toast until it
                // closes; a stage arriving behind it must not overwrite
                // *Canceling* with a percentage.
                if self.voice.cancel_hide_due.is_some() {
                    return;
                }
                let (line, fraction) =
                    crate::voice_install::progress_line(crate::voice_install::lang(), progress);
                self.update_voice_download(format!("{line} ({model} of {models})"), fraction);
            }
            VoiceMessage::Finished(done) => {
                let cancelled = done
                    .as_ref()
                    .as_ref()
                    .err()
                    .is_some_and(crate::voice_install::was_cancelled);
                self.voice.installing = false;
                self.voice.cancel = None;
                self.voice.entries = None;
                self.voice.offering = false;
                if cancelled {
                    // The hold is already running and closes the toast; saying
                    // anything else here would follow *Canceling* with a
                    // failure nobody failed at.
                    return;
                }
                self.clear_voice_download();
                match *done {
                    Ok(()) => {
                        tracing::info!(target: "koru::voice", "a voice model was installed");
                        self.voice_notice(koru_i18n::Key::VoiceDownloadReady);
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "koru::voice",
                            code = error.code.as_str(),
                            detail = error.message.as_str(),
                            "a voice model could not be installed"
                        );
                        self.notice(crate::voice_install::install_failure(&error));
                    }
                }
                // The Settings section holds a *reading* of the cache, so it
                // has to be told to take another. Last, after the notice, so
                // the two do not race for the same frame.
                self.voice_models_changed();
            }
            VoiceMessage::Devices(devices) => {
                self.voice.asking_devices = false;
                if crate::voice_devices::set(devices) {
                    // The Settings section holds a *reading* of this, the same
                    // way it holds one of the model cache -- so it is told
                    // rather than left to notice.
                    self.voice_models_changed();
                }
            }
            VoiceMessage::Removed(done) => {
                self.voice.removing = false;
                match *done {
                    Ok(builds) => {
                        tracing::info!(
                            target: "koru::voice",
                            builds,
                            "every installed voice model was deleted"
                        );
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "koru::voice",
                            code = error.code.as_str(),
                            "a voice model could not be deleted"
                        );
                        // **English, and reported rather than swallowed.**
                        // *Remove* is an instruction: one that quietly did
                        // nothing would leave somebody looking at a row that
                        // still says installed with no idea why.
                        self.report(ErrorReport::new(
                            "Remove speech models",
                            error.message.clone(),
                        ));
                    }
                }
                self.voice_models_changed();
            }
        }
    }

    /// **Delete every installed speech model**, off the event loop.
    ///
    /// Hundreds of megabytes and a directory rename per build, so it is a
    /// thread and an `mpsc` like every other piece of voice work here. A second
    /// press while one is running is ignored rather than queued: there is one
    /// cache, and somebody who pressed twice meant it once.
    ///
    /// **It does not touch consent.** Deleting the bytes says nothing about
    /// what somebody agreed to, and `koru-voice-consent` is explicit that the
    /// two are separate facts — so the next *Install* asks nothing again.
    fn remove_voice_models(&mut self) {
        if self.voice.busy() {
            return;
        }
        self.voice.removing = true;
        let tx = self.voice.tx.clone();
        let proxy = self.wake_proxy.clone();
        let root = self.voice.root.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-voice-remove".into())
            .spawn(move || {
                let done = crate::voice_install::remove_all_models(root);
                let _ = tx.send(crate::voice_install::VoiceMessage::Removed(Box::new(done)));
                let _ = proxy.send_event(Wake::Voice);
            })
        {
            self.voice.removing = false;
            tracing::warn!(
                target: "koru::voice",
                error = %error,
                "the voice model remover would not start"
            );
        }
    }

    /// **Ask the worker which microphones there are**, off the event loop.
    ///
    /// Blocking and bounded -- `VoiceClient::devices` dials a socket and waits
    /// -- so it is a thread and an `mpsc` like every other piece of voice work
    /// here. **It starts no worker**: drawing a picker is not somebody asking
    /// to dictate, and a machine with nothing running answers an empty list,
    /// which the settings row reads as *only what I already know*.
    ///
    /// A second ask while one is in flight is dropped rather than queued: a
    /// person who opened Settings twice wants one answer.
    fn ask_voice_devices(&mut self) {
        if self.voice.asking_devices || koru_dictation::disabled() {
            return;
        }
        self.voice.asking_devices = true;
        let client = self.dictation.client().clone();
        let tx = self.voice.tx.clone();
        let proxy = self.wake_proxy.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("koru-voice-devices".into())
            .spawn(move || {
                let devices = crate::voice_devices::read(&client);
                let _ = tx.send(crate::voice_install::VoiceMessage::Devices(devices));
                let _ = proxy.send_event(Wake::Voice);
            })
        {
            self.voice.asking_devices = false;
            tracing::warn!(
                target: "koru::voice",
                error = %error,
                "the voice device enumerator would not start"
            );
        }
    }

    /// Tell every window that what is installed has moved.
    fn voice_models_changed(&mut self) {
        for pane in self.panes.values_mut() {
            if pane.view.voice_models_changed() {
                pane.window.request_redraw();
            }
        }
    }

    fn show_voice_download(&mut self, message: String, fraction: Option<f32>) {
        if let Some(source) = self.voice.source
            && let Some(pane) = self.panes.get_mut(&source)
            && pane.view.show_voice_download(message.clone(), fraction)
        {
            pane.window.request_redraw();
            return;
        }
        self.notice(message);
    }

    fn update_voice_download(&mut self, message: String, fraction: Option<f32>) {
        if let Some(source) = self.voice.source
            && let Some(pane) = self.panes.get_mut(&source)
            && pane.view.update_voice_download(message, fraction)
        {
            pane.window.request_redraw();
        }
    }

    fn clear_voice_download(&mut self) {
        for pane in self.panes.values_mut() {
            if pane.view.clear_voice_download() {
                pane.window.request_redraw();
            }
        }
    }

    /// One translated sentence, in whichever window will take it.
    fn voice_notice(&mut self, key: koru_i18n::Key) {
        let message = koru_i18n::t(crate::voice_install::lang(), key).to_owned();
        self.notice(message);
    }
}

impl<H: Host> ApplicationHandler<Wake> for App<H> {
    /// **Deliberately empty.** A wake-up is not work; it is the end of a wait.
    /// winit runs `about_to_wait` after every batch of events, this one
    /// included, and that is where the drain has always been. Doing the work
    /// here as well would give it two entry points and one of them would drift.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, wake: Wake) {
        // Do not ask the accessibility revision to walk document rows on every
        // inspector redraw. An asynchronous app change instead retires the pin;
        // AT revisions are reused only when the normal AT pass computes them.
        if wake != Wake::Accessibility {
            for pane in self.panes.values_mut() {
                if let Some(inspector) = pane.inspector.as_mut() {
                    inspector.host_changed();
                    pane.needs_frame.set(true);
                    pane.window.request_redraw();
                }
            }
        }
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.panes.is_empty() {
            return;
        }
        let view = match self.pending.take() {
            Some(v) => v,
            None => self.host.duplicate(&self.runtime.workspace.ws),
        };
        let title = self.app_window_title();
        let id = self.open_pane(event_loop, view, &title);
        self.primary = Some(id);
        // **After the first window, not before.** The menu bar hangs off the
        // `NSApplication`, which does not exist until winit has created one.
        if self.menubar.is_none() {
            self.menubar = crate::menubar::MenuBar::install(&self.name);
            // Same moment, same reason: there is no `NSApplication` to hang
            // either on until the first window has been created.
            set_dock_icon();
        }
        self.titlebar_dark = Some(theme::is_dark());
        self.request_redraw(id);
        // A process launched for a new workspace has been granted foreground
        // permission by its parent, but Windows still requires the destination
        // window to consume it. Do that once the initial pane actually exists.
        // Background startup is the one deliberate exception: focusing a
        // window immediately before hiding it would flash and steal focus.
        if !self.background_start {
            self.reveal_pane(id);
            if let Some(pane) = self.panes.get(&id) {
                pane.window.focus_window();
            }
        }
        if self.background_start {
            self.background_start = false;
            // The surface has to hold real pixels even though nothing shows
            // now: a later tray-restore is a bare `set_visible(true)`, and a
            // surface that was never drawn would reveal as a blank flash.
            self.redraw(id);
            // `hide_to_tray` checks the agent first, so a background launch
            // with no recovery path remains visible — which, with windows now
            // created hidden, means revealing the one it could not hide.
            if !self.hide_to_tray() {
                self.reveal_pane(id);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.native_inspector_event(id, &event) {
            return;
        }
        if !self.panes.contains_key(&id) {
            return;
        }
        // **Before the application handles it**, which is `Adapter::process_event`'s
        // documented requirement rather than a preference of ours. On Windows
        // the adapter answers `WM_GETOBJECT` on the HWND and needs nothing from
        // here; macOS and the Unix adapter do use it, and the contract belongs
        // to the adapter. See `koru_window::accessibility`.
        if let Some(pane) = self.panes.get_mut(&id) {
            pane.access.process_event(&pane.window, &event);
        }
        // Before the match, deliberately: a `MouseButton` no arm below names
        // falls into the catch-all and vanishes, and that is the case worth
        // seeing. Inert unless `KORU_INPUT_DEBUG` is set.
        inputdebug::observe(&event, self.mods);
        // **Where a freeze is actually felt.** `about_to_wait` was watched from
        // the beginning and this was not, so a stall in the idle loop was
        // visible while the one that matters — a keystroke that takes 300 ms to
        // come back — was not watched at all. Inert unless `KORU_JANK` is set,
        // and it names the *kind* of event, never its content: a log somebody
        // can attach to a report must not be a record of what they typed.
        let _stall = timing::Watch::start(stall_name(&event));
        if self.inspector_event(event_loop, id, &event) {
            return;
        }
        match event {
            WindowEvent::CloseRequested => self.initiate_close(event_loop, id),

            WindowEvent::Focused(focused) => {
                self.host.window_focus_changed(focused);
                crate::search_debug!(
                    "[Koru][search-open][pid={}] focus event: pane={id:?} focused={focused} in_tray={} global_search={}",
                    std::process::id(),
                    self.in_tray,
                    self.panes
                        .get(&id)
                        .is_some_and(|pane| pane.view.is_global_search()),
                );
                // Key-press dispatch keys off this. Updated before anything
                // else in the arm so the trailing keypress of a window switch
                // — delivered after this event — sees the window as unfocused.
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.focused = focused;
                    pane.view.set_window_focused(focused);
                }
                // **The tie-break between two windows on one workspace.** The
                // agent stamps the order it observed rather than trusting a
                // clock, so "the window I was just in" wins — which is the
                // answer a person expects and the one a pid ordering gets
                // wrong after a restart.
                if focused {
                    // **Activation is when a share becomes importable.** The
                    // extension stages and activates us; this is the other end
                    // of that gesture. A flag rather than the work itself,
                    // because `about_to_wait` is where imports start and two
                    // entry points would drift (ADR-416).
                    self.share_drain_due = true;
                    self.host.agent_active();
                    if koru_env::switch(koru_env::name::KORU_NAV_DEBUG) {
                        eprintln!("[koru:nav] focus focused=true requested_redraw=true");
                    }
                    self.request_redraw(id);
                } else {
                    // **Losing focus is the backstop for a gesture that never
                    // ended.** A view starts one on mouse-down and ends it on
                    // mouse-up, so a mouse-up that never arrives leaves the grab
                    // cursor stuck and reads every later click as part of a drag
                    // that finished long ago. Nothing guarantees that mouse-up,
                    // and several things eat it — a native window drag, an OS
                    // drag session, a menu opening over the window, Mission
                    // Control, or the pointer being released outside the window.
                    // This is not a fix for any one of them: a window without
                    // focus cannot be mid-gesture, so ending the press here is
                    // always right and costs one call.
                    //
                    // Keys go the same way, for the same reason — a release is
                    // delivered to whoever has focus, so anything still marked
                    // down here is a key this window will never see come up.
                    self.down_keys.clear();
                    self.mods = ModifiersState::empty();
                    for pane in self.panes.values_mut() {
                        pane.view.set_mods(ModifiersState::empty());
                    }
                    // **A dictation discards with the focus**, and this is
                    // also the one backstop the gesture cannot do without:
                    // switch applications mid-hold and the key release is
                    // delivered to whoever has focus, so it never comes back
                    // here at all.
                    if self.dictation_pane == Some(id) {
                        self.interrupt_dictation(Interruption::FocusLost);
                    }
                    if let Some(pane) = self.panes.get_mut(&id)
                        && pane.view.mouse_up(&mut self.runtime.workspace.ws)
                    {
                        self.runtime.workspace.touched();
                        self.adopt_others(id);
                        self.request_redraw(id);
                    }
                }
                let dismiss = self.panes.get_mut(&id).is_some_and(|pane| {
                    if focused {
                        pane.focused_once = true;
                        false
                    } else {
                        pane.focused_once && pane.view.is_global_search()
                    }
                });
                if dismiss {
                    self.close_pane_with_restore(event_loop, id);
                }
            }

            // System light/dark flipped. Honoured only while preference is System
            // (and only delivered by winit when preferred theme is None).
            WindowEvent::ThemeChanged(os_theme) => {
                self.on_os_theme_changed(os_theme);
            }

            WindowEvent::Resized(size) => {
                if size.width > 0 && size.height > 0 {
                    if let Some(pane) = self.panes.get_mut(&id) {
                        self.context
                            .resize_surface(&mut pane.surface, size.width, size.height);
                    }
                    if let Some(pane) = self
                        .panes
                        .get(&id)
                        .filter(|pane| pane.view.is_global_search())
                    {
                        let height =
                            remembered_search_height(size.height, pane.window.scale_factor());
                        self.search_geometry.resized(height as f32);
                    }
                }
                self.sync_viewport(id);
                // Maximising and restoring arrive as resizes and nothing else,
                // so this is where the caption's restore button and the corner
                // style find out. The drawn hairline follows flush edges, which
                // snap also changes.
                self.sync_maximized(id);
                self.sync_flush(id);
                // The window root publishes the client area, so a resize moves
                // it. `sync_maximized` invalidates too when the state actually
                // flipped; both are one increment on a counter and neither
                // builds anything unless a screen reader is attached.
                self.invalidate_accessibility(id);
                self.request_redraw(id);
            }

            // Native window drags can emit one event per pixel. `sync_flush`
            // asks Windows for the window rect, monitor and work area, so doing
            // that here made the move loop itself stutter. Record activity and
            // recompute once the stream settles in `about_to_wait`.
            WindowEvent::Moved(position) => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.moved_at = Some(Instant::now());
                    if pane.view.floating_window() {
                        self.runtime.settings.floating_pos = Some((position.x, position.y));
                    }
                }
            }

            WindowEvent::ScaleFactorChanged { .. } => {
                self.sync_viewport(id);
                // Every published rectangle is logical times this scale.
                self.invalidate_accessibility(id);
                self.request_redraw(id);
            }

            WindowEvent::ModifiersChanged(m) => {
                let mods = modifiers::accept_report(m.state(), &mut self.down_keys);
                inputdebug::write(format_args!(
                    "[Koru][input] modifiers reported={} event_effective={}",
                    inputdebug::mods_of(m.state()),
                    inputdebug::mods_of(mods),
                ));
                self.mods = mods;
                // **The queued report is authoritative: a modifier it says is
                // up must not stay in `down_keys`.**
                //
                // That set exists to close a gap where `ModifiersChanged`
                // arrives *late* (see `modifiers_with_pressed_keys`), and it
                // only ever forgets a key on a release event. But a release can
                // simply never arrive: the global search hotkey consumes its own
                // Ctrl+Shift+K, so Koru sees the presses and never the releases
                // — and Ctrl stayed held for the rest of the session.
                //
                // Everything downstream then read a modifier nobody was
                // pressing. Enter in search became Ctrl+Enter, which means
                // *open in a new window*, which for a result in another
                // workspace **launches a second process** — the reported
                // "search keeps starting new processes", from a stuck key.
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.view.set_mods(mods);
                }
                self.request_redraw(id);
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Released {
                    // **The release seam.** Everything below this used to
                    // update `down_keys`, the modifiers and the quarantine and
                    // then return, so no release had ever reached a view. A
                    // hold-to-talk gesture has to end somewhere, and the only
                    // place that sees a release *and* a `physical_key` is
                    // here. Matched on the physical key captured at the press
                    // — see `dictation_key`.
                    if Some(event.physical_key) == self.dictation_key {
                        self.dictation_release();
                    }
                    self.down_keys.remove(&event.physical_key);
                    let mods =
                        modifiers_after_key_release(self.mods, event.physical_key, &self.down_keys);
                    if mods != self.mods {
                        self.mods = mods;
                        for pane in self.panes.values_mut() {
                            pane.view.set_mods(mods);
                        }
                    }
                    for pane in self.panes.values_mut() {
                        pane.quarantine.released(event.physical_key);
                    }
                    // …and the view under the keyboard hears it, for a gesture
                    // that ends on a release (`key_release.rs`).
                    self.key_released(id, &event.logical_key);
                    return;
                }
                let mods = modifiers::press(self.mods, &mut self.down_keys, event.physical_key);
                self.adopt_effective_mods(mods);
                // A held Ctrl auto-repeats. Each repeat is not a chord, and
                // after a Ctrl+click the key-up can be missing, so a repeat
                // would put Ctrl back into `down_keys` forever.
                if event.repeat && is_modifier_key(event.physical_key, &event.logical_key) {
                    return;
                }
                // A press that arrives while this window is unfocused is not
                // ours (see `drop_unfocused_key_press`). The press stays in
                // `down_keys` so its release still pairs; it simply never
                // reaches a view or a shell chord.
                if drop_unfocused_key_press(self.panes.get(&id).map(|pane| pane.focused)) {
                    inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell dropped=window-unfocused physical={:?}",
                        std::process::id(),
                        event.physical_key,
                    ));
                    return;
                }
                if self.panes.get_mut(&id).is_some_and(|pane| {
                    pane.quarantine.consumes(event.physical_key, Instant::now())
                }) {
                    inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell claimed=key-quarantine physical={:?}",
                        std::process::id(),
                        event.physical_key
                    ));
                    return;
                }
                // Modified digits can lose or transform their logical key (see
                // `alt_digit` and `shifted_digit`); rebuild them before shell
                // chords or views read the event.
                let recovered_editing_key = modified_editing_key(event.physical_key, mods);
                let logical_key = recovered_editing_key
                    .clone()
                    .or_else(|| shifted_digit(event.physical_key, mods))
                    .or_else(|| view_digit(event.physical_key, mods))
                    .or_else(|| {
                        // **The platform's primary modifier, not Control.**
                        // `mods` is the *effective* set — recovered from the
                        // physical keys still down — so this asks about ⌘ on
                        // macOS and Ctrl elsewhere.
                        (koru_keys::has_primary(mods) && mods.alt_key())
                            .then(|| {
                                alt_digit(&event.logical_key, event.physical_key)
                                    .or_else(|| alt_letter(&event.logical_key, event.physical_key))
                            })
                            .flatten()
                    })
                    .unwrap_or_else(|| event.logical_key.clone());
                if inputdebug::enabled() {
                    inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell normalized physical={} logical={} text={} reported_mods={} effective_mods={} physical_recovery={}",
                        std::process::id(),
                        inputdebug::physical_of(event.physical_key, &logical_key, mods),
                        inputdebug::key_of(&logical_key),
                        inputdebug::text_of(event.text.as_deref()),
                        inputdebug::mods_of(self.mods),
                        inputdebug::mods_of(mods),
                        recovered_editing_key.is_some(),
                    ));
                }
                // **Push to talk**, and the first shell chord asked, because
                // every other press is an interruption of it. A repeat is not
                // a second press: holding the key is the whole gesture.
                if Self::dictate_chord().matches(&logical_key, event.text.as_deref(), mods) {
                    crate::inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell claimed=dictate repeat={}",
                        std::process::id(),
                        event.repeat,
                    ));
                    if !event.repeat && self.dictation_key.is_none() {
                        self.dictation_press(id, event.physical_key);
                    }
                    return;
                }
                // `/dictate` has no held key to release. Plain Enter is its
                // explicit finish gesture: ask the worker for its final
                // transcript, consume the key, and never let Enter split a row
                // underneath the still-provisional sentence.
                if self.dictation.is_live()
                    && finishes_commanded_dictation(self.dictation_commanded, &logical_key, mods)
                {
                    self.stop_dictation();
                    return;
                }
                // **An ordinary local event, and it discards.** A click or a
                // keystroke arriving while a dictation is running throws the
                // unfinished sentence away and then lands immediately — it is
                // never held, and the latest partial is never committed
                // because the engine had not finished revising it. See
                // `koru_dictation`'s module note, which records both rejected
                // alternatives. Deliberately no `return`: the key goes on to
                // do what it was pressed for.
                if self.dictation.is_live() {
                    let why = if matches!(logical_key, Key::Named(NamedKey::Escape)) {
                        Interruption::Escape
                    } else {
                        Interruption::LocalEvent
                    };
                    self.interrupt_dictation(why);
                }
                // Window-level chords are the app shell's business, not a
                // view's: Ctrl+Shift+T needs the Window handle.
                if koru_keys::has_primary(mods)
                    && !koru_keys::has_foreign_primary(mods)
                    && mods.shift_key()
                    && !mods.alt_key()
                    && ctrl_letter(&logical_key, event.text.as_deref()) == Some('t')
                {
                    inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell claimed=toggle-pinned",
                        std::process::id()
                    ));
                    self.toggle_pinned(id);
                    self.wake(id);
                    return;
                }
                // Ctrl+N opens another window on the same workspace. Two views
                // of one document, which is the whole point of the seam: type
                // in either and both follow.
                //
                // **Alt has to be excluded, not just Shift.** `Ctrl+Alt+N` is a
                // document's Narrow layout, and without this it opened a second
                // window instead — the shell claiming a chord it does not own,
                // three modifiers deep, which is exactly why `keys::resolve`
                // checks all three.
                if koru_keys::primary_only(mods)
                    && ctrl_letter(&logical_key, event.text.as_deref()) == Some('n')
                {
                    inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell claimed=new-window",
                        std::process::id()
                    ));
                    let view = self.host.duplicate(&self.runtime.workspace.ws);
                    let title = self.app_window_title();
                    let new_id = self.open_pane(event_loop, view, &title);
                    self.reveal_pane(new_id);
                    // The first `set_visible(true)` on a hidden window is
                    // `SW_SHOWNOACTIVATE`, so the activation that creation-time
                    // visibility used to provide has to be asked for.
                    if let Some(pane) = self.panes.get(&new_id) {
                        pane.window.focus_window();
                    }
                    self.request_redraw(new_id);
                    return;
                }

                // Ctrl+Tab cycles tabs, and it can only be resolved here.
                //
                // On Windows Ctrl+I arrives as `NamedKey::Tab`, and the key map
                // deliberately maps that back to `'i'` so Ctrl+I italicises. So
                // the *logical* key cannot tell the two apart, and the physical
                // one is only visible on the `KeyEvent` — which never reaches a
                // view, because a `KeyEvent` cannot be constructed in a test.
                // The view is handed a direction instead.
                // **Literal Control on every platform, and not the primary
                // modifier.** ⌘+Tab is the macOS application switcher and is
                // not ours to take; Ctrl+Tab is what a Mac app uses to cycle
                // its own tabs. So this is the one chord in the shell that
                // does *not* follow `koru_keys::has_primary`.
                if mods.control_key() && event.physical_key == PhysicalKey::Code(KeyCode::Tab) {
                    inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell claimed=cycle-tab",
                        std::process::id()
                    ));
                    let delta = if mods.shift_key() { -1 } else { 1 };
                    if self
                        .panes
                        .get_mut(&id)
                        .is_some_and(|p| p.view.cycle_tab(delta))
                    {
                        self.wake(id);
                    }
                    return;
                }

                // Ctrl+M → Move to…  Physical only: Ctrl+M is 0x0D, the same
                // byte as Enter, so a logical binding steals Ctrl+Enter.
                // **Primary, not only Control** — Cmd+M on macOS is also Move
                // (system Minimize is not registered for this window's menubar
                // when we want this chord; see menubar if it steals again).
                if koru_keys::has_primary(mods)
                    && !koru_keys::has_foreign_primary(mods)
                    && !mods.shift_key()
                    && !mods.alt_key()
                    && event.physical_key == PhysicalKey::Code(KeyCode::KeyM)
                {
                    let opened = self
                        .panes
                        .get_mut(&id)
                        .is_some_and(|p| p.view.open_move_to());
                    if opened {
                        self.wake(id);
                    }
                    return;
                }

                // Walk the view's position history. Resolved here, beside
                // Ctrl+Tab and for the same reason: the other way to ask for
                // this is the mouse's thumb buttons, and a view is handed a
                // *direction* rather than either input.
                //
                // **The chord is not the same on both platforms, and it cannot
                // be.** Elsewhere it is `Alt`+←/→, the browser and VS Code
                // convention, and `Alt` alone is unbound in the editor's key map
                // (deliberately — see `keymap::DEFAULT`) so nothing competes.
                //
                // On macOS ⌥+← is **word-left in every text field on the
                // system**. Claiming it at the shell — which sees the key before
                // any view — would take word motion away from the editor
                // wholesale. The platform's own answer is `⌘[` / `⌘]`: Finder,
                // Safari and Xcode all use it, and it is free here because the
                // editor binds neither bracket.
                let history_delta = if cfg!(target_os = "macos") {
                    koru_keys::primary_only(mods)
                        .then(|| match ctrl_letter(&logical_key, event.text.as_deref()) {
                            Some('[') => Some(-1),
                            Some(']') => Some(1),
                            _ => None,
                        })
                        .flatten()
                } else if mods.alt_key()
                    && !koru_keys::has_primary(mods)
                    && !koru_keys::has_foreign_primary(mods)
                    && !mods.shift_key()
                {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::ArrowLeft) => Some(-1),
                        PhysicalKey::Code(KeyCode::ArrowRight) => Some(1),
                        _ => None,
                    }
                } else {
                    None
                };
                {
                    if let Some(delta) = history_delta {
                        inputdebug::write(format_args!(
                            "[Koru][input][pid={}] shell claimed=navigate-history delta={delta}",
                            std::process::id()
                        ));
                        let ws = &self.runtime.workspace.ws;
                        if self
                            .panes
                            .get_mut(&id)
                            .is_some_and(|p| p.view.navigate_history(ws, delta))
                        {
                            self.wake(id);
                        }
                        return;
                    }
                }

                // Display zoom: Ctrl+= in, Ctrl+- out, Ctrl+0 reset — the
                // browser convention. Not Ctrl+Plus (Shift+=), which the editor
                // already binds to superscript. Owned by the shell rather than a
                // view so a welcome tab zooms too. Alt is excluded for the same
                // reason as above: Ctrl+Alt+0 is a document's body text, not a
                // zoom reset.
                //
                // **Offered to the view first** (CANVAS P1.4): a canvas zooms
                // its own camera on these chords and claims them, and only a
                // view that declines leaves them to the display zoom. Either
                // way the chord is consumed here.
                if koru_keys::primary_only(mods)
                    && let Some(intent) =
                        zoom::chord_intent(ctrl_letter(&logical_key, event.text.as_deref()))
                {
                    let zoom = self.zoom;
                    let next = self.panes.get_mut(&id).map_or(Some(zoom), |pane| {
                        zoom::zoom_chord(&mut pane.view, intent, zoom)
                    });
                    match next {
                        Some(next) => {
                            inputdebug::write(format_args!(
                                "[Koru][input][pid={}] shell claimed=zoom next={next:.1}",
                                std::process::id()
                            ));
                            self.rezoom(next);
                        }
                        None => inputdebug::write(format_args!(
                            "[Koru][input][pid={}] view claimed=zoom-chord {intent:?}",
                            std::process::id()
                        )),
                    }
                    self.wake(id);
                    return;
                }

                // Font roles and density are process-global and baked into every
                // parley layout — a change has to relay out every window.
                let was_ui = theme::ui_font();
                let was_body = theme::body_font();
                let was_headings = theme::headings_font();
                let was_code = theme::code_font();
                let was_density = theme::density();
                // Voice preferences use the same process-global path as typography.
                let was_voice = crate::voice_prefs::snapshot();
                let document_before = self.runtime.workspace.ws.version();
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                if inputdebug::enabled() {
                    inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell dispatch target={} logical={} text={} mods={}",
                        std::process::id(),
                        if pane.view.is_global_search() {
                            "standalone-search"
                        } else {
                            "view"
                        },
                        inputdebug::key_of(&logical_key),
                        inputdebug::text_of(event.text.as_deref()),
                        inputdebug::mods_of(mods),
                    ));
                }
                let handled = pane.view.key(
                    &mut self.runtime.workspace.ws,
                    &logical_key,
                    event.text.as_deref(),
                    mods,
                );
                inputdebug::write(format_args!(
                    "[Koru][input][pid={}] shell dispatch-complete handled={handled}",
                    std::process::id()
                ));
                if !handled {
                    return;
                }
                // F5 asks for a sharing toggle; only the workspace can perform
                // it, since sharing belongs to the document.
                if pane.view.take_wants_sharing_toggle() {
                    self.runtime.workspace.toggle_sharing();
                }
                let open = pane.view.take_wants_open();
                let document_changed = self.runtime.workspace.ws.version() != document_before;
                if document_changed {
                    self.runtime.workspace.touched();
                }
                if let Some(doc) = open {
                    self.open_document(event_loop, doc);
                }

                let typography_moved = theme::ui_font() != was_ui
                    || theme::body_font() != was_body
                    || theme::headings_font() != was_headings
                    || theme::code_font() != was_code
                    || theme::density() != was_density;
                let voice_moved = crate::voice_prefs::snapshot() != was_voice;
                if typography_moved || voice_moved {
                    // A font or density change is baked into every layout, and a
                    // voice preference change has to be visible in every Settings
                    // tab — so every window has to relay out, not just this one.
                    self.invalidate_everywhere();
                    // And it is committed by the keystroke that made it, for
                    // the same reason the press is: there is no Apply, and the
                    // next tick is a tick in which the app can be closed.
                    // `capture_preferences`, not `capture_geometry` — a
                    // keystroke must not pay for asking the window server
                    // where the window is.
                    self.capture_preferences();
                    self.patch_settings();
                } else if document_changed {
                    // Typing in one window has to show up in every other window
                    // looking at the same document.
                    self.adopt_others(id);
                }
                if voice_moved {
                    // Voice preferences live in process globals; write them back
                    // to Settings immediately so a restart does not lose a choice
                    // made in this session.
                    self.persist_if_changed();
                }
                self.wake(id);
            }

            // **Losing focus ends any press in flight.**
            //
            WindowEvent::CursorMoved { position, .. } => {
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                let s = pane.scale() as f32;
                pane.cursor = (position.x as f32 / s, position.y as f32 / s);
                let (x, y) = pane.cursor;
                // **Close a press the OS never closed.** See
                // [`press_ended_unseen`] — on macOS the release can simply not
                // arrive, and without this the gesture it started runs forever.
                // Done here because a pointer move is the first thing that
                // happens after any lost release, and it costs one bitmask read.
                if press_ended_unseen(pane.pressed) {
                    pane.pressed = false;
                    pane.drag = None;
                    pane.press_origin = None;
                    inputdebug::note("release never arrived -> ending the press");
                    if pane.view.mouse_up(&mut self.runtime.workspace.ws) {
                        self.runtime.workspace.touched();
                        self.adopt_others(id);
                        self.request_redraw(id);
                    }
                    // `pane` is re-borrowed below; the arm continues so the move
                    // itself is still delivered.
                }
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                // The window's own border, claimed before the view is asked —
                // it is a fact about the window rather than about what is in it.
                pane.resize_edge = pane.resize_edge_at(x, y);
                // The view lives in unscaled logical pixels — zoom is folded
                // into the root transform — so hand it zoom-adjusted coords.
                let changed = pane.view.mouse_move(x / self.zoom, y / self.zoom);
                pane.sync_cursor_icon();
                if let Some(WindowRequest::Drag) = pane.view.take_window_request() {
                    let _ = pane.window.drag_window();
                }
                if changed {
                    self.request_redraw(id);
                }
                // A tab drag that has left this window's frame may be over
                // another window — its preview is this loop's to drive, since
                // the target never sees the move.
                self.hover_foreign_tab(id);
            }

            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                // A click is an ordinary local event, and it discards an open
                // dictation before it lands — the same rule a keystroke
                // follows, and for the same reason.
                //
                // **The listening surface is the one exception.** Its
                // microphone button *ends* the dictation, so discarding there
                // would throw away the sentence the button exists to keep; the
                // press is consumed by the surface and reaches no view.
                if state == ElementState::Pressed {
                    let on_surface = self.dictation.is_live()
                        && self.panes.get(&id).is_some_and(|pane| {
                            pane.view
                                .dictation_hit(pane.cursor.0 / self.zoom, pane.cursor.1 / self.zoom)
                        });
                    if on_surface {
                        self.stop_dictation();
                        return;
                    }
                    self.interrupt_dictation(Interruption::LocalEvent);
                }
                // ⌘-click on macOS, Ctrl+click elsewhere — both are the
                // platform's "open this somewhere else" click.
                //
                // **Repair modifiers before reading Ctrl.** Losing focus
                // (`Focused(false)`) clears `self.mods` so Alt+Tab's trailing
                // Tab cannot indent. Pinning a tab rebuilds the Jump List and
                // Explorer steals focus, so a Ctrl that is still physically
                // down is gone from winit's snapshot. Without this, the click
                // after a pin is SameTab and the pin looks like it is already
                // the target.
                let reported_mods = self.mods;
                let mods = current_modifier_state(modifiers_with_pressed_keys(
                    reported_mods,
                    &self.down_keys,
                ));
                self.adopt_effective_mods(mods);
                if state == ElementState::Pressed || reported_mods != mods {
                    inputdebug::write(format_args!(
                        "[Koru][input][pid={}] shell click-mods reported={} effective={} ctrl={} edge={state:?}",
                        std::process::id(),
                        inputdebug::mods_of(reported_mods),
                        inputdebug::mods_of(mods),
                        koru_keys::has_primary(mods),
                    ));
                }
                let ctrl = koru_keys::has_primary(self.mods);
                let document_before = self.runtime.workspace.ws.version();
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                let (x, y) = pane.cursor;
                pane.pressed = state == ElementState::Pressed;
                pane.press_origin = pane
                    .pressed
                    .then(|| pointer_on_screen().map(|p| (p, pane.cursor)))
                    .flatten();
                if !pane.pressed {
                    pane.drag = None;
                }
                match state {
                    ElementState::Pressed => {
                        // **A click in this window is a request for the keyboard.**
                        // Keys arriving while `pane.focused` is false are dropped
                        // (`drop_unfocused_key_press`) so Alt+Tab's trailing Tab
                        // cannot indent a paragraph. Mouse is not gated the same
                        // way, so a press can place the caret while typing still
                        // goes nowhere — pinning a document rebuilds the Jump
                        // List on another thread and Explorer steals focus; the
                        // Windows key does the same. Switching documents in the
                        // tree was a click that happened to activate; this makes
                        // every press do that.
                        if !pane.focused {
                            inputdebug::write(format_args!(
                                "[Koru][input][pid={}] shell focus-on-press",
                                std::process::id(),
                            ));
                            pane.window.focus_window();
                            pane.focused = true;
                        }
                        // The border first, before the view or the caption get a
                        // look: the outermost few pixels are the window's, and
                        // `drag_resize_window` only works while the button that
                        // asked for it is still down.
                        if let Some(dir) = pane.resize_edge {
                            let _ = pane.window.drag_resize_window(dir);
                            return;
                        }
                        let now = Instant::now();
                        let is_double = pane.last_click.is_some_and(|(t, lx, ly)| {
                            now.duration_since(t) < DOUBLE_CLICK
                                && (lx - x).abs() < 4.0
                                && (ly - y).abs() < 4.0
                        });
                        // **Recorded here, before the press is dispatched**, not
                        // at the tail of this arm. A press that raises a
                        // `WindowRequest` returns early below, and while this
                        // lived down there a click on the caption never recorded
                        // itself — so the *next* one could never be a double, and
                        // double-clicking the bare caption to maximise silently
                        // did nothing. The `Drag` arm still clears it on a double,
                        // so a maximise is not also the first half of another one.
                        pane.last_click = if is_double { None } else { Some((now, x, y)) };
                        // The press still has to set focus and place the caret
                        // before a double click can widen it to a word. A click
                        // may also change theme preference, a font role or the
                        // density (welcome-tab pickers, page-menu Typography),
                        // which are baked into every layout — snapshot before so
                        // the change can relay out to every window afterwards.
                        let was_theme_pref = theme::theme_preference();
                        let was_ui = theme::ui_font();
                        let was_body = theme::body_font();
                        let was_headings = theme::headings_font();
                        let was_code = theme::code_font();
                        let was_density = theme::density();
                        let was_caret = theme::caret_color();
                        let was_caret_w = theme::caret_width();
                        let was_caret_h = theme::caret_height();
                        // Default page layout is a process global for the same
                        // reason the theme is: a new document is built far from
                        // any `Settings`. Snapshot it with the rest, or the
                        // press that chose Page never reaches the authority and
                        // the next poll writes Full back over it.
                        let was_view_mode = theme::default_view_mode();
                        let was_canvas_bg = theme::default_canvas_bg();
                        // The voice section's three preferences are globals for
                        // the same reason and travel the same relay — one
                        // snapshot, so a guard extended for two of them cannot
                        // silently miss the third.
                        let was_voice = crate::voice_prefs::snapshot();
                        pane.view.mouse_down(
                            &mut self.runtime.workspace.ws,
                            x / self.zoom,
                            y / self.zoom,
                            ctrl,
                        );
                        if pane.view.take_wants_stay_focused() {
                            pane.window.focus_window();
                        }
                        let theme_pref_moved = theme::theme_preference() != was_theme_pref;
                        let typography_moved = theme::ui_font() != was_ui
                            || theme::body_font() != was_body
                            || theme::headings_font() != was_headings
                            || theme::code_font() != was_code
                            || theme::density() != was_density
                            || theme::caret_color() != was_caret
                            || theme::caret_width() != was_caret_w
                            || theme::caret_height() != was_caret_h;
                        let voice_moved = crate::voice_prefs::snapshot() != was_voice;
                        let layout_moved = theme::default_view_mode() != was_view_mode
                            || theme::default_canvas_bg() != was_canvas_bg;
                        // Performed here, inside the press, because winit only
                        // honours `drag_window` when the button went down
                        // immediately before it. A drag deferred to the tail of
                        // this arm does nothing at all.
                        if let Some(req) = pane.view.take_window_request() {
                            let window = pane.window.clone();
                            let size_locked = pane.size_locked;
                            match req {
                                WindowRequest::Drag => {
                                    // A double click on the caption is a
                                    // maximise, not a drag of zero distance —
                                    // and neither exists under a recording lock.
                                    if is_double {
                                        if !size_locked {
                                            window.set_maximized(!window.is_maximized());
                                        }
                                        pane.last_click = None;
                                    } else if !window.is_maximized() {
                                        // **macOS runs this itself; everywhere
                                        // else the OS does it better than we
                                        // can.** `performWindowDragWithEvent:`
                                        // answers `ok` and moves nothing, and
                                        // macOS then delivers no drag events at
                                        // all — so the shell polls the pointer
                                        // instead. See `Pane::drag`.
                                        //
                                        // The error is never discarded: winit
                                        // implements this on some platforms and
                                        // not others, and a `let _ =` here is
                                        // the defect `resize` had — a gesture
                                        // that silently does nothing with a
                                        // press already spent.
                                        #[cfg(target_os = "macos")]
                                        {
                                            if let Some(p) = self.panes.get_mut(&id) {
                                                p.drag = WindowDrag::begin(&p.window);
                                            }
                                            inputdebug::note("window drag: polling the pointer");
                                        }
                                        #[cfg(not(target_os = "macos"))]
                                        {
                                            let outcome = window.drag_window();
                                            inputdebug::note_result("drag_window", &outcome);
                                        }
                                    }
                                    // **A maximised window is not dragged.**
                                    // `drag_window` hands the press to Windows'
                                    // modal move loop, which on a maximised
                                    // window waits for the pointer to travel far
                                    // enough to be worth restoring for — and
                                    // swallows the *second* click of a double
                                    // while it waits. That made double-clicking
                                    // the caption a one-way trip: it maximised,
                                    // and then nothing could restore it.
                                    //
                                    // The cost is that a maximised window cannot
                                    // be dragged straight off the top of the
                                    // screen; restore it first, which is now
                                    // what the second click does. That is the
                                    // smaller loss, and the only one of the two
                                    // that has another way to do it.
                                }
                                WindowRequest::Place(what) => {
                                    // Snap rows are part of the recording
                                    // size lock.
                                    if !size_locked {
                                        let d = crate::settings::DEFAULT_WINDOW_SIZE;
                                        snap::place(&window, what, (d.0 as u32, d.1 as u32))
                                    }
                                }
                                WindowRequest::Focus => {
                                    window.set_minimized(false);
                                    window.focus_window();
                                    activate_app();
                                }
                                WindowRequest::Minimise => window.set_minimized(true),
                                WindowRequest::ToggleMaximise => {
                                    if !size_locked {
                                        window.set_maximized(!window.is_maximized());
                                    }
                                }
                                WindowRequest::TogglePin => {
                                    self.toggle_pinned(id);
                                }
                                WindowRequest::PlaceAndLockExact { width, height } => {
                                    self.place_and_lock_exact(id, width, height);
                                }
                                WindowRequest::LockCurrentSize => {
                                    let size = window.inner_size();
                                    self.place_and_lock_exact(id, size.width, size.height);
                                }
                                WindowRequest::UnlockSizeAndRestore => {
                                    self.unlock_size_and_restore(id);
                                }
                                // The capture is the shell's wherever the
                                // request was raised — a press or a countdown.
                                WindowRequest::StartRecording(config) => {
                                    self.start_recording(id, config);
                                }
                                WindowRequest::StopRecording => {
                                    self.stop_recording(id);
                                }
                                WindowRequest::Close => {
                                    self.initiate_close(event_loop, id);
                                    return;
                                }
                                WindowRequest::HideToTray => {
                                    // Falls back to a normal close when there
                                    // is no tray to go to — see `hide_to_tray`.
                                    if !self.hide_to_tray() {
                                        self.initiate_close(event_loop, id);
                                    }
                                    return;
                                }
                                WindowRequest::SetCloseBehavior(value) => {
                                    self.set_close_behavior(value);
                                }
                                WindowRequest::TestUpgrade => {
                                    match self.launch_test_updater() {
                                        Ok(()) => self.initiate_close(event_loop, id),
                                        Err(error) => {
                                            self.report(ErrorReport::new("Test upgrade", error))
                                        }
                                    }
                                    return;
                                }
                            }
                            self.request_redraw(id);
                            return;
                        }
                        if is_double {
                            pane.view.mouse_double_click(
                                &self.runtime.workspace.ws,
                                x / self.zoom,
                                y / self.zoom,
                            );
                            pane.last_click = None;
                        } else {
                            pane.last_click = Some((now, x, y));
                        }
                        // Clicking a document pill asks for a window, which
                        // only the shell can create.
                        let open = pane.view.take_wants_open();
                        let document_changed =
                            self.runtime.workspace.ws.version() != document_before;
                        if document_changed {
                            self.runtime.workspace.touched();
                        }
                        // Theme preference or typography change — relay after
                        // the `pane` borrow ends (`invalidate_everywhere` walks
                        // all panes; theme also retargets window chrome).
                        if theme_pref_moved {
                            self.apply_theme_preference();
                        } else if typography_moved || voice_moved {
                            self.invalidate_everywhere();
                        }
                        // **Saved as you click, not on the next tick.** The
                        // Settings surface has no Apply button, so the press
                        // that changed the value is the moment it has to be
                        // committed — a second later is a second in which the
                        // app can be closed, and a preference that did not
                        // survive that is a preference nobody trusts.
                        if theme_pref_moved || typography_moved || voice_moved || layout_moved {
                            // The cheap half only: geometry is asked for once a
                            // second, and a press must not pay 3.5 ms of window
                            // server round trips to save a colour.
                            self.capture_preferences();
                            self.patch_settings();
                        }
                        if document_changed {
                            self.adopt_others(id);
                        }
                        self.wake(id);
                        if let Some(doc) = open {
                            self.open_document(event_loop, doc);
                        }
                    }
                    ElementState::Released => {
                        // A press that starts a gesture and a release that ends
                        // it are two events a handler can silently fail to
                        // pair; this says the release reached the view at all.
                        inputdebug::note("release -> view.mouse_up");
                        // A tab dropped into *another* window lands there — the
                        // drop its hover previewed. The source's own `mouse_up`
                        // must not also run: the take is already committed.
                        if self.drop_foreign_tab(id) {
                            return;
                        }
                        let Some(pane) = self.panes.get_mut(&id) else {
                            return;
                        };
                        if pane.view.mouse_up(&mut self.runtime.workspace.ws) {
                            if self.runtime.workspace.ws.version() != document_before {
                                self.runtime.workspace.touched();
                                self.adopt_others(id);
                            }
                            self.request_redraw(id);
                        }
                    }
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let Some(pane) = self.panes.get_mut(&id) else {
                    return;
                };
                let (x, y) = pane.cursor;
                // Same contract as mouse_move / mouse_down / draw: view space is
                // pre-divided by zoom. Skipping it anchors the menu in DPI-logical
                // coords against a zoom-shrunk viewport.
                if pane.view.context_menu(
                    &mut self.runtime.workspace.ws,
                    x / self.zoom,
                    y / self.zoom,
                ) {
                    self.request_redraw(id);
                }
            }

            // The mouse's two thumb buttons, which every browser and file
            // manager binds to back and forward. They reached the catch-all arm
            // below and were thrown away unread until now.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: button @ (MouseButton::Back | MouseButton::Forward),
                ..
            } => {
                let delta = if button == MouseButton::Back { -1 } else { 1 };
                let ws = &self.runtime.workspace.ws;
                if self
                    .panes
                    .get_mut(&id)
                    .is_some_and(|p| p.view.navigate_history(ws, delta))
                {
                    self.wake(id);
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                // Primary+wheel zooms instead of scrolling — the mouse and
                // trackpad equivalent of the zoom chords above, and ⌘+wheel on
                // macOS for the same reason they are ⌘ there.
                //
                // **The view under the pointer is asked first**, with the raw
                // delta and the pointer in its own coordinates: a canvas zooms
                // its camera around that point and claims the gesture, and the
                // display zoom moves only when the view declines (CANVAS P1.4).
                if koru_keys::has_primary(self.mods) {
                    let zoom = self.zoom;
                    let next = self.panes.get_mut(&id).and_then(|pane| {
                        zoom::primary_wheel(&mut pane.view, delta, pane.cursor, zoom)
                    });
                    if let Some(next) = next {
                        self.rezoom(next);
                    }
                } else if let Some(pane) = self.panes.get_mut(&id) {
                    pane.view.scroll_by(delta);
                }
                self.request_redraw(id);
            }

            // Document packages are imported before the existing image fallback.
            // Unsupported files still reach the image decoder, whose no-op is
            // what has always kept an arbitrary drop from mutating content.
            WindowEvent::DroppedFile(path) => {
                let format = dropped_content_format(&path);
                if let Some(format) = format {
                    queue_content_drop(&mut self.pending_content_drops, id, format, path);
                } else if dropped_image(&path) {
                    if self.import_image(id, None, path) {
                        self.runtime.workspace.touched();
                        self.adopt_others(id);
                    }
                } else {
                    queue_content_drop(&mut self.pending_content_drops, id, "files", path);
                }
            }

            WindowEvent::RedrawRequested => self.redraw(id),

            _ => {}
        }
    }

    fn suspended(&mut self, _event_loop: &ActiveEventLoop) {
        // A lifecycle event discards. Half a sentence landing in a document
        // you were not looking at is worse than losing it.
        self.interrupt_dictation(Interruption::Suspended);
        self.settle_reminder_startup_before_persist();
        self.persist_if_changed();
        self.persist_search_height();
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.host.exiting();
        self.interrupt_dictation(Interruption::Suspended);
        self.settle_reminder_startup_before_persist();
        self.persist_if_changed();
        self.persist_search_height();
        self.runtime.workspace.save();
        self.runtime.profile_session.save(&self.runtime.profile);
        // Before the store can be asked to stop on the way out, and bounded:
        // the session's end either leaves now or is let go — a crash is what
        // the *next* start's `previous_session_clean` reports.
        crate::events_host::session_ended_and_flush(std::time::Duration::from_secs(2));
        // Last, so the summary sits under a session's worth of lines rather
        // than in the middle of the saving. Nothing when the switch is off.
        timing::report_stalls();
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let _stall = timing::Watch::start("about_to_wait");
        // **A native Windows move owns the interaction until it settles.**
        //
        // `drag_window` enters Windows' modal move loop. winit still calls this
        // handler after each `Moved` event, and running the ordinary Store,
        // profile, pane and view maintenance below once per pixel puts our
        // application work directly in the compositor's critical path. None of
        // it can affect the position being dragged, and edits cannot be made
        // while the pointer is holding the caption, so defer it for the short
        // debounce and resume as soon as the move stream stops.
        //
        // `settle_window_moves` performs the one post-move edge query when the
        // deadline expires. Until then the event stream itself keeps Windows
        // moving; `WaitUntil` is only the backstop for a release with no final
        // event.
        let now = Instant::now();
        if self.settle_window_moves(now) {
            let deadline = self
                .panes
                .values()
                .filter_map(|pane| pane.moved_at.map(|at| at + WINDOW_MOVE_SETTLE))
                .min()
                .unwrap_or(now + WINDOW_MOVE_SETTLE);
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline.max(now)));
            return;
        }
        // The connected-Profile worker wakes this loop after every filesystem
        // boundary. Only this drain advances to the hosted acknowledgement.
        self.service_embedded_profile_restores();
        self.service_connected_profile_creation(event_loop);
        let disconnected_profile = self
            .pending_connected_profile_creation
            .as_ref()
            .and_then(|pending| pending.stage.waiting_on_agent(self.host.agent_available()));
        if let Some(profile) = disconnected_profile {
            if self
                .pending_connected_profile_creation
                .as_ref()
                .is_some_and(|pending| {
                    matches!(
                        pending.stage,
                        ConnectedProfileCreationStage::AwaitingHostedProfile(_)
                    )
                })
            {
                self.pause_connected_profile_creation(profile);
            } else {
                self.finish_connected_profile_creation(profile, false);
            }
        }

        // The whole of what this process now does about updating: notice that
        // the agent said something, and draw. The checking, the downloading and
        // the swap are `koru-agent`'s.
        if self.host.take_account_dirty() {
            let state = self.host.account_state();
            let mut changed = self.sync_pane_context();
            for pane in self.panes.values_mut() {
                changed |= pane.view.set_account_state(state.clone());
            }
            while let Some(admission) = self.host.take_account_command_admission() {
                for pane in self.panes.values_mut() {
                    changed |= pane.view.account_command_admitted(admission);
                }
                if let Some(pending) = self.pending_connected_profile_creation.as_mut()
                    && pending.accepts_admission(admission)
                {
                    pending.generation = Some(admission.generation);
                }
            }
            while let Some((owner_scope, generation, result)) =
                self.host.take_scoped_account_result()
            {
                if let koru_agent_wire::AccountOperationResult::Profile { profile } = &result {
                    self.accept_connected_profile_hosted_ack(owner_scope, profile);
                }
                let failed_connected_profile = if matches!(
                    &result,
                    koru_agent_wire::AccountOperationResult::Failed { .. }
                ) {
                    self.pending_connected_profile_creation
                        .as_ref()
                        .filter(|pending| {
                            pending.owner_scope == owner_scope
                                && pending.generation == Some(generation)
                        })
                        .and_then(|pending| match pending.stage {
                            ConnectedProfileCreationStage::AwaitingHostedProfile(profile) => {
                                Some(profile)
                            }
                            _ => None,
                        })
                } else {
                    None
                };
                if let Some(profile) = failed_connected_profile {
                    self.pause_connected_profile_creation(profile);
                }
                if let koru_agent_wire::AccountOperationResult::Profiles { profiles } = &result {
                    self.reconcile_pending_profile_bindings(profiles);
                }
                for pane in self.panes.values_mut() {
                    changed |=
                        pane.view
                            .account_scoped_result(owner_scope, generation, result.clone());
                }
            }
            while let Some(failure) = self.host.take_account_command_failure() {
                if let Some(pending) = self.pending_connected_profile_creation.as_ref()
                    && pending.submission == Some(failure.submission)
                    && let ConnectedProfileCreationStage::AwaitingHostedProfile(profile) =
                        pending.stage
                {
                    self.pause_connected_profile_creation(profile);
                }
                for pane in self.panes.values_mut() {
                    changed |= pane.view.account_command_failed(failure);
                }
            }
            if changed {
                self.invalidate_everywhere();
            }
        }
        self.service_binding_markers();
        self.service_profile_bindings();
        self.service_profile_cascades();
        self.service_profile_removals(event_loop);
        if self.host.take_update_redraw() {
            self.request_redraw_all();
        }
        // OS light/dark while System — ThemeChanged alone has missed the first
        // flip after launch; registry poll is the backstop.
        {
            let _phase = timing::Watch::start("maintenance.os_theme");
            self.poll_os_theme();
        }
        // Menu-bar rows: this loop already runs for the caret, so the menu needs
        // no clock of its own.
        {
            let _phase = timing::Watch::start("maintenance.menubar");
            self.service_menubar(event_loop);
        }
        // A window drag, which on macOS has no events of its own to ride on.
        let dragging = self.drive_window_drags() | self.drive_view_drags();
        // A press whose release the OS never delivered — see
        // [`press_ended_unseen`]. The pointer-move path catches most of these,
        // but a click that never moves would otherwise hang until it did, so
        // this closes the gap on a loop that already runs for the caret.
        //
        // **Free when nothing is held**: a `bool` per pane and no further call,
        // which is the bar anything added here has to meet.
        self.end_presses_the_os_dropped();
        // **The dictation drain, and the whole of what a partial costs an idle
        // loop.** Free when nothing is running: `pump` on an idle controller
        // drains an empty inbox and answers an empty vector. The reader thread
        // is what wakes this loop (`Wake::Voice`); nothing here polls.
        {
            let _phase = timing::Watch::start("maintenance.dictation");
            // **`/dictate` first**, so a session asked for by the menu is begun
            // in the same pass its actions are drained in — otherwise `Begin`
            // would wait a whole wake before reaching the surface, which is
            // the latency ADR-47 was written about.
            //
            // **A `bool` per pane and nothing else when nobody asked.**
            self.begin_commanded_dictations();
            self.service_dictation();
        }
        if let Some(activation) = self.host.take_global_search_request() {
            self.open_search(event_loop, Some(PhysicalKey::Code(activation.trigger)));
        }
        if let Some(message) = self.host.take_global_search_notice() {
            self.report(ErrorReport::new("Global search shortcut", message));
        }
        if let Some(report) = self.host.take_agent_notice() {
            self.report(report);
        }
        #[cfg(target_os = "macos")]
        for url in crate::macos_urls::take_pending() {
            if let Err(message) = self.host.open_external_url(url) {
                let lang = koru_i18n::Lang::for_system(
                    koru_window::system_locale::system_locale().as_deref(),
                );
                self.report(ErrorReport::new(
                    koru_i18n::t(lang, koru_i18n::Key::KoruLinkOpenErrorTitle),
                    message,
                ));
            }
        }
        // A Jump List click asks the window to come forward. The same call the
        // tray's *Restore* makes, and for the same reason it lives before the
        // tray early-return below: the window may be sat in the notification
        // area, and only a drain that runs while hidden can make it visible.
        if self.host.take_activation_focus() {
            self.restore_from_tray();
        }
        // A Jump List *Search* click. The overlay is the agent's window, so the
        // host performs the whole ask; nothing here needs to focus or draw.
        let _ = self.host.take_search_activation();
        // AI Chat is also agent-owned and singleton. The taskbar listener only
        // wakes this loop; the host passes the foreground grant and request on.
        let _ = self.host.take_ai_chat_activation();
        for id in self.host.take_unpinned_documents() {
            for pane in self.panes.values_mut() {
                let _ = pane.view.unpin_document(id);
            }
        }
        let pins = self.pane_pins();
        if pins != self.last_agent_pins {
            self.last_agent_pins = pins.clone();
            let (profile, workspace, profile_name, workspace_name) = self.runtime.agent_showing();
            self.host
                .agent_showing(profile, workspace, &profile_name, &workspace_name, &pins);
        }
        // **Before the tray early-return below**, for the same reason profile
        // activations are: a window sitting in the notification area is exactly
        // the one an agent's `Restore` is addressed to, and a queue drained
        // only while visible would never deliver the message that makes it
        // visible.
        for ask in self.host.take_agent_asks() {
            match ask {
                crate::AgentAsk::Open(request) => {
                    if let PaneRequest::Navigate {
                        target: crate::NavigationTarget::Document { document, node },
                        ..
                    } = request.as_ref()
                    {
                        tracing::info!(
                            target: "koru::notification_activation",
                            document = %document.to_storage(),
                            node = %trace_node(*node),
                            "Koru host drained the Agent Open request"
                        );
                    }
                    self.from_the_agent.push(*request);
                }
                crate::AgentAsk::ReminderSnooze { intent, delivery } => {
                    // `from_the_agent` is a stack. The Agent's durable Open is
                    // already in this queue when both frames arrive together,
                    // so put the semantic handoff below every pending request.
                    // If Open was serviced on the previous loop, this is the
                    // only remaining request. Repeating Open here used to place
                    // the picker between two identical navigations and the
                    // second navigation immediately closed it.
                    queue_agent_snooze(&mut self.from_the_agent, intent);
                    tracing::info!(
                        target: "koru::notification_activation",
                        queued_requests = self.from_the_agent.len(),
                        "Koru host retained the native Snooze picker request behind pending navigation"
                    );
                    self.host.accept_reminder_snooze(delivery);
                }
                crate::AgentAsk::OpenReminders => {
                    self.restore_from_tray();
                    if let Some(pane) = self.primary.and_then(|id| self.panes.get_mut(&id))
                        && pane.view.open_all_reminders()
                    {
                        pane.window.request_redraw();
                    }
                }
                crate::AgentAsk::Restore => self.restore_from_tray(),
                // **Come forward first, then ask.** The row was pressed in the
                // notification area, so the window this modal is about to open
                // in may not be on screen — and a consent step behind a hidden
                // window is a download that never starts and a person who
                // thinks the row is broken.
                crate::AgentAsk::SetUpVoice => {
                    self.restore_from_tray();
                    if let Some(id) = self.primary.or_else(|| self.panes.keys().copied().next()) {
                        self.offer_voice_model(id);
                    }
                }
                // **No window has to be restored for this one.** It changes
                // a preference and draws nothing; the next dictation, from
                // wherever it is asked for, uses it. Saved immediately rather
                // than at the next periodic pass, because the process that
                // made the choice is the notification area and a person who
                // chose a microphone there may never open a window again this
                // session.
                crate::AgentAsk::SetVoiceInput(device) => {
                    let choice = match device {
                        Some(name) => crate::voice_prefs::parse_input(&name),
                        None => crate::voice_prefs::InputChoice::SystemDefault,
                    };
                    crate::voice_prefs::set_input(choice);
                    self.capture_geometry();
                    self.persist_if_changed();
                    self.invalidate_everywhere();
                }
                crate::AgentAsk::DictationGesture { route, gesture } => {
                    self.routed_dictation_gesture(route, gesture);
                }
                crate::AgentAsk::AgentDisconnected => self.agent_disconnected(),
                crate::AgentAsk::ReportError(error) => self.report(error),
                crate::AgentAsk::CloseForUpdate => {
                    // The loop saw the ask. Between "received" (the reader
                    // thread's line) and this one lies every way the drain can
                    // be deferred — a native window drag, a blocking dialog.
                    tracing::info!(
                        target: "koru::update",
                        panes = self.panes.len(),
                        "close-for-update drained"
                    );
                    self.close_for_update(event_loop);
                }
                // **The primary pane, and only it.** An install produces one
                // page, not one per window; the agent already sends this to a
                // single shell, and giving it to every pane in *that* shell
                // would undo the same care one level down.
                //
                // A view with nowhere to put a page answers `false` and the
                // notice is simply not shown — the agent has already forgotten
                // it, which is the right trade: a dedicated search window is
                // not a place to read release notes, and holding the message
                // for a tab host that may never open is how it ends up
                // appearing weeks later.
                crate::AgentAsk::ShowWhatsNew(version) => {
                    if let Some(pane) = self.primary.and_then(|id| self.panes.get_mut(&id))
                        && pane.view.open_whats_new(&version)
                    {
                        pane.window.request_redraw();
                    }
                }
                crate::AgentAsk::ShowFeedback => {
                    self.restore_from_tray();
                    self.open_feedback(event_loop);
                }
                // **The pane showing the session wins; a container is the
                // fallback** — Koru Terminal's "the exact window and tab"
                // rule, applied inside the one process the Agent routed to.
                // `open_session_tab` on the winner focuses the tab it already
                // has or opens one attached by id; it never launches.
                crate::AgentAsk::ShowSession(session) => {
                    self.show_from_tray();
                    let target = self
                        .panes
                        .iter()
                        .find(|(_, pane)| pane.view.shows_session(session))
                        .map(|(id, _)| *id)
                        .or_else(|| self.container_pane());
                    if let Some(id) = target
                        && let Some(pane) = self.panes.get_mut(&id)
                    {
                        pane.view.open_session_tab(session);
                        pane.window.focus_window();
                        pane.window.request_redraw();
                    }
                    activate_app();
                }
                // Come forward and offer the chooser: which agent to start is
                // the person's, so the ask opens the `+` menu rather than
                // guessing a profile.
                crate::AgentAsk::NewAgentSession => {
                    self.show_from_tray();
                    if let Some(id) = self.container_pane()
                        && let Some(pane) = self.panes.get_mut(&id)
                    {
                        pane.view.open_new_session_chooser();
                        pane.window.focus_window();
                        pane.window.request_redraw();
                    }
                    activate_app();
                }
                crate::AgentAsk::ShowSupport => {
                    self.restore_from_tray();
                    self.open_support(event_loop);
                }
                crate::AgentAsk::ProfileLifecycleDeleted(profile) => {
                    self.hosted_profile_removals
                        .insert(profile, HostedProfileRemovalSource::Peer);
                    self.start_profile_cascade(profile);
                }
            }
        }
        // Profile activations are machine-local input, not visible-window
        // maintenance. A background/tray process can own the profile pipe, so
        // this must run before the tray early return or the listener will keep
        // accepting targets into a queue that is never drained.
        {
            let _phase = timing::Watch::start("maintenance.profile_activations");
            self.drain_profile_activations(event_loop);
        }
        {
            let _phase = timing::Watch::start("maintenance.import");
            self.drain_import_messages();
            self.maintain_content_import_cancellation(Instant::now());
            self.start_pending_content_drop();
            self.start_share_drain();
        }
        self.drain_delegate_folder_checks();
        self.drain_recording_answers();
        {
            // First use: the catalog answer, the download's progress, and the
            // three-second hold after Stop. Two `Option` reads and an empty
            // `try_iter` when nobody is setting voice up.
            let _phase = timing::Watch::start("maintenance.voice_setup");
            self.drain_voice_messages();
            self.maintain_voice_cancellation(Instant::now());
        }
        // The spell dictionary thread starts once the profile is known — a
        // bool and an Option check on every later pass, which is the bar.
        // The client is built on that thread, never here.
        if let Some(profile) = self.runtime.profile.id()
            && !crate::spelling::dictionary_connected_to(
                koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid()),
            )
        {
            let root = cli::root_from_args();
            crate::spelling::connect_dictionary(
                &self.wake_proxy,
                koru_store_core::ids::ProfileId::from_uuid(profile.as_uuid()),
                move || client_at(root.as_deref()),
            );
        }
        {
            let _phase = timing::Watch::start("maintenance.document_transfers");
            self.drain_transfer_results();
        }
        {
            let _phase = timing::Watch::start("maintenance.selection_moves");
            self.drain_selection_moves();
        }
        {
            let _phase = timing::Watch::start("maintenance.profile_catalog");
            self.drain_profile_catalog();
        }
        {
            let _phase = timing::Watch::start("maintenance.reminder_startup");
            self.drain_reminder_startup();
        }
        // **Before the tray early return.** `take_agent_asks` above transfers
        // the IPC inbox into `from_the_agent`; it does not navigate by itself.
        // An Open request is the thing that restores and focuses the target
        // pane, so leaving it for the ordinary pane-request phase below would
        // make a hidden process return forever without consuming it.
        if !self.from_the_agent.is_empty() {
            let _phase = timing::Watch::start("maintenance.agent_requests");
            self.drain_agent_requests(event_loop);
        }
        let search_visible = self.panes.values().any(|pane| {
            (pane.view.is_global_search() || pane.view.is_auxiliary())
                && pane.window.is_visible() == Some(true)
        });
        if self.in_tray && !search_visible {
            // **macOS may have unhidden us already** — a Dock click does it
            // directly. Sync the shell's own state to what the OS did, or the
            // status item lingers over a visible window.
            //
            // **Armed only once the hide has actually taken.** `hide:` is not
            // instant, so a poll landing between the call and the state change
            // would read "not hidden", restore immediately, and make closing
            // the window look like it did nothing.
            if application_was_unhidden() {
                if self.seen_hidden {
                    self.restore_from_tray();
                    return;
                }
            } else {
                self.seen_hidden = true;
            }
            // **The way back went away, so go.**
            //
            // Hiding is refused unless there is an agent to restore this window
            // ([ADR-41](../../../docs/decisions/README.md)), but that is checked once,
            // at the moment of hiding. The agent can quit — or be killed —
            // afterwards, and what is left then is exactly what the rule
            // forbids: a process with no window, no icon and no way to be
            // reached, still holding a workspace.
            //
            // **Closing, not reappearing.** Being hidden was a convenience the
            // agent provided; with the agent gone the convenience is over, and
            // a window that springs onto the screen because something *else*
            // quit is its own surprise. It goes through the ordinary close, so
            // unsent work is still noticed — and anything that refuses to
            // close is made visible below rather than left hidden, because a
            // window nobody can see is the thing this branch exists to
            // prevent.
            if !self.host.agent_available() {
                tracing::info!(
                    target: "koru::startup",
                    "the agent this window was hidden behind is gone; closing"
                );
                // Cleared first: `initiate_close` consults the tray and would
                // otherwise try to hide into an agent that is not there, and
                // this branch must not be re-entered on the next poll.
                self.in_tray = false;
                self.seen_hidden = false;
                for id in self.panes.keys().copied().collect::<Vec<_>>() {
                    self.initiate_close(event_loop, id);
                }
                if !self.panes.is_empty() {
                    self.show_from_tray();
                }
                return;
            }
            // Nothing else has anything to do: no window is on screen, so there
            // is no frame to prepare and no cursor to chase. The document still
            // syncs — a peer's edits should not queue up behind a tray icon.
            let changed = {
                let _phase = timing::Watch::start("workspace.tick");
                self.runtime.workspace.tick()
            };
            let entries_changed = self.runtime.workspace.take_entries_changed();
            if changed {
                self.images_dirty = true;
                self.adopt_everywhere();
            }
            if entries_changed {
                self.push_entries();
            }
            let now = Instant::now();
            let background = self.background_maintain_all(now, true);
            self.drain_background_intents(event_loop);
            let mut wake = now + HIDDEN_POLL;
            if let Some(due) = background {
                wake = wake.min(due);
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(wake));
            return;
        }
        // Touches disk only when one of the three files' contents actually
        // moved — which, until the clock stamp in `capture_geometry` was made
        // coarse, was every tick of every session.
        // Two watches inside it, because "persist_settings was slow" does not
        // say whether the time went on asking Windows where the window is or on
        // writing three files, and those are different bugs. See
        // `capture_geometry` and `Settings::save_all_changed`.
        //
        // **Throttled as well, because asking is not free either.**
        // `capture_geometry` asks for maximised, minimised, scale, inner size
        // and outer position; on macOS each is a round trip to the window
        // server, and together they measured **3.5 ms** — against microseconds
        // for everything else in this loop. Run on every pass it cost a whole
        // CPU core at idle, which Windows only hid by waking the loop far less
        // often. Once a second is plenty: geometry moves when a person moves a
        // window, and every path that actually loses state — close, suspend,
        // exit, hide — calls `persist_if_changed` directly and unthrottled.
        if self
            .last_persist
            .is_none_or(|t| t.elapsed() >= PERSIST_EVERY)
        {
            let _phase = timing::Watch::start("maintenance.persist_settings");
            self.last_persist = Some(Instant::now());
            self.persist_if_changed();
        }
        // The idle half of the content-burst boundary (ADR-273). Three things
        // it deliberately is not: it is not a timer (the quiet period is
        // compared against a time this loop already had), it is not a document
        // walk (the key *is* the task), and it is not an emission per keystroke
        // (`quiet` answers only bursts that have gone five seconds untouched).
        // In the ordinary case it is one mutex lock over an empty map.
        if self
            .last_burst_tick
            .is_none_or(|t| t.elapsed() >= BURST_TICK_EVERY)
        {
            let _phase = timing::Watch::start("maintenance.task_bursts");
            let now = Instant::now();
            self.last_burst_tick = Some(now);
            let flushed = crate::tasks::content_quiet(now);
            if !flushed.is_empty() {
                crate::tasks::TaskHost::default().content_changed(&flushed);
                tasks_debug_log(format_args!("{}", crate::tasks::counters().describe()));
            }
        }
        // **Unthrottled, because it is a `try_recv` and nothing else.** The
        // startup wait resolving and another window's change both arrive here,
        // and a settings correction a second late is a settings correction the
        // person watches happen.
        {
            let _phase = timing::Watch::start("maintenance.poll_settings");
            self.poll_settings();
        }
        // A tab the container popped out becomes its own window here, between
        // events — the one place that both has the event loop and is not a press.
        {
            let _phase = timing::Watch::start("maintenance.popouts");
            self.drain_popouts(event_loop);
            self.drain_session_popouts(event_loop);
        }
        // `/image` requests (a blocking file dialog) and any images not yet
        // decoded — on startup, after a peer added one, anywhere. A decode has
        // to paint, so it is a reason to draw alongside the caret.
        let images_decoded = {
            let _s = timing::Watch::start("service_images");
            self.service_images()
        };
        // Tag traffic: create-tag requests up to the workspace system
        // document, the resolved table down into every pane. Lazy — a
        // workspace that never used a tag joins nothing.
        {
            let _s = timing::Watch::start("maintenance.service_tags");
            self.service_tags();
        }
        // Publish local edits and pull in anyone else's. Rate-limited inside.
        // A remote change means every window has to rebuild from the document.
        let changed = {
            let _s = timing::Watch::start("workspace.tick");
            self.runtime.workspace.tick()
        };
        let entries_changed = self.runtime.workspace.take_entries_changed();
        self.report_write_failure();
        self.report_replay_trouble();
        // Every tick, not only on a change: the chip's own comparison decides
        // whether anything is redrawn, and part of what it reports — how long
        // this has been going on — moves without anything happening.
        let trouble = self.runtime.workspace.store_trouble();
        for pane in self.panes.values_mut() {
            if pane.view.set_store_trouble(trouble.clone()) {
                pane.window.request_redraw();
            }
        }
        if changed {
            // A peer may have added a picture, and its bytes arrive in `blobs/`
            // independently of the op that references them.
            self.images_dirty = true;
            let _s = timing::Watch::start("adopt_everywhere");
            self.adopt_everywhere();
        }
        if entries_changed {
            // The manifest is separate from the active `WorkspaceDoc`. Refresh
            // Explore and pill captions without rebuilding the editor that may
            // currently be typing the title which caused this change.
            self.push_entries();
        }
        // The profile has its own, much slower loop: another window on this
        // folder may have added or renamed a workspace, and the switcher should
        // grow a row without either window knowing about the other.
        {
            let _phase = timing::Watch::start("maintenance.profile_tick");
            self.runtime.profile_session.tick(&mut self.runtime.profile);
        }
        {
            let _phase = timing::Watch::start("maintenance.pane_context");
            self.poll_pane_context();
        }
        // Every one of these ends in a document swap, a folder picker or a new
        // process, and none of those may happen inside a press.
        {
            let _phase = timing::Watch::start("maintenance.pane_requests");
            self.drain_pane_requests(event_loop);
        }
        let background = self.background_maintain_all(Instant::now(), false);
        self.drain_background_intents(event_loop);
        {
            let _phase = timing::Watch::start("maintenance.sync_titles");
            self.sync_titles();
        }
        // **After the state above has settled, and on this thread.** A screen
        // reader's request arrives on whichever thread the platform chose and
        // is only a note; this is where it is read, answered against current
        // state, and — if it is still about something that exists — performed
        // through the owner that already performs it. Costs one drained
        // mailbox per window when nobody is listening.
        {
            let _phase = timing::Watch::start("maintenance.accessibility");
            self.service_accessibility(event_loop);
            self.service_inspector_accessibility();
        }

        // A view with motion in flight is the one thing that needs frames on a
        // clock rather than on input, so it gets a fast one — and only it does.
        // The slow timer stays the rule; this is the exception, and it ends by
        // itself when `animating` goes false.
        let now = Instant::now();
        let maintained: Vec<WindowId> = {
            let _phase = timing::Watch::start("maintenance.views");
            let workspace = &mut self.runtime.workspace.ws;
            self.panes
                .iter_mut()
                .filter_map(|(id, pane)| pane.view.maintain(workspace, now).then_some(*id))
                .collect()
        };
        for id in self.panes.keys().copied().collect::<Vec<_>>() {
            self.reconcile_surface(id);
        }
        for id in maintained {
            self.request_redraw(id);
        }
        // **Window requests a maintain pass raised** — presses drain their own
        // inside the press, but a maintain-raised request (the recording
        // countdown's `StartRecording`, a close re-asked after a finalize) has
        // no press to ride, and the view's slot is a single one: one ask per
        // pass, performed in the same pass.
        for id in self.panes.keys().copied().collect::<Vec<_>>() {
            let request = self
                .panes
                .get_mut(&id)
                .and_then(|pane| pane.view.take_window_request());
            if let Some(request) = request {
                self.perform_window_request(event_loop, id, request);
            }
        }
        let animating: Vec<WindowId> = self
            .panes
            .iter()
            .filter(|(_, p)| p.view.animating())
            .map(|(id, _)| *id)
            .collect();
        let mut deadline = if animating.is_empty() {
            // Wake on a slow timer so the caret can blink, instead of spinning.
            now + CARET_BLINK / 3
        } else {
            now + ANIMATION_FRAME
        };
        // **And no later than the document needs.** The caret's timer used to be
        // the only clock, so a 300 ms poll checked on a 177 ms wake really ran
        // every 353 ms, and an edit waiting to go out sat for up to a wake
        // before it was even written. Both were latency the peer at the other
        // window could feel, and neither was anybody's intent — the constants
        // now mean what they say.
        if let Some(due) = self.runtime.workspace.next_due() {
            deadline = deadline.min(due);
        }
        if let Some(due) = self.import_cancel_hide_due {
            deadline = deadline.min(due);
        }
        if let Some(due) = self.image_loader.retry_due() {
            deadline = deadline.min(due);
        }
        if let Some(due) = background {
            deadline = deadline.min(due);
        }
        // The hold cap and the finalisation timeout, and nothing else.
        // `None` whenever no dictation is running, so an idle window is never
        // woken by this feature.
        if let Some(due) = self.dictation.deadline() {
            deadline = deadline.min(due);
        }
        for pane in self.panes.values() {
            if let Some(due) = pane.view.maintenance_deadline() {
                deadline = deadline.min(due);
            }
            if let Some(moved_at) = pane.moved_at {
                deadline = deadline.min(moved_at + WINDOW_MOVE_SETTLE);
            }
        }
        // **A live drag polls.** It is the one thing here with no event to
        // wake it and no clock of its own — waiting on the caret's would move
        // the window a few times a second. `ControlFlow::Poll` is confined to
        // the gesture: it ends when the button does, so it cannot swallow the
        // maintenance deadline above for longer than a press.
        if dragging {
            event_loop.set_control_flow(ControlFlow::Poll);
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline.max(now)));
        }
        if !animating.is_empty() {
            for id in animating {
                self.request_redraw(id);
            }
            return;
        }

        // Waking is not a reason to draw. Asking for a frame on every wake put
        // one in the queue three times a second with nothing new in it, and a
        // keystroke arriving behind one waits for it to go out first. Every
        // *other* source of change asks for its own redraw, so the only ones
        // left here are the caret changing phase and an image just decoded.
        if !images_decoded && self.caret_phase == Some(self.caret_on()) {
            return;
        }
        let ids: Vec<WindowId> = self.panes.keys().copied().collect();
        for id in ids {
            self.request_redraw(id);
        }
    }
}

/// Run `host`'s views in windows until the last one closes.
///
/// Takes the workspace over: opening the store, the peer transport and the undo
/// history are all things exactly one thing in a process may do.
pub fn run<H: Host>(host: H, name: &str) {
    run_with_args(host, name, std::env::args());
}

/// Run a host after validating the arguments that belong to the shared shell.
/// A thin product binary can consume one of its own private launch arguments
/// first, while every argument handed here retains the strict Koru parser.
pub fn run_with_args<H: Host, I: IntoIterator<Item = String>>(host: H, name: &str, args: I) {
    // Before anything else, so "first frame" is measured from as close to the
    // process starting as this function can see. Free unless the switch is on.
    timing::start();
    // The always-on twin: the GPU readiness lines print on every launch, so
    // their origin has to exist on every launch too.
    let _ = process_started();
    // The command line first, and strictly. An argument that is not understood
    // is not run: a mistyped `--profile` that gets dropped opens *some other*
    // workspace and says nothing about it.
    let program = cli::program_name();
    match cli::parse(args) {
        Ok(cli::Command::Run) => {}
        Ok(cli::Command::Help) => {
            println!("{}", cli::help(&program));
            return;
        }
        Ok(cli::Command::Version) => {
            println!("Koru {}", cli::version());
            return;
        }
        Ok(cli::Command::Backup(dest)) => {
            if !startup::run_backup(dest) {
                std::process::exit(1);
            }
            return;
        }
        Ok(cli::Command::Where) => {
            startup::report_location();
            return;
        }
        Ok(cli::Command::Profiles) => {
            startup::report_profiles();
            return;
        }
        Ok(cli::Command::Envs) => {
            print!("{}", koru_env::table());
            return;
        }
        Err(bad) => {
            bad.report(&program);
            // 2 for a usage error, the same code `koru.sh` exits with for an
            // unknown subcommand.
            std::process::exit(2);
        }
    }

    // The GPU has nothing to do with any of what follows, and asking the driver
    // for a device takes about as long as reading the workspace does. Started
    // here so the two overlap instead of queueing.
    let gpu = GpuWarmup::start();

    // Before any window: load Profile-independent shell settings. A plain Store
    // launch never opens or creates the retired `local-profile` scaffold;
    // explicitly selected legacy folders retain their supported path.
    let Some(dir) = startup::launch_settings_folder() else {
        eprintln!("[Koru] no local settings folder available; nothing was opened");
        return;
    };
    let mut builder = EventLoop::<Wake>::with_user_event();
    // **Koru's own loop, not only `koru_window`'s.** The X11 decision and its
    // reason are at `koru_window::app::run` (D5); this is the third loop it
    // names. Without it Koru alone came up as a Wayland client, where
    // `set_visible(false)` does nothing — so the ✕, which hides, did nothing.
    #[cfg(target_os = "linux")]
    {
        use winit::platform::x11::EventLoopBuilderExtX11;
        builder.with_x11();
    }
    let event_loop = builder.build().expect("create event loop");
    let proxy = event_loop.create_proxy();
    // Before `run_app`, which is what finishes launching: that is the moment
    // AppKit decides whether the delegate answers URLs.
    #[cfg(target_os = "macos")]
    crate::macos_urls::install(proxy.clone());
    // `None` means opening failed and the user has already been shown why.
    let Some(mut app) = App::new(host, name, dir, gpu, proxy) else {
        return;
    };
    event_loop.run_app(&mut app).expect("run app");

    // **The last thing out turns the lights off.**
    //
    // The store draws nothing and has no icon; the agent stops it when *it*
    // quits, but only when no window needs it — so an agent quit while a
    // window is open correctly leaves the store up, and then nothing is left
    // to stop it when that window finally closes. What remains is a process
    // nobody can see and nobody asked for.
    //
    // Decided here, after the loop, and acted on **after `app` is dropped**:
    // dropping it closes the sync sockets, and the store's idle check counts
    // sockets. Asking first would be asking whether we ourselves are here.
    let ask_the_store_to_stop = app.should_stop_the_store_on_exit();
    let gateway_root = app.gateway_root().map(Path::to_path_buf);
    drop(app);
    if ask_the_store_to_stop {
        stop_the_store_if_nothing_else_wants_it(gateway_root.as_deref());
    }
}

/// **Two questions, and the second one is the authority.**
///
/// `own_link` is this process's private presence connection to the Agent. It
/// answers "did *my* link come up", which is not the same as "is an agent
/// running" and was quietly standing in for it: the link is dialled once, at
/// start-up, and is never retried — so an Agent that started a moment after this
/// window, or one that dropped and came back, leaves it `false` for the life of
/// the process. On the machine this was found on it was `false` while a live
/// Agent was supervising a Koru Terminal window, and the last Koru window
/// closing therefore stopped a gateway that a working terminal was using.
///
/// `discoverable` is [`koru_api_client::grants::agent_running`] — the Agent's
/// endpoint, asked at the moment of the decision, which is how everything else
/// in Koru finds the Agent. The own link is still consulted first because it is
/// free and, when it is up, conclusive; the probe is the answer when it is not.
///
/// Stopping is the destructive direction, so this is written to be wrong in the
/// safe way: any sign of an agent leaves the store alone. A store left running
/// is a process nobody asked for; a store stopped under a live window is work
/// the person can see stop working.
fn stop_the_store_on_exit(own_link: bool, discoverable: bool) -> bool {
    !own_link && !discoverable
}

/// Ask the store to stop, and accept "no".
///
/// Only when this window found no agent — with one running, quitting *it* is
/// what stops the store, and doing it from here as well would race that. The
/// store decides whether anything is still connected, because it is the only
/// party that can answer without a race between a window closing and another
/// one opening.
///
/// Every failure is silent on purpose: the process is on its way out, there is
/// no window left to report into, and a store that would not stop is a
/// smaller problem than a dialog from an application that has already gone.
fn stop_the_store_if_nothing_else_wants_it(root: Option<&std::path::Path>) {
    let outcome = match root {
        Some(root) => koru_api_client::Client::open_at_with_scopes(
            koru_workspace::fromstore::segment(),
            root,
            &[koru_api_client::CONTROL_SCOPE],
        ),
        None => koru_api_client::Client::open_with_scopes(
            koru_workspace::fromstore::segment(),
            &[koru_api_client::CONTROL_SCOPE],
        ),
    }
    .and_then(|client| {
        client
            .shutdown_if_idle()
            .map_err(koru_store_core::errors::Error::from)
    });
    match outcome {
        Ok(true) => tracing::info!(
            target: "koru::startup",
            "the last window closed and no agent is running, so the store was stopped"
        ),
        Ok(false) => tracing::info!(
            target: "koru::startup",
            "the last window closed, but something is still connected to the store"
        ),
        Err(error) => tracing::warn!(
            target: "koru::startup",
            %error,
            "could not ask the store to stop on the way out"
        ),
    }
}

/// How long a window waits for the store it expects its agent to be starting,
/// and how often it asks again. The bound is `ensure_running`'s own launch
/// timeout, because the thing being waited for is the same process coming up.
const STORE_OPEN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);
const STORE_OPEN_RETRY: std::time::Duration = std::time::Duration::from_millis(100);

/// Only "nothing is listening" is worth waiting on, and only inside the
/// patience window. A refused session, a version mismatch or a missing profile
/// would still be one after a wait, and waiting on those hides the real error
/// behind ten seconds of nothing.
fn store_open_should_wait(
    code: koru_store_core::errors::Code,
    elapsed: std::time::Duration,
) -> bool {
    code == koru_store_core::errors::Code::StoreUnreachable && elapsed < STORE_OPEN_PATIENCE
}

/// Whether closing this window is the moment to ask "keep Koru ready?" (or to
/// hide to the tray on a remembered answer). Only the last window, and never
/// while the agent is closing windows for an update — then the window has to
/// *leave*, or the agent waits its whole deadline for a process that merely
/// hid and the update is reported as "a window did not close in time".
fn last_window_close_asks_about_the_tray(open_windows: usize, closing_for_update: bool) -> bool {
    open_windows == 1 && !closing_for_update
}

/// Whether a new window's remembered maximise is deferred to its reveal.
///
/// Every window is created hidden until its first frame has been presented, and
/// winit's `with_maximized(true)` on a hidden window *shows it* — `set_maximized`
/// runs after `CreateWindowExW` and applies `ShowWindow(SW_MAXIMIZE)` — so the
/// attributes may never carry the maximise. It is applied at reveal instead, and
/// only for the first ordinary window, which is the one that restores the
/// remembered session.
fn maximize_at_reveal(first: bool, remembered: bool, global_search: bool) -> bool {
    first && remembered && !global_search
}

/// Which panes are offered a landing, and in what order.
///
/// Generic over the id so the policy can be read — and tested — without a
/// window: `WindowId` cannot be built in a test, and a routing rule that only
/// exists inside a function taking `&mut self` over a `HashMap` of live windows
/// is a rule nothing can check.
///
/// Two orders, because there are two kinds of landing:
///
/// - **A destination somebody chose** (a search hit, a pill, Back). The window
///   that asked comes first, then the primary container, then the rest — and
///   then the whole list is stably re-sorted so a window that already *holds*
///   the document is offered it first. Raising the copy that is open beats
///   putting a second one in front of somebody, and a dedicated search window
///   declines the offer anyway, which is why an order rather than a single
///   choice.
/// - **Half of a gesture** (`source_only`): the window that asked and no other.
///   The preference above is exactly wrong here — a second window holding the
///   document would take the landing, open the picker nobody asked it for, and
///   strand the request in the window that did ask (Q44). See
///   [`crate::SearchTargetDisposition::SourceWindow`].
fn landing_order<Id: Copy + PartialEq>(
    source: Id,
    primary: Option<Id>,
    panes: &[(Id, Option<crate::TabPresence>)],
    source_only: bool,
) -> Vec<Id> {
    let known = |id: Id| panes.iter().any(|(pane, _)| *pane == id);
    if source_only {
        // A pane that has gone since the request was raised is not replaced by
        // somebody else's: the gesture is dropped, and the window that made it
        // is not there to miss it.
        return if known(source) {
            vec![source]
        } else {
            Vec::new()
        };
    }
    let mut preferred = Vec::with_capacity(panes.len());
    if known(source) {
        preferred.push(source);
    }
    if let Some(primary) = primary
        && primary != source
        && known(primary)
    {
        preferred.push(primary);
    }
    for (id, _) in panes {
        if !preferred.contains(id) {
            preferred.push(*id);
        }
    }
    let presence = |id: Id| {
        panes
            .iter()
            .find(|(pane, _)| *pane == id)
            .and_then(|(_, presence)| *presence)
    };
    let mut ordered = Vec::with_capacity(preferred.len());
    for want in [
        Some(crate::TabPresence::Pinned),
        Some(crate::TabPresence::Open),
    ] {
        for id in &preferred {
            if presence(*id) == want {
                ordered.push(*id);
            }
        }
    }
    for id in &preferred {
        if !ordered.contains(id) {
            ordered.push(*id);
        }
    }
    ordered
}

/// Keep a notification's semantic Snooze ask behind navigation already
/// retained from the preceding Agent frame. `PaneRequest` is consumed as a
/// stack, hence insertion at the front rather than another push.
fn queue_agent_snooze(queue: &mut Vec<PaneRequest>, intent: crate::ReminderSnoozeIntent) {
    queue.insert(0, PaneRequest::ReminderSnooze(intent));
}

/// One change to a workspace's switcher appearance — the three verbs that
/// share `set_workspace_flag`.
#[derive(Clone, Debug, PartialEq, Eq)]
enum WorkspaceFlag {
    Appearance {
        icon: Option<String>,
        color: Option<String>,
    },
    Pinned(bool),
    Archived(bool),
}

/// The four appearance values the Store writes together, plus the name an
/// error report quotes.
#[derive(Clone, Debug, PartialEq, Eq)]
struct WorkspaceAppearance {
    name: String,
    icon: Option<String>,
    color: Option<String>,
    pinned: bool,
    archived: bool,
}

impl WorkspaceFlag {
    /// Apply to the open profile's view — the path a workspace of the open
    /// profile takes.
    fn apply(
        &self,
        profile: &mut koru_core::profile::ProfileDoc,
        id: WorkspaceId,
    ) -> koru_core::schema::Result<()> {
        match self {
            Self::Appearance { icon, color } => {
                profile.set_workspace_icon(id, icon.as_deref())?;
                profile.set_workspace_color(id, color.as_deref())
            }
            Self::Pinned(on) => profile.set_workspace_pinned(id, *on),
            Self::Archived(on) => profile.set_workspace_archived(id, *on),
        }
    }

    /// The catalog's current values with this one change made. The Store's
    /// appearance write replaces all four, so the three this verb does not
    /// touch are carried from the catalog unchanged.
    fn applied_to(&self, workspace: &koru_store_core::model::Workspace) -> WorkspaceAppearance {
        let mut values = WorkspaceAppearance {
            name: workspace.name.clone(),
            icon: workspace.icon.clone(),
            color: workspace.color.clone(),
            pinned: workspace.pinned,
            archived: workspace.archived,
        };
        match self {
            Self::Appearance { icon, color } => {
                values.icon = icon.clone();
                values.color = color.clone();
            }
            Self::Pinned(on) => values.pinned = *on,
            Self::Archived(on) => values.archived = *on,
        }
        values
    }
}

/// The catalog row of `id` when it belongs to a profile this window has not
/// opened — the open `ProfileDoc` does not list it. `None` sends the verb
/// down the open-profile path, which is also where an unknown id is reported.
fn other_profile_workspace<'a>(
    profile: &koru_core::profile::ProfileDoc,
    catalog: &'a [koru_store_core::model::Workspace],
    id: WorkspaceId,
) -> Option<&'a koru_store_core::model::Workspace> {
    if profile.workspace(id).is_some() {
        return None;
    }
    catalog
        .iter()
        .find(|workspace| workspace.id.as_uuid() == id.as_uuid())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A canvas counts as a document** in the workspace manager's column:
    /// pages and canvases together, and nothing else the Store counts — and
    /// only pages while the build hides canvases (QUESTIONS #7).
    #[test]
    fn the_document_count_is_pages_and_shown_canvases() {
        let mut kinds = std::collections::BTreeMap::new();
        kinds.insert(koru_core::explore::KIND_DOCUMENT.to_owned(), 4);
        kinds.insert(koru_core::explore::KIND_CANVAS.to_owned(), 2);
        kinds.insert(koru_core::explore::KIND_FILE.to_owned(), 7);
        kinds.insert(koru_core::explore::KIND_NOTE.to_owned(), 1);
        assert_eq!(document_count(&kinds, true), 6);
        assert_eq!(document_count(&kinds, false), 4);
        assert_eq!(document_count(&std::collections::BTreeMap::new(), true), 0);
        // The call site reads the flag: all-off hides canvases.
        koru_features::testing::with(koru_features::Features::ALL_OFF, || {
            assert_eq!(document_count(&kinds, koru_features::canvas()), 4);
        });
    }

    fn catalog_workspace(
        owner: koru_core::ProfileId,
        name: &str,
    ) -> koru_store_core::model::Workspace {
        let mut workspace = koru_store_core::model::Workspace::new(
            koru_store_core::ids::ProfileId::from_uuid(owner.as_uuid()),
            name,
            koru_store_core::model::Timestamp::from_millis(1),
        )
        .unwrap();
        workspace.icon = Some("🌿".into());
        workspace.color = Some("green".into());
        workspace.pinned = true;
        workspace
    }

    /// The Manage tab lists every profile's workspaces, so a verb can name a
    /// workspace the open `ProfileDoc` does not hold. That one is written
    /// straight to the Store from its catalog row; the open profile's own
    /// workspaces, and ids nobody knows, keep the original path.
    #[test]
    fn only_a_workspace_the_open_profile_lacks_takes_the_catalog_path() {
        let open = koru_core::ProfileId::new();
        let other = koru_core::ProfileId::new();
        let mut profile = koru_core::profile::ProfileDoc::create(open, "Open", None).unwrap();
        let mine = catalog_workspace(open, "Mine");
        let theirs = catalog_workspace(other, "Theirs");
        let mine_id = WorkspaceId::from_uuid(mine.id.as_uuid());
        let theirs_id = WorkspaceId::from_uuid(theirs.id.as_uuid());
        profile.ensure_workspace(mine_id, "Mine", 1).unwrap();
        let catalog = vec![mine, theirs];

        assert!(other_profile_workspace(&profile, &catalog, mine_id).is_none());
        assert_eq!(
            other_profile_workspace(&profile, &catalog, theirs_id).map(|w| w.name.as_str()),
            Some("Theirs")
        );
        assert!(other_profile_workspace(&profile, &catalog, WorkspaceId::new()).is_none());
    }

    /// The Store's appearance write replaces icon, colour, pinned and
    /// archived together, so another profile's workspace keeps the three a
    /// verb does not change exactly as its catalog row has them.
    #[test]
    fn another_profiles_workspace_changes_only_the_one_flag() {
        let workspace = catalog_workspace(koru_core::ProfileId::new(), "Theirs");
        let base = WorkspaceAppearance {
            name: "Theirs".into(),
            icon: Some("🌿".into()),
            color: Some("green".into()),
            pinned: true,
            archived: false,
        };
        assert_eq!(
            WorkspaceFlag::Appearance {
                icon: Some("📚".into()),
                color: None,
            }
            .applied_to(&workspace),
            WorkspaceAppearance {
                icon: Some("📚".into()),
                color: None,
                ..base.clone()
            }
        );
        assert_eq!(
            WorkspaceFlag::Pinned(false).applied_to(&workspace),
            WorkspaceAppearance {
                pinned: false,
                ..base.clone()
            }
        );
        assert_eq!(
            WorkspaceFlag::Archived(true).applied_to(&workspace),
            WorkspaceAppearance {
                archived: true,
                ..base
            }
        );
    }

    /// The open profile's path is unchanged: the flag lands on its
    /// `ProfileDoc` entry, which is what the Store write then reads.
    #[test]
    fn the_open_profiles_workspace_is_still_edited_through_its_profile() {
        let open = koru_core::ProfileId::new();
        let mut profile = koru_core::profile::ProfileDoc::create(open, "Open", None).unwrap();
        let id = WorkspaceId::new();
        profile.ensure_workspace(id, "Mine", 1).unwrap();
        WorkspaceFlag::Appearance {
            icon: Some("📚".into()),
            color: Some("blue".into()),
        }
        .apply(&mut profile, id)
        .unwrap();
        WorkspaceFlag::Pinned(true).apply(&mut profile, id).unwrap();
        let entry = profile.workspace(id).unwrap();
        assert_eq!(entry.icon.as_deref(), Some("📚"));
        assert_eq!(entry.color.as_deref(), Some("blue"));
        assert!(entry.pinned);
        assert!(
            WorkspaceFlag::Pinned(true)
                .apply(&mut profile, WorkspaceId::new())
                .is_err(),
            "an id the profile does not hold is still an error on this path"
        );
    }

    /// The catalog is refetched for the three facts that change what it
    /// answers, and for nothing else — a dictionary wake is the spelling
    /// overlay's business and must not turn into a profile refetch.
    #[test]
    fn only_a_catalog_changing_profile_fact_asks_for_a_refresh() {
        assert!(profile_catalog_event("profile.created"));
        assert!(profile_catalog_event("profile.renamed"));
        assert!(profile_catalog_event("profile.appearance_changed"));
        assert!(!profile_catalog_event("profile.dictionary.changed"));
        assert!(!profile_catalog_event("workspace.catalog.renamed"));
        assert!(!profile_catalog_event("profile"));
    }

    #[test]
    fn live_profile_name_keeps_store_owned_account_attribution() {
        let live = koru_core::ProfileSummary {
            id: koru_core::ProfileId::new(),
            name: "Renamed".into(),
            email: String::new(),
            avatar: None,
            icon: None,
            color: None,
            account: None,
            folder: String::new(),
        };
        let mut catalog = live.clone();
        catalog.name = "Previous name".into();
        catalog.account = Some("koru:10000000-0000-4000-8000-000000000041".into());
        let projected = pane_profile_summary(live, Some(&catalog));
        assert_eq!(projected.name, "Renamed");
        assert_eq!(projected.account, catalog.account);
    }

    #[test]
    fn deferred_connection_keeps_original_profile_and_correlates_admission_to_owner_and_submission()
    {
        let profile = koru_core::ProfileId::new();
        let submission = crate::account_ui::CommandSubmissionId::next();
        let mut pending = PendingConnectedProfileCreation {
            owner_scope: 17,
            submission: Some(submission),
            generation: None,
            source: WindowId::from(1u64),
            owner_display_name: "A owner".into(),
            profile_name: "Existing local Profile".into(),
            workspace_name: "Work".into(),
            connect_account: true,
            stage: ConnectedProfileCreationStage::CreatingLocally,
        };
        pending.stage = pending.stage.after_local_creation(profile);
        let original = pending.retry_command(profile);
        pending.stage = ConnectedProfileCreationStage::AwaitingAccountReturn(profile);
        assert_eq!(pending.retry_command(profile), original);
        assert!(
            matches!(original,koru_agent_wire::AccountCommand::CreateProfile { ref profile_id,ref owner_display_name,.. } if *profile_id==profile.to_storage() && owner_display_name=="A owner")
        );
        assert!(
            !pending.accepts_admission(crate::account_ui::CommandAdmission {
                submission,
                owner_scope: 18,
                generation: 50
            })
        );
        assert!(
            !pending.accepts_admission(crate::account_ui::CommandAdmission {
                submission: crate::account_ui::CommandSubmissionId::next(),
                owner_scope: 17,
                generation: 50
            })
        );
        assert!(
            pending.accepts_admission(crate::account_ui::CommandAdmission {
                submission,
                owner_scope: 17,
                generation: 50
            })
        );
        assert_eq!(
            pending.stage.waiting_on_agent(false),
            None,
            "a parked retry cannot become an automatic Profile switch"
        );
        assert_eq!(
            pending.stage.after_hosted_ack(profile),
            ConnectedProfileCreationStage::AwaitingLocalBinding(profile)
        );
        assert_eq!(pending.owner_scope, 17);
    }

    #[test]
    fn workspace_routed_press_adopts_only_the_same_physical_native_press() {
        let d = PhysicalKey::Code(KeyCode::KeyD);
        let m = PhysicalKey::Code(KeyCode::KeyM);
        for mode in [
            koru_dictation::Mode::PushToTalk,
            koru_dictation::Mode::Toggle,
        ] {
            assert_eq!(
                workspace_routed_press(None, 7, koru_dictation::Stage::Live, Some(d), d),
                koru_dictation::RoutedPress::Adopt,
                "same-key native duplicate in {mode:?}"
            );
            assert_eq!(
                workspace_routed_press(None, 7, koru_dictation::Stage::Live, Some(d), m),
                koru_dictation::RoutedPress::Refuse,
                "different-key session in {mode:?}"
            );
            assert_eq!(
                workspace_routed_press(None, 7, koru_dictation::Stage::Live, None, d),
                koru_dictation::RoutedPress::Refuse,
                "command-started session in {mode:?}"
            );
        }
        assert_eq!(
            workspace_routed_press(Some(7), 7, koru_dictation::Stage::Live, Some(d), d),
            koru_dictation::RoutedPress::Continue
        );
        assert_eq!(
            workspace_routed_press(Some(8), 7, koru_dictation::Stage::Live, Some(d), d),
            koru_dictation::RoutedPress::Ignore
        );
    }

    #[test]
    fn agent_disconnect_discards_routed_listening_clears_owner_and_accepts_restart() {
        let mut dictation = koru_dictation::Dictation::new();
        dictation.set_consent(Some(koru_voice_consent::Granted::recorded(
            koru_voice_consent::Terms {
                model: "parakeet-tdt-v3".into(),
                version: "1.2.0".into(),
                licence: "Apache-2.0".into(),
                attribution: "k2-fsa, Apache-2.0".into(),
                download_bytes: 1,
            },
            1,
        )));
        let now = Instant::now();
        assert!(matches!(
            dictation.press(now),
            Some(koru_dictation::DictationAction::Begin { .. })
        ));
        dictation.began(true);
        let mut route = Some(7);

        assert!(clear_agent_route_on_disconnect(
            &mut route,
            dictation.stage()
        ));
        dictation.interrupt(Interruption::Suspended);
        let actions = dictation.pump(now);
        assert_eq!(route, None);
        assert!(
            actions
                .iter()
                .any(|action| matches!(action, koru_dictation::DictationAction::Cancel { .. }))
        );
        assert!(
            !actions
                .iter()
                .any(|action| matches!(action, koru_dictation::DictationAction::Commit { .. }))
        );
        assert_eq!(dictation.stage(), koru_dictation::Stage::Idle);
        assert_eq!(
            workspace_routed_press(
                route,
                8,
                dictation.stage(),
                None,
                PhysicalKey::Code(KeyCode::KeyD)
            ),
            koru_dictation::RoutedPress::Begin,
            "a restarted Agent's fresh route is accepted"
        );
    }

    #[test]
    fn native_snooze_is_serviced_after_the_preceding_navigation_without_repeating_it() {
        let intent = crate::ReminderSnoozeIntent {
            profile: koru_store_core::ProfileId::new(),
            reminder: koru_store_core::ReminderId::new(),
            occurrence: koru_store_core::OccurrenceKey::parse(&format!("occ1-{}", "01".repeat(32)))
                .unwrap(),
            entity: koru_store_core::EntityRef::new(
                "task",
                koru_store_core::WorkspaceId::new(),
                koru_store_core::DocumentId::new(),
                uuid::Uuid::new_v4(),
            )
            .unwrap(),
        };
        let mut queue = vec![PaneRequest::OpenSearch];

        queue_agent_snooze(&mut queue, intent.clone());

        assert_eq!(queue.len(), 2, "Snooze must not add another navigation");
        assert_eq!(queue.pop(), Some(PaneRequest::OpenSearch));
        assert_eq!(queue.pop(), Some(PaneRequest::ReminderSnooze(intent)));
    }

    /// **A discoverable agent counts, even when this process never linked to
    /// one.** The own link is dialled once at start-up and never retried, so
    /// `false` there means "my link was not up when I started", not "no agent
    /// exists" — and reading it as the second was how the last Koru window
    /// closing stopped the gateway a live Koru Terminal was using.
    ///
    /// The safe direction is asymmetric on purpose: any sign of an agent leaves
    /// the store alone, because a store left running is a process nobody asked
    /// for while a store stopped under a live window is work that visibly
    /// breaks.
    #[test]
    fn a_discoverable_agent_keeps_the_store_running_even_with_no_own_link() {
        assert!(
            !stop_the_store_on_exit(false, true),
            "an agent this process never linked to still owns the store's lifetime"
        );
        assert!(
            !stop_the_store_on_exit(true, false),
            "a live own link is conclusive on its own"
        );
        assert!(!stop_the_store_on_exit(true, true));
        assert!(
            stop_the_store_on_exit(false, false),
            "with no agent by either answer, the last window turns the lights off"
        );
    }

    #[test]
    fn resumed_onboarding_keeps_one_live_neutral_window() {
        assert!(needs_onboarding_window(false));
        assert!(!needs_onboarding_window(true));
        // `OnboardingApp` has no ProfileRuntime and its ApplicationHandler has
        // no call to `Host::agent_showing`; those absences are the no-id/no-Hello
        // boundary rather than a runtime flag ordinary editing must remember.
    }

    #[test]
    fn signed_in_profile_catalog_is_requested_once_until_the_session_leaves() {
        let mut requested = false;
        assert!(take_signed_in_profile_request(
            &mut requested,
            koru_agent_wire::AccountStage::SignedIn
        ));
        assert!(!take_signed_in_profile_request(
            &mut requested,
            koru_agent_wire::AccountStage::SignedIn
        ));
        assert!(!take_signed_in_profile_request(
            &mut requested,
            koru_agent_wire::AccountStage::Refreshing
        ));
        assert!(take_signed_in_profile_request(
            &mut requested,
            koru_agent_wire::AccountStage::SignedIn
        ));
    }

    #[test]
    fn a_created_profile_enters_account_connection_only_once() {
        let mut connect_created_profile = true;
        assert!(take_created_profile_connection(
            &mut connect_created_profile
        ));
        assert!(!take_created_profile_connection(
            &mut connect_created_profile
        ));
    }

    #[test]
    fn empty_catalog_still_reconciles_removed_profile_settings() {
        let launch = koru_workspace::fromstore::StoreLaunch::NeedsProfile;
        assert!(live_profile_ids_for_settings(&launch).is_empty());
    }

    #[test]
    fn failed_connected_profile_worker_start_clears_the_retry_latch() {
        let mut pending = Some("draft");
        assert!(clear_failed_connected_profile_start(&mut pending));
        assert!(pending.is_none());
        assert!(!clear_failed_connected_profile_start(&mut pending));
    }

    #[test]
    fn failed_connected_profile_reopen_never_authorizes_welcome_on_the_old_profile() {
        let old = koru_core::ProfileId::new();
        let created = koru_core::ProfileId::new();
        assert!(!connected_profile_is_open(Some(old), created));
        assert!(connected_profile_is_open(Some(created), created));
        assert!(!connected_profile_is_open(None, created));
    }

    #[test]
    fn connected_profile_is_complete_only_after_store_hosted_and_local_acks() {
        let profile = koru_core::ProfileId::new();
        let creating = ConnectedProfileCreationStage::CreatingLocally;
        assert!(!creating.completes_after_local_ack(profile));

        let hosted = creating.after_local_creation(profile);
        assert_eq!(
            hosted,
            ConnectedProfileCreationStage::AwaitingHostedProfile(profile)
        );
        assert!(!hosted.completes_after_local_ack(profile));

        let local = hosted.after_hosted_ack(profile);
        assert_eq!(
            local,
            ConnectedProfileCreationStage::AwaitingLocalBinding(profile)
        );
        assert!(local.completes_after_local_ack(profile));
        assert!(!local.completes_after_local_ack(koru_core::ProfileId::new()));
    }

    #[test]
    fn agent_loss_releases_an_already_saved_connected_profile() {
        let profile = koru_core::ProfileId::new();
        assert_eq!(
            ConnectedProfileCreationStage::AwaitingHostedProfile(profile).waiting_on_agent(false),
            Some(profile)
        );
        assert_eq!(
            ConnectedProfileCreationStage::AwaitingLocalBinding(profile).waiting_on_agent(false),
            Some(profile)
        );
        assert_eq!(
            ConnectedProfileCreationStage::AwaitingHostedProfile(profile).waiting_on_agent(true),
            None
        );
        assert_eq!(
            ConnectedProfileCreationStage::CreatingLocally.waiting_on_agent(false),
            None,
            "Store creation itself still owns the durable boundary"
        );
    }

    #[test]
    fn duplicate_reminder_startup_answers_are_coalesced_across_windows() {
        let mut queue = ReminderStartupQueue::default();
        let first = ReminderStartupRequest::new(true);
        let second = ReminderStartupRequest::new(true);
        assert_eq!(queue.enqueue(first), ReminderStartupEnqueue::Start);
        assert_eq!(queue.enqueue(second), ReminderStartupEnqueue::Coalesced);
        let completed = queue.finish();
        assert_eq!(completed.waiters, vec![first.id, second.id]);
        assert!(completed.superseded.is_empty());
        assert!(completed.next.is_none());
        assert_eq!(queue, ReminderStartupQueue::default());
    }

    #[test]
    fn conflicting_reminder_startup_answers_are_serialized() {
        let mut queue = ReminderStartupQueue::default();
        let first = ReminderStartupRequest::new(false);
        let second = ReminderStartupRequest::new(true);
        assert_eq!(queue.enqueue(first), ReminderStartupEnqueue::Start);
        assert_eq!(queue.enqueue(second), ReminderStartupEnqueue::Queued);
        let completed = queue.finish();
        assert_eq!(completed.waiters, vec![first.id]);
        assert!(completed.superseded.is_empty());
        assert_eq!(completed.next, Some(second));

        assert_eq!(queue.enqueue(second), ReminderStartupEnqueue::Start);
        let completed = queue.finish();
        assert_eq!(completed.waiters, vec![second.id]);
        assert!(completed.next.is_none());
    }

    #[test]
    fn latest_reminder_startup_answer_wins_three_window_races() {
        for first in [false, true] {
            let mut queue = ReminderStartupQueue::default();
            let a = ReminderStartupRequest::new(first);
            let b = ReminderStartupRequest::new(!first);
            let c = ReminderStartupRequest::new(first);
            assert_eq!(queue.enqueue(a), ReminderStartupEnqueue::Start);
            assert_eq!(queue.enqueue(b), ReminderStartupEnqueue::Queued);
            assert_eq!(queue.enqueue(c), ReminderStartupEnqueue::Coalesced);
            let completed = queue.finish();
            assert_eq!(completed.waiters, vec![a.id, c.id]);
            assert_eq!(completed.superseded, vec![b.id]);
            assert!(completed.next.is_none());
        }
    }

    #[test]
    fn reminder_startup_facts_are_projected_from_the_global_authority() {
        let section = koru_settings::GlobalSettings {
            start_on_login: true,
            reminder_start_on_login_explained: true,
            ..Default::default()
        };
        assert_eq!(
            reminder_startup_facts(&section),
            koru_window::ReminderStartupFacts {
                start_on_login: true,
                explained: true,
            }
        );
    }

    #[test]
    fn failure_before_profile_acknowledgement_is_retryable() {
        let called = std::cell::Cell::new(false);
        let outcome = OnboardingCheckpoint::BeforeProfile.resolve(
            || "retry",
            |_, _| {
                called.set(true);
                "committed"
            },
        );
        assert_eq!(outcome, "retry");
        assert!(
            !called.get(),
            "pre-commit failure must not attempt a strict reopen"
        );
    }

    #[test]
    fn failure_after_profile_acknowledgement_reopens_that_profile() {
        let profile = koru_core::ProfileId::new();
        let target = OnboardingCheckpoint::Profile { profile }.resolve(
            || None,
            |reopened_profile, workspace| Some((reopened_profile, workspace)),
        );
        assert_eq!(target, Some((profile, None)));
    }

    #[test]
    fn failure_after_workspace_acknowledgement_reopens_both_ids() {
        let profile = koru_core::ProfileId::new();
        let workspace = koru_core::WorkspaceId::new();
        let target = OnboardingCheckpoint::Workspace { profile, workspace }.resolve(
            || None,
            |reopened_profile, reopened_workspace| Some((reopened_profile, reopened_workspace)),
        );
        assert_eq!(target, Some((profile, Some(workspace))));
    }

    #[test]
    fn accepting_reminder_startup_patches_both_device_local_facts() {
        let patch = reminder_startup_patch(true);
        assert_eq!(patch.start_on_login, Some(true));
        assert_eq!(patch.reminder_start_on_login_explained, Some(true));
        assert!(patch.touches_device());
        assert!(!patch.touches_roaming());
    }

    #[test]
    fn declining_reminder_startup_records_only_the_explanation() {
        let patch = reminder_startup_patch(false);
        assert_eq!(patch.start_on_login, None);
        assert_eq!(patch.reminder_start_on_login_explained, Some(true));
        assert!(patch.touches_device());
        assert!(!patch.touches_roaming());
    }

    #[test]
    fn plain_enter_finishes_only_a_slash_commanded_dictation() {
        let enter = Key::Named(NamedKey::Enter);
        assert!(finishes_commanded_dictation(
            true,
            &enter,
            ModifiersState::empty()
        ));
        assert!(!finishes_commanded_dictation(
            false,
            &enter,
            ModifiersState::empty()
        ));
        assert!(!finishes_commanded_dictation(
            true,
            &Key::Character("x".into()),
            ModifiersState::empty()
        ));
        assert!(!finishes_commanded_dictation(
            true,
            &enter,
            ModifiersState::SHIFT
        ));
    }

    /// **Q44.** Two windows on one workspace, and the second one holds the
    /// document — pinned, so it wins the ordinary preference outright. A search
    /// hit still goes there, which is the point of that preference. A gesture
    /// does not: it is half-finished business belonging to the window it was
    /// made in, and offering the landing anywhere else opens a picker nobody
    /// asked for in one window while the other waits forever.
    #[test]
    fn a_gesture_lands_only_in_the_window_that_asked() {
        use crate::TabPresence;
        let (a, b) = (1u8, 2u8);
        let panes = [
            (a, Some(TabPresence::Absent)),
            (b, Some(TabPresence::Pinned)),
        ];

        // The search-result order: B first, because it already has the page.
        assert_eq!(landing_order(a, Some(a), &panes, false), vec![b, a]);
        // The gesture order: A, and nothing after it to fall through to.
        assert_eq!(landing_order(a, Some(a), &panes, true), vec![a]);
        // ...and the same when A is neither source-preferred nor the primary.
        assert_eq!(landing_order(b, Some(a), &panes, true), vec![b]);

        // A window that has gone takes no landing, and does not hand its
        // gesture to a bystander.
        assert!(landing_order(3u8, Some(a), &panes, true).is_empty());
    }

    /// The rest of the preference order, which the gesture case must not have
    /// disturbed: the asking window first, then the primary, then whoever is
    /// left — and all of it re-sorted so a pinned tab beats an open one beats
    /// no tab at all.
    #[test]
    fn a_chosen_destination_prefers_the_window_already_holding_it() {
        use crate::TabPresence;
        let (a, b, c) = (1u8, 2u8, 3u8);
        let panes = [
            (c, Some(TabPresence::Absent)),
            (b, Some(TabPresence::Open)),
            (a, Some(TabPresence::Absent)),
        ];
        assert_eq!(landing_order(a, Some(c), &panes, false), vec![b, a, c]);

        // Nothing knows anything about the document (a workspace-level hit):
        // the plain source/primary/rest order survives.
        let unknown = [(c, None), (b, None), (a, None)];
        assert_eq!(landing_order(a, Some(c), &unknown, false), vec![a, c, b]);
    }

    /// A remembered maximise belongs to the reveal, never to the attributes —
    /// winit shows a hidden window when the attributes carry it — and only the
    /// first ordinary window restores it.
    #[test]
    fn a_remembered_maximise_is_deferred_to_the_reveal() {
        assert!(maximize_at_reveal(true, true, false));
        // Later windows and the search panel start unmaximised, like the
        // remembered pin.
        assert!(!maximize_at_reveal(false, true, false));
        assert!(!maximize_at_reveal(true, true, true));
        assert!(!maximize_at_reveal(true, false, false));
    }

    /// The nightly's first launch on a Mac: the agent answered, Koru dialed
    /// the store once, 0.36 s before it bound, and declared it unreachable.
    /// Not listening yet is waited on, up to the launch timeout; anything
    /// else — and not-listening past the timeout — is the fatal dialog.
    #[test]
    fn a_store_that_is_not_listening_yet_is_waited_for_and_nothing_else_is() {
        use koru_store_core::errors::Code;
        use std::time::Duration;
        assert!(store_open_should_wait(
            Code::StoreUnreachable,
            Duration::ZERO
        ));
        assert!(store_open_should_wait(
            Code::StoreUnreachable,
            Duration::from_secs(9)
        ));
        assert!(!store_open_should_wait(
            Code::StoreUnreachable,
            STORE_OPEN_PATIENCE
        ));
        assert!(!store_open_should_wait(
            Code::ProfileNotFound,
            Duration::ZERO
        ));
        assert!(!store_open_should_wait(
            Code::Unauthenticated,
            Duration::ZERO
        ));
    }

    /// The beta.5 → beta.8 install on a Mac that had chosen "keep Koru in
    /// the tray": the agent asked the windows to close, the last one hid to
    /// the tray instead, and the agent timed out with nothing replaced. A
    /// close for an update quits; an ordinary last-window close still asks.
    #[test]
    fn a_close_for_an_update_never_detours_through_the_tray() {
        assert!(last_window_close_asks_about_the_tray(1, false));
        assert!(!last_window_close_asks_about_the_tray(1, true));
        assert!(!last_window_close_asks_about_the_tray(2, false));
        assert!(!last_window_close_asks_about_the_tray(2, true));
    }

    /// The daemon names the devices a parked hydration waits on; the sentence
    /// the user reads should name them too, and stay generic when nobody is
    /// named.
    #[test]
    fn a_parked_download_names_the_device_it_waits_for() {
        let error = "storage_failed: blob hydration failed: blob_waiting_for_peer (retryable=true) waiting_on=Pixel 8\u{1f}Studio";
        assert_eq!(
            waiting_device_names(error).as_deref(),
            Some("Pixel 8, Studio")
        );
        assert!(download_failure_hint(error).contains("(Pixel 8, Studio)"));

        let unnamed =
            "storage_failed: blob hydration failed: blob_waiting_for_peer (retryable=true)";
        assert_eq!(waiting_device_names(unnamed), None);
        assert!(download_failure_hint(unnamed).contains("The device holding this file is offline"));
    }

    #[test]
    fn a_key_press_is_dropped_exactly_when_the_window_is_unfocused() {
        // The Alt+Tab trace: the trailing Tab arrives after `Focused(false)`,
        // with the modifier bookkeeping already cleared. Dropped here, it
        // never reaches the editor as a bare Tab.
        assert!(drop_unfocused_key_press(Some(false)));
        assert!(!drop_unfocused_key_press(Some(true)));
        assert!(
            !drop_unfocused_key_press(None),
            "an unknown window is the dispatch path's own refusal, not this gate's"
        );
    }

    /// The caret-moves-but-typing-does-not failure after pinning a tab: mouse
    /// is delivered to an unfocused pane and keys are not. The press has to
    /// ask for the keyboard, or the next keystroke is `dropped=window-unfocused`.
    #[test]
    fn a_press_on_an_unfocused_window_asks_for_the_keyboard() {
        let source = include_str!("app.rs");
        assert!(
            source.contains("shell focus-on-press"),
            "a click that places the caret must also take the keyboard back"
        );
        assert!(
            source.contains("if !pane.focused"),
            "only when the pane has actually lost OS focus, not on every click"
        );
    }

    /// Pinning rebuilds the Jump List and Explorer steals focus, which clears
    /// `self.mods`. The next Ctrl+click has to read the physical Ctrl bit or
    /// it is SameTab against the pin.
    #[test]
    fn a_click_repairs_modifiers_cleared_by_a_focus_loss() {
        let source = include_str!("app.rs");
        assert!(
            source.contains("shell click-mods"),
            "a click after focus loss must log reported vs effective modifiers"
        );
        assert!(
            source.contains("current_modifier_state(modifiers_with_pressed_keys"),
            "Ctrl on the click is the OS/physical set, not the snapshot Focused(false) cleared"
        );
        assert!(
            source.contains("adopt_effective_mods"),
            "repairing mods without forgetting down_keys lets the next key put Ctrl back"
        );
    }

    #[test]
    fn a_held_modifier_repeat_is_not_a_key() {
        assert!(is_modifier_key(
            PhysicalKey::Code(KeyCode::ControlLeft),
            &Key::Named(NamedKey::Control)
        ));
        assert!(!is_modifier_key(
            PhysicalKey::Code(KeyCode::KeyA),
            &Key::Character("a".into())
        ));
        let source = include_str!("app.rs");
        assert!(
            source.contains("event.repeat && is_modifier_key"),
            "Ctrl auto-repeat after a Ctrl+click must not keep the bit held"
        );
    }

    #[test]
    fn temporary_open_paths_are_leaf_only_and_keep_the_extension() {
        let id = koru_store_core::ids::DocumentId::new();
        let path = temporary_open_path(id, "..\\..\\CON:<report>?.pdf");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("koru-CON__report__.pdf")
        );
        assert!(path.ends_with(format!("{}/koru-CON__report__.pdf", id.to_storage())));
    }

    #[test]
    fn an_empty_shared_link_is_not_handed_to_windows_as_a_shortcut() {
        let id = koru_store_core::ids::DocumentId::new();
        let err = open_file_bytes(id, "Shared link.url", Some("text/uri-list"), b"\n")
            .expect_err("no URL to open");
        assert!(err.contains("no URL to open"));
    }

    #[test]
    fn evernote_task_warning_names_the_note_and_the_details_that_were_not_imported() {
        let warning = koru_import::ImportWarning {
            source: PathBuf::from("Notebook.enex"),
            code: Some(koru_import::ImportWarningCode::EvernoteTaskPayloadUnsupported),
            arguments: vec!["2".into(), "Project launch".into()],
            message: "fallback".into(),
        };

        assert_eq!(
            rendered_import_warning(koru_i18n::Lang::En, &warning),
            "Evernote note 2 (“Project launch”): task completion status, due date, reminder, recurrence, flag, and assignee were not imported. Visible task text was kept."
        );
    }

    #[test]
    fn partial_import_failures_become_a_failed_child_with_readable_details() {
        let workspace = koru_store_core::ids::WorkspaceId::new();
        let parent = koru_store_core::ids::DocumentId::new();
        let imported = failed_import_document(
            workspace,
            parent,
            Path::new("Notebook.one"),
            koru_i18n::t(koru_i18n::Lang::En, koru_i18n::Key::ImportFailedIntro),
            &[
                "Notebook.one: page 3 could not be decoded".into(),
                "Archive.one: attachment missing\nsecond detail".into(),
            ],
        )
        .unwrap();

        assert_eq!(imported.title, "FAILED");
        assert_eq!(imported.parent, Some(parent));
        let doc = koru_core::WorkspaceDoc::from_snapshot(&imported.snapshot).unwrap();
        let root = doc.children(koru_core::schema::Parent::Root).unwrap()[0];
        assert_eq!(doc.read(root).unwrap().content.as_text(), Some("FAILED"));
        let details: Vec<String> = doc
            .children(koru_core::schema::Parent::Node(root))
            .unwrap()
            .into_iter()
            .map(|id| doc.read(id).unwrap().content.as_text().unwrap().to_owned())
            .collect();
        assert_eq!(
            details,
            [
                "Some content from this source could not be imported.",
                "Notebook.one: page 3 could not be decoded",
                "Archive.one: attachment missing",
                "second detail",
            ]
        );
    }

    #[test]
    fn partial_failures_create_one_failed_child_per_source_container() {
        let workspace = koru_store_core::ids::WorkspaceId::new();
        let fallback = koru_store_core::ids::DocumentId::new();
        let a = koru_core::DocumentId::new();
        let b = koru_core::DocumentId::new();
        let roots = std::collections::BTreeMap::from([
            (PathBuf::from("a.enex"), a),
            (PathBuf::from("b.enex"), b),
        ]);
        let groups = std::collections::BTreeMap::from([
            (PathBuf::from("a.enex"), vec!["first".into()]),
            (PathBuf::from("b.enex"), vec!["second".into()]),
        ]);
        let failed =
            failed_import_documents(workspace, fallback, &roots, groups, koru_i18n::Lang::En)
                .into_iter()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();

        assert_eq!(failed.len(), 2);
        assert_eq!(failed[0].source, PathBuf::from("a.enex"));
        assert_eq!(
            failed[0].parent,
            Some(koru_store_core::ids::DocumentId::from_uuid(a.as_uuid()))
        );
        assert_eq!(failed[1].source, PathBuf::from("b.enex"));
        assert_eq!(
            failed[1].parent,
            Some(koru_store_core::ids::DocumentId::from_uuid(b.as_uuid()))
        );
    }

    #[test]
    fn deleted_import_on_an_old_store_stops_before_the_first_write() {
        let mut writes = 0;
        let error = after_deleted_import_capability_check(true, &[], || {
            writes += 1;
            Ok(())
        })
        .unwrap_err();
        assert_eq!(writes, 0);
        assert_eq!(
            error.code(),
            Some(koru_import::ImportErrorCode::StoreUpdateRequired)
        );
        assert_eq!(
            error.code().map(import_error_key),
            Some(koru_i18n::Key::ImportStoreUpdateRequired)
        );
        for lang in koru_i18n::Lang::ALL {
            assert!(!koru_i18n::t(lang, import_error_key(error.code().unwrap())).is_empty());
        }
    }

    #[test]
    fn imports_without_deleted_documents_still_run_on_an_old_store() {
        let mut writes = 0;
        after_deleted_import_capability_check(false, &[], || {
            writes += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(writes, 1);
    }

    #[test]
    fn onenote_attachment_is_not_mistaken_for_the_notebook_root() {
        let attachment = koru_core::DocumentId::new();
        let notebook = koru_core::DocumentId::new();
        assert!(
            content_import_root("onenote", koru_core::explore::KIND_FILE, None, attachment,)
                .is_none()
        );
        assert!(matches!(
            content_import_root(
                "onenote",
                koru_core::explore::KIND_DOCUMENT,
                None,
                notebook,
            ),
            Some(ContentImportRoot::Notebook(id)) if id == notebook
        ));
    }

    #[test]
    fn file_snapshot_exposes_the_content_address_used_for_deduplication() {
        use koru_core::node::{BlobHash, Node};
        use koru_core::schema::Parent;

        let workspace = koru_core::WorkspaceId::new();
        let mut doc = koru_core::WorkspaceDoc::create(workspace, "same.pdf").unwrap();
        doc.insert(
            Parent::Root,
            Node::file("same.pdf", BlobHash("b3:abc".into()), "application/pdf").unwrap(),
        )
        .unwrap();
        let snapshot = doc.export_snapshot().unwrap();
        assert_eq!(
            snapshot_file_locator(&snapshot).map(|(hash, _)| hash),
            Some("b3:abc".into())
        );
    }

    #[test]
    fn deduplicating_an_attachment_rewrites_its_pill_to_the_known_file() {
        use koru_core::ids::{DocRef, Ref};
        use koru_core::node::{Attr, AttrValue, Node};
        use koru_core::schema::Parent;

        let workspace = koru_core::WorkspaceId::new();
        let old_document = koru_core::DocumentId::new();
        let old_node = koru_core::NodeId::from_uuid(old_document.as_uuid());
        let known_document = koru_core::DocumentId::new();
        let known_node = koru_core::NodeId::new();
        let mut doc = koru_core::WorkspaceDoc::create(workspace, "Page").unwrap();
        let root = doc
            .insert(Parent::Root, Node::document("Page").unwrap())
            .unwrap();
        let paragraph = doc
            .insert(Parent::Node(root), Node::paragraph("").unwrap())
            .unwrap();
        doc.set_attr(
            paragraph,
            &Attr::new(
                "attachment",
                AttrValue::Ref(Ref::in_doc(old_node, DocRef::Document(old_document))),
            ),
        )
        .unwrap();
        let snapshot = doc.export_snapshot().unwrap();
        let rewritten = remap_imported_file_references(
            &snapshot,
            &[((
                old_document,
                KnownFile {
                    document: known_document,
                    node: known_node,
                    title: "Known.pdf".into(),
                },
            ))]
            .into_iter()
            .collect(),
        )
        .unwrap();
        let loaded = koru_core::WorkspaceDoc::from_snapshot(&rewritten).unwrap();
        let node = loaded.read(paragraph).unwrap();
        let attr = &node.content.attrs()[0];
        assert_eq!(
            attr.as_ref_target(),
            Some(Ref::in_doc(known_node, DocRef::Document(known_document)))
        );
    }

    #[test]
    fn one_os_event_per_file_becomes_one_drop_batch() {
        let mut pending = VecDeque::new();
        queue_content_drop(&mut pending, 7_u8, "markdown", PathBuf::from("one.md"));
        queue_content_drop(
            &mut pending,
            7_u8,
            "markdown",
            PathBuf::from("two.markdown"),
        );

        assert_eq!(
            pending.pop_front(),
            Some((
                7,
                "markdown",
                vec![PathBuf::from("one.md"), PathBuf::from("two.markdown")]
            ))
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn simultaneous_drops_in_different_windows_remain_separate_batches() {
        let mut pending = VecDeque::new();
        queue_content_drop(&mut pending, 1_u8, "markdown", PathBuf::from("first.md"));
        queue_content_drop(&mut pending, 2_u8, "markdown", PathBuf::from("other.md"));
        queue_content_drop(&mut pending, 1_u8, "markdown", PathBuf::from("second.md"));

        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].2.len(), 2);
        assert_eq!(pending[1].2, vec![PathBuf::from("other.md")]);
    }

    #[test]
    fn different_import_formats_remain_separate_drop_batches() {
        let mut pending = VecDeque::new();
        queue_content_drop(&mut pending, 1_u8, "markdown", PathBuf::from("note.md"));
        queue_content_drop(
            &mut pending,
            1_u8,
            "textbundle",
            PathBuf::from("archive.textbundle"),
        );

        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].1, "markdown");
        assert_eq!(pending[1].1, "textbundle");
    }

    #[test]
    fn zip_drop_is_recognised_as_a_compressed_textpack() {
        assert_eq!(
            dropped_content_format(Path::new("Craft export.ZIP")),
            Some("textbundle")
        );
    }

    fn drag_at(pointer: (f64, f64), window: (i32, i32), scale: f64) -> WindowDrag {
        WindowDrag {
            start_pointer: pointer,
            start_window: window,
            scale,
        }
    }

    /// The window follows the pointer one-for-one horizontally.
    #[test]
    fn a_dragged_window_follows_the_pointer_sideways() {
        let d = drag_at((100.0, 500.0), (200, 300), 1.0);
        assert_eq!(d.position_for((130.0, 500.0)), (230, 300));
        assert_eq!(d.position_for((70.0, 500.0)), (170, 300));
    }

    /// **The Y axis is flipped, and this is the assertion that says so.**
    /// macOS screen coordinates grow *upward*; a window's position grows
    /// downward. Without the negation the window races away vertically while
    /// tracking the pointer perfectly from side to side — the exact shape of
    /// "it flickers and goes back".
    #[test]
    fn moving_the_pointer_up_the_screen_moves_the_window_up() {
        let d = drag_at((100.0, 500.0), (200, 300), 1.0);
        // Pointer 50 points *higher* on screen: the window's Y gets smaller.
        assert_eq!(d.position_for((100.0, 550.0)), (200, 250));
        // …and lower on screen means a larger Y.
        assert_eq!(d.position_for((100.0, 450.0)), (200, 350));
    }

    /// The pointer is in logical points and a window position is in physical
    /// pixels, so a Retina display doubles the travel.
    #[test]
    fn the_scale_factor_is_applied() {
        let d = drag_at((0.0, 0.0), (0, 0), 2.0);
        assert_eq!(d.position_for((10.0, 0.0)), (20, 0));
        assert_eq!(d.position_for((0.0, 10.0)), (0, -20));
    }

    /// **Absolute, never incremental.** Two half-steps land exactly where one
    /// whole step does, so a missed poll cannot accumulate error — which is
    /// what would make the window slide out from under the pointer.
    #[test]
    fn a_missed_tick_cannot_make_the_window_drift() {
        let d = drag_at((100.0, 500.0), (0, 0), 1.0);
        let halfway = d.position_for((150.0, 480.0));
        let full = d.position_for((200.0, 460.0));
        assert_eq!(halfway, (50, 20));
        // Skipping the halfway tick entirely reaches the same place.
        assert_eq!(full, (100, 40));
    }

    #[test]
    fn window_titles_name_the_profile_and_only_explicit_replica_slots() {
        assert_eq!(
            profiled_window_title("Koru", Some("Work"), session::DEFAULT_PEER),
            "Koru — Work"
        );
        assert_eq!(
            profiled_window_title("Launch plan", Some("Work"), "review"),
            "Launch plan — Work — review"
        );
        assert_eq!(
            profiled_window_title("Koru Search", Some("  "), session::DEFAULT_PEER),
            "Koru Search"
        );
    }

    /// **A modifier the OS says is up must not survive in `down_keys`.**
    ///
    /// The set exists to close a *late* `ModifiersChanged`, and it only forgets
    /// a key on a release event — but the global search hotkey consumes its own
    /// Ctrl+Shift+K, so those releases never arrive and Ctrl stayed held for the
    /// rest of the session. Everything downstream then read a modifier nobody
    /// was pressing: Enter in search became Ctrl+Enter, which means *open in a
    /// new window*, which for a result in another workspace launches a second
    /// process.
    #[test]
    fn a_modifier_the_os_reports_up_is_forgotten() {
        let mut down = HashSet::new();
        down.insert(PhysicalKey::Code(winit::keyboard::KeyCode::ControlLeft));
        down.insert(PhysicalKey::Code(winit::keyboard::KeyCode::ShiftLeft));

        // While they really are held, the supplement stands.
        let held = modifiers_with_pressed_keys(ModifiersState::empty(), &down);
        assert!(held.control_key() && held.shift_key());

        // The OS now reports nothing held: purging leaves the supplement with
        // nothing to add, so the stuck modifier is gone.
        for key in [
            winit::keyboard::KeyCode::ControlLeft,
            winit::keyboard::KeyCode::ControlRight,
            winit::keyboard::KeyCode::ShiftLeft,
            winit::keyboard::KeyCode::ShiftRight,
        ] {
            down.remove(&PhysicalKey::Code(key));
        }
        let after = modifiers_with_pressed_keys(ModifiersState::empty(), &down);
        assert!(
            !after.control_key() && !after.shift_key(),
            "a released modifier must not be re-added from `down_keys`"
        );
    }

    #[test]
    fn the_embedded_window_icon_decodes() {
        let rgba = image::load_from_memory(include_bytes!("../../../assets/logo.png"))
            .expect("embedded app icon is a valid image")
            .to_rgba8();
        let (w, h) = rgba.dimensions();
        assert_eq!(w, h, "the window icon must be square");
        assert!(app_icon().is_some());
    }

    #[test]
    fn search_window_position_is_centered_on_the_selected_monitor() {
        let position = centered_window_position(
            winit::dpi::PhysicalPosition::new(-1920, 0),
            winit::dpi::PhysicalSize::new(1920, 1080),
            winit::dpi::PhysicalSize::new(600, 400),
        );
        assert_eq!(position, winit::dpi::PhysicalPosition::new(-1260, 340));
    }

    /// The cross-window drop broker's arithmetic: a global point lands in the
    /// target's *view* space — scale and zoom divided back out — and a point
    /// off any edge is not over the window at all.
    #[test]
    fn the_foreign_drop_point_maps_into_the_targets_view_space() {
        use winit::dpi::{PhysicalPosition, PhysicalSize};
        let origin = PhysicalPosition::new(100, 200);
        let size = PhysicalSize::new(800, 600);
        // 400 physical across, 300 down, at 2x DPI and 150% zoom.
        assert_eq!(
            window_view_point(origin, size, 2.0, 1.5, (500.0, 500.0)),
            Some((400.0 / 2.0 / 1.5, 300.0 / 2.0 / 1.5))
        );
        for off in [
            (99.0, 300.0),
            (900.0, 300.0),
            (300.0, 199.0),
            (300.0, 800.0),
        ] {
            assert_eq!(
                window_view_point(origin, size, 2.0, 1.5, off),
                None,
                "off the edge at {off:?}"
            );
        }
    }

    #[test]
    fn search_height_is_remembered_in_logical_pixels_and_safely_clamped() {
        assert_eq!(remembered_search_height(1_080, 1.5), 720);
        assert_eq!(remembered_search_height(200, 1.0), 400);
        assert_eq!(remembered_search_height(4_000, 1.0), 1_600);
    }

    /// **The relocation.** The search window's height came out of
    /// `desktop-v1.json`, where the settings authority now writes; it is
    /// geometry and was only ever in that file by adjacency. It reads out of
    /// the `global-search` row the Agent has always owned, and a row that has
    /// never carried one opens at the product default rather than at nothing.
    #[test]
    fn the_search_window_takes_its_height_from_the_row_the_agent_owns() {
        let root = tempfile::tempdir().unwrap();
        let path = koru_window::window_geometry_path(Some(root.path())).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "global-search = x=1;y=2;h=900\n").unwrap();
        assert_eq!(
            search_window_height(&koru_window::SearchGeometry::open(Some(path.clone()))),
            900.0
        );

        // A row with no height at all, and a height nobody could have meant.
        std::fs::write(&path, "global-search = x=1;y=2\n").unwrap();
        assert_eq!(
            search_window_height(&koru_window::SearchGeometry::open(Some(path.clone()))),
            desktop_preferences::GLOBAL_SEARCH_DEFAULT_HEIGHT as f32
        );
        std::fs::write(&path, "global-search = h=19000\n").unwrap();
        assert_eq!(
            search_window_height(&koru_window::SearchGeometry::open(Some(path))),
            desktop_preferences::GLOBAL_SEARCH_MAX_HEIGHT as f32
        );
    }

    #[test]
    fn a_modified_physical_backspace_reaches_text_fields_as_backspace() {
        assert_eq!(
            modified_editing_key(
                PhysicalKey::Code(KeyCode::Backspace),
                ModifiersState::CONTROL,
            ),
            Some(Key::Named(NamedKey::Backspace))
        );
        assert_eq!(
            modified_editing_key(PhysicalKey::Code(KeyCode::Delete), ModifiersState::SUPER,),
            Some(Key::Named(NamedKey::Delete))
        );
        assert_eq!(
            modified_editing_key(
                PhysicalKey::Code(KeyCode::Backspace),
                ModifiersState::empty(),
            ),
            None
        );
    }

    #[test]
    fn a_pressed_physical_ctrl_repairs_a_late_modifier_snapshot() {
        let down = HashSet::from([
            PhysicalKey::Code(KeyCode::ControlLeft),
            PhysicalKey::Code(KeyCode::Backspace),
        ]);
        let repaired = modifiers_with_pressed_keys(ModifiersState::empty(), &down);

        assert!(repaired.control_key());
        assert_eq!(
            modified_editing_key(PhysicalKey::Code(KeyCode::Backspace), repaired),
            Some(Key::Named(NamedKey::Backspace))
        );
    }

    #[test]
    fn releasing_the_last_physical_shift_clears_a_stale_snapshot() {
        let down = HashSet::new();
        let repaired = modifiers_after_key_release(
            ModifiersState::SHIFT,
            PhysicalKey::Code(KeyCode::ShiftLeft),
            &down,
        );
        assert!(!repaired.shift_key());
    }

    #[test]
    fn releasing_one_shift_keeps_the_other_held_shift_active() {
        let down = HashSet::from([PhysicalKey::Code(KeyCode::ShiftRight)]);
        let repaired = modifiers_after_key_release(
            ModifiersState::SHIFT,
            PhysicalKey::Code(KeyCode::ShiftLeft),
            &down,
        );
        assert!(repaired.shift_key());
    }

    #[test]
    fn a_late_shift_snapshot_cannot_restore_a_physically_released_shift() {
        let repaired = reconcile_physical_modifiers(ModifiersState::SHIFT, ModifiersState::empty());
        assert!(!repaired.shift_key());

        let held = reconcile_physical_modifiers(ModifiersState::empty(), ModifiersState::SHIFT);
        assert!(held.shift_key());
    }

    #[test]
    fn a_physically_finished_click_ends_only_a_believed_press() {
        assert!(physically_ended(true, false));
        assert!(!physically_ended(true, true));
        assert!(!physically_ended(false, false));
    }

    /// `Ctrl+Alt+1` reaches a view as `Character("1")` even when Windows could
    /// not name it — the chord a document's headings hang off.
    #[test]
    fn an_unidentified_ctrl_alt_digit_recovers_its_logical_key() {
        use winit::keyboard::NativeKeyCode;
        let unidentified = Key::Unidentified(NativeKeyCode::Unidentified.into());
        for (code, digit) in [
            (KeyCode::Digit0, "0"),
            (KeyCode::Digit1, "1"),
            (KeyCode::Digit2, "2"),
            (KeyCode::Digit3, "3"),
            (KeyCode::Digit4, "4"),
            (KeyCode::Digit9, "9"),
        ] {
            assert_eq!(
                alt_digit(&unidentified, PhysicalKey::Code(code)),
                Some(Key::Character(digit.into())),
                "{code:?}"
            );
        }
        // A layout whose AltGr table *does* name the digit keeps its own
        // character — nothing is rebuilt over a real logical key.
        assert_eq!(
            alt_digit(
                &Key::Character("@".into()),
                PhysicalKey::Code(KeyCode::Digit2)
            ),
            None
        );
        // And a non-digit stays unidentified.
        assert_eq!(
            alt_digit(&unidentified, PhysicalKey::Code(KeyCode::KeyA)),
            None
        );
    }

    /// `Ctrl+Alt+W/P` reach a view as characters — the two switcher chords.
    ///
    /// **Without this it does not fire at all**, which is the reason the page
    /// widths gave up `Ctrl+Alt+P/N/W` for `Ctrl+Shift` before this existed. A
    /// layout that *does* name `AltGr+W` keeps its own character and the chord
    /// stays unbound there; taking a real letter away would be worse.
    /// ⌃⌘digit — the Mac's view chords — is rebuilt from the physical key, so
    /// whatever AppKit reports with ⌃ held, the resolver sees the digit (M42).
    /// Nothing else is rebuilt: ⌥digit keeps its typed character.
    #[test]
    fn the_mac_view_chord_recovers_its_digit_from_the_physical_key() {
        let control_command = ModifiersState::CONTROL | ModifiersState::SUPER;
        let recovered = view_digit(PhysicalKey::Code(KeyCode::Digit2), control_command);
        if cfg!(target_os = "macos") {
            assert_eq!(recovered, Some(Key::Character("2".into())));
        } else {
            assert_eq!(recovered, None, "off macOS the view modifier is Alt alone");
        }
        assert_eq!(
            view_digit(PhysicalKey::Code(KeyCode::Digit2), ModifiersState::ALT),
            None
        );
        assert_eq!(
            view_digit(PhysicalKey::Code(KeyCode::KeyA), control_command),
            None
        );
    }

    #[test]
    fn an_unidentified_ctrl_alt_letter_recovers_its_logical_key() {
        use winit::keyboard::NativeKeyCode;
        let unidentified = Key::Unidentified(NativeKeyCode::Unidentified.into());
        assert_eq!(
            alt_letter(&unidentified, PhysicalKey::Code(KeyCode::KeyW)),
            Some(Key::Character("w".into()))
        );
        assert_eq!(
            alt_letter(&unidentified, PhysicalKey::Code(KeyCode::KeyP)),
            Some(Key::Character("p".into()))
        );
        // Nothing else is claimed: every other Ctrl+Alt+letter belongs to
        // whatever the layout says it is.
        assert_eq!(
            alt_letter(&unidentified, PhysicalKey::Code(KeyCode::KeyN)),
            None
        );
        // And a layout whose AltGr table names the key keeps its character.
        assert_eq!(
            alt_letter(
                &Key::Character("ł".into()),
                PhysicalKey::Code(KeyCode::KeyW)
            ),
            None
        );
    }

    #[test]
    fn a_shifted_command_digit_recovers_the_number_row_key() {
        let mods = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            shifted_digit(PhysicalKey::Code(KeyCode::Digit7), mods),
            Some(Key::Character("7".into()))
        );
        assert_eq!(
            shifted_digit(PhysicalKey::Code(KeyCode::Digit8), mods),
            Some(Key::Character("8".into()))
        );
        assert_eq!(
            shifted_digit(PhysicalKey::Code(KeyCode::Digit8), ModifiersState::SHIFT),
            None,
            "ordinary shifted typing keeps the layout's character"
        );
    }

    #[test]
    fn pending_binding_reconciles_only_the_exact_profile_and_identity() {
        let profile = koru_core::ProfileId::new();
        let marker = crate::binding_markers::PendingProfileAccountBinding {
            profile,
            expected_identity_id: None,
            new_identity_id: "00000000-0000-4000-8000-000000000099".into(),
        };
        assert_eq!(
            profile_binding_reconcile_action(&marker, None, &[]),
            ProfileBindingReconcile::WaitForExactServerProfile
        );
        let exact = koru_agent_wire::AccountProfileRow {
            koru_identity_id: marker.new_identity_id.clone(),
            profile_id: profile.to_storage(),
            name: None,
            locale: None,
            auth_identities: Vec::new(),
        };
        assert_eq!(
            profile_binding_reconcile_action(&marker, None, &[exact]),
            ProfileBindingReconcile::Bind
        );
        assert_eq!(
            profile_binding_reconcile_action(
                &marker,
                Some("koru:00000000-0000-4000-8000-000000000099"),
                &[],
            ),
            ProfileBindingReconcile::AlreadyBound
        );
        assert_eq!(
            profile_binding_reconcile_action(
                &marker,
                Some("koru:00000000-0000-4000-8000-000000000001"),
                &[],
            ),
            ProfileBindingReconcile::Conflict
        );
    }
}

/// Whether the task wrapper's coalesced counters are being printed.
///
/// Read once. The switch belongs to whoever prints, which is why
/// `crate::tasks` deliberately holds no environment read of its own — this is
/// the tick's copy of the same gate `apps/koru`'s drain uses, so a burst
/// flushed by the clock reports the same line as one flushed by the caret.
fn tasks_debug() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| koru_env::switch(koru_env::name::KORU_TASKS_DEBUG))
}

/// One line of the task diagnostic, to stderr **and** to a file.
///
/// **A console nobody can send is not a diagnostic.** `run-all` leaves Koru's
/// output on the terminal it was launched from and redirects it only under
/// `--performance`, so the answer to *did the command run* lived in a scrollback
/// buffer and reached whoever was asking by being retyped. `KORU_THEME_DEBUG`
/// already solved this the obvious way, and this is the same two lines —
/// `koru-tasks-debug.log` beside `koru-theme-debug.log`, appended, so one
/// session's evidence is a path rather than a paste.
///
/// `std::env::temp_dir` rather than `%TEMP%`, for the reason the theme log
/// records: the variable is Windows-only, and the file silently went nowhere
/// everywhere else.
///
/// The switch's own rule is unchanged and is the caller's: closed tokens,
/// counts and a causation id, never a title, a due date or row text. A file
/// makes that promise matter more, not less — a console line is gone when the
/// window closes and this one is not.
///
/// **Public for the desktop's own lines** — the Tasks surfaces' waits on the
/// read model report through here too, so one session's evidence is one file
/// whichever half of the switch wrote it.
pub fn tasks_debug_log(line: std::fmt::Arguments<'_>) {
    if !tasks_debug() {
        return;
    }
    eprintln!("[koru:tasks] {line}");
    use std::io::Write as _;
    let path = std::env::temp_dir().join("koru-tasks-debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "[koru:tasks] {line}");
    }
}
