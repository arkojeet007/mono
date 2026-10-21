// SPDX-License-Identifier: MPL-2.0

//! The Welcome tab: one page that greets the owner, offers the first five
//! minutes of Koru as a checklist, and points at what comes after.
//!
//! **Not an editor.** It has no `Body` and no `ViewState`, which is precisely
//! why `TabView` implements only [`View`] while [`Leaf`](crate::leaf::Leaf)
//! implements both — see the note on `Leaf` for why the two are separate enums.
//!
//! ## One page, not two
//!
//! Until the First Run redesign this view had two modes: an ordinary Welcome
//! (brand, Pinned, Recent with *Clear*, import chips, *Getting around*) and a
//! "first minutes" checklist shown once after onboarding. Every Welcome tab is
//! now the checklist page. The recents moved to **+ → Documents** (with
//! *Clear recents* at the bottom), pins live on the tab strip, and the import
//! chips and the *Getting around* hints became rows of the checklist.
//!
//! ## Where things are
//!
//! - [`rows`] — the stable row ids, which rows are counted, which tick.
//! - [`layout`] — every rectangle, measured before anything is drawn. Drawing
//!   and hit-testing both read the one [`layout::Layout`], so a control can
//!   never be pressable somewhere other than where it is painted.
//! - [`draw`] — painting, every element inside a named inspector scope.
//! - [`hints`] — the *Learn your way around* lines, from `koru_keys`.
//! - [`mcp`] — the command an MCP host runs to reach this installation.
//! - [`build`] — the build stamp and channel copy the update surface uses.
//!
//! The view never performs an effect. It raises [`WelcomeWant`]s and newly
//! ticked rows, and the container drains both: the wants become the existing
//! commands (new document, import, Settings, Link another device, …) and the
//! ticks are written machine-wide to the Store's `store_state` table.

use std::collections::BTreeSet;

use koru_core::schema::WorkspaceDoc;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{ImageCache, theme};
use koru_workspace::Workspace;
use koru_workspace_host::{Settings, View};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::CursorIcon;

mod build;
mod draw;
mod hints;
mod layout;
mod mcp;
mod rows;

pub(crate) use build::{DOWNLOAD_URL, build_stamp, channel_label, channel_note};
pub(crate) use mcp::command as mcp_command;
pub(crate) use rows::RowId;
use rows::Section;

// ------------------------------------------------------------------- copy
//
// English only this pass; translating the page is a named backlog item in
// `docs/product/INITIATIVES.md`. Every string the page draws is here.

const CUSTOMIZE_TITLE: &str = "Customize Koru";
const CUSTOMIZE_HINT: &str = "Theme, fonts, caret color and the default page width.";
const CUSTOMIZE_BUTTON: &str = "Customize\u{2026}";

const FIRST_MINUTES_LABEL: &str = "Your first five minutes";
const FIRST_MINUTES_DONE: &str = "All done — you know your way around.";
const DEVICES_LABEL: &str = "Connect your devices";
const FURTHER_LABEL: &str = "When you're ready to go further";
const FURTHER_DONE: &str = "All set up — Koru is fully yours.";
/// `{n} of {m} done`.
const PROGRESS: &str = "{n} of {m} done";
const SHOW: &str = "Show";
const HIDE: &str = "Hide";

const FIRST_DOCUMENT_TITLE: &str = "Make your first document";
const FIRST_DOCUMENT_BEFORE: &str = "Type it, or ";
const FIRST_DOCUMENT_MIC: &str = "set up your mic";
const FIRST_DOCUMENT_AFTER: &str = " and dictate it.";

const DOCUMENTS_TITLE: &str = "Bring your documents in";
const DOCUMENTS_HINT: &str = "Import from the app you use today.";

const FILES_TITLE: &str = "Bring your files in";
const FILES_HINT: &str = "Drop files or folders here — PDFs, images and more land in {workspace} as nodes you can link to.";
const FILES_BUTTON: &str = "Choose files\u{2026}";

const LEARN_TITLE: &str = "Learn your way around";
const LEARN_HINT: &str = "The handful of shortcuts that make Koru feel fast.";

const TASK_LIST_TITLE: &str = "Turn one document into a task list";

