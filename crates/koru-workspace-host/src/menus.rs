// SPDX-License-Identifier: MPL-2.0

//! One transport for every native menu event: the macOS menu bar's rows and
//! a popup a window shows.
//!
//! `muda` delivers a chosen row either to an installed handler or to its
//! built-in receiver — never both: `set_event_handler` replaces the channel.
//! Two consumers draining the receiver would swallow each other's events, so
//! there is one consumer, here. The handler pushes the row's id into a queue
//! and wakes the loop; [`drain`] hands the ids out in order on the next pass,
//! in **both** loop paths (ordinary maintenance and the tray branch), so a
//! popup chosen from a floating icon while Koru is hidden still lands.
//!
//! Tests enter through [`ingest`], the same function the handler calls,
//! because muda's own `MenuEvent::send` is crate-private.
//!
//! **Linux has no native menu yet**, and `muda` is not linked there. Only the
//! muda calls are gated: the queue, [`ingest`], [`drain`], the row lookup and
//! their tests are one transport and one suite on every platform, and
//! [`MenuId`] is a local type with muda's shape so no caller reads it two ways.
//! A popup on Linux shows nothing and no row is ever chosen
//! (docs/features/linux/TECHNICAL.final.md, D2).

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

#[cfg(any(windows, target_os = "macos"))]
pub use muda::MenuId;
#[cfg(any(windows, target_os = "macos"))]
use muda::{ContextMenu, Menu, MenuEvent, MenuItem};
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

use crate::app::Wake;

/// A menu row's id where muda is not linked: muda's shape, a string in `.0`,
/// which is what [`drain`]'s caller and [`Popup::tag_of_str`] read.
#[cfg(not(any(windows, target_os = "macos")))]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MenuId(pub String);

#[cfg(not(any(windows, target_os = "macos")))]
impl MenuId {
    pub fn new<S: AsRef<str>>(id: S) -> Self {
        Self(id.as_ref().to_owned())
    }
}

static QUEUE: OnceLock<Mutex<VecDeque<MenuId>>> = OnceLock::new();

fn queue() -> &'static Mutex<VecDeque<MenuId>> {
    QUEUE.get_or_init(|| Mutex::new(VecDeque::new()))
}

/// Install the one handler. Before any menu exists — the menubar and every
/// popup are built after this — so no event can go to the receiver first.
pub fn install(proxy: EventLoopProxy<Wake>) {
    #[cfg(any(windows, target_os = "macos"))]
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        ingest(event.id, Some(&proxy));
    }));
    // Nothing to install: no native menu exists to deliver an event.
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = proxy;
}

/// Queue one chosen row and wake the loop. The handler's body, and the
/// tests' way in.
pub fn ingest(id: MenuId, proxy: Option<&EventLoopProxy<Wake>>) {
    if let Ok(mut q) = queue().lock() {
        q.push_back(id);
    }
    if let Some(proxy) = proxy {
        let _ = proxy.send_event(Wake::Menu);
    }
}

/// Every row chosen since the last drain, in order.
pub fn drain() -> Vec<MenuId> {
    queue()
        .lock()
        .map(|mut q| q.drain(..).collect())
        .unwrap_or_default()
}

/// A popup menu a window shows at the pointer: a list of rows, each with a
/// small integer the caller maps back to its own meaning. The rows stay
/// registered on this object until it is dropped, so a selection delivered
/// after the synchronous show call returns is still recognised.
pub struct Popup {
    #[cfg(any(windows, target_os = "macos"))]
    menu: Menu,
    rows: Vec<(MenuId, u32)>,
}

impl Popup {
    /// `rows` are `(label, tag)`; the tag comes back from [`Popup::tag_of`].
    #[cfg(any(windows, target_os = "macos"))]
    pub fn new(rows: &[(&str, u32)]) -> Self {
        let menu = Menu::new();
        let mut ids = Vec::with_capacity(rows.len());
        for (label, tag) in rows {
            let item = MenuItem::new(*label, true, None);
            ids.push((item.id().clone(), *tag));
            let _ = menu.append(&item);
        }
        Self { menu, rows: ids }
    }

