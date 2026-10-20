// SPDX-License-Identifier: MPL-2.0

//! **The window itself — the parts that know nothing about documents.**
//!
//! Every trap Windows sets for an undecorated window, in one place: the border
//! `WM_NCHITTEST` will not report, the DWM hairline painted at the *window*
//! rect rather than the client rect, the snap shadow that needs an explicit
//! attribute to survive aero-snap, and cold-start light/dark before a window
//! exists.
//!
//! ## Why it is a crate and not six modules in `koru-workspace-host`
//!
//! Because Koru and Agent can both open windows while only Koru owns a
//! workspace. `koru-workspace-host` cannot exist without `koru-workspace` and
//! `koru-docstore`, while the Agent overlay must not acquire either.
//!
//! The platform code belongs here on one argument: **this is the code
//! AGENTS.md keeps a list of platform traps for, and each entry on that list
//! was a real bug.** A second copy means the next
//! Windows fix has to be found twice, and the shadow-fill-border recipe in
//! `koru-draw` is the standing example of what happens when it is not — three
//! separate drifts before it was made one function.
//!
//! Nothing here names a node, a workspace or a document. That is the same rule
//! `koru-ui` follows and the same reason: the constraint is only real while
//! the dependency list makes it impossible to break.
//!
//! ## The single-window loop *is* here, and `koru-workspace-host`'s is not
//!
//! This used to say the event loop was deliberately absent, on the grounds that
//! `koru-workspace-host`'s `app.rs` reaches a workspace on almost every one of its
//! 5,300 lines and a shared trait would have a dozen methods to hide the
//! difference. That is still true of *that* loop.
//!
//! Agent needs a smaller shape — one undecorated window, its own caption, no
//! tabs and no workspace. That loop lives here as [`WindowApp`] and [`run`],
//! with the three capabilities Agent needs: an optional window, a `Renderer`
//! that outlives it, and a way for a background thread to wake the loop.
//!
//! `koru-workspace-host` keeps its own multi-window loop. It hosts tabs, panes, undo,
//! drag and drop and a `Workspace`, and folding those two together would be a
//! trait that describes neither.
//!
//! ## There are two runners now, and one discipline
//!
//! [`multi::run_windows`] drives a set of windows for the app shell
//! (ADR-262). It is **not** a second event loop: the rules between winit and an
//! application — key quarantine, resize borders, drag timing, click counts,
//! capture, IME, focus and occlusion, theme and scale ordering, present
//! acknowledgement, renderer warm-up and device-keyed reuse, settle and
//! reschedule — live in one private `dispatch` module that both runners call,
//! proven by parity tests driving the same sequences through both. Copying that
//! discipline into a second `ApplicationHandler` would fork exactly what this
//! crate exists to hold in one place.

/// The AccessKit adapter's lifetime against one window: created before the
/// first reveal, fed events before the application, drained on the loop
/// thread, retired with the window. It knows nothing about what the window
/// contains — see its module doc for the sentence and what it excludes.
pub mod accessibility;
pub mod activation;
pub mod app;
/// The projection of the settings authority's Global section onto the process
/// globals a window draws from. Replaced `user_preferences`, which parsed the
/// file directly — see ADR-266.
pub mod appearance;
pub mod console;
pub mod desktop_preferences;
/// The event discipline both runners obey. Private: what it exposes is
/// [`WindowApp`] and [`multi::MultiWindowApp`], not the machinery between them.
mod dispatch;
pub mod dock;
pub mod frame;
pub mod framestats;
/// Warm-up, device-keyed renderer reuse and the frame, shared by both runners.
mod gpu;
pub mod hairline;
pub mod input;
pub mod key_quarantine;
pub mod multi;
mod non_activating;
/// A scene rendered to pixels with no window: the read-back recipe the
/// tests carried in four copies, as the one the product saves a PNG with.
pub mod offscreen;
pub mod open_uri;
pub mod renderer;
pub mod request;
pub mod resize;
mod reveal;
mod rounded_region;
/// The `global-search` row's value schema, shared by the Agent's overlay and
/// the shell's ephemeral search window — two processes, one row.
pub mod search_geometry;
pub mod snap;
pub mod system_locale;
pub mod system_theme;
// The OS version, which only macOS asks this crate for: `koru-agent` and
// `koru-workspace-host` both need it and cannot see each other. See the module's own
// doc for why Windows keeps its two readers.
#[cfg(target_os = "macos")]
pub mod system_version;

pub mod window_geometry;

pub use accessibility::{
    Pending as AccessibilityPending, Provider as AccessibilityProvider, WindowBridge,
};
pub use activation::{activate_app, become_accessory};
pub use app::{ClosingWindowState, WindowApp, run};
pub use appearance::{Appearance, CaretAppearance};
pub use desktop_preferences::{CloseBehavior, DesktopPreferences, ReminderStartupFacts};
pub use dock::set_dock_icon;
pub use input::{ImeEvent, KeyState, RichKeyEvent, RichMouseEvent};
pub use key_quarantine::KeyQuarantine;
pub use multi::{MultiWindowApp, WindowKey, run_windows};
pub use non_activating::WindowActivation;
pub use open_uri::{FEEDBACK_ADDRESS, FEEDBACK_MAILTO, open_uri};
pub use renderer::{
    RendererBackend, build_renderer, configured_backends, context_with, default_backends,
};
pub use request::{RecordingRequest, WindowRequest};
pub use search_geometry::SearchGeometry;
pub use window_geometry::{
    WindowRowWriter, local_state_dir, read_window_row, search_state_path, user_preferences_path,
    voice_consent_path, window_geometry_path,
};