const REMINDER_TITLE: &str = "Set up a reminder";
const REMINDER_HINT: &str = "Nudge yourself about a task or a document, on your schedule.";

const FEEDBACK_TITLE: &str = "Let us know how it went";
const FEEDBACK_BEFORE: &str = "We love to hear from you — ";
const FEEDBACK_FORM: &str = "feedback form";
const FEEDBACK_OR: &str = " or ";
const FEEDBACK_EMAIL: &str = "email";
const FEEDBACK_AFTER: &str = ".";

const PHONE_TITLE: &str = "Phone";
const PHONE_HINT: &str = "Capture notes and photos on the go.";
const COMPUTER_TITLE: &str = "Another computer";
const COMPUTER_HINT: &str = "Keep this workspace in step across your machines.";
const WATCH_TITLE: &str = "Watch";
const WATCH_HINT: &str = "Reminders and quick task ticks on your wrist.";
const SCAN_TO_PAIR: &str = "Scan to pair";

const AI_TITLE: &str = "Set up your AI";
const AI_HINT: &str = "Pick a model and connect a key — Koru works fine without it.";
const MCP_TITLE: &str = "Manage documents over MCP";
const MCP_HINT: &str = "Let outside tools read and write your workspace through the MCP service.";
const MCP_COPY: &str = "Copy";
const AGENT_TITLE: &str = "Run an agent in a terminal";
const AGENT_HINT: &str = "Point a coding agent at your workspace from the command line.";
const AGENT_COMMAND: &str = "koru agent";
const APPS_TITLE: &str = "Write your own apps";
const APPS_HINT: &str =
    "Review the API documentation and browse community apps built for Koru on GitHub.";

/// Text lines one notch of the wheel moves.
///
/// **Three, which is what the document editor already does** (`scroll_by` in
/// `koru-editor`, `FONT_SIZE * LINE_HEIGHT * 3.0`) and what Windows itself
/// means by a wheel click.
const WHEEL_LINES: f32 = 3.0;

fn files_hint(workspace: &str) -> String {
    let named = if workspace.trim().is_empty() {
        "your workspace"
    } else {
        workspace
    };
    FILES_HINT.replace("{workspace}", named)
}

fn progress(done: usize, of: usize) -> String {
    PROGRESS
        .replace("{n}", &done.to_string())
        .replace("{m}", &of.to_string())
}

/// What the page asks the container to do. The view requests; it never
/// performs — each of these is an existing command the container already has.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WelcomeWant {
    /// *Make your first document* — the Ctrl+T name prompt.
    NewDocument,
    /// *set up your mic* — Settings, scrolled to Voice.
    VoiceSettings,
    /// One import format's chip, by the format registry's stable id.
    Import(&'static str),
    /// *Choose files…* — the file picker behind `PaneRequest::ImportFiles`.
    ImportFiles,
    /// *Set up a reminder* — All Reminders.
    Reminders,
    /// *feedback form* — the Send feedback tab.
    Feedback,
    /// *email* — the support mailto.
    Email,
    /// *Copy* — the MCP command, [`mcp_command`], on the clipboard.
    CopyMcpCommand,
    /// *Phone*, *Another computer*, the pairing square — Link another device.
    LinkDevice,
    /// *Customize…* — the Settings tab.
    Settings,
    /// *Set up your AI* — the Providers and models tab Settings → AI opens.
    AiSetup,
    /// *Run an agent in a terminal* — the `+` menu's list of agents, at the
    /// pointer.
    Agents,
}

/// Who the page greets and whose page it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WelcomeIdentity {
    /// The owner's display name, when this session knows it.
    pub owner: Option<String>,
    pub workspace: String,
    /// The Profile, so the owner's name is only used for the Profile it was
    /// given with.
    pub profile: String,
}

/// Something the page can be pressed on. Hit-testing, focus and drawing all
/// name targets, never indices.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Customize,
    /// A row whose whole surface is the action.
    Row(RowId),
    MicLink,
    Import(&'static str),
    ChooseFiles,
    FeedbackForm,
    Email,
    /// The MCP row's *Copy* button and the command beside it.
    CopyMcp,
    /// The *Show* / *Hide* link of a section whose counted rows are all done.
    Toggle(Section),
    /// The pairing square beside the device rows.
    PairQr,
}