    /// The same rows with no menu behind them. Ids are minted the way muda
    /// mints its own, from a process-wide counter, so two popups never share
    /// one.
    #[cfg(not(any(windows, target_os = "macos")))]
    pub fn new(rows: &[(&str, u32)]) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let rows = rows
            .iter()
            .map(|(_, tag)| {
                let n = NEXT.fetch_add(1, Ordering::Relaxed);
                (MenuId(format!("popup-row-{n}")), *tag)
            })
            .collect();
        Self { rows }
    }

    /// Show at the pointer and return at once; the choice arrives through
    /// [`drain`]. Windows and macOS; elsewhere a no-op.
    pub fn show(&self, window: &Window) {
        #[cfg(windows)]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = window.window_handle()
                && let RawWindowHandle::Win32(h) = handle.as_raw()
            {
                // SAFETY: a live HWND of a window we own; muda only posts a
                // menu to it.
                unsafe {
                    self.menu.show_context_menu_for_hwnd(h.hwnd.get(), None);
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(handle) = window.window_handle()
                && let RawWindowHandle::AppKit(h) = handle.as_raw()
            {
                // SAFETY: the NSView of a window we own, alive for the call.
                unsafe {
                    self.menu
                        .show_context_menu_for_nsview(h.ns_view.as_ptr() as *const _, None);
                }
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        let _ = window;
    }

    /// The tag for a chosen row, if it is one of this popup's.
    pub fn tag_of(&self, id: &MenuId) -> Option<u32> {
        tag_in(&self.rows, |row| row == id)
    }

    /// The same, for the id as the host hands it to views (a string, so no
    /// muda type crosses the View trait).
    pub fn tag_of_str(&self, id: &str) -> Option<u32> {
        tag_in(&self.rows, |row| row.0 == id)
    }
}

/// The row→tag lookup, as a free function over the pair list.
///
/// **Split out so it can be tested at all on macOS.** `muda::Menu::new`
/// panics unless it is called on the main thread, and libtest runs every test
/// on a spawned one — so a test that builds a `Popup` to ask what its rows map
/// to cannot pass on a Mac, however correct the mapping is. The mapping is
/// plain data and owes AppKit nothing; this is the half worth asserting, and
/// the half a test can reach on both platforms.
fn tag_in(rows: &[(MenuId, u32)], is_match: impl Fn(&MenuId) -> bool) -> Option<u32> {
    rows.iter()
        .find(|(row, _)| is_match(row))
        .map(|(_, tag)| *tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_then_drain_keeps_order_and_empties_the_queue() {
        // The queue is process-wide; use ids no other test would.
        let a = MenuId::new("menus-test-a");
        let b = MenuId::new("menus-test-b");
        ingest(a.clone(), None);
        ingest(b.clone(), None);
        let drained = drain();
        let ours: Vec<&MenuId> = drained.iter().filter(|id| **id == a || **id == b).collect();
        assert_eq!(ours, vec![&a, &b]);
        assert!(drain().iter().all(|id| *id != a && *id != b));
    }

    /// The rows a `Popup` would hold, without building the menu that holds
    /// them: `Popup::new` calls `muda::Menu::new`, which panics off the main
    /// thread on macOS, and libtest has no main thread to offer.
    fn rows() -> Vec<(MenuId, u32)> {
        [
            ("menus-test-open", 1),
            ("menus-test-close", 2),
            ("menus-test-end", 3),
        ]
        .into_iter()
        .map(|(id, tag)| (MenuId::new(id), tag))
        .collect()
    }

    #[test]
    fn a_popup_maps_its_rows_and_nothing_else() {
        let rows = rows();
        assert_eq!(tag_in(&rows, |row| *row == rows[2].0), Some(3));
        assert_eq!(
            tag_in(&rows, |row| *row == MenuId::new("menus-test-foreign")),
            None
        );
        // The string door the views use resolves the same row.
        assert_eq!(tag_in(&rows, |row| row.0 == "menus-test-end"), Some(3));
    }

    /// A selection ingested after the show call returned is still recognised:
    /// the queue holds it and the row list still maps it.
    #[test]
    fn a_row_chosen_after_the_show_call_is_still_mapped() {
        let rows = rows();
        ingest(rows[1].0.clone(), None);
        let chosen: Vec<u32> = drain()
            .iter()
            .filter_map(|id| tag_in(&rows, |row| row == id))
            .collect();
        assert_eq!(chosen, vec![2]);
    }
}
