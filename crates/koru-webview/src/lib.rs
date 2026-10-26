// SPDX-License-Identifier: MPL-2.0

//! A native web surface hosted as a child of a window we own.
//!
//! That one sentence is the crate's charter, and it excludes everything else:
//! no Intercom, no store root, no window management, no browser opening. The
//! app decides what URL, what policy and what directory; the host decides
//! where the surface sits; this crate turns those decisions into a WebView2
//! (Windows) or WKWebView (macOS) child through `wry`, and turns what the
//! page does into [`WebEvent`]s on a channel.
//!
//! **Threading.** Everything here runs on the UI thread that owns the parent
//! window — wry's `WebView` is `!Send`, and so is [`WebSurface`]. The page's
//! callbacks arrive on that same thread; they are pushed to the receiver and
//! the `wake` closure pings the owner's event loop, so the owner drains on
//! its next maintenance pass and never blocks on the engine.
//!
//! **Creating the surface stalls the UI thread** — 260–560 ms on the machine
//! that measured it (guide/contributing/deps/WRY.md), because WebView2's
//! environment and controller are created through COM callbacks that wry
//! waits for by pumping messages. That is a known cost the caller pays once
//! per surface and shows a frame *before* calling [`WebSurface::open`].
//!
//! **Under `no-webview`** [`WebSurface::open`] answers
//! [`WebError::RuntimeUnavailable`] and links no engine instance, so a suite
//! exercises every state around the surface without a browser process.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use raw_window_handle::HasWindowHandle;

mod debug;
// The stub under `no-webview`, and on every platform wry is not linked for
// (Linux, see Cargo.toml).
#[cfg(all(any(windows, target_os = "macos"), not(feature = "no-webview")))]
mod engine;
#[cfg(any(feature = "no-webview", not(any(windows, target_os = "macos"))))]
#[path = "stub.rs"]
mod engine;
mod js;

#[cfg(test)]
mod tests;

pub use debug::enabled as debug_enabled;
pub use js::string_literal as js_string_literal;

/// A rectangle in the parent window's **physical** pixels, origin top-left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// What the caller wants done with a top-level navigation the page asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Navigation {
    /// Let the surface navigate.
    Allow,
    /// Refuse it here and hand the URL to the caller as [`WebEvent::External`]
    /// — the caller opens the browser; this crate never does.
    External,
    /// Refuse it and say so as [`WebEvent::Refused`].
    Refuse,
}

/// Everything [`WebSurface::open`] needs, decided by the caller.
pub struct WebConfig {
    /// The first URL to load.
    pub url: String,
    /// Where the engine keeps cookies and storage. **Resolved by the caller**:
    /// this crate has no idea of a store root, a channel or a profile.
    pub data_directory: PathBuf,
    /// The top-level navigation policy. Called on the UI thread for every
    /// navigation the page asks for, including the first one.
    pub policy: Box<dyn Fn(&str) -> Navigation + Send>,
    /// Whether the engine's developer tools may be opened.
    pub devtools: bool,
    /// Appended to the engine's user agent, so a support inbox can tell a
    /// Koru window from a browser tab.
    pub user_agent_suffix: Option<String>,
}

/// What the page did. Drained by the owner on its own thread and schedule.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebEvent {
    PageStarted {
        url: String,
    },
    PageFinished {
        url: String,
    },
    /// `window.ipc.postMessage(body)` from the page, with the URI of the frame
    /// that sent it. The caller validates `source` against the origin it
    /// expects before believing `body`.
    Ipc {
        body: String,
        source: String,
    },
    /// The policy said [`Navigation::External`]: the caller opens it.
    External {
        url: String,
    },
    /// The policy said [`Navigation::Refuse`].
    Refused {
        url: String,
    },
    /// `window.open` from the page. Always denied by the engine; the caller
    /// applies its policy and usually opens it externally.
    NewWindow {
        url: String,
    },
}

/// Why the surface could not do what was asked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebError {
    /// No engine on this machine, or none under `no-webview`.
    RuntimeUnavailable(String),
    /// The engine refused to create the surface.
    Create(String),
    /// A call on a live surface failed.
    Call(String),
}

impl std::fmt::Display for WebError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RuntimeUnavailable(why) => write!(f, "no web runtime: {why}"),
            Self::Create(why) => write!(f, "could not create the web surface: {why}"),
            Self::Call(why) => write!(f, "web surface call failed: {why}"),
        }
    }
}

impl std::error::Error for WebError {}

/// The fault a developer can inject through `KORU_WEBVIEW_FAULT`, so the
/// error state can be demonstrated without breaking a machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// `open` fails with [`WebError::Create`].
    Create,
}