impl Target {
    /// A row-wide target loses to anything drawn on top of it.
    fn is_row(self) -> bool {
        matches!(self, Self::Row(_))
    }
}

pub struct WelcomeView {
    identity: WelcomeIdentity,
    /// Ticked rows. Machine-wide: seeded from the Store by the container.
    done: BTreeSet<RowId>,
    /// Rows ticked here since the container last asked, to be persisted.
    newly_done: Vec<RowId>,
    wants: Vec<WelcomeWant>,
    /// *Learn your way around* is open.
    learn_open: bool,
    /// A completed section the person asked to see again with *Show*.
    shown: BTreeSet<Section>,
    focus: Option<Target>,
    viewport: (f32, f32),
    engine: TextEngine,
    pointer: (f32, f32),
    /// Vertical scroll of the column, in logical pixels.
    scroll: f32,
    /// The measured page, rebuilt whenever something it depends on changes.
    laid: Option<layout::Layout>,
    /// The interactive rectangles the last draw painted, in paint order — the
    /// evidence that hit-testing and drawing share one geometry.
    drawn: Vec<(Target, Rect)>,
    /// Whether a pinned tab shows its title: a window preference the tab strip
    /// reads from any open Welcome tab.
    pinned_show_title: bool,
    /// What the MCP row shows, read once: the executable's path does not move
    /// while it runs.
    mcp_command: String,
}

impl Default for WelcomeView {
    fn default() -> Self {
        Self::new()
    }
}

impl WelcomeView {
    pub fn new() -> Self {
        Self {
            identity: WelcomeIdentity::default(),
            done: BTreeSet::new(),
            newly_done: Vec::new(),
            wants: Vec::new(),
            learn_open: false,
            shown: BTreeSet::new(),
            focus: None,
            viewport: (900.0, 560.0),
            engine: TextEngine::new(),
            pointer: (0.0, 0.0),
            scroll: 0.0,
            laid: None,
            drawn: Vec::new(),
            pinned_show_title: false,
            mcp_command: mcp::command(),
        }
    }

    /// The page right after onboarding: greets `owner` and names `workspace`.
    /// The container fills the rest of the identity on its next sync.
    pub fn first_minutes(owner: impl Into<String>, workspace: impl Into<String>) -> Self {
        let mut view = Self::new();
        let owner = owner.into();
        view.identity.owner = (!owner.trim().is_empty()).then_some(owner);
        view.identity.workspace = workspace.into();
        view
    }

    #[cfg(test)]
    pub fn identity(&self) -> &WelcomeIdentity {
        &self.identity
    }

    /// Who the page greets — pushed by the container, which owns the answer.
    pub fn set_identity(&mut self, identity: WelcomeIdentity) {
        if identity != self.identity {
            self.identity = identity;
            self.laid = None;
        }
    }

    /// The machine-wide ticks, by stable row id. Unknown ids are ignored, so
    /// a newer build's rows do not confuse an older one.
    pub fn set_done<'a>(&mut self, ids: impl IntoIterator<Item = &'a str>) {
        let done: BTreeSet<RowId> = ids.into_iter().filter_map(RowId::from_key).collect();
        let merged: BTreeSet<RowId> = done.union(&self.done).copied().collect();
        if merged != self.done {
            self.done = merged;
            self.laid = None;
        }
    }

    #[cfg(test)]
    pub fn is_done(&self, row: RowId) -> bool {
        self.done.contains(&row)
    }

    /// Rows ticked since the last ask, as their stable ids.
    pub fn take_newly_done(&mut self) -> Vec<&'static str> {
        std::mem::take(&mut self.newly_done)
            .into_iter()
            .map(RowId::key)
            .collect()
    }

    pub fn take_wants(&mut self) -> Vec<WelcomeWant> {
        std::mem::take(&mut self.wants)
    }

    pub fn pinned_show_title(&self) -> bool {
        self.pinned_show_title
    }

    fn tick(&mut self, row: RowId) {
        if row.ticks() && self.done.insert(row) {
            self.newly_done.push(row);
            self.laid = None;
        }
    }

    /// `(done, of)` for a section's counted rows.
    pub(crate) fn progress_of(&self, section: Section) -> (usize, usize) {
        let counted: Vec<RowId> = section
            .rows()
            .iter()
            .copied()
            .filter(|row| row.counted())
            .collect();
        let done = counted.iter().filter(|row| self.done.contains(row)).count();
        (done, counted.len())
    }

    /// A section with a count whose counted rows are all done folds to one
    /// line, until *Show* opens it again.
    pub(crate) fn collapsed(&self, section: Section) -> bool {
        let (done, of) = self.progress_of(section);
        section.counts() && of > 0 && done == of && !self.shown.contains(&section)
    }

    fn activate(&mut self, target: Target) {
        match target {
            Target::Customize => self.wants.push(WelcomeWant::Settings),
            Target::MicLink => self.wants.push(WelcomeWant::VoiceSettings),
            Target::Import(format) => {
                self.wants.push(WelcomeWant::Import(format));
                self.tick(RowId::Documents);
            }
            Target::ChooseFiles => {
                self.wants.push(WelcomeWant::ImportFiles);
                self.tick(RowId::Files);
            }
            Target::FeedbackForm => {
                self.wants.push(WelcomeWant::Feedback);
                self.tick(RowId::Feedback);
            }
            Target::Email => {
                self.wants.push(WelcomeWant::Email);
                self.tick(RowId::Feedback);
            }
            Target::CopyMcp => {
                self.wants.push(WelcomeWant::CopyMcpCommand);
                self.tick(RowId::Mcp);
            }
            Target::PairQr => self.wants.push(WelcomeWant::LinkDevice),
            Target::Toggle(section) => {
                if !self.shown.remove(&section) {
                    self.shown.insert(section);
                }
                self.laid = None;
            }
            Target::Row(row) => match row {
                RowId::FirstDocument => {
                    self.wants.push(WelcomeWant::NewDocument);
                    self.tick(row);
                }
                RowId::Learn => {
                    self.learn_open = !self.learn_open;
                    self.laid = None;
                    if self.learn_open {
                        self.tick(row);
                    }
                }
                RowId::Reminder => {
                    self.wants.push(WelcomeWant::Reminders);
                    self.tick(row);
                }
                RowId::Phone | RowId::Computer => {
                    self.wants.push(WelcomeWant::LinkDevice);
                    self.tick(row);
                }
                RowId::Ai => {
                    self.wants.push(WelcomeWant::AiSetup);
                    self.tick(row);
                }
                RowId::TerminalAgent => {
                    self.wants.push(WelcomeWant::Agents);
                    self.tick(row);
                }
                // Rows that act only through their links, chips and buttons,
                // and drawn rows with nothing behind them yet; never a target.
                RowId::Documents
                | RowId::Files
                | RowId::TaskList
                | RowId::Feedback
                | RowId::Watch
                | RowId::Mcp
                | RowId::OwnApps => {}
            },
        }
    }

    /// The measured page, measuring it first if anything has changed.
    fn ensure_laid(&mut self) -> &layout::Layout {
        if self.laid.is_none() {
            let laid = layout::measure(self);
            self.laid = Some(laid);
        }
        self.laid.as_ref().expect("measured above")
    }

    fn max_scroll(&self) -> f32 {
        self.laid
            .as_ref()
            .map_or(0.0, |laid| (laid.bottom - self.viewport.1).max(0.0))
    }

    /// What is under a content-space point: anything drawn over a row wins
    /// over the row itself.
    fn target_at(laid: &layout::Layout, x: f32, y: f32) -> Option<Target> {
        let hits = laid.hits.iter().filter(|(_, rect)| rect.contains(x, y));
        let mut row = None;
        for (target, _) in hits {
            if !target.is_row() {
                return Some(*target);
            }
            row.get_or_insert(*target);
        }
        row
    }

    fn focus_order(&mut self) -> Vec<Target> {
        self.ensure_laid()
            .hits
            .iter()
            .map(|(target, _)| *target)
            .collect()
    }

    fn move_focus(&mut self, forward: bool) {
        let order = self.focus_order();
        if order.is_empty() {
            self.focus = None;
            return;
        }
        let at = self
            .focus
            .and_then(|focus| order.iter().position(|t| *t == focus));
        let next = match (at, forward) {
            (None, true) => 0,
            (None, false) => order.len() - 1,
            (Some(i), true) => (i + 1) % order.len(),
            (Some(i), false) => i.checked_sub(1).unwrap_or(order.len() - 1),
        };
        self.focus = Some(order[next]);
        self.reveal_focus();
    }

    /// Scroll just far enough that the focused control is on screen.
    fn reveal_focus(&mut self) {
        let Some(focus) = self.focus else {
            return;
        };
        let Some(rect) = self
            .ensure_laid()
            .hits
            .iter()
            .find(|(target, _)| *target == focus)
            .map(|(_, rect)| *rect)
        else {
            return;
        };
        let vh = self.viewport.1;
        if rect.y < self.scroll {
            self.scroll = (rect.y - 16.0).max(0.0);
        } else if rect.y + rect.h > self.scroll + vh {
            self.scroll = (rect.y + rect.h + 16.0 - vh).min(self.max_scroll());
        }
    }

    fn hot(&self) -> Option<Target> {
        let laid = self.laid.as_ref()?;
        Self::target_at(laid, self.pointer.0, self.pointer.1 + self.scroll)
    }
}