impl Fault {
    /// Pure, so a test hands the value in rather than setting the variable.
    pub fn parse(value: Option<&str>) -> Option<Self> {
        match value.map(str::trim) {
            Some("create") => Some(Self::Create),
            _ => None,
        }
    }

    fn from_env() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        Self::parse(koru_env::value(koru_env::name::KORU_WEBVIEW_FAULT).as_deref())
    }
}

/// One child web surface. `!Send`: it lives and dies on the UI thread that
/// owns its parent window, and it must be dropped **before** that window.
pub struct WebSurface {
    inner: engine::Inner,
    /// `!Send` in every build, so a `Mutex` around it is a compile error
    /// rather than a discovery.
    _ui_thread: std::marker::PhantomData<*const ()>,
}

impl WebSurface {
    /// Create the surface as a child of `parent` at `bounds`.
    ///
    /// Blocks the calling thread for as long as the engine takes to create its
    /// environment and controller — hundreds of milliseconds on Windows. The
    /// receiver carries every [`WebEvent`]; `wake` is called after each event
    /// is queued and must be cheap (an `EventLoopProxy` ping).
    pub fn open(
        parent: &impl HasWindowHandle,
        bounds: PhysicalRect,
        config: WebConfig,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<(Self, Receiver<WebEvent>), WebError> {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(wake);
        if let Some(Fault::Create) = Fault::from_env() {
            debug::log("create", "fault-injected", "");
            return Err(WebError::Create("KORU_WEBVIEW_FAULT=create".into()));
        }
        Self::open_inner(parent, bounds, config, wake)
    }

    fn open_inner(
        parent: &impl HasWindowHandle,
        bounds: PhysicalRect,
        config: WebConfig,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(Self, Receiver<WebEvent>), WebError> {
        let (inner, receiver) = engine::Inner::open(parent, bounds, config, wake)?;
        Ok((
            Self {
                inner,
                _ui_thread: std::marker::PhantomData,
            },
            receiver,
        ))
    }

    /// Move and resize the child. Physical pixels of the parent window.
    pub fn set_bounds(&self, bounds: PhysicalRect) -> Result<(), WebError> {
        self.with_engine(|e| e.set_bounds(bounds))
    }

    pub fn set_visible(&self, visible: bool) -> Result<(), WebError> {
        self.with_engine(|e| e.set_visible(visible))
    }

    /// Put keyboard focus in the page.
    pub fn focus(&self) -> Result<(), WebError> {
        self.with_engine(|e| e.focus())
    }

    /// Hand keyboard focus back to the parent window.
    pub fn focus_parent(&self) -> Result<(), WebError> {
        self.with_engine(|e| e.focus_parent())
    }

    /// Whether keyboard focus is inside this surface right now — the
    /// platform's answer, never inferred from visibility.
    pub fn has_focus(&self) -> bool {
        self.with_engine(|e| Ok(e.has_focus())).unwrap_or(false)
    }

    /// Run JavaScript in the page. Fire and forget; the page answers, if it
    /// answers, over IPC. Use [`js_string_literal`] for any string you splice.
    pub fn eval(&self, js: &str) -> Result<(), WebError> {
        self.with_engine(|e| e.eval(js))
    }

    /// Load another URL. Goes through the policy like any navigation.
    pub fn navigate(&self, url: &str) -> Result<(), WebError> {
        self.with_engine(|e| e.navigate(url))
    }

    /// Ask the engine to clear cookies and storage. **`Ok` means started, not
    /// done**: on Windows the engine clears asynchronously and reports no
    /// completion through wry, so a caller that needs the data gone before
    /// booting a new identity drops the surface and removes the data
    /// directory instead.
    pub fn clear_all_browsing_data(&self) -> Result<(), WebError> {
        self.with_engine(|e| e.clear_all_browsing_data())
    }

    fn with_engine<T>(
        &self,
        f: impl FnOnce(&engine::Inner) -> Result<T, WebError>,
    ) -> Result<T, WebError> {
        f(&self.inner)
    }
}

/// The host part of a URL, for a log line that must not carry the URL.
pub fn host_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    &rest[..end]
}

/// Apply a policy to a top-level navigation and say what to do and what to
/// tell the owner. Pure: the engine's handler and the tests share it.
#[cfg_attr(
    any(feature = "no-webview", not(any(windows, target_os = "macos"))),
    allow(dead_code)
)]
pub(crate) fn decide(policy: &dyn Fn(&str) -> Navigation, url: &str) -> (bool, Option<WebEvent>) {
    match policy(url) {
        Navigation::Allow => (true, None),
        Navigation::External => (
            false,
            Some(WebEvent::External {
                url: url.to_string(),
            }),
        ),
        Navigation::Refuse => (
            false,
            Some(WebEvent::Refused {
                url: url.to_string(),
            }),
        ),
    }
}