impl View for WelcomeView {
    /// A workspace switch changes nothing this page draws by itself; the
    /// container pushes the new workspace's name with the next identity.
    fn reseat(&mut self, _ws: &WorkspaceDoc) {}

    fn key(
        &mut self,
        _ws: &mut WorkspaceDoc,
        key: &Key,
        _text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        match key {
            Key::Named(NamedKey::Tab) if mods == ModifiersState::SHIFT => {
                self.move_focus(false);
                true
            }
            Key::Named(NamedKey::Tab) if mods.is_empty() => {
                self.move_focus(true);
                true
            }
            Key::Named(NamedKey::Enter | NamedKey::Space) if mods.is_empty() => {
                let Some(focus) = self.focus else {
                    return false;
                };
                self.activate(focus);
                true
            }
            Key::Named(NamedKey::Escape) if mods.is_empty() && self.focus.is_some() => {
                self.focus = None;
                true
            }
            _ => false,
        }
    }

    fn mouse_down(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32, _ctrl: bool) -> bool {
        // The column scrolls, so hit-tests compare against content-space y.
        let cy = y + self.scroll;
        let Some(target) = Self::target_at(self.ensure_laid(), x, cy) else {
            return false;
        };
        self.focus = Some(target);
        self.activate(target);
        true
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let before = self.hot();
        let moved = self.pointer != (x, y);
        self.pointer = (x, y);
        moved && (before != self.hot() || self.max_scroll() > 0.0)
    }

    fn mouse_up(&mut self, _ws: &mut WorkspaceDoc) -> bool {
        false
    }

    fn mouse_double_click(&mut self, _ws: &WorkspaceDoc, _x: f32, _y: f32) -> bool {
        false
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                y * theme::text_metrics::DOC_BODY.line_height * WHEEL_LINES
            }
            // A trackpad or precision wheel already reports pixels, and those
            // are the distance the finger asked for.
            MouseScrollDelta::PixelDelta(p) => p.y as f32,
        };
        self.ensure_laid();
        self.scroll = (self.scroll - lines).clamp(0.0, self.max_scroll());
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        if self.viewport != (w, h) {
            self.viewport = (w, h);
            self.laid = None;
        }
    }

    fn cursor_icon(&self) -> CursorIcon {
        // Koru does not use the hand cursor (`docs/features/design/DESIGN.md`).
        if self.hot().is_some() {
            koru_ui::cursor::PRESS
        } else {
            koru_ui::cursor::IDLE
        }
    }

    fn adopt(&mut self, _ws: &WorkspaceDoc) {}

    fn invalidate(&mut self) {
        // A theme or font change moves every measurement.
        self.engine.clear_pill_cache();
        self.laid = None;
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        _workspace: &Workspace,
        _images: &ImageCache,
        root: Affine,
        _caret_on: bool,
        _pointer: (f32, f32),
    ) {
        self.draw_page(scene, root);
    }

    /// A pinned tab's title is drawn beside its icon; the *choice* is a
    /// Settings row, and this view only carries it for the tab strip.
    fn adopt_preferences(&mut self, settings: &Settings) {
        self.pinned_show_title = settings.pinned_show_title;
    }
}

#[cfg(test)]
mod tests;
