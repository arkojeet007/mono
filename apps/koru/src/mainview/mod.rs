// SPDX-License-Identifier: MPL-2.0

//! The container: a caption row of tabs, two collapsible side columns, and the
//! active tab's view between them.
//!
//! This is the one thing only something that can see every view crate is able to
//! say — which is why it lives in the manager rather than in `koru-shell-chrome`. The
//! chrome knows *where* things are; this knows what they mean.
//!
//! ## What is in which file
//!
//! | | |
//! | --- | --- |
//! | `mod.rs` | the struct, what a tab means, and `impl View` — the routing |
//! | [`open`] | how a document arrives in a tab — Explore, pills, replace, land |
//! | [`nav`] | where the window is standing, and how it gets back |
//! | [`tabset`] | the open tabs as a type, holding the active-index invariant |
//! | [`layout`] | cutting the window up, and keeping the visible tab laid out |
//! | [`draw`] | the caption row, and the order a frame goes down in |
//! | [`session`] | what survives a restart |
//! | [`panel`] | one side column, and its slide |
//! | [`tabslide`] | how far a reordered tab still has to travel |
//! | [`tab`] | `Tab` and `TabView` — what a tab holds |
//! | [`keys`] | the chords the container claims before the view sees them |
//!
//! ## It implements `View` and not `Editing`, deliberately
//!
//! A container has no `Body` and no `ViewState`, so [`Editing`] is not even
//! implementable here — and that is the payoff rather than a limitation. The
//! eight method names the two traits share (`key`, `mouse_down`, `mouse_move`,
//! `mouse_up`, `mouse_double_click`, `scroll_by`, `set_viewport`, `adopt`) are a
//! silent-dispatch hazard wherever both are in scope. Confining that to
//! [`Leaf`], where it is already solved with inherent methods and covered by the
//! suite, means the container cannot reintroduce it.
//!
//! ## The leaf keeps believing it owns a window
//!
//! Rather than teaching `ViewState` about insets, the container **lies about the
//! window**: it passes the content rect's *size* as the viewport and subtracts
//! the content rect's *origin* from every pointer coordinate before delegating.
//! Everything inside — scroll clamping, the scrollbar's geometry, `over_content`,
//! the toolbar's hit test — is then correct with no change at all.
//!
//! Three things that scheme does not cover, each handled here:
//!
//! - `View::draw`'s `pointer` is window-space and feeds the link tooltip, so it
//!   is translated alongside the transform.
//! - `scroll_by` carries no coordinates, so the wheel is routed on the last
//!   pointer position this view remembers.
//! - `hover_at` resolves a row from `y` alone and ignores `x`, so a pointer over
//!   a side column would still light a row. The leaf is only asked for a cursor
//!   while the pointer is inside content, and its hover is cleared on the way
//!   out.

#[cfg(test)]
mod harness;

/// What this window offers a screen reader, and what a request to one of its
/// controls means. A focused module rather than another subsystem grown
/// inside this file (§7.10); this file gets four small `impl View` methods
/// that call into it.
pub(crate) mod accessibility;
mod canvas;
mod canvas_chain;
mod canvas_runs;
pub(crate) mod cloud_sync;
pub(crate) mod context_menu;
mod delegate;
pub(crate) mod dictation;
pub(crate) mod draw;
mod error_dialog;
mod history;
mod icon_action;
mod import_dialog;
mod keys;
pub(crate) mod layout;
mod link_device;
pub(crate) mod linked_devices;
mod move_to;
mod nav;
mod nav_debug;
mod new_document;
mod new_list;
mod open;
pub mod panes;
mod profile_probe;
mod recording;
mod reminder_creation;
mod reminder_management;
mod reminder_shell;
mod reminders;
mod reminders_view;
pub(crate) mod routing;
mod run_trash;
mod runs;
mod schedules;
mod session;
mod session_backend;
mod snapshot_prompt;
pub mod tab;
pub mod tab_rules;
pub(crate) mod taskcounts;
pub(crate) mod tasks;
pub(crate) mod trash;
mod update_ui;
mod welcome_progress;
mod workspace_icon;
mod workspace_menu;
mod workspace_rename;
mod zoomlabel;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use std::collections::{HashMap, HashSet};

use koru_ai_provider::{CredentialStore, OsCredentialStore, SecretString};
use koru_ai_ui::{ChatSurface, SurfaceMode};
use koru_app_shell::panel::Panel;
use koru_app_shell::tabslide::TabSlides;
use koru_app_shell::tabstrip::TabStrip;
use koru_shell_chrome::regions::{self, Hit, Regions};
use koru_shell_chrome::tabs;
use koru_workspace_host::routing::{Blockers, ModalKind, PromptKind};
use koru_workspace_host::{PaneRequest, Settings, View, ZoomIntent};

/// The same workspace, in this crate's vocabulary.
///
/// **Two `WorkspaceId` types and no conversion between them**, because the
/// neutral surfaces speak `koru-store-core`'s ids and Koru's model speaks
/// `koru-core`'s. They are the same uuid; a `From` impl would need one crate
/// to depend on the other, which is the coupling the surface split exists to
/// avoid. So it is spelled once, here, where the seam is.
/// What a Profile controller's effect asks of the window, as the
/// `PaneRequest` the shell carries out. **One mapping**, so no caller spells
/// what the controller's effects mean a second time.
fn profiles_effect_request(effect: crate::manage::profiles::Effect) -> PaneRequest {
    use crate::manage::profiles::Effect;
    match effect {
        Effect::OpenWebsite(url) => PaneRequest::OpenWebsite(url),
        Effect::OpenUri(uri) => PaneRequest::OpenUri(uri),
        Effect::CreateConnectedProfile { owner_scope, draft } => {
            PaneRequest::CreateConnectedProfile { owner_scope, draft }
        }
        Effect::RestoreLocalProfile(profile) => PaneRequest::RestoreLocalProfile { profile },
        Effect::SwitchProfile(profile) => PaneRequest::SwitchProfile(profile),
        Effect::BindAccount {
            profile,
            expected_identity_id,
            new_identity_id,
        } => PaneRequest::BindLocalProfileAccount {
            profile,
            expected_identity_id,
            new_identity_id,
        },
        Effect::RemoveLocalProfile {
            profile,
            delete_workspaces,
        } => PaneRequest::RemoveLocalProfile {
            profile,
            delete_workspaces,
        },
        Effect::FinishHostedDeletion {
            profile,
            discovered_after_offline,
        } => PaneRequest::FinishHostedProfileDeletion {
            profile,
            discovered_after_offline,
        },
        Effect::ContinueDeletedOffline {
            profile,
            expected_identity_id,
        } => PaneRequest::ContinueDeletedProfileOffline {
            profile,
            expected_identity_id,
        },
        Effect::Account {
            owner_scope,
            command,
            submission: Some(submission),
        } => PaneRequest::AccountTracked {
            submission,
            owner_scope,
            command,
        },
        Effect::Account {
            owner_scope,
            command,
            submission: None,
        } => PaneRequest::AccountOwned {
            owner_scope,
            command,
        },
    }
}

use crate::panes::context::{ContextView, Section};
use crate::panes::explore::ExploreView;
use crate::panes::{DropTarget, DropZone, Outcome};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{DocumentId, Kind, NodeId, ProfileId, ProfileSummary, WorkspaceEntry, WorkspaceId};
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::{ImageCache, scene as draw_scene, theme};
use koru_editor::{Editing, OpenWhere};
use koru_ui::caption::Action as CaptionAction;
use koru_ui::keys::debug as inputdebug;
use koru_ui::{Tooltip, TooltipContent};
use koru_window::{WindowRequest, snap};
use koru_workspace::Workspace;
use trash::ConfirmSubject;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::Fill;
use winit::event::MouseScrollDelta;
use winit::keyboard::{Key, ModifiersState};
use winit::window::CursorIcon;

use crate::leaf::Leaf;
use context_menu::{Anchor as MenuAnchor, ContextMenu, MenuAction, MenuSubject};
use error_dialog::{ErrorDialog, Outcome as ErrorOutcome};
use import_dialog::{ImportDialog, Outcome as ImportOutcome};
use keys::Chord;
use link_device::{LinkDevice, Outcome as LinkDeviceOutcome};
use linked_devices::LinkedDevices;
use nav::Spot;
use new_document::{Action as NewDocumentAction, NewDocument, Purpose as PromptPurpose};
use snapshot_prompt::{Action as SnapshotAction, SnapshotPrompt};
use tab::{Tab, TabView};
use tab_rules::KoruTabs;
use workspace_icon::{EmojiColorPanel, Outcome as IconOutcome, Target as IconTarget};
use workspace_rename::{Outcome as RenameOutcome, Target as RenameTarget, WorkspaceRename};
use zoomlabel::ZoomLabel;

/// **Two `WorkspaceId` types and no conversion between them**, because the

/// The same crossing for a profile. Two id types for one uuid is the

/// Now, as the epoch milliseconds every relative time in this window is
/// read against. A clock a surface is *handed* rather than one it holds:
/// `koru-i18n`'s formatters take a moment and never ask for one.
fn epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn reminder_language(lang: koru_i18n::Lang) -> koru_editor::utils::when::Language {
    match lang {
        koru_i18n::Lang::En => koru_editor::utils::when::Language::English,
        koru_i18n::Lang::Es => koru_editor::utils::when::Language::Spanish,
        koru_i18n::Lang::PtBr => koru_editor::utils::when::Language::PortugueseBrazil,
        koru_i18n::Lang::De => koru_editor::utils::when::Language::German,
        koru_i18n::Lang::Fr => koru_editor::utils::when::Language::French,
        koru_i18n::Lang::Ar => koru_editor::utils::when::Language::Arabic,
        koru_i18n::Lang::ZhHans => koru_editor::utils::when::Language::ChineseSimplified,
        koru_i18n::Lang::Ja => koru_editor::utils::when::Language::Japanese,
    }
}

fn reminder_system_week_start() -> koru_editor::utils::when::WeekStart {
    use koru_editor::utils::when::WeekStart;
    use koru_workspace_host::environment::FirstDayOfWeek;

    match koru_workspace_host::environment::preferred_first_day_of_week() {
        Some(FirstDayOfWeek::Monday) => WeekStart::Monday,
        Some(FirstDayOfWeek::Saturday) => WeekStart::Saturday,
        Some(FirstDayOfWeek::Sunday) => WeekStart::Sunday,
        None => WeekStart::Monday,
    }
}

#[derive(Clone)]
struct ReminderEnvironment {
    resolution: koru_editor::utils::when::ResolutionContext,
    locale: koru_i18n::time::LocaleFormat,
    display_rules: koru_editor::utils::zone::IanaZoneRules,
}

/// Sample the device clock, zone, and full locale capabilities during
/// maintenance. Presentation keeps the region and OS clock preference while
/// natural-language resolution receives its corresponding semantic inputs.
fn reminder_environment() -> ReminderEnvironment {
    let zone = koru_editor::utils::zone::SystemZone::detect();
    let current_zone = koru_core::time::ZoneRules::zone(&zone).clone();
    let display_rules = koru_editor::utils::zone::IanaZoneRules::from_name(&current_zone)
        .expect("SystemZone always contains validated IANA rules");
    let system_locale = koru_window::system_locale::system_locale();
    let locale = koru_i18n::time::LocaleFormat::for_system(
        system_locale.as_deref(),
        koru_workspace_host::environment::preferred_hour_cycle(),
    );
    let resolution = koru_editor::utils::when::ResolutionContext {
        now_ms: chrono::Utc::now().timestamp_millis(),
        language: reminder_language(locale.lang()),
        date_order: reminder_date_order(&locale),
        current_zone,
        selection: None,
        end_of_day: None,
        due_anchor: None,
        system_week_start: reminder_system_week_start(),
        week_start_override: None,
    };
    ReminderEnvironment {
        resolution,
        locale,
        display_rules,
    }
}

fn reminder_date_order(
    locale: &koru_i18n::time::LocaleFormat,
) -> koru_editor::utils::when::DateOrder {
    use koru_editor::utils::when::DateOrder;
    use koru_i18n::time::NumericDateOrder;

    match locale.date_order() {
        NumericDateOrder::MonthDayYear => DateOrder::MonthFirst,
        NumericDateOrder::DayMonthYear => DateOrder::DayFirst,
        NumericDateOrder::YearMonthDay => DateOrder::YearFirst,
    }
}

/// Whether the caption draws its own minimise/maximise/close.
///
/// True now that the window is undecorated. Kept as a named constant rather
/// than inlined because it is the single switch between "the OS draws the
/// titlebar" and "we do", and every piece of caption geometry takes it — so
/// putting the OS titlebar back is one line here, not a search.
const WINDOW_BUTTONS: bool = true;
/// The reminder-attention bell owns the optional product caption slot —
/// when this build shows reminders (koru-features). With them hidden the slot
/// is empty and the tab strip takes the room back.
fn reminder_bell() -> bool {
    koru_features::reminders()
}
/// The first [`TooltipContent`] key a menu **control** claims; the control's
/// index is added to it.
///
/// A key is what tells the chip it has moved to a different target rather than
/// been re-hovered, so every family of adjacent targets needs a band of its
/// own. This one sits clear of the caption's small numbers and of the two
/// existing bands (`40 +` for view modes, `100 +` for pinned rows).
const MENU_CONTROL_TIP: u64 = 200;
/// One post-activation retry lets a just-finished Store mutation reach its
/// indexed usage row without turning the workspace manager into a poller.
// The Store's projection worker deliberately sleeps for up to 250 ms while
// idle. Leave enough room for it to wake, mirror the manifest listing, and let
// the usage reader observe that transaction before the one-shot retry.
/// How often an open workspace manager takes what its link has.
///
/// **A tick rather than a wake, because a pane inside Koru has no proxy.** The
/// surface's answers arrive on its own thread — a catalog, a consequence, an
/// outcome — and something has to notice. `SettingsSurface` reaches this tab
/// the same way and says so: the event-loop proxy belongs to the shell, and
/// `maintenance_deadline`/`maintain` is a `try_recv` and a clock comparison.
///
/// Only armed while the manager is the visible tab, so a window with a
/// document open ticks for nothing.
const WORKSPACES_POLL: Duration = Duration::from_millis(120);

/// How often a deep link re-asks what its heading is called.
///
/// The same two seconds the pane context already runs on. There is nothing to
/// subscribe to: the app is told about manifest changes -- titles, trash -- and
/// never about a heading being retyped, least of all one in another workspace.
const DEEP_LINK_POLL: Duration = Duration::from_secs(2);

/// Enough to reverse a *Move to…* while its toast is up.
#[derive(Clone, Debug)]
struct MoveUndo {
    id: koru_core::DocumentId,
    /// Where it was before the move.
    from_ws: WorkspaceId,
    from_parent: Option<koru_core::DocumentId>,
    from_index: usize,
    /// Where it is now (after the confirmed move).
    to_ws: WorkspaceId,
}

/// Whether the caption offers the context column's **toggle**.
///
/// Context belongs to the active document, so its control lives with that
/// document's title block (and on `Ctrl+J`), not beside the machine-wide AI
/// launcher in the window caption. False deliberately produces zero-width
/// geometry, letting the tabs reclaim the slot without another layout path.
const CONTEXT_TOGGLE: bool = false;

const SEARCH_MODAL_W: f32 = 620.0;
const SEARCH_MODAL_H: f32 = 400.0;
const SEARCH_MODAL_MARGIN: f32 = 16.0;
const MOVE_TO_MODAL_W: f32 = 420.0;
const MOVE_TO_MODAL_H: f32 = 360.0;

fn search_modal_bounds(viewport: (f32, f32)) -> Rect {
    let w = SEARCH_MODAL_W.min((viewport.0 - SEARCH_MODAL_MARGIN * 2.0).max(1.0));
    let h = SEARCH_MODAL_H.min((viewport.1 - SEARCH_MODAL_MARGIN * 2.0).max(1.0));
    Rect::new((viewport.0 - w) * 0.5, (viewport.1 - h) * 0.5, w, h)
}

fn move_to_modal_bounds(viewport: (f32, f32)) -> Rect {
    let w = MOVE_TO_MODAL_W.min((viewport.0 - SEARCH_MODAL_MARGIN * 2.0).max(1.0));
    let h = MOVE_TO_MODAL_H.min((viewport.1 - SEARCH_MODAL_MARGIN * 2.0).max(1.0));
    Rect::new((viewport.0 - w) * 0.5, (viewport.1 - h) * 0.5, w, h)
}

/// Roots of a multi-select: drop any id that is a descendant of another chosen
/// id so a parent move does not double-count children.
fn move_roots(
    ids: &[koru_core::DocumentId],
    entries: &[koru_core::explore::ExploreEntry],
) -> Vec<koru_core::DocumentId> {
    let chosen: HashSet<_> = ids.iter().copied().collect();
    let parent_of: std::collections::HashMap<_, _> =
        entries.iter().map(|e| (e.id, e.parent)).collect();
    ids.iter()
        .copied()
        .filter(|id| {
            let mut at = parent_of.get(id).copied().flatten();
            while let Some(p) = at {
                if chosen.contains(&p) {
                    return false;
                }
                at = parent_of.get(&p).copied().flatten();
            }
            true
        })
        .collect()
}

/// A search result waiting for the document it names.
///
/// **A result can name a document this window does not have open**, and opening
/// one is a round trip through the shell: the node cannot be resolved until it
/// comes back. So the target waits here — and *keeps* waiting, which is the
/// whole of the fix it represents. The drain used to `take()` the target and
/// only then ask whether it resolved, so the first `adopt` after the request
/// threw it away: a workspace switch alone fires several, so a result in
/// another workspace reliably opened nothing at all.
///
/// It is bounded by a deadline rather than kept forever, because a target that
/// never resolves is one nobody is waiting for any more — and an unbounded one
/// would fire minutes later, on a document somebody opened for their own
/// reasons.
struct DeferredTarget {
    /// The document to reveal in the tree once it is here. Carried so the
    /// retry can reveal it: the first attempt is the one that fails, and a
    /// reveal on a failed attempt is a reveal nobody sees.
    document: Option<NodeId>,
    /// The row to land on, when the hit named one. **`None` with a `document`
    /// means the hit was that document's title**, whose row is row 0 of its own
    /// body — knowable only once the document is open, which is why it cannot
    /// be filled in here.
    node: Option<NodeId>,
    trash_root: Option<NodeId>,
    match_text: Option<String>,
    /// Whether the shell has been asked for the document yet. The ask fails
    /// while the manifest has not caught up with a workspace just switched to,
    /// and without this nothing would ever ask again — the drain would wait for
    /// a document nobody had requested.
    asked: bool,
    /// When to give up. `None` for a cold-launch target, which is drained once
    /// by `restore` and never waits.
    until: Option<Instant>,
}

/// How long a deferred target waits for its document before it is dropped.
///
/// Long enough for a workspace switch and a document open over a socket, short
/// enough that nobody connects it to something they did next.
const DEFERRED_TARGET_WAIT: Duration = Duration::from_secs(10);

fn trace_navigation_node(node: Option<NodeId>) -> String {
    node.map(|node| node.to_storage())
        .unwrap_or_else(|| "none".to_owned())
}
/// What the toast's action button does, if it has one.
///
/// The toast itself only carries a label; this is the payload the container
/// remembers so pressing the button can act without the widget knowing about
/// nodes or paths.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum ToastPayload {
    /// Undo trash / restore these document roots.
    Restore(Vec<NodeId>),
    /// Undo trash on **runs**: take them back out of the workspace's bin.
    ///
    /// Its own arm rather than nodes in [`Self::Restore`], because a run's
    /// restore is two halves — the worker forgets what was decided about
    /// its results, and the document comes back out — and only one of them
    /// is a manifest edit.
    ///
    /// A set, for [`Self::Restore`]'s reason: an Undo that took back the
    /// last of four is not an undo of what was asked for.
    RestoreRuns(Vec<koru_delegate_core::RunId>),
    /// Reveal this path in the OS file manager (file or folder).
    Reveal(PathBuf),
    /// Open/focus the first document produced by a content import.
    Open(koru_core::DocumentId),
    /// Switch Explore to Files and reveal the imported file there.
    ShowFile(koru_core::DocumentId),
    /// A running import. Its action asks the shell to cancel; the marker also
    /// prevents worker updates from recreating the toast after it is closed.
    ImportProgress,
    /// A running speech-model download. Its action is **Stop** and asks the
    /// shell for cooperative cancellation; the marker keeps a stage update
    /// from recreating the toast somebody closed, exactly as the import's
    /// does. Closing it hides progress and never changes the download.
    VoiceDownload,
    /// Support replied: *Open* reveals the chat.
    OpenSupport,
    /// A trusted device deleted a Profile; reveal the remaining Profiles so
    /// the person can choose where to go next.
    OpenProfiles,
    /// A feedback report both attempts failed to deliver. Its action reopens
    /// the form, which the restored draft has already refilled, so the person
    /// can look at what they wrote before pressing Send again.
    ReopenFeedback,
    /// A task captured on a page it will not show on. *View task* opens the
    /// queue or list it went to with the row selected; *Undo* takes it back.
    TaskAdded {
        document: koru_core::DocumentId,
        node: NodeId,
        target: crate::panes::explore::rail::RailTarget,
    },
    /// Message only — no action button.
    None,
}

/// What a task command's receipt is owed beyond the counts: nothing, a new
/// home to open, or a toast saying where a capture went.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Landing {
    Plain,
    /// *Create document*: open the page the receipt names, caret on the task.
    OpenHome,
    /// A capture that lands somewhere other than the page it was typed on:
    /// say where, with a way there and a way back.
    Announce {
        message: String,
        target: crate::panes::explore::rail::RailTarget,
    },
}

/// A download's fraction as the bar's own mode. `None` is indeterminate — see
/// `koru_workspace_host::voice_install::progress_line` for which stages have a figure
/// and which deliberately do not.
fn voice_progress(fraction: Option<f32>) -> koru_ui::ProgressMode {
    match fraction {
        Some(fraction) => koru_ui::ProgressMode::Determinate(fraction),
        None => koru_ui::ProgressMode::Indeterminate,
    }
}

/// Which attribute picker a deferred explore-row gesture opens once its
/// document is active.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum AttrPickerKind {
    Tags,
    Color,
    Due,
    Priority,
}

impl AttrPickerKind {
    /// The editor-side name for the same choice.
    fn picker(self) -> koru_editor::AttrPicker {
        match self {
            Self::Tags => koru_editor::AttrPicker::Tags,
            Self::Color => koru_editor::AttrPicker::Color,
            Self::Due => koru_editor::AttrPicker::Due,
            Self::Priority => koru_editor::AttrPicker::Priority,
        }
    }
}

/// Which chat surface asked the shell for a folder.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum AiChatFolderSource {
    Pane,
    AiChatTab,
}

pub struct MainView {
    /// The pane tree: independent tab groups laid out inside the content area.
    panes: panes::PaneTree,
    viewport: (f32, f32),
    regions: Regions,
    /// The chrome's own fonts. Separate from any leaf's, because a leaf's cache
    /// is sized to what *it* draws.
    engine: TextEngine,

    /// Welcome default copied onto a newly pinned tab.
    pinned_show_title: bool,
    /// Active split preview while a tab is being dragged over a group view.
    split_intent: Option<panes::SplitIntent>,
    /// A tab being dragged between groups, or out for a tear-off/pop-out.
    pane_drag: Option<panes::PaneDrag>,
    /// The slot a cross-pane drop would land in while the pointer is over a
    /// target group's strip: the group, and the flat index the marker showed.
    drop_slot: Option<(panes::PanePath, usize)>,
    /// Set by the shell while this window's in-flight tab drag is over another
    /// window that accepts it. The pop-out pill promises a window of its own;
    /// over a receiving window that promise is false and the target's preview
    /// is speaking, so the pill stays down.
    foreign_targeted: bool,

    /// The explore column, and whether it is in or out.
    sidebar: Panel,
    explore: ExploreView,
    /// Whether the tree needs rebuilding before it is next drawn. Set by
    /// `adopt` and by anything that expands a row, so a rebuild costs one walk
    /// per change rather than one per frame.
    explore_stale: bool,
    /// The document whose title the caret was in on the last frame.
    ///
    /// A document's name is its first row now, so it is typed rather than
    /// committed — and the tab strip and the explore tree are *caches* of that
    /// name. Following it keystroke by keystroke means a tab whose width
    /// changes under the pointer and a tree row that re-sorts while you are
    /// looking at it, for a name that is not finished being written.
    ///
    /// So they wait. This holds what was being named last frame; when it stops
    /// matching, the caret has left the title and both caches take the finished
    /// name in one go. Compared in `draw_frame`, which is the one point every
    /// way of moving the caret goes through — a key, a click, a tab switch,
    /// clicking away from the rows entirely.
    naming: Option<NodeId>,
    /// Whether the taskbar's Recent list is behind the recents. Same shape as
    /// `explore_stale`, and for the same reason: the names come out of the
    /// document, and that is not a per-frame lookup.
    recents_stale: bool,
    /// Whether `pane_context` has changed since the explore column was last
    /// given it. Same shape again, and the same reason: handing it over means
    /// cloning two `Vec`s, which is not a per-frame cost either.
    context_stale: bool,

    /// The context column, on the right.
    context: Panel,
    context_view: ContextView,
    /// Koru AI is its own full-height left pane. It is intentionally not a
    /// section of the context column: chat has independent width, focus and
    /// lifetime.
    ai_panel: Panel,
    ai_chat: ChatSurface,
    /// The chat whose folder pick is out with the shell, and which surface
    /// asked: the pane's, or an AI chat tab's.
    ai_chat_folder_ask: Option<(AiChatFolderSource, koru_ai_ui::ChatId)>,
    ai_focused: bool,
    document_history: history::HistoryPanel,
    /// What the context column's Cloud tab shows — one query per refresh on a
    /// worker thread, and nothing at all while the tab is not showing.
    cloud_sync: cloud_sync::CloudSyncPanel,
    /// The Cloud tab's image sweep is owed. Set by the tab's own poll and by a
    /// document change, consumed on the next frame — the image cache is the
    /// frame thread's and the document tree is the maintenance pass's, so the
    /// two are joined by a flag rather than by walking the document every
    /// frame (see `draw::missing_images`).
    cloud_images_due: bool,
    /// The last answer that sweep gave: how many images are not here, and the
    /// devices known to hold them.
    cloud_images: (usize, Vec<String>),
    /// What the tasks rail's numbers come from — one query per refresh on a
    /// worker thread, and nothing at all while the rail is not showing.
    task_counts: taskcounts::TaskCountsPanel,
    /// **The one place the local date is watched.** Every bucket is a function
    /// of *today*, so midnight — and waking on the other side of one — makes all
    /// five wrong at the same instant and nothing else announces it. Compared on
    /// the maintenance tick this window already runs; there is no timer of its
    /// own and no clock read per frame.
    tasks_day: crate::tasks_view::query::DayWatch,
    /// The context column's Tasks section is out of date.
    ///
    /// **A flag rather than a walk on a tick.** Filling that section reads the
    /// open document, and *nothing walks the document on a timer* — so it is
    /// rebuilt when something that could have changed it happened, and only
    /// while the section is on screen to see it.
    context_tasks_stale: bool,
    /// The document those rows were read from. A tab change is a change like
    /// any other, and noticing it here is cheaper than threading a flag through
    /// every path that opens a document.
    context_tasks_doc: Option<koru_core::DocumentId>,
    /// The tasks rail's inline "+ New list" field, while one is being typed.
    /// `None` is the common case and costs nothing, the same shape the rename
    /// field has.
    new_list: Option<new_list::NewList>,
    /// A list the rail asked for, waiting for the shell to create it in the
    /// workspace system document. The tag chooser's `wants_create_tag` seam,
    /// one for one.
    wants_create_list: Option<(String, Option<String>)>,
    /// Renames, recolours and deletions of lists that already exist, waiting for
    /// the same sweep. A `Vec` rather than a second `Option` because two of them
    /// can happen between two turns — and because a **delete only ever arrives
    /// here from the confirmation**, which is what makes that dialog the one
    /// door rather than a thing that can be walked around.
    list_edits: Vec<koru_workspace_host::tasks::ListEdit>,
    /// The workspace list table as the shell last pushed it. The rail's names,
    /// swatches and order come from here; only the counts come off the wire.
    list_table: Vec<koru_core::system::ListEntry>,
    /// Store-backed reminder behavior and cached semantic state. The final
    /// surface geometry/navigation is intentionally not represented here.
    reminders: reminders::Controller,
    reminder_creation: reminder_creation::Surface,
    /// Clock, locale and device zone sampled by maintenance. Reminder input
    /// handlers only clone this value and add the target's due anchor.
    reminder_environment: ReminderEnvironment,
    /// Transient reminder-only chrome over cached presentation data.
    reminder_shell: reminder_shell::Shell,
    /// Optimistic submissions waiting until their document is active, because
    /// `maintain` receives only that document's `WorkspaceDoc`.
    pending_reminder_submissions:
        std::collections::VecDeque<(reminders::Target, koru_editor::reminders::RequestIdentity)>,
    pending_reminder_cancels:
        std::collections::VecDeque<(reminders::Target, koru_editor::reminders::RequestIdentity)>,

    /// Where the pointer last was, in window space. Kept because `scroll_by`
    /// arrives without coordinates and has to be routed somewhere.
    pointer: (f32, f32),
    /// Whether the pointer was inside the content rect last time it moved, so
    /// the crossing out can clear the leaf's hover exactly once.
    pointer_in_content: bool,
    hover: Option<Hit>,

    /// The right-click menu over a tab or a workspace row, when one is open.
    /// Modal while it is up: a left click chooses an item or dismisses it, `Esc`
    /// closes it, and the keys `Up`/`Down`/`Enter` drive it. `None` the rest of
    /// the time.
    ///
    /// Modal **over the switcher popover, not instead of it** — a menu opened
    /// from a workspace row leaves the row it names on screen underneath.
    menu: Option<ContextMenu>,
    /// The modal opened by the taskbar's “New document” action. It is kept at
    /// container level so completing it can create a document and open its tab
    /// in one workspace edit.
    new_document: Option<NewDocument>,
    /// The Profile a *New workspace* prompt makes its workspace in, when it is
    /// not the open one — the switcher can list another Profile's workspaces.
    new_workspace_owner: Option<ProfileId>,
    /// The Ctrl+S save-a-snapshot prompt. See [`snapshot_prompt`].
    snapshot_prompt: Option<SnapshotPrompt>,
    /// Stable target named by a cold jump-list or global-search launch, or by a
    /// result in a document this window has asked the shell to open. Applied
    /// **after** session restore so the restored tab set does not replace it.
    /// Keeping node and Trash root together also preserves deleted-page hits.
    deferred_search_target: Option<DeferredTarget>,
    /// Stable destination received by a newly launched process. It is queued
    /// only after restore, then goes through the shell's Store resolver just
    /// like an in-process pill or search result.
    deferred_navigation: Option<(DocumentId, Option<NodeId>, Option<String>)>,
    /// Explore row to select and scroll after a Reveal / Ctrl+Shift+E that may
    /// first switch workspace. Applied once the tree for the home workspace is
    /// rebuilt (see [`Self::apply_pending_explore_reveal`]).
    pending_explore_reveal: Option<NodeId>,
    /// How the next `land_on` after a Store load should place the document.
    ///
    /// A **queue**, not a single slot: multi-select *Open in new tab* asks for
    /// several documents at once, and each Navigate arrival must claim its own
    /// disposition. Matching is by document id so an intermediate switch
    /// announcement cannot steal another page's NewTab.
    pending_lands: std::collections::VecDeque<(koru_core::DocumentId, OpenWhere)>,
    /// Where a store-routed explore drop puts its tab once the document
    /// arrives — see [`panes::PendingPaneDrop`]. A Vec for the same reason
    /// `pending_lands` is a queue: two drops can be in flight together, and
    /// each arrival claims only its own.
    pending_pane_drops: Vec<panes::PendingPaneDrop>,
    /// What each task command still waiting for its receipt owes when it
    /// lands, in order — `pending_count_deltas`'s twin, popped on the same
    /// two turns.
    pending_landings: std::collections::VecDeque<Landing>,
    /// A failure the person at the keyboard has to see. The topmost surface in
    /// the window, and the one a press outside does not dismiss — see
    /// [`error_dialog`]. [`Self::updates_modal`] dismisses on the *release* of
    /// an outside press, so the paired mouse-up cannot reach the document
    /// underneath (ADR-150).
    error: Option<ErrorDialog>,
    /// Legacy workspace inspection, rendered in-app so the tree can scroll
    /// independently of the question and its buttons.
    import_dialog: Option<ImportDialog>,

    // ------------------------------------------------------------ the trash
    //
    // The explicit row menu and drag-to-Trash can each carry several nodes, and
    // `apply` holds only a **shared** document, so neither can act where it is
    // raised. One queue and one drain keep confirmation, undo and toasts on the
    // same path.
    /// A node waiting to be moved to the Trash, and **whether the person has
    /// already been asked about it**.
    ///
    /// The flag is not bookkeeping: without it the drain would raise the same
    /// confirmation again the moment the last one was answered, because
    /// answering it is what clears the dialog. Carrying "already asked" on the
    /// request rather than inferring it from the dialog is what makes the loop
    /// terminate.
    pending_trash: Option<(NodeId, bool)>,
    /// More documents waiting their turn, from a drag that carried several.
    ///
    /// A queue rather than a set handed to one call, so **each still gets its
    /// own confirmation** — dropping four pages on the bin should ask about the
    /// one with twenty children, not silently take the lot because the first was
    /// small.
    trash_queue: std::collections::VecDeque<NodeId>,
    pending_restore: Option<Vec<NodeId>>,
    /// A trashed document somebody asked to fork rather than restore.
    pending_save_copy: Option<NodeId>,
    /// Whether the *In Trash* band is up, and where its two buttons are — both
    /// cached at draw time.
    ///
    /// `cursor_icon` has no document to ask, and the band sits over the editor,
    /// so without this the editor answers and the pointer becomes an I-beam over
    /// a strip there is nothing to type in.
    notice_showing: bool,
    notice_buttons: Vec<Rect>,
    pending_delete_forever: Option<(Vec<NodeId>, bool)>,
    pending_empty_trash: Option<bool>,
    /// What **this window** binned, so it can tell when somebody else takes one
    /// of them back out. Bounded by what this session deleted.
    trashed_here: std::collections::HashSet<NodeId>,
    /// A question that has to be answered before something is destroyed, and
    /// what it is about — the dialog itself carries only strings.
    confirm: Option<(koru_ui::Confirm, ConfirmSubject)>,
    /// The transient chip at the foot of the content, and what its action means.
    /// The toast, and what its action button does (if anything).
    ///
    /// Trash offers **Undo** with the roots to restore; export offers **Show**
    /// with a path to reveal. A plain notice (including Move to… undo) carries
    /// [`ToastPayload::None`].
    toast: Option<(koru_ui::Toast, ToastPayload)>,
    /// Reverse of a *Move to…* while its toast is up. **Every root** of a
    /// multi-move, so Undo takes the whole gesture back (not only the first).
    move_undo: Option<Vec<MoveUndo>>,
    /// The centred **Move to…** picker, when open.
    move_to: Option<move_to::MoveToSurface>,
    /// Who this window says it is, written into documents it bins so the *other*
    /// window can say who did it.
    ///
    /// **Self-declared, never inferred.** Taken from the open profile — the one
    /// place a person has already told us their name — in `set_pane_context`.
    /// `None` is a normal state and every message built from it has a form with
    /// no "by" clause; see `NodeMeta::trashed_by`.
    actor: Option<String>,
    /// `(profile name, owner display name)` from onboarding, for the Welcome
    /// greeting. The owner name is not stored anywhere else.
    welcome_owner: Option<(String, String)>,
    /// The workspace onboarding just created, until the window context names it.
    welcome_workspace_hint: Option<String>,
    /// The Welcome page's machine-wide ticks, read from and written to the Store.
    welcome_progress: welcome_progress::WelcomeProgress,
    /// Renaming a workspace in its switcher row.
    ///
    /// **The field lives here, not in the pane.** The pane was built in a chrome
    /// crate that could not depend on `koru-editor` (ADR-262 later moved it into
    /// this app), and `TextField` is the editor's — so the pane hands
    /// out the row's rectangle (`popover_workspace_rect`) and the container
    /// draws an editable box in it. That keeps the crate boundary and reuses the
    /// one text field the app has, rather than growing a second one in chrome.
    ///
    /// The `hint` is how a name that is not acceptable is refused: **inline,
    /// not a dialog**. The field is still open and the caret is still in it, so
    /// a modal would take the text away in order to complain about it.
    workspace_rename: Option<WorkspaceRename>,
    /// A resize of the explore pane in flight: the gap between the pointer and
    /// the pane's edge when it was grabbed, so the edge does not jump to the
    /// pointer on the first move.
    sidebar_drag: Option<f32>,
    /// Pointer-to-right-edge offset while the AI pane is being resized.
    ai_drag: Option<f32>,
    /// A split-pane divider being dragged: which split, and where on the
    /// hairline it was grabbed.
    divider_drag: Option<panes::DividerDrag>,
    /// The emoji-and-colour modal, when one is open.
    workspace_icon: Option<EmojiColorPanel>,
    /// Global device-linking modal, reached from the profile popover. It owns
    /// presentation only; Agent owns identity, link codes, trust and revocation.
    link_device: Option<LinkDevice>,
    /// What Agent last said about this device, its linked peers and the
    /// relay — fed from every snapshot whether or not anything shows it.
    /// The Link dialog's Linked-devices tab reads it.
    linked_devices: LinkedDevices,
    /// The transient "{n}%" indicator shown when the display zoom changes.
    zoom_label: ZoomLabel,
    /// The zoom value last frame, so a change to the `theme::zoom()` global can
    /// be detected and the indicator armed. Zoom is a shell-level transform the
    /// view otherwise never sees.
    last_zoom: f32,

    /// Which tab asked for `/image`, so the file the shell comes back with
    /// lands where it was requested rather than wherever the focus went.
    image_request: Option<usize>,
    attachment_request: Option<usize>,

    /// A window-level action the caption row asked for. Drained by the shell
    /// inside the same press, because dragging a window has to be.
    window_request: Option<WindowRequest>,
    /// What the pane's switcher asked for. At most one can be in flight: each
    /// ends in a document being swapped, a folder picker, or a new process.
    /// What this view has asked the shell for, in the order it asked.
    ///
    /// A queue rather than one slot: see [`ask_pane`](Self::ask_pane).
    pane_requests: std::collections::VecDeque<PaneRequest>,
    /// **The manager's next poll, while that tab is in front.** Dropped the
    /// moment it is not: a window looking at a document does no work for a
    /// manager nobody can see.
    workspaces_poll_due: Option<Instant>,
    /// **The preferences as this container last heard them.** Most of what a
    /// Settings page draws reads a process global, but the pieces it keeps a
    /// field of do not — a tab opened an hour into a session would otherwise
    /// show the defaults rather than what is stored. `restore` keeps this
    /// current.
    preferences: Settings,
    /// Who and where this window is, as the pane draws it. Set by the shell,
    /// which is the only thing that knows — a view holds no profile document.
    /// What the workspace holds, pushed by the shell. `None` before the first
    /// push; empty is a real answer meaning a workspace with no documents.
    entries: Option<Vec<koru_core::explore::ExploreEntry>>,
    /// Why this window is not reaching the store, pushed by the shell.
    ///
    /// `None` — the overwhelmingly common case — is what the chip's absence
    /// means. A view has no store of its own to ask.
    store_trouble: Option<koru_workspace::StoreTrouble>,
    /// The workspace's trashed documents, pushed by the shell. Kept so a Trash
    /// tab opened later can be handed the list it needs.
    trashed_documents: Option<Vec<koru_core::explore::ExploreEntry>>,
    /// How wide the branch pill wants to be, measured with the tab labels.
    ///
    /// The name is fixed at compile time, so this only moves when the font or
    /// the density does — which is exactly when the tab widths are remeasured,
    /// so it rides along in `remeasure_tab_widths` rather than earning a hook
    /// of its own. `0.0` means "not measured yet", and the pill is not
    /// reserved for that frame; every path that consults the strip calls
    /// `ensure_tab_widths` first, so no frame is ever actually drawn with it.
    branch_pill_w: f32,
    /// Whether the offline chip's popover is open.
    ///
    /// The chip says *that* something is wrong; the popover says what, and how
    /// much. Splitting them is what keeps the chip small enough to live in the
    /// caption without shouting — a count sitting there permanently reads as an
    /// alarm, and this is a state a store restart puts a window in for a second
    /// at a time.
    offline_open: bool,
    /// **What the Store has confirmed, per document, and what has already been
    /// said about it.** Pushed by the shell — and only while a screen reader is
    /// attached, so an ordinary session never fills it in. The subsystem is
    /// `mainview/accessibility/persistence.rs`; this file is a call site
    /// (§7.10).
    save_status: accessibility::persistence::Persistence,
    /// **What has become of the reminders this person submitted, and what has
    /// already been said about it.** Kept up to date by the maintenance pass
    /// that already collects the optimistic records, so unlike
    /// [`save_status`](Self::save_status) it costs a comparison over a list
    /// rather than a reading nobody asked for. The subsystem is
    /// `mainview/accessibility/reminders.rs`; this file is a call site (§7.10).
    reminder_news: accessibility::reminders::ReminderNews,
    /// Which of [`entries`](Self::entries) is open, pushed beside them.
    ///
    /// **Not derivable from the tab.** A row is keyed by `DocumentId` and a
    /// tab's root is a `NodeId` inside a document — two different uuids — so
    /// asking `document_root()` which row is current answers `None` every time.
    /// That is what left no row in Explore drawing as the one you are on.
    active_document: Option<koru_core::DocumentId>,
    /// A Store document whose rename prompt should open once the shell has
    /// loaded it, and when to stop waiting for it. Explore rows carry manifest
    /// `DocumentId`s, while the prompt needs the root node inside that
    /// document, so renaming any document other than the active one
    /// necessarily crosses this asynchronous swap.
    ///
    /// **The deadline is what keeps an unanswered gesture from firing later**,
    /// on a document somebody opened for their own reasons — the same bound,
    /// and the same reasoning, as [`DeferredTarget::until`]. It is read on the
    /// next arrival rather than on a tick: nothing here is worth waking the
    /// loop for, and an expired request that nothing ever announces is one
    /// nobody can see.
    pending_document_rename: Option<(koru_core::DocumentId, Instant)>,
    /// A Store document whose attribute picker (tags, color, due or
    /// priority) should open once the shell has loaded it — the explore
    /// rows' Attributes submenu, the same asynchronous swap and the same
    /// deadline `pending_document_rename` carries.
    pending_document_attrs: Option<(koru_core::DocumentId, AttrPickerKind, Instant)>,
    /// A task the document asked to be shown on the Tasks page, and the
    /// deadline the ask expires at.
    ///
    /// **Latched, because the page answers on its own schedule.** Selecting a
    /// row the store has not sent yet writes a selection `catch_up` drops on
    /// the next answer — it drops one whose row has left the page, and a row
    /// that has not arrived yet looks exactly the same. So the ask is held and
    /// re-applied until the row shows up or the deadline passes.
    pending_task_reveal: Option<(koru_core::DocumentId, NodeId, std::time::Instant)>,
    /// **A task open waiting to be checked against the resolver**, because the
    /// document that answers is not open yet when the row is pressed.
    ///
    /// SPEC §3 forbids the fallback: a `task:` reference that no longer names a
    /// task must not quietly open the paragraph underneath it. The check has to
    /// be made where the document is in hand, so the gesture is latched here and
    /// spent by [`MainView::refuse_stale_task_open`] at the landing — see
    /// `mainview/tasks.rs`. Same deadline as the pickers above, and for the same
    /// reason.
    pending_task_open: Option<(koru_core::DocumentId, NodeId, Instant)>,
    /// Task changes the Tasks tab asked for, waiting for the shell — which is
    /// the side that holds a `&mut Workspace` and can park the document the
    /// task lives in. See `mainview/tasks.rs`.
    task_commands: Vec<koru_workspace_host::tasks::TaskCommand>,
    /// What each queued task command will do to the rail's numbers, computed
    /// from the row when the request was made and applied when the receipt
    /// says the write landed — in the order the commands are served.
    pending_count_deltas: Vec<crate::tasks_view::echo::CountDelta>,
    /// The workspace tag table as last pushed by the shell, kept so a tab
    /// opened later can still be given it.
    tag_table: Vec<koru_core::system::TagEntry>,
    /// A draft has asked the shell for a real document and is waiting for it.
    ///
    /// Creating one is a round trip now, so promotion cannot finish inside the
    /// keystroke that triggers it. Without this the draft would ask again on
    /// every character and make one document per keypress.
    promoting: bool,
    /// A promoted draft's text, waiting for a `&mut WorkspaceDoc` to write it
    /// through. `show_active_document` has only `&`, and the characters typed
    /// during the round trip must not be lost between the two.
    ///
    /// The last field is **where the caret was**, as `(row, byte offset)`. It
    /// used to be a bare `in_title: bool`, which lost the offset in the title
    /// and the caret entirely everywhere else — see `Leaf::caret_spot`.
    pending_draft_text: Option<(NodeId, String, Vec<String>, Option<(usize, usize)>)>,
    pane_context: Option<(
        Vec<ProfileSummary>,
        ProfileId,
        Vec<WorkspaceEntry>,
        Option<WorkspaceId>,
    )>,
    /// Last Store-backed active Workspace count for every visible Profile.
    /// Replayed when Manage opens after the host published the fact.
    profile_workspace_counts: std::collections::HashMap<ProfileId, usize>,
    /// The signed-in generation for which this window requested exact local
    /// lifecycle reconciliation. This is independent of whether Manage
    /// Profiles happens to be open.
    profile_lifecycle_reconciled_generation: Option<u64>,
    account_scope: u64,
    account_state: koru_agent_wire::AccountState,
    hidden_profile_count: usize,
    retained_profiles: Vec<ProfileSummary>,
    /// A document tab the container asked to **pop out** into its own window.
    /// Drained by the shell, which is the only thing that can make a window.
    wants_popout: Option<NodeId>,
    /// Whether the window fills the screen, pushed by the shell. The caption's
    /// maximise button becomes a restore button when it does.
    maximized: bool,
    /// Whether this window is kept above the others, pushed by the shell. The
    /// caption's pin button shows it pressed (in the accent colour) when it is.
    pinned: bool,
    /// Current pointer modifiers. The shell's mouse-down vocabulary carries
    /// Ctrl directly; Shift arrives through `View::set_mods`.
    mods: ModifiersState,
    /// The submenu hanging off a row of [`menu`](Self::menu), and which row.
    ///
    /// A second `ContextMenu` rather than nesting one inside the other: it is
    /// the same thing at the same level of the z-order, positioned differently,
    /// and every rule the parent follows — clamped to the viewport, rows fixed
    /// at open, hover tracked per frame — is one it should follow too. Nesting
    /// would have meant teaching `panel`, `item_at` and `step` about depth.
    submenu: Option<(usize, ContextMenu)>,
    /// The flyout hanging off a row of [`submenu`](Self::submenu) — *Active ▸*
    /// under Shell or Agents. A third panel at the same z-order, for the same
    /// reason the first flyout is not nested inside the widget.
    nested: Option<(usize, ContextMenu)>,
    /// Whether the pane toggle's tooltip is muted until the pointer leaves it.
    ///
    /// **Set by pressing the toggle.** The hint explains what the button will
    /// do; once you have done it, leaving the hint sitting under the pointer
    /// re-explains a thing you have just watched happen, and its label flips to
    /// the opposite verb while you are still looking at it. Cleared the moment
    /// the pointer moves off the button, so pointing at it again asks again.
    toggle_tip_muted: bool,
    /// Shared timing for the adjacent icon controls in the caption.
    tooltip: Tooltip,
    /// Recently opened documents, most recent first.
    ///
    /// **Stable document ids, not workspace nodes.** The store catalogues a
    /// document under one id across every workspace, so a recent survives a
    /// workspace switch and is resolved against the live listing rather than
    /// against whichever document happens to be under the caret.
    recents: Vec<koru_core::DocumentId>,
    /// The same list with titles — `(document, title)`, most recent first —
    /// recorded at `land_on`, the one moment the root node and the store id
    /// are both in hand. It is what the move picker's empty query lists: the
    /// search service only remembers search-driven navigations, so its
    /// empty-query answer is not what a person means by "recent".
    recent_documents: Vec<(koru_core::DocumentId, String)>,
    /// Where you have been, oldest first, and where in it you are standing.
    ///
    /// **Not `recents`.** That list de-duplicates, is document-granular and is
    /// written to `Settings`; this one is ordered, keeps duplicates, truncates
    /// its forward branch on a new jump, and is **not persisted** — the same
    /// choice VS Code makes, and the reason is that a back button restoring a
    /// place from a week ago is not what "back" means to anyone.
    ///
    /// On `MainView` because its lifetime is the window's: tab switches, closes
    /// and reorders all happen inside it, which is what `recents` already
    /// relies on.
    history: Vec<Spot>,
    /// Where in [`history`](Self::history) we are standing — the index of the
    /// spot currently showing, once there is one.
    history_at: usize,
    /// Set while a back/forward step is being performed, so restoring a spot
    /// does not record a jump of its own and walk the cursor forward again.
    restoring: bool,
    /// A Store-backed history destination waiting for the shell to swap the
    /// active document. Another document cannot be resolved in the currently
    /// loaded `WorkspaceDoc`, so its row and scroll are restored on the
    /// subsequent `show_active_document` push.
    history_pending: Option<Spot>,
    /// The pending history target was requested through `Navigate`.
    /// Workspace switching announces the active document before that original
    /// routed request resumes; keep history suppression through the second
    /// phase so it cannot truncate the forward branch as a new jump.
    history_pending_routed: bool,
    /// A pin-safe tab click waiting for the shell to swap the active document.
    /// Separate from [`history_pending`]: Back and a pin click can be in flight
    /// at once, and one field serving both waiters is how a restore lands on
    /// the wrong document.
    tab_pending: Option<Spot>,
    /// Set by the welcome page's *Apply to all* button. The next `capture`
    /// clears every per-document view-mode override for this workspace, so
    /// documents not currently open follow the global default too. `Cell`
    /// because `capture` is `&self`.
    clear_doc_view_prefs: std::cell::Cell<bool>,
    /// The agent's last word on updating, and the way to answer it. Cheap to
    /// clone, absent in every test, and never `Option` — a window with no agent
    /// holds a feed that reports nothing rather than a `None` every call site
    /// has to unwrap.
    updates: crate::agent::UpdateFeed,
    /// Shared control-plane connection used by the device-linking modal.
    agent: crate::agent::AgentLink,
    reminder_preferences: crate::reminder_settings::Controller,
    update_was_offered: bool,
    /// The header's explicit update-management surface. Distinct from the
    /// agent's consent modal, which appears only when a channel mismatch needs
    /// acknowledgement.
    updates_modal: bool,
    /// A press began outside the updates modal's panel. The dismissal happens
    /// on the matching release — closing on the press let the mouse-up land in
    /// the document underneath (ADR-150), and swallowing the up with no memory
    /// of where the down was would make an inside press-and-slide dismiss too.
    updates_modal_outside_press: bool,
    /// A consent has left this window and is waiting for the agent's next
    /// report. The transport is asynchronous, so this prevents a second click
    /// from enqueueing another install before `Installing` reaches us.
    ///
    /// Stamped so it can expire: an agent that died between the click and its
    /// reply never sends the stage change that clears this, and a banner deaf
    /// forever is worse than a rare double-enqueue against a fresh agent
    /// (`UPDATE_ACTION_TIMEOUT`). The proper fix is a liveness signal on the
    /// feed - clear when the connection drops or `instance_id` changes.
    update_action_pending: Option<(koru_agent_wire::UpdateStage, std::time::Instant)>,
    /// A consent whose fate the person deserves to hear: it timed out
    /// unanswered, or the link was already gone when they clicked. Shown as a
    /// banner line while the report is still on that stage — a silently
    /// re-armed button reads as a reset, which is worse than the truth.
    update_action_stalled: Option<(koru_agent_wire::UpdateStage, &'static str)>,
    /// The same domain surface used by the dedicated search window. `Some`
    /// here makes it a modal overlay over the container.
    search: Option<crate::search::SearchSurface>,
    /// **Where a live dictation is going, decided once** — see
    /// [`routing::LiveDictation`] and `MainView::dictate`.
    dictation: Option<routing::LiveDictation>,
    /// **The listening surface, alive exactly while a session is.**
    ///
    /// Separate from `dictation` above, which is a *routing* decision this
    /// container makes at `Begin`; this is what the shell pushes back about
    /// the session's own state, and it is `None` for the whole of the time
    /// nothing is being said. See [`dictation`] for why that `None` is the
    /// lifetime rule rather than a flag anybody has to remember to clear.
    listening: Option<dictation::Listening>,
    /// Store-backed title search for the inline `@` picker. Presentation and
    /// anchoring stay in the editor; this owns the non-blocking service seam.
    entity_search: koru_search_ui::EntitySearch,
    entity_workspaces: Vec<koru_editor::components::entity_picker::WorkspaceChoice>,
    /// Manifest/search title cache keyed by stable document id.
    entity_titles: std::collections::HashMap<String, String>,
    /// Documents known to be in Trash. Metadata is retained across workspace
    /// switches so returning to a backlink can immediately show it.
    entity_trashed: std::collections::HashSet<String>,
    /// Heading text by `"{document}#{node}"` — the other half of a deep-link
    /// caption. The title half lives in `entity_titles` under the bare document
    /// key, and the two are recomposed rather than stored joined, so a rename
    /// of either shows up without the other being re-fetched.
    entity_headings: std::collections::HashMap<String, String>,
    /// Documents an open tab deep-links into. One outline per document, however
    /// many pills point into it.
    deep_link_documents: std::collections::HashSet<koru_core::DocumentId>,
    /// Documents whose manifest kind is `"note"` — a file's attached page —
    /// as of the last `set_entries`. The tab strip reads it to wear the file
    /// glyph for a note tab; kept as a set so the draw path asks one lookup
    /// rather than scanning the listing.
    note_documents: std::collections::HashSet<koru_core::DocumentId>,
    /// When the deep-link captions were last re-asked for.
    deep_links_polled: Option<Instant>,
    /// A search hidden by successful navigation. Reopening Ctrl+P restores the
    /// exact query instead of constructing an empty surface; explicit close or
    /// outside-click still discards it.
    retained_search: Option<crate::search::SearchSurface>,
    /// A dedicated search window closes when its surface closes; an in-app
    /// overlay merely reveals the container underneath.
    search_only: bool,
    /// The singleton support window: the chat and only the chat, never a
    /// tab (ADR-276). Drawn and routed instead of the tab strip.
    support: Option<crate::support::SupportView>,
    support_only: bool,
    /// The floating support icon (D17), a container in icon-only mode.
    fab: Option<crate::support::FabView>,
    fab_only: bool,
    /// What this pane wants done to itself off the input path; the host
    /// drains it in both loop paths.
    background_intents:
        std::collections::VecDeque<koru_workspace_host::native_surface::BackgroundIntent>,
    /// A dedicated Feedback window the tray opened.
    feedback_only: bool,
    /// Whether this window offers search at all.
    ///
    /// **`--no-search`, carried rather than looked up.** It used to be a
    /// process-wide flag on the index host; the index is gone and this is what
    /// is left of it, so it travels as a value from `main` instead of becoming
    /// a second global.
    search_enabled: bool,
    /// A separate Loro subscriber feeds the derived local index. It batches
    /// only actual CRDT changes, so navigation and pointer handling do no index
    /// work, and a sustained edit is still published within 30 seconds.
    /// Pointer over the OK button on the "installed build is newer" modal.
    update_newer_ok_hot: bool,
    /// The task this window's caret was last in — the caret half of the
    /// content-burst boundary ([ADR-286]).
    ///
    /// On the container rather than on a tab because a *window* has one caret:
    /// switching tabs is itself the caret leaving, and a tab that is not in
    /// front has no caret to leave anything.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    task_caret: crate::tasks::TaskCaret,

    // ---------------------------------------------------------- run tabs
    /// The workspace's delegated runs, listed for the Agents navigator on a
    /// thread of its own. A window that never opens the Agents mode still
    /// holds it; it asks nothing until a workspace is named.
    run_index: koru_runs_ui::RunIndex,
    /// The workspace's repeating delegations. Its worker blocks on the typed
    /// schedule watch; no timer is attached to this field.
    schedule_index: crate::schedule_index::ScheduleIndex,
    /// **One device-local value, kept current** (ADR-365): where Koru makes a
    /// folder for a delegated run that belongs to no project.
    ///
    /// A link and not a read, because the delegation modal has to *show* that
    /// path and opening the modal may touch no disk. The worker behind this
    /// holds the whole Global section; this window reads one field of it and
    /// writes one field of it, which is what a per-field patch is for.
    delegate_folder: koru_api_client::GlobalLink,

    // ------------------------------------------------------- session tabs
    /// This window's connection to the session backend, made on first session
    /// use — a window that never opens one pays nothing. See
    /// `mainview/session_backend.rs`.
    session_backend: Option<session_backend::SessionBackend>,
    /// The session tab whose controller lease is in flight; the host's
    /// `Attached` answer names a session onto it. The tab, not "the active
    /// one" — the person may have switched away before the answer lands.
    session_attaching: Option<u64>,
    /// The session tab holding the window's one controller lease.
    session_attached: Option<u64>,
    /// The tab the last settle drove the controller for — edge-triggering, so
    /// a failed attach presents once rather than retrying every pass.
    session_settled: Option<u64>,
    /// A session a *Terminate* is waiting to confirm once this window holds its
    /// controller. Terminate acts on the controller the backend holds, so a
    /// session this window is not attached to is attached first (open/focus its
    /// tab) and the confirm fires when the attach lands — Koru Terminal's
    /// `terminate_after_attach`, per window (Q-E7 P8).
    terminate_after_attach: Option<koru_session_protocol::SessionId>,
    /// A session the container asked to move to a new window (Q-E7 P8's pop-
    /// out). Drained by the shell, the only thing that can open a window; it
    /// duplicates a container and seats the session there.
    wants_session_popout: Option<koru_session_protocol::SessionId>,
    /// Which `+` menu profile rows this machine can launch — the off-thread
    /// presence probe's cached answer, refreshed each time the menu opens.
    /// See `mainview/profile_probe.rs`.
    profile_probe: profile_probe::ProfileProbe,
    provider_readiness: Option<Vec<koru_session_protocol::ProviderReadiness>>,
    /// **The one delegation surface** (ROADMAP P1.14). Four gestures reach it
    /// and `MainView::begin_delegation` is the only thing that opens it.
    delegation: Option<delegate::Delegation>,
    /// **The session recorder's setup modal** (SESSIONRECORD, internal Windows
    /// builds only). The caption-menu row and F9 are gated before this is
    /// reached, so on a public build the field is `None` forever.
    recording_modal: Option<recording::modal::Modal>,
    /// **The recorder's lifecycle**, once *Record* has settled — countdown,
    /// clean present, start, recording, stop. `None` is Idle. The modal and a
    /// lifecycle never stand at once. See `mainview::recording::Lifecycle`.
    recording_lifecycle: Option<recording::Lifecycle>,
    /// The validated request a *Record* press settled, driving the countdown
    /// and the `StartRecording` that follows it. Held so the size lock and
    /// the capture cannot disagree about what was asked for.
    recording_request: Option<recording::modal::Request>,
    /// A stop asked for while the shell's start worker still held the answer:
    /// it lands the moment `RecordingEvent::Started` does.
    recording_stop_when_started: bool,
    /// A close the view vetoed mid-lifecycle. Re-asked when the lifecycle
    /// ends — the `Stopped`/`Failed` events, or the abort's next maintain.
    recording_close_after_stop: bool,
    /// The recorder's eyes while a capture is live: observed actions and the
    /// 500 ms mouse heartbeat, stamped from the encoder's first-frame origin.
    /// Created at `Started`, drained by the shell's finalize worker.
    recording_telemetry: Option<recording::telemetry::Recorder>,
    /// What the menu was on the last maintain pass. The rising edge is the
    /// recorder's menu-open observation — one detector covers every kind of
    /// menu and every gesture that opens one, so a fifteenth `menu = Some`
    /// site can never forget to observe.
    recording_menu_was_open: bool,
    /// The clean frame the start waits for has been drawn. Set in the draw
    /// pass, read in the next maintain — the two halves of the handshake.
    clean_frame_drawn: bool,
    /// The pin-and-create in flight, on its own thread. One at a time. It
    /// outlives a closed modal and is never replaced before maintenance
    /// drains its answer; opening another delegation is gated until then.
    delegate_job: Option<delegate::job::Job>,
    /// **Crumbs owed to documents** (D7, D8): a pill per accepted delegation,
    /// waiting for the shell's sweep. A `Vec` because two delegations can be
    /// accepted between two turns, and a queue rather than a write here
    /// because the document the gesture named is not necessarily the one this
    /// view is holding — `koru_workspace_host::runpill` says why.
    pending_run_pills: Vec<koru_workspace_host::runpill::RunPill>,
    /// Schedule crumbs owed to their document-origin rows, through the same
    /// workspace edit seam as Run crumbs.
    pending_schedule_pills: Vec<koru_workspace_host::schedulepill::SchedulePill>,
    /// **What a run pill says**, keyed by the Run document's storage id: the
    /// status caption resolved off the window thread, and the run's own header
    /// so a redraw needs no second request. See `delegate::pills`.
    run_pill_states: std::collections::HashMap<String, delegate::pills::RunPillState>,
    /// The worker that resolves them. Idle until a run pill is on screen.
    run_pill_worker: delegate::pills::RunPills,
    /// Which runs the open leaves are drawing pills for, keyed the same way.
    /// Recomputed each maintain pass — a leaf that closed asks for nothing.
    run_pills_wanted: std::collections::HashMap<String, koru_store_core::ids::DocumentId>,
    /// **The runs a canvas's agent nodes start**, keyed by `(canvas, node)`:
    /// many at once, apart from the modal's one job. See `canvas_runs`.
    canvas_runs: canvas_runs::CanvasRuns,
    /// **The chains a canvas's start nodes run**, and the agent nodes run on
    /// their own: what each step is doing, derived from the runs above and
    /// from what the canvas's nodes record. See `canvas_chain`.
    canvas_chains: canvas_chain::CanvasChains,
    /// **What this window calls the devices it knows**, and which one it is.
    ///
    /// D16 names the executing device on a device that is not running the
    /// work, and a name is a display-time lookup — the run's header carries an
    /// id, because a stable identifier never derives from a name. Kept here
    /// rather than read from the linked-devices dialog, which is usually shut.
    device_names: std::collections::HashMap<koru_device::DeviceId, String>,
    this_device: Option<koru_device::DeviceId>,
    /// Agent connection generations on which the device-list bootstrap was
    /// requested and acknowledged. Schedule rows and run pills share this
    /// state: one ask per live generation, retried only after reconnect, with
    /// no idle timer.
    device_names_requested_generation: Option<u64>,
    device_names_acknowledged_generation: Option<u64>,
    /// The gateway client the job clones, cached during drawing exactly as
    /// the tasks rail and the history panel cache theirs. **No request is
    /// made where it is set**, and a window without one delegates nothing and
    /// says so.
    delegate_client: Option<koru_api_client::Client>,
    /// Which window this container was seated in, told once at pane
    /// creation by `View::seat_accessibility`. Every occurrence this view
    /// publishes is scoped by it, and a view does not choose it.
    accessibility_window: koru_workspace_host::accessibility::WindowSlot,
    /// **What this window has published about the open document's body**, and
    /// to whom.
    ///
    /// A `RefCell` because `View::semantics` is `&self` — deliberately, since
    /// that is what makes `Editing::reflow` and the Store unreachable from a
    /// semantic query — while minting an AccessKit id for a text run is
    /// mutation. Plan 1b.2's answer was a cell beside the view rather than a
    /// widened seam; see `accessibility::document`.
    bodies: accessibility::document::State,
    /// **What this window's chrome published last time it went out**, so the
    /// next one can name what has left the window.
    ///
    /// A second record beside `bodies` because the two halves are published
    /// differently: the body is a difference and knows row by row what it
    /// removed; the chrome is re-sent whole and can only know by comparing one
    /// publish with the last. See `accessibility::Surfaces` — without it the
    /// host's occurrence map grows for the life of a screen-reader session and
    /// goes on routing requests to menus that closed.
    surfaces: accessibility::SurfaceState,
}

impl MainView {
    fn focused_group(&self) -> &panes::TabGroup {
        self.panes.focused()
    }
    fn focused_group_mut(&mut self) -> &mut panes::TabGroup {
        self.panes.focused_mut()
    }
    fn group_at(&self, path: &panes::PanePath) -> &panes::TabGroup {
        self.panes.group(path).unwrap()
    }

    fn tabs(&self) -> &TabStrip<Tab> {
        self.focused_group().tabs()
    }
    fn tabs_mut(&mut self) -> &mut TabStrip<Tab> {
        self.focused_group_mut().tabs_mut()
    }
    fn tab_scroll(&self) -> f32 {
        self.focused_group().scroll
    }
    fn set_tab_scroll(&mut self, v: f32) {
        self.focused_group_mut().scroll = v;
    }
    fn tab_widths(&self) -> &Vec<f32> {
        &self.focused_group().widths
    }
    fn tab_widths_mut(&mut self) -> &mut Vec<f32> {
        &mut self.focused_group_mut().widths
    }
    fn tab_widths_stale(&self) -> bool {
        self.focused_group().widths_stale
    }
    fn set_tab_widths_stale(&mut self, v: bool) {
        self.focused_group_mut().widths_stale = v;
    }
    fn tab_scroll_drag(&self) -> Option<f32> {
        self.focused_group().scroll_drag
    }
    fn set_tab_scroll_drag(&mut self, v: Option<f32>) {
        self.focused_group_mut().scroll_drag = v;
    }
    fn slides(&self) -> &TabSlides {
        &self.focused_group().slides
    }
    fn slides_mut(&mut self) -> &mut TabSlides {
        &mut self.focused_group_mut().slides
    }
    fn group_drag(&self) -> &Option<panes::TabDrag> {
        &self.focused_group().drag
    }
    fn group_drag_mut(&mut self) -> &mut Option<panes::TabDrag> {
        &mut self.focused_group_mut().drag
    }

    /// Fold a bool-returning visit across every tab of every pane group. The
    /// bools OR together: any tab reporting a change is a change. This is the
    /// paned spelling of what a single strip used to do with
    /// `tabs.iter_mut().fold(false, ..)`.
    fn each_tab_mut(&mut self, mut visit: impl FnMut(&mut Tab) -> bool) -> bool {
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                changed |= visit(tab);
            }
        }
        changed
    }

    fn focused_view_rect(&self) -> Rect {
        let content = self.content();
        let path = self.panes.focused_path().clone();
        let rect = self
            .panes
            .leaf_rects(content)
            .into_iter()
            .find(|(p, _)| p == &path)
            .map(|(_, r)| r)
            .unwrap_or(content);
        panes::PaneTree::view_rect_in(content, rect)
    }

    fn update_split_intent(&mut self, x: f32, y: f32) -> bool {
        let old = self.split_intent.clone();
        if !self.content().contains(x, y) {
            self.split_intent = None;
        } else if let Some(target) = self.pane_at(x, y) {
            let view_rect = self
                .panes
                .view_rect_for(self.content(), &target)
                .unwrap_or_else(|| self.content());
            self.split_intent =
                panes::resolve_split_intent(view_rect, (x, y), &target, self.split_intent.as_ref());
        } else {
            self.split_intent = None;
        }
        self.split_intent != old
    }

    /// The slot marker's side of a cross-pane drag: while the pointer is over
    /// a *target* group's strip, the flat index a drop would land at. Over
    /// content the split preview speaks instead; over the source strip the
    /// drop is a no-op, so no slot. The lane is the share the window chrome
    /// left the pane, so a slot can never be offered under a window button.
    fn update_drop_slot(&mut self, x: f32, y: f32) {
        self.drop_slot = None;
        let Some(drag) = self.pane_drag.as_ref() else {
            return;
        };
        let source = drag.source.clone();
        for (path, _) in self.panes.leaf_rects(self.content()) {
            if path == source {
                continue;
            }
            let Some(header) = self.lane_rect_for(&path) else {
                continue;
            };
            if !header.contains(x, y) {
                continue;
            }
            if let Some(slot) = self.panes.group(&path).and_then(|g| g.drop_slot(header, x)) {
                self.drop_slot = Some((path, slot.0));
            }
            break;
        }
    }

    fn commit_pane_drag(&mut self, _ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        let Some(drag) = self.pane_drag.take() else {
            return false;
        };
        self.split_intent = None;
        let drop_slot = self.drop_slot.take();

        // A release over a target strip lands in the slot the marker showed.
        // First, not after the pop-out check: a top-row lane is in the title
        // bar, which is above the content rect, and a strip drop is not a
        // tear-off.
        if let Some((target, index)) = drop_slot {
            if let Some(tab) = self.take_tab_from_group(&drag.source, drag.tab_id) {
                let doc = tab.document;
                self.panes.insert_tab_at(&target, tab, index);
                self.settle_pane_drop(doc);
            }
            return true;
        }

        if !self.content().contains(x, y) {
            // Only a document leaves the window. A shell or agent tab released
            // outside simply goes home: it was never taken out of its group,
            // so there is nothing to undo.
            if !self.pane_drag_poppable(&drag) {
                return true;
            }
            if let Some(tab) = self.take_tab_from_group(&drag.source, drag.tab_id) {
                let doc = tab.document;
                if let Some(node) = tab.view.root() {
                    self.wants_popout = Some(node);
                }
                self.settle_pane_drop(doc);
            }
            return true;
        }

        let Some(target) = self.pane_at(x, y) else {
            return true;
        };

        // Split if the pointer is in an edge zone of the target pane.
        if let Some(view_rect) = self.panes.view_rect_for(self.content(), &target) {
            if let Some(intent) = panes::resolve_split_intent(view_rect, (x, y), &target, None) {
                if let Some(tab) = self.take_tab_from_group(&drag.source, drag.tab_id) {
                    let doc = tab.document;
                    self.panes.split(&target, intent.dir, tab);
                    self.settle_pane_drop(doc);
                }
                return true;
            }
        }

        // Dead centre (or over a header) means a normal drop into the target group.
        if target != drag.source {
            if let Some(tab) = self.take_tab_from_group(&drag.source, drag.tab_id) {
                let doc = tab.document;
                self.panes.insert_into_group(&target, tab);
                self.settle_pane_drop(doc);
            }
        }
        true
    }

    /// Lift a tab out of its group. **Nothing else** — no collapse, no
    /// document close: between the take and the landing the tree is meant to
    /// stand exactly as it did, because every path the landing is aimed at was
    /// computed against it, and the document is about to be shown again.
    /// [`settle_pane_drop`](Self::settle_pane_drop) is the tail that runs once
    /// the tab has landed.
    fn take_tab_from_group(&mut self, source: &panes::PanePath, tab_id: u64) -> Option<Tab> {
        let index = self
            .panes
            .group(source)?
            .iter_tabs()
            .position(|t| t.id == tab_id)?;
        self.panes.group_mut(source)?.take_tab(index)
    }

    /// The one tail every drop path runs, **after** the tab has landed:
    /// emptied groups collapse (only now is it safe — the landing's paths were
    /// computed against the pre-collapse tree), and a document no longer shown
    /// anywhere is let go — a check made after the landing precisely so a
    /// moved document is still shown. A pop-out passes its document here
    /// deliberately: this window no longer shows it, and the close is what the
    /// pop-out means. The collapse owns the focus hand-off.
    fn settle_pane_drop(&mut self, doc: Option<koru_core::DocumentId>) {
        self.panes.collapse_empty_groups();
        if let Some(doc) = doc {
            if !self
                .panes
                .iter_groups()
                .any(|(_, g)| g.iter_tabs().any(|t| t.document == Some(doc)))
            {
                self.ask_pane(PaneRequest::CloseDocument(doc));
            }
        }
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.slides_mut().clear();
    }

    /// Feed the first-reminder prerequisite from the existing authoritative
    /// reminder reconciliation. Piece 7 deliberately owns no poller.
    pub(crate) fn observe_reminders(&mut self, any: bool) -> bool {
        self.reminder_preferences.observe_reminders(any)
    }

    pub(crate) fn apply_reminder_settings_intent(
        &mut self,
        intent: crate::reminder_settings::Intent,
    ) {
        self.reminder_preferences
            .apply(intent, crate::format::lang());
    }

    /// Push the controller's content-free presentation into every open
    /// Settings tab. This is a state copy only; views never poll the Agent.
    fn sync_reminder_settings_views(&mut self) -> bool {
        let model = &self.reminder_preferences.model;
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Settings(view) = &mut tab.view {
                    changed |= view.set_reminder_preferences(model);
                }
            }
        }
        changed
    }

    /// A container showing one welcome tab, which is what a window opens on.
    ///
    /// There is no permanent tab. The outline used to be one — it was the only
    /// way back to the `outline/` realm — but it is becoming a `TaskGroup` node
    /// inside a document rather than a view of its own, so nothing is left that
    /// only a fixed tab could reach. What a fresh profile lands on instead is
    /// the seeded Get started document, opened from the pane.
    #[allow(dead_code)] // test harnesses build containers without an agent
    pub fn new() -> Self {
        Self::new_with(crate::agent::UpdateFeed::absent(), true)
    }

    pub fn new_with(updates: crate::agent::UpdateFeed, search_enabled: bool) -> Self {
        Self::new_with_agent(updates, crate::agent::AgentLink::absent(), search_enabled)
    }

    pub fn new_with_agent(
        updates: crate::agent::UpdateFeed,
        agent: crate::agent::AgentLink,
        search_enabled: bool,
    ) -> Self {
        Self {
            panes: panes::PaneTree::new(Tab::welcome()),
            viewport: (900.0, 560.0),
            regions: regions::solve(900.0, 560.0, 0.0, 0.0),
            engine: TextEngine::new(),
            pinned_show_title: false,
            split_intent: None,
            pane_drag: None,
            drop_slot: None,
            foreign_targeted: false,
            // Shut by default: an empty tree beside an empty workspace is worse
            // than no tree, and the toggle is one keystroke away.
            sidebar: Panel::new(theme::SIDEBAR_W, false),
            explore: ExploreView::new(),
            explore_stale: true,
            naming: None,
            recents_stale: true,
            context_stale: true,
            context: Panel::new(theme::CONTEXT_W, false),
            context_view: ContextView::new(),
            ai_panel: Panel::new(480.0, false),
            ai_chat_folder_ask: None,
            ai_chat: ChatSurface::new_at(
                SurfaceMode::ContextPane,
                koru_workspace_host::cli::root_from_args(),
            ),
            ai_focused: false,
            document_history: {
                // One resolved language for everything the app dates. Stored
                // both here and as `format`'s process global, set before any
                // draw can ask.
                let lang = koru_i18n::Lang::for_system(
                    koru_window::system_locale::system_locale().as_deref(),
                );
                crate::format::set_lang(lang);
                let mut panel = history::HistoryPanel::default();
                panel.set_lang(lang);
                panel
            },
            cloud_sync: {
                // The language is already resolved and stored by the block
                // above; read it back rather than resolving the locale twice.
                let mut panel = cloud_sync::CloudSyncPanel::default();
                panel.set_lang(crate::format::lang());
                panel
            },
            cloud_images_due: true,
            cloud_images: (0, Vec::new()),
            task_counts: taskcounts::TaskCountsPanel::default(),
            tasks_day: crate::tasks_view::query::DayWatch::default(),
            context_tasks_stale: true,
            context_tasks_doc: None,
            new_list: None,
            wants_create_list: None,
            list_edits: Vec::new(),
            list_table: Vec::new(),
            reminders: reminders::Controller::default(),
            reminder_creation: reminder_creation::Surface::default(),
            reminder_environment: reminder_environment(),
            reminder_shell: {
                let mut shell = reminder_shell::Shell::default();
                shell.set_edit_available(true);
                shell.set_recreate_available(true);
                shell
            },
            pending_reminder_submissions: std::collections::VecDeque::new(),
            pending_reminder_cancels: std::collections::VecDeque::new(),
            pointer: (0.0, 0.0),
            pointer_in_content: false,
            hover: None,
            menu: None,
            new_document: None,
            new_workspace_owner: None,
            snapshot_prompt: None,
            deferred_search_target: None,
            deferred_navigation: None,
            pending_explore_reveal: None,
            pending_lands: std::collections::VecDeque::new(),
            pending_pane_drops: Vec::new(),
            pending_landings: std::collections::VecDeque::new(),
            error: None,
            import_dialog: None,
            pending_trash: None,
            trash_queue: std::collections::VecDeque::new(),
            pending_restore: None,
            pending_save_copy: None,
            notice_showing: false,
            notice_buttons: Vec::new(),
            pending_delete_forever: None,
            pending_empty_trash: None,
            trashed_here: std::collections::HashSet::new(),
            confirm: None,
            toast: None,
            move_undo: None,
            move_to: None,
            actor: None,
            welcome_owner: None,
            welcome_workspace_hint: None,
            welcome_progress: welcome_progress::WelcomeProgress::default(),
            workspace_rename: None,
            sidebar_drag: None,
            ai_drag: None,
            divider_drag: None,
            workspace_icon: None,
            link_device: None,
            linked_devices: LinkedDevices::default(),
            zoom_label: ZoomLabel::new(),
            last_zoom: theme::zoom(),
            image_request: None,
            attachment_request: None,
            window_request: None,
            pane_requests: std::collections::VecDeque::new(),
            workspaces_poll_due: None,
            preferences: Settings::default(),
            entries: None,
            store_trouble: None,
            trashed_documents: None,
            branch_pill_w: 0.0,
            offline_open: false,
            save_status: accessibility::persistence::Persistence::default(),
            reminder_news: accessibility::reminders::ReminderNews::default(),
            active_document: None,
            pending_document_rename: None,
            pending_document_attrs: None,
            pending_task_reveal: None,
            pending_task_open: None,
            task_commands: Vec::new(),
            pending_count_deltas: Vec::new(),
            tag_table: Vec::new(),
            promoting: false,
            pending_draft_text: None,
            pane_context: None,
            profile_workspace_counts: std::collections::HashMap::new(),
            profile_lifecycle_reconciled_generation: None,
            account_scope: 0,
            account_state: Default::default(),
            hidden_profile_count: 0,
            retained_profiles: Vec::new(),
            wants_popout: None,
            maximized: false,
            pinned: false,
            mods: ModifiersState::empty(),
            submenu: None,
            nested: None,
            toggle_tip_muted: false,
            tooltip: Tooltip::default(),
            recents: Vec::new(),
            recent_documents: Vec::new(),
            history: Vec::new(),
            history_at: 0,
            restoring: false,
            history_pending: None,
            history_pending_routed: false,
            tab_pending: None,
            clear_doc_view_prefs: std::cell::Cell::new(false),
            updates,
            agent,
            reminder_preferences: crate::reminder_settings::Controller::default(),
            update_was_offered: false,
            updates_modal: false,
            updates_modal_outside_press: false,
            update_action_pending: None,
            update_action_stalled: None,
            search: None,
            dictation: None,
            listening: None,
            // The window's own store, like the global-search overlay — never
            // the channel default, or a rooted window's mention picker would
            // offer another profile's workspaces and documents.
            entity_search: koru_search_ui::EntitySearch::new_at(crate::search::gateway_root()),
            entity_workspaces: Vec::new(),
            entity_titles: std::collections::HashMap::new(),
            entity_trashed: std::collections::HashSet::new(),
            entity_headings: std::collections::HashMap::new(),
            deep_link_documents: std::collections::HashSet::new(),
            note_documents: std::collections::HashSet::new(),
            deep_links_polled: None,
            retained_search: None,
            search_only: false,
            support: None,
            support_only: false,
            fab: None,
            fab_only: false,
            background_intents: std::collections::VecDeque::new(),
            feedback_only: false,
            search_enabled,
            update_newer_ok_hot: false,
            task_caret: crate::tasks::TaskCaret::default(),
            // **Under test it dials nothing.** A dependency is never
            // compiled with `cfg(test)`, so an inert index here is what
            // keeps this binary's suite from listing runs off whatever
            // gateway the machine has running.
            run_index: match cfg!(test) {
                true => koru_runs_ui::RunIndex::inert(),
                false => koru_runs_ui::RunIndex::open(
                    koru_channel::segment(),
                    koru_workspace_host::cli::root_from_args(),
                ),
            },
            schedule_index: match cfg!(test) {
                true => crate::schedule_index::ScheduleIndex::inert(),
                false => crate::schedule_index::ScheduleIndex::open(),
            },
            // Disabled under test for the same reason the run index is inert:
            // a suite must not read or write the settings of whatever stack
            // the machine happens to be running. A disabled link answers the
            // registry's defaults forever, which is what a first run reads.
            delegate_folder: match cfg!(test) {
                true => koru_api_client::GlobalLink::disabled(),
                false => koru_api_client::GlobalLink::global(
                    koru_channel::segment().to_owned(),
                    koru_workspace_host::cli::root_from_args(),
                ),
            },
            session_backend: None,
            session_attaching: None,
            session_attached: None,
            session_settled: None,
            terminate_after_attach: None,
            wants_session_popout: None,
            profile_probe: profile_probe::ProfileProbe::new(),
            provider_readiness: None,
            delegation: None,
            recording_modal: None,
            recording_lifecycle: None,
            recording_request: None,
            recording_stop_when_started: false,
            recording_close_after_stop: false,
            recording_telemetry: None,
            recording_menu_was_open: false,
            clean_frame_drawn: false,
            delegate_job: None,
            pending_run_pills: Vec::new(),
            pending_schedule_pills: Vec::new(),
            run_pill_states: std::collections::HashMap::new(),
            run_pill_worker: delegate::pills::RunPills::default(),
            run_pills_wanted: std::collections::HashMap::new(),
            canvas_runs: canvas_runs::CanvasRuns::default(),
            canvas_chains: canvas_chain::CanvasChains::default(),
            device_names: std::collections::HashMap::new(),
            this_device: None,
            device_names_requested_generation: None,
            device_names_acknowledged_generation: None,
            delegate_client: None,
            // Overwritten before the window is ever shown. Zero is not a
            // slot the host ever hands out, so a projection built from an
            // unseated view is visibly wrong rather than quietly wrong.
            accessibility_window: koru_workspace_host::accessibility::WindowSlot::from_raw(0),
            bodies: accessibility::document::State::default(),
            surfaces: accessibility::SurfaceState::default(),
        }
    }

    pub fn install_reminder_wake_proxy(
        &mut self,
        proxy: winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>,
    ) {
        self.reminders.set_waker(std::sync::Arc::new({
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send_event(koru_workspace_host::app::Wake::Store);
            }
        }));
        self.run_index.set_waker(std::sync::Arc::new({
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send_event(koru_workspace_host::app::Wake::Store);
            }
        }));
        self.schedule_index.set_waker(std::sync::Arc::new(move || {
            let _ = proxy.send_event(koru_workspace_host::app::Wake::Store);
        }));
    }

    pub fn search_window(updates: crate::agent::UpdateFeed) -> Self {
        let mut view = Self::new_with(updates, true);
        view.search = Some(crate::search::SearchSurface::new());
        view.search_only = true;
        view
    }

    pub fn feedback_window(updates: crate::agent::UpdateFeed) -> Self {
        let mut view = Self::new_with(updates, true);
        *view.tabs_mut() = TabStrip::new(Tab::feedback(), Tab::welcome);
        view.feedback_only = true;
        view
    }

    /// The singleton support window. A container in support-only mode, the
    /// way the search window is a container in search-only mode: the caption
    /// row with its window buttons, and the chat below — no tab strip, no
    /// sidebar, nothing else. The session outlives this view; it lives in
    /// the app's `Manager`.
    pub fn support_window(
        updates: crate::agent::UpdateFeed,
        session: crate::support::Session,
    ) -> Self {
        let mut view = Self::new_with(updates, true);
        view.support = Some(crate::support::SupportView::new(
            session,
            crate::format::lang(),
        ));
        view.support_only = true;
        view
    }

    /// The floating support icon: a container in icon-only mode, 48 × 48,
    /// drawing nothing but the icon and its badge.
    pub fn fab_window(updates: crate::agent::UpdateFeed, session: crate::support::Session) -> Self {
        let mut view = Self::new_with(updates, true);
        view.fab = Some(crate::support::FabView::new(session, crate::format::lang()));
        view.fab_only = true;
        view
    }

    /// Collect what the support view or the icon asked the host for.
    fn gather_support_intents(&mut self) {
        if let Some(support) = self.support.as_mut() {
            while let Some(intent) = support.take_intent() {
                self.background_intents.push_back(intent);
            }
        }
        if let Some(fab) = self.fab.as_mut() {
            while let Some(intent) = fab.take_intent() {
                self.background_intents.push_back(intent);
            }
            if std::mem::take(&mut fab.wants_drag) {
                self.window_request = Some(WindowRequest::Drag);
            }
        }
    }

    /// The chat's area: everything below the caption row.
    pub(super) fn support_content(&self) -> Rect {
        let cap = self.regions.caption;
        Rect::new(
            0.0,
            cap.y + cap.h,
            self.viewport.0,
            (self.viewport.1 - cap.y - cap.h).max(0.0),
        )
    }

    /// Whether the pointer is over the chat area rather than the caption.
    fn in_support_content(&self, x: f32, y: f32) -> bool {
        self.support_only && self.support_content().contains(x, y)
    }

    fn search_bounds(&self) -> Rect {
        if self.search_only {
            return Rect::new(0.0, 0.0, self.viewport.0, self.viewport.1);
        }
        search_modal_bounds(self.viewport)
    }

    fn search_point(&self, x: f32, y: f32) -> (f32, f32) {
        let bounds = self.search_bounds();
        (x - bounds.x, y - bounds.y)
    }

    /// Open the **Move to…** picker for one document.
    ///
    /// Subject comes from the explore selection, the row menu, or the active
    /// document tab. Returns false when there is nothing to move or the picker
    /// is already open.
    fn open_move_to(&mut self, ids: Vec<koru_core::DocumentId>) -> bool {
        if self.move_to.is_some() || self.search.is_some() || ids.is_empty() {
            return false;
        }
        let Some((_, _, workspaces, current)) = self.pane_context.clone() else {
            return false;
        };
        let Some(current) = current else {
            return false;
        };
        let entries = self.entries.clone().unwrap_or_default();
        // Keep only roots that are still in the live library listing.
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| entries.iter().any(|e| e.id == *id))
            .collect();
        if ids.is_empty() {
            return false;
        }
        // Move only top-level roots of the selection (children ride with parents).
        let root_ids = move_roots(&ids, &entries);
        let title = if root_ids.len() == 1 {
            self.name_of_document(root_ids[0])
        } else {
            format!("{} documents", root_ids.len())
        };
        let mut surface = move_to::MoveToSurface::new(
            &mut self.engine,
            root_ids,
            title,
            &workspaces,
            current,
            &entries,
        );
        let bounds = move_to_modal_bounds(self.viewport);
        surface.set_viewport(bounds.w, bounds.h);
        self.move_to = Some(surface);
        self.hover = None;
        // Other workspace listings are loaded only when the person expands
        // one. Eagerly queueing every workspace here made opening Ctrl+M issue
        // a burst of synchronous Store requests before the picker had drawn.
        // Text search already uses `entity_search`'s reader thread and needs no
        // listing prefetch.
        true
    }

    /// Ctrl+M / menu: pick the subject from explore focus or the active tab.
    ///
    /// Prefer explore multi-selection when the tree is focused; otherwise the
    /// active tab's document, then the shell's active document id.
    fn open_move_to_here(&mut self) -> bool {
        if self.move_to.is_some() {
            return true;
        }
        if self.search.is_some() {
            return false;
        }
        let ids = self.move_subjects_here();
        if !ids.is_empty() && self.open_move_to(ids) {
            return true;
        }
        self.toast = Some((
            koru_ui::Toast::new("Select a document to move".into(), None, Instant::now()),
            ToastPayload::None,
        ));
        true
    }

    fn move_subjects_here(&self) -> Vec<koru_core::DocumentId> {
        // Explore multi-selection when the tree holds focus.
        if self.sidebar.is_open() && self.explore.has_focus() {
            let selected = self.explore_selected_documents();
            if !selected.is_empty() {
                return selected;
            }
            if let Some(id) = self.explore_focused_document() {
                return vec![id];
            }
        }
        // Active tab's document id (more reliable than the shell push alone).
        if let Some(id) = self.tabs().active_tab().document {
            return vec![id];
        }
        self.active_document.into_iter().collect()
    }

    fn explore_selected_documents(&self) -> Vec<koru_core::DocumentId> {
        let entries = match self.entries.as_ref() {
            Some(e) => e,
            None => return Vec::new(),
        };
        self.explore
            .selection_nodes()
            .into_iter()
            .filter_map(|n| {
                let id = koru_core::DocumentId::from_uuid(n.as_uuid());
                entries.iter().any(|e| e.id == id).then_some(id)
            })
            .collect()
    }

    fn explore_focused_document(&self) -> Option<koru_core::DocumentId> {
        let node = self.explore.focused()?;
        let entries = self.entries.as_ref()?;
        entries
            .iter()
            .find(|e| e.id.as_uuid() == node.as_uuid())
            .map(|e| e.id)
    }

    fn move_to_bounds(&self) -> Rect {
        move_to_modal_bounds(self.viewport)
    }

    fn apply_move_to_action(&mut self, action: move_to::Action) -> bool {
        match action {
            move_to::Action::Keep => true,
            move_to::Action::Cancel => {
                self.move_to = None;
                true
            }
            move_to::Action::NeedListing(ws) => {
                self.ask_pane(PaneRequest::ListWorkspaceDocuments(ws));
                true
            }
            move_to::Action::QueryChanged(q) => {
                self.request_move_to_search(q);
                true
            }
            move_to::Action::Confirm(dest) => self.confirm_move_to(dest),
        }
    }

    /// Hit the store search index for Move to… filter mode (same service as
    /// global search / entity picker — not a second matcher).
    fn request_move_to_search(&mut self, query: String) {
        let Some(surface) = self.move_to.as_ref() else {
            return;
        };
        let workspaces = surface.workspace_ids();
        // Convert to store ids for the search client.
        let store_ws: Vec<_> = workspaces
            .iter()
            .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()))
            .collect();
        self.entity_search.request(query, store_ws);
    }

    fn poll_move_to_search(&mut self) -> bool {
        if self.move_to.is_none() {
            return false;
        }
        let Some(update) = self.entity_search.poll() else {
            return false;
        };
        if update.unavailable {
            return false;
        }
        let hits: Vec<_> = update
            .documents
            .into_iter()
            .map(|d| {
                (
                    WorkspaceId::from_uuid(d.workspace.as_uuid()),
                    koru_core::DocumentId::from_uuid(d.document.as_uuid()),
                    d.title,
                    d.breadcrumb,
                )
            })
            .collect();
        let Some(surface) = self.move_to.as_mut() else {
            return false;
        };
        surface.set_entity_hits(hits);
        true
    }

    fn move_to_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        let Some(surface) = self.move_to.as_mut() else {
            return false;
        };
        let action = surface.key(&mut self.engine, key, text, mods);
        self.apply_move_to_action(action)
    }

    fn move_to_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let bounds = self.move_to_bounds();
        if !bounds.contains(x, y) {
            self.move_to = None;
            return true;
        }
        let Some(surface) = self.move_to.as_mut() else {
            return false;
        };
        let action = surface.mouse_down(&mut self.engine, x - bounds.x, y - bounds.y);
        self.apply_move_to_action(action)
    }

    fn confirm_move_to(&mut self, dest: move_to::Destination) -> bool {
        let Some(surface) = self.move_to.take() else {
            return false;
        };
        let subjects: Vec<_> = surface.subjects().to_vec();
        let Some(current) = self.current_workspace() else {
            return false;
        };
        let entries = self.entries.clone().unwrap_or_default();
        // Snapshot prior locations for **every** root so Undo reverses the
        // whole multi-move, not only the first.
        let mut undos = Vec::new();
        for id in &subjects {
            if let Some(e) = entries.iter().find(|e| e.id == *id) {
                undos.push(MoveUndo {
                    id: *id,
                    from_ws: current,
                    from_parent: e.parent,
                    from_index: e.ordinal as usize,
                    to_ws: dest.workspace,
                });
            }
        }
        if undos.is_empty() {
            return false;
        }

        if dest.workspace == current {
            // Place each root at the destination; subsequent roots append so
            // they land contiguous after the first.
            for (i, id) in subjects.iter().enumerate() {
                let index = if dest.index == usize::MAX {
                    usize::MAX
                } else {
                    dest.index.saturating_add(i)
                };
                self.ask_pane(PaneRequest::MoveDocument {
                    id: *id,
                    parent: dest.parent,
                    index,
                });
            }
            self.move_undo = Some(undos);
            if let Some(id) = subjects.first() {
                self.explore
                    .reveal(koru_core::NodeId::from_uuid(id.as_uuid()));
            }
            self.explore_stale = true;
            let msg = if subjects.len() == 1 {
                format!("Moved to {}", dest.path_label)
            } else {
                format!("Moved {} nodes to {}", subjects.len(), dest.path_label)
            };
            self.toast = Some((
                koru_ui::Toast::new(msg, Some("Undo"), Instant::now()),
                ToastPayload::None,
            ));
            return true;
        }

        // Cross-workspace: one transfer per root (subtree included by store).
        self.move_undo = Some(undos);
        for id in &subjects {
            self.close_tabs_showing_document(koru_core::NodeId::from_uuid(id.as_uuid()));
            self.ask_pane(PaneRequest::TransferDocument {
                id: *id,
                source: current,
                target: dest.workspace,
                parent: dest.parent,
                index: dest.index,
                follow: dest.follow && subjects.len() == 1,
                path_label: dest.path_label.clone(),
            });
        }
        let msg = if subjects.len() == 1 {
            format!("Moved to {}", dest.path_label)
        } else {
            format!("Moved {} nodes to {}", subjects.len(), dest.path_label)
        };
        self.toast = Some((
            koru_ui::Toast::new(msg, Some("Undo"), Instant::now()),
            ToastPayload::None,
        ));
        true
    }

    fn undo_move(&mut self) -> bool {
        let Some(undos) = self.move_undo.take() else {
            return false;
        };
        self.toast = None;
        // Reverse order so same-parent ordinals stay valid when several roots
        // were appended as a contiguous block.
        for u in undos.into_iter().rev() {
            if u.from_ws == u.to_ws {
                self.ask_pane(PaneRequest::MoveDocument {
                    id: u.id,
                    parent: u.from_parent,
                    index: u.from_index,
                });
                self.explore
                    .reveal(koru_core::NodeId::from_uuid(u.id.as_uuid()));
            } else {
                self.ask_pane(PaneRequest::TransferDocument {
                    id: u.id,
                    source: u.to_ws,
                    target: u.from_ws,
                    parent: u.from_parent,
                    index: u.from_index,
                    follow: false,
                    path_label: String::new(),
                });
            }
        }
        self.explore_stale = true;
        true
    }

    fn open_search(&mut self) -> bool {
        // `--no-search`: no surface, and so no store session opened for one.
        if !self.search_enabled || self.search.is_some() {
            return false;
        }
        let workspace_scope =
            self.pane_context
                .as_ref()
                .and_then(|(_, profile, workspaces, workspace)| {
                    let workspace = (*workspace)?;
                    workspaces
                        .iter()
                        .find(|entry| entry.id == workspace)
                        .map(|entry| (*profile, workspace, entry.name.clone()))
                });
        let mut search = match (self.retained_search.take(), workspace_scope) {
            (Some(mut search), Some((profile, workspace, name))) => {
                search.resume_in_workspace(profile, workspace, name);
                search
            }
            (Some(mut search), None) => {
                search.resume();
                search
            }
            (None, Some((profile, workspace, name))) => {
                crate::search::SearchSurface::for_workspace(profile, workspace, name)
            }
            (None, None) => crate::search::SearchSurface::new(),
        };
        let bounds = self.search_bounds();
        View::set_viewport(&mut search, bounds.w, bounds.h);
        self.search = Some(search);
        // The observation, not the work: one channel send, a no-op under
        // `cargo test` (nothing starts the host there).
        koru_workspace_host::events_host::search_opened(
            koru_workspace_host::events_host::SearchSurface::InApp,
        );
        true
    }

    fn service_search_outputs(&mut self) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        // Drain first: recording a jump mutably borrows `self`, and cannot
        // overlap the search surface borrow.
        let mut requests = Vec::new();
        while let Some(request) = View::take_pane_request(search) {
            requests.push(request);
        }
        let window_request = View::take_window_request(search);
        // Forwarded rather than replacing: the search panel's ask joins
        // whatever this view has already queued. A same-window target records
        // *here* — the shell landing that follows must not, or a workspace
        // switch would insert the destination's default document into Back.
        for request in requests {
            if matches!(
                &request,
                koru_workspace_host::PaneRequest::Navigate {
                    disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                    ..
                }
            ) {
                self.record_jump();
            }
            self.pane_requests.push_back(request);
        }
        if let Some(request) = window_request {
            if self.search_only {
                self.window_request = Some(request);
            } else {
                self.search = None;
            }
        }
    }

    // ------------------------------------------------------------------ tabs
    //
    // Opening, replacing and landing a document live in [`open`]. What remains
    // here is what a tab *means* once it is open.

    // ------------------------------------------------------ back and forward
    //
    // `here`, `record_jump`, `restore_spot`, `apply_spot_view` and
    // `step_history` live in [`nav`].

    /// Explore's image double-click is one action: open its document, reveal
    /// the picture, and focus the rename field.
    fn rename_image(&mut self, ws: &WorkspaceDoc, image: NodeId) -> bool {
        if !self.open_node(ws, image) {
            return false;
        }
        // The prompt that is about to open owns the keyboard; the tree letting
        // go here is what makes that unambiguous rather than relying on the
        // guard in `key` alone.
        self.explore.clear_focus();
        self.tabs_mut().active_view().start_image_rename(ws, image)
    }

    /// What `F2` renames: the tree row under the pointer, or — when the pointer
    /// is not on one — the document showing.
    ///
    /// **The pointer rather than a selection**, because the tree has no keyboard
    /// focus of its own: its highlight follows the *active tab*, so renaming
    /// "the selected row" would be an obscure way of spelling "the tab you are
    /// looking at". Pointing at a row is how you say which one you mean.
    fn rename_here(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let (px, py) = self.pointer;
        // A Profile page first when it is the tab showing: `F2` there means
        // the workspace row you have selected, which is the only thing it
        // could mean.
        if let TabView::Manage(manage) = &self.tabs().active_tab().view
            && let Some(id) = manage.selected_workspace()
        {
            return self.start_workspace_rename(id);
        }
        if self.sidebar.is_open()
            && self.explore.navigator_mode() == crate::panes::explore::NavigatorMode::Files
        {
            if let Some(node) = self.explore.row_node_at(self.regions.sidebar, px, py) {
                return self.start_file_rename(node);
            }
            let selected = self.explore.selection_nodes();
            if selected.len() == 1 {
                return self.start_file_rename(selected[0]);
            }
            return false;
        }
        if self.sidebar.is_open()
            && self.explore.navigator_mode() == crate::panes::explore::NavigatorMode::Agents
            && let Some(node) = self.explore.row_node_at(self.regions.sidebar, px, py)
            && let Some(schedule) = self.explore.agent_schedule_at(node)
        {
            return self.start_schedule_rename(schedule);
        }
        if self.sidebar.is_open()
            && let Some(node) = self.explore.row_node_at(self.regions.sidebar, px, py)
        {
            return self.rename_node(ws, node);
        }
        self.tabs_mut().active_view().start_rename(ws)
    }

    /// Rename a file in its Explore row. The listing title is the name; there
    /// is no document to open.
    fn start_file_rename(&mut self, node: NodeId) -> bool {
        let id = koru_core::DocumentId::from_uuid(node.as_uuid());
        let Some(name) = self.entries.as_ref().and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.title.clone())
        }) else {
            return false;
        };
        self.workspace_rename = Some(WorkspaceRename::for_file(&mut self.engine, id, &name));
        true
    }

    fn start_schedule_rename(&mut self, schedule: koru_delegate_core::ScheduleId) -> bool {
        let Some(subject) = self.schedule_index.subject(schedule) else {
            return false;
        };
        if Some(subject.owner) != self.this_device {
            let owner = self
                .device_names
                .get(&subject.owner)
                .map(String::as_str)
                .unwrap_or_else(|| {
                    koru_i18n::t(
                        crate::format::lang(),
                        koru_i18n::Key::ScheduleNavigatorOtherDevice,
                    )
                });
            return self.show_notice(koru_i18n::fill(
                koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ScheduleManageOnDevice,
                ),
                &[owner],
            ));
        }
        self.workspace_rename = Some(WorkspaceRename::for_schedule(
            &mut self.engine,
            schedule,
            subject.name.as_str(),
        ));
        true
    }

    /// Rename one node, in whatever way that node is named.
    ///
    /// An image carries a display name of its own; everything else in the tree
    /// is a document, whose name is its title. Two prompts, one entry point —
    /// so the tree does not have to know which is which.
    fn rename_node(&mut self, ws: &mut WorkspaceDoc, node: NodeId) -> bool {
        self.pending_document_rename = None;
        // Explore's Store-backed document rows are manifest ids, not nodes in
        // `ws`. Load another document first and open its prompt only when the
        // shell pushes that document back through `show_active_document`.
        if let Some(document) = self.entries.as_ref().and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.id.as_uuid() == node.as_uuid())
                .map(|entry| entry.id)
        }) {
            if self.active_document != Some(document) {
                self.pending_document_rename = Some((document, Self::gesture_deadline()));
                // Pinned to this window: the prompt is owed to whoever pressed
                // F2 here. See [`request_store_document_here`].
                let asked = self.request_store_document_here(node, true);
                if !asked {
                    self.pending_document_rename = None;
                }
                return asked;
            }
            // The Store document is loaded, but a Welcome or manager tab may
            // be in front of it. Land on its real root before asking the active
            // view to open the title prompt.
            let Some(root) = koru_workspace::document_root(ws) else {
                return false;
            };
            if !self.open_node(ws, root) {
                return false;
            }
            self.explore.clear_focus();
            return self.tabs_mut().active_view().start_rename(ws);
        }
        let image = ws
            .read(node)
            .map(|n| n.kind == Kind::Image)
            .unwrap_or(false);
        if image {
            return self.rename_image(ws, node);
        }
        if !self.open_node(ws, node) {
            return false;
        }
        self.explore.clear_focus();
        self.tabs_mut().active_view().start_rename(ws)
    }

    /// Open a document the taskbar named, if this workspace still has it.
    ///
    /// **Silent when it does not.** A Recent entry outlives the document it
    /// names — deleted, or in a workspace this window is not on — and a dialog
    /// about a stale shortcut is worse than the shortcut quietly doing nothing.
    pub fn open_document_by_id(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        if ws.read(node).is_err() {
            return false;
        }
        self.open_node(ws, node)
    }

    /// Remember a jump-list or global-search target until session restore finishes.
    ///
    /// Cold launches run `Host::initial` then `View::restore`. Opening in
    /// `initial` alone is overwritten by the previous session's tabs; this is
    /// drained at the end of restore instead. See `MainView::restore`.
    pub fn defer_search_target(
        &mut self,
        document: Option<DocumentId>,
        node: Option<NodeId>,
        trash_root: Option<NodeId>,
        match_text: Option<String>,
    ) {
        if let Some(document) = document {
            self.deferred_navigation = Some((document, node, match_text));
            return;
        }
        self.deferred_search_target = Some(DeferredTarget {
            document: None,
            node,
            trash_root,
            match_text,
            asked: true,
            // A cold launch has no deadline: `restore` drains it once, and
            // there is no round trip for it to outlive.
            until: None,
        });
    }

    /// Wait for a document so its **title row** can be landed on and flashed.
    ///
    /// The counterpart of [`defer_until_document_arrives`](Self::defer_until_document_arrives)
    /// for a hit on a document's own name, which names no node at all.
    fn defer_title_landing(&mut self, document: NodeId, match_text: Option<String>) {
        self.deferred_search_target = Some(DeferredTarget {
            document: Some(document),
            node: None,
            trash_root: None,
            match_text,
            asked: false,
            until: Some(Instant::now() + DEFERRED_TARGET_WAIT),
        });
    }

    /// The same, for a result whose document this window has just asked the
    /// shell to open — so the retry knows what it is waiting for and can
    /// reveal it in the tree when it arrives.
    fn defer_until_document_arrives(
        &mut self,
        document: NodeId,
        node: Option<NodeId>,
        match_text: Option<String>,
    ) {
        self.deferred_search_target = Some(DeferredTarget {
            document: Some(document),
            node,
            trash_root: None,
            match_text,
            asked: false,
            until: Some(Instant::now() + DEFERRED_TARGET_WAIT),
        });
    }

    /// Whether the Store room loaded as the workspace's active document is the
    /// one this row key names.
    ///
    /// The row key is a `DocumentId` wearing a `NodeId` — how the explore pane
    /// and `PaneRequest::Navigate` both spell a document — and
    /// `active_document` is the `DocumentId` itself, so the comparison has to
    /// be made in one spelling or the other. It is made here so the two places
    /// that ask cannot disagree about which.
    fn workspace_document_is_active(&self, document: Option<NodeId>) -> bool {
        match (document, self.active_document) {
            (Some(row), Some(active)) => row == NodeId::from_uuid(active.as_uuid()),
            _ => false,
        }
    }

    /// Finish an explore-row gesture that had to wait for its document — the
    /// context menu's *Rename* and its four *Attributes* rows.
    ///
    /// **Neither has a `&WorkspaceDoc` at the click**: `run_menu_action` holds
    /// none, so the row records what it wants in `pending_document_rename` /
    /// `pending_document_attrs` and the container completes it once the
    /// document is in hand. The trap is that *"in hand"* arrives two different
    /// ways. A document that has to be loaded comes back through
    /// `show_active_document`; one that is **already the active document never
    /// does**, because the workspace host's `navigate` only pushes that announcement
    /// when `workspace.active()` actually changes. It always calls
    /// `open_search_target` afterwards, though — so the drain lives here and is
    /// called from both, and the four rows work on the document you are looking
    /// at, which is the one you right-click most.
    ///
    /// **The guard is what the window is showing, not what `land_on`
    /// answered.** That was the other half of the same bug: `opened` is `true`
    /// for a landing that changed nothing and says nothing about *which*
    /// document the tab in front is over, and a picker drawn over the wrong
    /// page is worse than one that did not open.
    ///
    /// A request only completes on the arrival of the document it named: any
    /// other document landing first — a workspace switch's default — leaves it
    /// standing rather than dropping it. Once its own document has arrived it
    /// is cleared either way, so a gesture that cannot be honoured here does
    /// not fire later over something nobody asked about.
    ///
    /// **And it is this window's to complete.** The landing that carries the
    /// document is asked for with
    /// [`SourceWindow`](koru_workspace_host::SearchTargetDisposition::SourceWindow), so
    /// a second window on the same workspace — which under the ordinary
    /// preference order would take it, because it already holds the document —
    /// is never offered it. That was Q44: the picker opened in the window
    /// nobody had right-clicked in, and the one that asked went on waiting.
    ///
    /// The deadline is the backstop for the case no announcement ever arrives
    /// (the shell could not resolve the document, and said so in a dialog): the
    /// request is dropped at the next arrival of *anything* rather than firing
    /// minutes later over a page somebody opened for their own reasons.
    fn finish_explore_gesture(&mut self, ws: &WorkspaceDoc) -> bool {
        // **Ahead of the early return, because they are not explore
        // gestures.** *View in Tasks* from a document and the latched Tasks
        // reveal used to be drained below the check for a pending rename or
        // attribute picker — so they ran only while one of those happened to
        // be waiting, and a chip's *Go to task* pressed on its own did nothing
        // at all (finding 32).
        let mut acted = self.drain_task_view_request(ws);
        acted |= self.finish_task_reveal();
        if self.pending_document_rename.is_none() && self.pending_document_attrs.is_none() {
            return acted;
        }
        let now = Instant::now();
        let active = self.active_document;
        let root = koru_workspace::document_root(ws);
        // A Welcome, Trash or manager tab has no root node to rename or to hang
        // an attribute on, and `root()` answers `None` for all three.
        let landed = root.is_some() && self.tabs().active_tab().view.root() == root;
        let mut acted = false;
        if let Some((document, until)) = self.pending_document_rename {
            let arrived = active == Some(document);
            if arrived || now >= until {
                self.pending_document_rename = None;
                if arrived && landed {
                    self.explore.clear_focus();
                    acted |= self.tabs_mut().active_view().start_rename(ws);
                }
            }
        }
        if let Some((document, kind, until)) = self.pending_document_attrs {
            let arrived = active == Some(document);
            if arrived || now >= until {
                self.pending_document_attrs = None;
                if arrived && landed {
                    self.explore.clear_focus();
                    if let TabView::Leaf(leaf) = self.active_view() {
                        acted |= leaf.open_document_attr_picker(ws, kind.picker());
                    }
                }
            }
        }
        acted
    }

    /// When an explore-row gesture stops waiting for its document.
    ///
    /// The same bound a deferred search target gets, for the same reason: long
    /// enough for a workspace switch and a document open over a socket, short
    /// enough that nobody connects it to something they did next.
    fn gesture_deadline() -> Instant {
        Instant::now() + DEFERRED_TARGET_WAIT
    }

    /// Ask for a title before creating a document. Used both for a cold taskbar
    /// launch and for an activation handed over by a second short-lived exe.
    pub fn prompt_new_document(&mut self) {
        self.menu = None;
        if self.new_document.is_none() {
            // A cancelled *New workspace in…* prompt leaves its owner behind;
            // this prompt's workspace, if it asks for one, is the open Profile's.
            self.new_workspace_owner = None;
            self.new_document = Some(if self.current_workspace().is_some() {
                NewDocument::new(&mut self.engine)
            } else {
                NewDocument::workspace(&mut self.engine)
            });
        }
    }

    /// Ask for the workspace's name before creating any durable catalog row.
    /// The prompt is the same `NamePrompt` used for documents, so caret,
    /// selection, paste, undo, and platform editing keys do not fork here.
    fn prompt_new_workspace(&mut self) -> bool {
        self.menu = None;
        if self.new_document.is_none() {
            self.new_workspace_owner = None;
            self.new_document = Some(NewDocument::workspace(&mut self.engine));
        }
        true
    }

    /// The same prompt, for a workspace in another Profile. The window
    /// switches to that Profile once the workspace exists.
    fn prompt_new_workspace_in(&mut self, profile: ProfileId) -> bool {
        self.menu = None;
        if self.new_document.is_none() {
            self.new_workspace_owner = Some(profile);
            self.new_document = Some(NewDocument::workspace(&mut self.engine));
        }
        true
    }

    /// The Agents tree's `+`: ask for a name before creating a project. The
    /// same modal the taskbar's *New document* opens — one panel recipe, one
    /// routing layer — with the commit sent as the typed create verb.
    fn prompt_new_agent_project(&mut self) {
        self.menu = None;
        if self.new_document.is_none() {
            self.new_document = Some(NewDocument::new_agent_project(&mut self.engine));
        }
    }

    /// A project row's Rename…, seeded with the registry's current name so the
    /// field is an edit rather than a retype.
    fn prompt_rename_agent_project(&mut self, id: koru_session_protocol::ProjectId) -> bool {
        let name = self
            .explore
            .agent_project_name(id)
            .unwrap_or_default()
            .to_owned();
        self.menu = None;
        if self.new_document.is_some() {
            return false;
        }
        self.new_document = Some(NewDocument::rename_agent_project(
            &mut self.engine,
            id,
            &name,
        ));
        true
    }

    /// Delete a project, behind Koru Terminal's own confirmation: the record
    /// and its folder setting go, nothing on disk does (ADR-264). The verb is
    /// sent by `settle_confirm` on a yes, so cancel costs nothing.
    fn confirm_delete_agent_project(&mut self, id: koru_session_protocol::ProjectId) -> bool {
        let lang = crate::format::lang();
        self.menu = None;
        self.confirm = Some((
            koru_ui::Confirm::new(
                koru_i18n::t(lang, koru_i18n::Key::TerminalDeleteProjectTitle),
                koru_i18n::t(lang, koru_i18n::Key::TerminalDeleteProjectBody),
                koru_i18n::t(lang, koru_i18n::Key::TerminalDeleteProject),
            ),
            crate::mainview::trash::ConfirmSubject::DeleteAgentProject(id),
        ));
        true
    }

    /// The `+` menu's *Agents* flyout on its own, at the pointer — what
    /// Welcome's *Run an agent in a terminal* opens. The same rows, dimmed off
    /// the same probe, and a pick opens the same session tab as it does from
    /// `+`. Nothing opens when this build shows no agents.
    fn open_agents_menu(&mut self) -> bool {
        self.profile_probe.refresh(self.agent.wake_proxy());
        self.snapshot_plus_menu();
        let items = context_menu::shown(context_menu::agent_plus_items());
        if items.is_empty() {
            return false;
        }
        let mut menu = ContextMenu::new(&mut self.engine, MenuSubject::NewTab, self.pointer, items);
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// A right-click in the Agents tree, as the pane reported it: a project
    /// row's menu, or the launch-only menu for the bucket and empty space.
    /// The probe refresh is the `+` menu's own, so the profile rows dim off
    /// the same answer wherever they are opened from.
    fn open_agent_tree_menu(
        &mut self,
        project: Option<koru_session_protocol::ProjectId>,
        at: (f32, f32),
    ) -> bool {
        self.profile_probe.refresh(self.agent.wake_proxy());
        self.snapshot_plus_menu();
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::AgentTree { project },
            at,
            context_menu::agent_tree_items(project.is_some()),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    fn confirm_delete_schedules(&mut self, schedules: Vec<koru_delegate_core::ScheduleId>) -> bool {
        if schedules.is_empty() {
            return false;
        }
        if schedules.iter().any(|schedule| {
            self.schedule_index
                .subject(*schedule)
                .is_none_or(|subject| Some(subject.owner) != self.this_device)
        }) {
            return false;
        }
        let lang = crate::format::lang();
        let many = schedules.len() > 1;
        self.menu = None;
        self.confirm = Some((
            koru_ui::Confirm::localized(
                koru_i18n::t(
                    lang,
                    if many {
                        koru_i18n::Key::ScheduleDeleteManyTitle
                    } else {
                        koru_i18n::Key::ScheduleDeleteTitle
                    },
                ),
                koru_i18n::t(
                    lang,
                    if many {
                        koru_i18n::Key::ScheduleDeleteManyBody
                    } else {
                        koru_i18n::Key::ScheduleDeleteBody
                    },
                ),
                koru_i18n::t(lang, koru_i18n::Key::ScheduleDelete),
                koru_i18n::t(lang, koru_i18n::Key::DelegateCancel),
            ),
            crate::mainview::trash::ConfirmSubject::RetireSchedules(schedules),
        ));
        true
    }

    /// A right-click on a session row, as the pane reported it: Koru Terminal's
    /// session menu (Q-E7 P8). The project names for the *Move to project ▸*
    /// flyout and the *Terminate* refusal are snapshotted first — the two rows
    /// whose text is device state rather than a constant, reached by `label()`
    /// and `disabled_reason()` the way those already reach `format::lang()`.
    fn open_session_menu(
        &mut self,
        session: koru_session_protocol::SessionId,
        at: (f32, f32),
    ) -> bool {
        context_menu::set_session_project_names(self.explore.agent_menu_projects());
        context_menu::set_session_terminate_refusal(self.session_terminate_refusal(session));
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::AgentSession { session },
            at,
            context_menu::session_items(),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// **A run row's menu**: one row, and it deletes the runs the column has
    /// chosen — the right-clicked one, or the whole multi-selection when the
    /// click landed inside it (ADR-379).
    ///
    /// Why it may be refused is snapshotted first, the same way the session
    /// menu's *Terminate* refusal is and for the same reason — a run that is
    /// still working cannot be put away reversibly, and the row says so
    /// rather than disappearing or failing after the fact. Over a set the
    /// row is dimmed only when **every** one of them is working: one that is
    /// would otherwise take the verb away from the others, and the dialog is
    /// where a partial gesture says what it is leaving behind.
    fn open_run_menu(&mut self, runs: Vec<koru_delegate_core::RunId>, at: (f32, f32)) -> bool {
        let count = runs.len();
        let all_working = self.runs_still_working(&runs).len() == count;
        context_menu::set_run_delete_refusal(all_working.then(|| {
            koru_i18n::t(
                crate::format::lang(),
                koru_i18n::Key::RunTrashRunningRefusal,
            )
        }));
        context_menu::set_run_delete_count(count);
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::AgentRuns { runs },
            at,
            context_menu::agent_run_items(),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    fn open_schedule_menu(
        &mut self,
        schedules: Vec<koru_delegate_core::ScheduleId>,
        at: (f32, f32),
    ) -> bool {
        if schedules.is_empty() {
            return false;
        }
        let subjects = schedules
            .iter()
            .filter_map(|schedule| self.schedule_index.subject(*schedule))
            .collect::<Vec<_>>();
        if subjects.len() != schedules.len() {
            return false;
        }
        let refusal_subject = subjects
            .iter()
            .find(|subject| Some(subject.owner) != self.this_device);
        let refusal = refusal_subject.map(|subject| {
            let owner = self
                .device_names
                .get(&subject.owner)
                .map(String::as_str)
                .unwrap_or_else(|| {
                    koru_i18n::t(
                        crate::format::lang(),
                        koru_i18n::Key::ScheduleNavigatorOtherDevice,
                    )
                });
            koru_i18n::fill(
                koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ScheduleManageOnDevice,
                ),
                &[owner],
            )
        });
        context_menu::set_schedule_manage_refusal(refusal);
        let states = subjects
            .iter()
            .map(|subject| subject.state.clone())
            .collect::<Vec<_>>();
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::AgentSchedules { schedules },
            at,
            context_menu::agent_schedule_items(&states),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// Route one key through the modal, creating and opening the document only
    /// after a non-empty name is committed.
    /// The tab in front and `active_document` must name the same document —
    /// Ctrl+S snapshots what is showing, and the two disagree while a draft or
    /// the What's New tab (both leaves over a private in-memory workspace, both
    /// with `Tab::document == None`) sits in front of a strip whose
    /// `active_document` still names the last real document opened. Its own
    /// function so the two halves cannot drift apart from the prose that
    /// justifies them; the modal's scrim is what guarantees the tab in front
    /// cannot change between the press and the probe's answer.
    fn snapshot_target_agrees(&self) -> bool {
        let showing = self.tabs().active_tab().document;
        showing.is_some() && showing == self.active_document
    }

    /// The document History and Ctrl+S are both about, in the store's ids.
    ///
    /// One function because two callers must not disagree about it: the
    /// maintenance pass hands it to the panel as the target, and Ctrl+S hands
    /// it to the panel as the document its question is about.
    fn history_target(
        &self,
    ) -> Option<(
        koru_store_core::ids::WorkspaceId,
        koru_store_core::ids::DocumentId,
    )> {
        self.current_workspace()
            .zip(self.active_document)
            .map(|(workspace, document)| {
                (
                    koru_store_core::ids::WorkspaceId::from_uuid(workspace.as_uuid()),
                    koru_store_core::ids::DocumentId::from_uuid(document.as_uuid()),
                )
            })
    }

    /// Step the context column to its next section, opening the column first
    /// if it is shut.
    ///
    /// **The column's only keyboard door.** Its tabs are icons in a row with
    /// no focus ring and no traversal, so without this the Cloud tab — and
    /// Tasks, and Reminders — could be reached only with a pointer.
    pub(super) fn next_context_section(&mut self) -> bool {
        use crate::panes::context::Section;
        if !self.context.is_open() {
            // A section asked for by name is one you meant to see; opening on
            // the way there is what `ShowNavigator` already does for the
            // explore column.
            self.toggle_context();
            return true;
        }
        let current = self.context_view.section();
        let visible = Section::visible();
        let at = visible
            .iter()
            .position(|section| *section == current)
            .unwrap_or(0);
        let next = visible[(at + 1) % visible.len()];
        self.context_view.select(next);
        true
    }

    /// Put the Cloud tab's facts on the clipboard, and say so under them.
    ///
    /// Reachable from the button and from the keyboard, through this one
    /// function — an action that means two things depending on how it was
    /// asked for is how the two drift apart.
    pub(super) fn copy_cloud_details(&mut self) -> bool {
        if !self.context_view.has_cloud_details() {
            return false;
        }
        koru_editor::clipboard::put_system_text(&self.context_view.cloud_details());
        if self.cloud_sync.acknowledge_copy() {
            self.push_cloud_lines();
        }
        true
    }

    /// Hand the Cloud section's lines to chrome. **One place**, because three
    /// call sites that each listed the things to push is three places for a
    /// new one to be forgotten.
    fn push_cloud_lines(&mut self) -> bool {
        self.context_view.set_cloud(
            self.cloud_sync.section_label(),
            self.cloud_sync.lines(),
            self.cloud_sync.note(),
            self.cloud_sync.copy_label(),
        )
    }

    /// What the Cloud tab needs from the rest of the window: what the linked
    /// devices are called, which images are not here, **which file is
    /// selected** (D259 §4), and the open Profile's Cloud connection. All four
    /// are already known somewhere else; none of them is a second request.
    fn cloud_context(&self, visible: bool) -> cloud_sync::CloudContext {
        cloud_sync::CloudContext {
            devices: self.agent.linked_device_names(),
            missing_images: self.cloud_images.0,
            missing_image_devices: self.cloud_images.1.clone(),
            // **Only while the section is on screen.** A node read per
            // maintenance tick for a pane nobody is looking at is exactly the
            // kind of standing cost every performance rule here exists to
            // refuse — and a hidden section makes no request either way.
            file: visible.then(|| self.selected_file()).flatten(),
            connection: self.current_profile_id().map(|profile| {
                cloud_sync::Connection::of(&self.account_state, &profile.to_string(), false)
            }),
        }
    }

    /// **The file a person has clicked**, from the row they clicked (D259 §4).
    ///
    /// **No document read at all.** Everything here is already on the explore
    /// row — its label, its id and the media type the manifest recorded — and
    /// everything about the *bytes* is the Store's to answer on the poll that
    /// was already going. Resolving the file's blob address here would mean
    /// fetching and decoding its snapshot per click, on the thread that has
    /// to stay ready, which is the exact shape `PRINCIPLES.md` refuses.
    fn selected_file(&self) -> Option<cloud_sync::SelectedFile> {
        let row = self.explore.selected_file()?;
        Some(cloud_sync::SelectedFile {
            name: row.label.clone(),
            document: koru_store_core::ids::DocumentId::from_uuid(row.node.as_uuid()),
            mime: row.mime.clone().unwrap_or_default(),
        })
    }

    /// Ctrl+S. Opens the snapshot prompt over the document showing — in Save
    /// mode immediately, while the history worker probes whether anything
    /// changed since the newest version; an unchanged answer morphs the open
    /// prompt into a rename (see [`snapshot_prompt`]). A tab that is not a
    /// document has no history to snapshot, and the chord does nothing.
    fn prompt_save_snapshot(&mut self) -> bool {
        // `TabView::Leaf` is *not* the gate. A draft and the What's New tab are
        // both leaves over a private in-memory workspace, and `Tab::document`
        // is `None` for both — while `active_document` keeps naming the last
        // real document opened. Requiring the two to agree is what stops a
        // snapshot being taken against a document other than the one showing.
        if self.current_workspace().is_none()
            || !self.snapshot_target_agrees()
            || self.snapshot_prompt.is_some()
        {
            return false;
        }
        self.menu = None;
        self.snapshot_prompt = Some(SnapshotPrompt::new(&mut self.engine));
        self.document_history.probe();
        true
    }

    fn snapshot_key(&mut self, key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
        let Some(dialog) = self.snapshot_prompt.as_mut() else {
            return false;
        };
        match dialog.key(&mut self.engine, key, text, mods) {
            SnapshotAction::Keep => true,
            SnapshotAction::Cancel => {
                self.snapshot_prompt = None;
                true
            }
            SnapshotAction::Save(label) => {
                let label = Some(label.trim().to_string()).filter(|l| !l.is_empty());
                self.document_history.save_with_label(label);
                self.snapshot_prompt = None;
                true
            }
            SnapshotAction::Rename { entry, label } => {
                let label = Some(label.trim().to_string()).filter(|l| !l.is_empty());
                self.document_history.relabel(entry, label);
                self.snapshot_prompt = None;
                true
            }
        }
    }

    fn new_document_key(
        &mut self,
        // Kept in the signature: the other arms still edit the open document,
        // and only `Create` moved to the shell.
        _ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        let Some(dialog) = self.new_document.as_mut() else {
            return false;
        };
        let purpose = dialog.purpose();
        let action = dialog.key(&mut self.engine, key, text, mods);
        match action {
            NewDocumentAction::Keep => true,
            NewDocumentAction::Cancel => {
                self.new_document = None;
                true
            }
            // The Agents tree's asks: the committed name becomes a typed
            // backend verb, and the tree reflects the `ProjectsChanged` that
            // answers — no local project row is written (ADR-264).
            NewDocumentAction::CreateDocument(name)
                if purpose == PromptPurpose::NewAgentProject =>
            {
                self.new_document = None;
                // The id is minted here, which is what makes `CreateProject`
                // replay-safe: the same id with the same content is the same
                // project, not a second one. The scope is the window's own
                // `(profile, workspace)` — Koru Terminal's creation rule — so
                // the new project appears in the tree that asked for it.
                let scope = self.explore.agent_creation_scope();
                if let Some(sender) = self.session_verbs() {
                    sender.create_project(
                        koru_session_protocol::ProjectId::mint(),
                        name.trim().to_owned(),
                        scope,
                    );
                }
                true
            }
            NewDocumentAction::CreateDocument(name)
                if matches!(purpose, PromptPurpose::RenameAgentProject(_)) =>
            {
                self.new_document = None;
                if let (PromptPurpose::RenameAgentProject(id), Some(sender)) =
                    (purpose, self.session_verbs())
                {
                    sender.rename_project(id, name.trim().to_owned());
                }
                true
            }
            NewDocumentAction::CreateDocument(title) => {
                // **A document belongs to the workspace, not to the document in
                // hand.** `create_document_after` inserts a node into the
                // `WorkspaceDoc` this view holds — which is the *active
                // document* now, so a new page would land inside whatever is
                // open and look right in the sidebar until the next reopen.
                //
                // The shell owns the `Workspace` and the connection to the
                // store, so it is asked. `App::new` refuses to start without a
                // store, so there is no folder branch left to take here.
                self.new_document = None;
                self.ask_pane(koru_workspace_host::PaneRequest::CreateDocument(title));
                return true;
            }
            NewDocumentAction::CreateWorkspace(name) => {
                self.new_document = None;
                match self.new_workspace_owner.take() {
                    Some(owner) => self.ask_pane(koru_workspace_host::PaneRequest::NewWorkspaceIn(
                        owner, name,
                    )),
                    None => self.ask_pane(koru_workspace_host::PaneRequest::NewWorkspace(name)),
                }
            }
        }
    }

    /// Make the active tab's draft real: create the document in the workspace
    /// and swap the throwaway leaf for one over it.
    ///
    /// A no-op unless the active tab is a draft, which is what lets the caller
    /// ask on every keystroke rather than testing first.
    ///
    /// The tab keeps its position, its label and its size — only which document
    /// it is over changes — so from the outside nothing happens except that
    /// *Untitled* appears in the library, which is exactly the moment somebody
    /// committed to it.
    /// Write a promoted draft's text into the real document and swap the tab.
    ///
    /// Runs on the next key after the shell answered, which is the first moment
    /// the document is available mutably. A draft is at most a title and a line
    /// — promotion fires on the first keystroke — so this copies text rather
    /// than nodes: the draft is a separate `LoroDoc`, so its ids mean nothing
    /// here.
    fn flush_promoted_draft(&mut self, ws: &mut WorkspaceDoc) {
        let Some((root, title, body, caret)) = self.pending_draft_text.take() else {
            return;
        };
        if ws.read(root).is_err() {
            return;
        }
        if !title.is_empty() {
            let _ = ws.set_text(root, &title);
        }
        // The seed left one empty paragraph; fill that, then append the rest.
        let existing = ws
            .children(koru_core::schema::Parent::Node(root))
            .unwrap_or_default();
        for (i, line) in body.iter().enumerate() {
            if line.is_empty() {
                continue;
            }
            match existing.get(i) {
                Some(&node) => {
                    let _ = ws.set_text(node, line);
                }
                None => {
                    if let Ok(para) = koru_core::node::Node::paragraph(line) {
                        let _ = ws.insert(koru_core::schema::Parent::Node(root), para);
                    }
                }
            }
        }

        let home = self.current_workspace();
        let document = self.active_document;
        let name = koru_workspace::name_of(ws, root);
        let mut leaf = self.document_leaf(ws, root);
        // **Every row here is new**, over a different `LoroDoc` with different
        // ids, so nothing was reused and every caret is at 0. Putting the one
        // the person actually had back is the whole of the fix for `rstFi`.
        if let Some((row, at)) = caret {
            leaf.place_caret_at(row, at);
        }
        let tab = self.tabs_mut().active_mut();
        tab.view = TabView::Leaf(leaf);
        // It is over a real document now, and the tab has to know which — that
        // is what makes selecting it open the right one and what stops the next
        // click on it in Explore opening a second tab.
        tab.document = document;
        tab.home = home;
        tab.set_name(&name);
        tab.draft = None;
        tab.invalidate_size();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.explore_stale = true;
        self.ensure_tab_visible(self.tabs().active());
    }

    /// **Ask the shell, and stay a draft until it answers.**
    ///
    /// A document belongs to the workspace and creating one is a round trip, so
    /// promotion cannot complete inside the keystroke that triggers it. The
    /// draft keeps taking keys meanwhile — it is a real editable document, just
    /// a private one — and its text and caret are carried over at the *swap*
    /// rather than at the request, so nothing typed while waiting is lost. The
    /// swap is [`flush_promoted_draft`](Self::flush_promoted_draft).
    ///
    /// Returning `false` is not a failure: it says *this keystroke was not
    /// consumed by promotion*, and the draft handles it as usual.
    ///
    /// **`create_document`, never `document`.** The latter is find-or-create by
    /// title, which is right for a `/document` pill — a pill is a reference, so
    /// one name must mean one node — and catastrophic here: a second blank page
    /// would resolve to the *first* `Untitled`, show its contents, and take the
    /// keystroke that was meant for a new one. Two documents may share a name;
    /// identity is the uuid.
    ///
    /// The title is asked for **empty** — the row shows its "Title" placeholder
    /// and the display fallbacks read *Untitled* — so a name is something
    /// somebody wrote, never something they had to select and delete first.
    ///
    /// This used to insert a node into `ws` and swap the leaf here, which
    /// store-backed is the *active document*: the new page landed inside
    /// whatever was open, never reached the manifest, and disappeared from
    /// Explore the moment anything re-read it. That branch sat below an
    /// unconditional `return` for a while; it is deleted rather than kept,
    /// because dead code that still compiles is the kind that gets revived.
    fn promote_draft(&mut self) -> bool {
        if self.tabs().active_tab().draft.is_none() {
            return false;
        }
        if !self.promoting {
            self.promoting = true;
            self.ask_pane(koru_workspace_host::PaneRequest::CreateDocument(
                String::new(),
            ));
        }
        false
    }

    /// Push the active document onto the recents, most recent first and without
    /// duplicates.
    ///
    /// Called at the one place the shell tells the container *which* document is
    /// now active — the store id, not a workspace node, so the recents survive a
    /// workspace switch and resolve through the store listing.
    fn remember(&mut self) {
        let Some(document) = self.active_document else {
            // A workspace with no manifest has no document id to record; the
            // harness that stands in for the store is exactly that case.
            return;
        };
        self.recents.retain(|&id| id != document);
        self.recents.insert(0, document);
        self.recents.truncate(koru_workspace_host::MAX_RECENTS);
        self.recents_stale = true;
    }

    fn select(&mut self, i: usize) -> bool {
        // Switching tabs is a jump like any other: Back after it should return
        // you to the tab you came from, which is what it does everywhere else
        // that has both tabs and a history.
        if i != self.tabs().active() {
            self.record_jump();
        }
        if !self.tabs_mut().select(i) {
            return false;
        }
        // The recorder's v1 set: a tab switch, kind only — which tab is
        // content, and content stays out.
        self.observe_recording_tab_switch();
        // **The window opens the document the tab is over — without moving
        // Explore.** ActivateDocument parks/opens the room in place; Navigate
        // would switch the active workspace whenever locate_document differed.
        if let Some(doc) = self.tabs().active_tab().document
            && self.active_document != Some(doc)
        {
            // Snapshot the tab being shown *before* catch_up can see the
            // stranger document. History's pending slot is a different waiter.
            if let Some(spot) = self.here_for_tab(i) {
                self.tab_pending = Some(spot);
            }
            let home = self.tabs().active_tab().home_workspace();
            crate::mainview::nav_debug::nav_debug!(
                "[koru:nav] tab-select to={} doc={} active={} needs_activate=true",
                self.tabs().active_tab().id,
                doc.to_storage(),
                self.active_document
                    .map(|id| id.to_storage())
                    .unwrap_or_default(),
            );
            self.ask_pane(koru_workspace_host::PaneRequest::ActivateDocument {
                document: doc,
                workspace: home,
            });
        }
        self.ensure_tab_visible(i);
        // The tab being shown may have been left stale by a resize or an adopt
        // it slept through, so it lays out now — once, on the way in.
        self.size_visible_groups();
        self.wake_workspaces_tab();
        self.sync_welcome_lists();
        true
    }

    /// Refresh the Manage tab's data exactly when it becomes visible.
    ///
    /// Tab selection has several entry points (pointer, keyboard cycling, and
    /// closing the tab in front of it). This hook avoids both hidden polling
    /// and stale data when a generic tab path brings Manage back: it asks
    /// for Cloud, and starts the Workspaces tick while Manage is in front.
    /// Every selection path already calls this, which is why it kept the
    /// hook rather than growing a second one.
    fn wake_workspaces_tab(&mut self) {
        if let TabView::Manage(view) = &mut self.tabs_mut().active_mut().view {
            view.refresh_cloud();
        }
        self.arm_workspaces_poll();
    }

    /// Which documents the strip is showing, so a close can tell what it let go
    /// of. Duplicates included: what matters is what is left afterwards.
    fn shown_documents(&self) -> Vec<koru_core::DocumentId> {
        self.tabs().iter().filter_map(|t| t.document).collect()
    }

    /// Ask the shell to let go of every document no tab shows any more.
    ///
    /// **A tab is what keeps a document open**, and the workspace cannot see one
    /// close: an open document is a `LoroDoc`, a room and a socket thread, so a
    /// close that says nothing leaks all three for as long as the window is up.
    /// Paired with [`shown_documents`](Self::shown_documents) around every close,
    /// because a document may be in two tabs and the *last* one is what releases
    /// it.
    fn released(&mut self, before: Vec<koru_core::DocumentId>) {
        for id in before {
            if !self.tabs().iter().any(|t| t.document == Some(id)) {
                self.ask_pane(koru_workspace_host::PaneRequest::CloseDocument(id));
            }
        }
    }

    fn close(&mut self, i: usize) -> bool {
        if self.socials_close_veto(|index| index == i) {
            return true;
        }
        let before = self.shown_documents();
        if !self.tabs_mut().close(i) {
            return false;
        }
        self.released(before);
        self.flush_closed_tab_bursts();
        // Closing renumbers everything after `i`, and a slide is recorded
        // against an index — leave it and some *other* tab inherits the motion.
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.wake_workspaces_tab();
        true
    }

    /// Close tab `i` **because the user asked for this tab to go** — its ✕,
    /// `Ctrl+W`, or *Close* in its menu.
    ///
    /// **Closing the last one closes what contains it.** `TabSet` still refuses
    /// to be empty, and mints a welcome tab rather than leaving none; that is
    /// right for every other way a tab goes away — popping one out leaves the
    /// window you popped it from, and *Close All Tabs* means *start again*,
    /// which is why both keep going through [`close`](Self::close) directly. But
    /// ✕ on the only tab you have is the gesture every browser answers by going
    /// away — so the *pane* goes when the split has others, and only the last
    /// pane standing answers with the window close.
    fn close_requested(&mut self, i: usize) -> bool {
        if self.tabs().get(i).is_some_and(|t| t.is_pinned()) {
            return false;
        }
        if self.socials_close_veto(|index| index == i) {
            return true;
        }
        if self.tabs().pinned_count() == 0 && self.tabs().len() <= 1 {
            if self.panes.iter_groups().count() > 1 {
                // Take, not close: close floors at a welcome tab, and this pane
                // is meant to go away, not to start again.
                let before = self.shown_documents();
                if self.panes.focused_mut().take_tab(i).is_none() {
                    return false;
                }
                self.panes.collapse_empty_groups();
                self.released(before);
                self.flush_closed_tab_bursts();
                self.slides_mut().clear();
                self.mark_tab_widths_stale();
                self.relayout();
                self.wake_workspaces_tab();
                return true;
            }
            return self.ask_window(WindowRequest::Close);
        }
        self.close(i)
    }

    /// Whether tab `i` is a document that can be popped out into its own window.
    ///
    /// A popped-out tab becomes a document window, so only a real document tab
    /// qualifies: the welcome screen and the permanent workspace tab do not.
    /// Read off the tab's root rather than its kind so this stays a question
    /// about the tab, not about the document.
    fn is_poppable(&self, i: usize) -> bool {
        self.tabs().get(i).is_some_and(|t| t.view.pops_out())
    }

    /// Whether tab `i` travels between panes: torn off its strip, dropped into
    /// another group, or split out. Documents can also leave the window
    /// entirely (`is_poppable`); shell and agent tabs are views over live
    /// state, so they move and split but never pop out.
    fn is_pane_draggable(&self, i: usize) -> bool {
        self.tabs().get(i).is_some_and(|t| {
            t.view.root().is_some() || matches!(t.view, TabView::Session(_) | TabView::AiChat(_))
        })
    }

    /// Whether the tab a cross-pane drag is carrying can pop out. Looked up in
    /// the source group, because the tab is still there until the drop lands.
    fn pane_drag_poppable(&self, drag: &panes::PaneDrag) -> bool {
        self.panes
            .group(&drag.source)
            .and_then(|g| g.iter_tabs().find(|t| t.id == drag.tab_id))
            .is_some_and(|t| t.view.pops_out())
    }

    /// Pop tab `i` out into its own window: close the tab here and ask the shell
    /// for a window over the same document. `false` if the tab cannot be popped.
    fn pin_tab(&mut self, i: usize) -> bool {
        let home = self
            .tabs()
            .get(i)
            .and_then(|t| t.home_workspace())
            .or_else(|| self.current_workspace());
        let Some(home) = home else {
            return false;
        };
        let show_title = self
            .tabs()
            .iter()
            .find_map(|t| match &t.view {
                TabView::Welcome(w) => Some(w.pinned_show_title()),
                _ => None,
            })
            .unwrap_or(self.pinned_show_title);
        if !self.tabs_mut().pin(i, home.as_uuid(), show_title) {
            return false;
        }
        inputdebug::write(format_args!(
            "[Koru][input] view pin-tab i={i} active={} pinned_count={}",
            self.tabs().active(),
            self.tabs().pinned_count(),
        ));
        self.recents_stale = true;
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.sync_welcome_lists();
        true
    }

    fn unpin_tab(&mut self, i: usize) -> bool {
        let before = self.shown_documents();
        let current = self.current_workspace();
        if !self.tabs_mut().unpin(i, current.map(|id| id.as_uuid())) {
            return false;
        }
        inputdebug::write(format_args!(
            "[Koru][input] view unpin-tab i={i} active={} pinned_count={}",
            self.tabs().active(),
            self.tabs().pinned_count(),
        ));
        self.recents_stale = true;
        self.released(before);
        self.flush_closed_tab_bursts();
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.sync_welcome_lists();
        true
    }

    fn unpin_live_document(&mut self, id: koru_core::DocumentId) -> bool {
        let keep_welcome = self.tabs().has_welcome();
        let Some(i) = self.tabs().showing_document(id) else {
            return false;
        };
        if !self.unpin_tab(i) {
            return false;
        }
        if keep_welcome {
            let welcome = self
                .tabs()
                .iter()
                .position(|t| matches!(t.view, TabView::Welcome(_)) && t.draft.is_none());
            if let Some(w) = welcome {
                let _ = self.select(w);
            }
        }
        true
    }

    fn set_pinned_title(&mut self, i: usize, show: bool) -> bool {
        if !self.tabs_mut().set_show_title(i, show) {
            return false;
        }
        self.mark_tab_widths_stale();
        self.ensure_tab_visible(self.tabs().active());
        true
    }

    fn pop_out(&mut self, i: usize) -> bool {
        let Some(node) = self.tabs().get(i).and_then(|t| t.view.root()) else {
            return false;
        };
        if !self.is_poppable(i) {
            return false;
        }
        self.close(i);
        self.wants_popout = Some(node);
        true
    }

    /// The three context-menu bulk closes. Each shares `close`'s tail — a
    /// renumbered strip invalidates any slide in flight, and the now-active tab
    /// has to lay out — so the strip and the selection cannot disagree.
    fn close_others(&mut self, i: usize) -> bool {
        if self.socials_close_veto(|index| index != i) {
            return true;
        }
        let before = self.shown_documents();
        if !self.tabs_mut().close_others(i) {
            return false;
        }
        self.released(before);
        self.flush_closed_tab_bursts();
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.wake_workspaces_tab();
        true
    }

    /// Close every tab, leaving the window on one fresh welcome tab.
    ///
    /// The same landing place closing the last tab by hand gives you — the list
    /// is never empty ([`TabSet`]) — so this is one action rather than a loop
    /// that has to stop before the end.
    fn close_all(&mut self) -> bool {
        if self.socials_close_veto(|_| true) {
            return true;
        }
        let before = self.shown_documents();
        if !self.tabs_mut().close_all_unpinned() {
            return false;
        }
        self.released(before);
        self.flush_closed_tab_bursts();
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        true
    }

    fn close_left(&mut self, i: usize) -> bool {
        if self.socials_close_veto(|index| index < i) {
            return true;
        }
        let before = self.shown_documents();
        if !self.tabs_mut().close_left(i) {
            return false;
        }
        self.released(before);
        self.flush_closed_tab_bursts();
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.wake_workspaces_tab();
        true
    }

    fn close_right(&mut self, i: usize) -> bool {
        if self.socials_close_veto(|index| index > i) {
            return true;
        }
        let before = self.shown_documents();
        if !self.tabs_mut().close_right(i) {
            return false;
        }
        self.released(before);
        self.flush_closed_tab_bursts();
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.wake_workspaces_tab();
        true
    }

    /// Which menu items can be chosen right now, in the open menu's own order.
    /// Computed live so a menu left open through a change — a tab deleted in
    /// another window, a workspace archived in the other one — never offers
    /// something it cannot do.
    fn enabled_items(&self, menu: &ContextMenu) -> Vec<bool> {
        menu.items()
            .iter()
            .map(|a| self.item_is_enabled(menu, *a))
            .collect()
    }

    fn item_is_enabled(&self, menu: &ContextMenu, action: MenuAction) -> bool {
        match &menu.subject {
            MenuSubject::NewTab => match action {
                // A profile row reflects the off-thread presence probe: a
                // program — shell or agent, one uniform rule — whose
                // executable is not on this machine is dimmed with its
                // *Not installed* note rather than hidden
                // (`profile_probe::enabled_rows`).
                MenuAction::TerminalProfile(index) => self.profile_probe.row_enabled(index),
                // Dimmed, with its reason beside it, when this build carries
                // no configuration for the importer (C6) — never hidden.
                MenuAction::ImportFormat(id) => {
                    koru_workspace_host::formats::import_unavailable_reason(
                        id,
                        crate::format::lang(),
                    )
                    .is_none()
                }
                _ => matches!(
                    action,
                    MenuAction::Documents
                        | MenuAction::NewDocument
                        | MenuAction::OpenRecentDocument(_)
                        | MenuAction::Canvases
                        | MenuAction::NewCanvas
                        | MenuAction::NewShell
                        | MenuAction::NewAgent
                        | MenuAction::ActiveShells
                        | MenuAction::ActiveAgents
                        | MenuAction::OpenRunningSession(_)
                        | MenuAction::Chats
                        | MenuAction::NewAiChat
                        | MenuAction::Socials
                        | MenuAction::Delegate
                        | MenuAction::OpenRecentChat(_)
                        | MenuAction::OpenWelcome
                        | MenuAction::ClearRecents
                        | MenuAction::ImportAs
                ),
            },
            // No tab is special any more, so this is just counting. Closing the
            // last one is allowed and leaves a welcome tab, which is why `Close`
            // is unconditional rather than guarded by a count.
            MenuSubject::Tab(tab) => {
                let tab = *tab;
                let n = self.tabs().len();
                let in_range = tab < n;
                let document = self.tabs().get(tab).and_then(|tab| tab.document);
                let pinned = self.tabs().get(tab).is_some_and(|t| t.is_pinned());
                let show_title = self.tabs().get(tab).is_some_and(|t| t.show_title);
                match action {
                    MenuAction::PopOut => self.is_poppable(tab),
                    MenuAction::PinTab => {
                        in_range
                            && (document.is_some()
                                || self.tabs().get(tab).is_some_and(|t| t.view.is_pinnable()))
                            && !pinned
                    }
                    MenuAction::UnpinTab => in_range && pinned,
                    MenuAction::ShowTabTitle => in_range && pinned && !show_title,
                    MenuAction::HideTabTitle => in_range && pinned && show_title,
                    MenuAction::Close => in_range,
                    MenuAction::CloseOthers => {
                        in_range && self.tabs().main_count() > if pinned { 0 } else { 1 }
                    }
                    MenuAction::CloseLeft => {
                        in_range
                            && !pinned
                            && tab >= self.tabs().pinned_count()
                            && tab > self.tabs().pinned_count()
                    }
                    MenuAction::CloseRight => in_range && !pinned && tab + 1 < n,
                    // **Dimmed on a lone tab.** "Close everything" is a way of
                    // saying *start again*, and it still is — but with one tab
                    // open it starts you again where you already are, and an
                    // item that does nothing visible is an item that reads as
                    // broken. Closing that one tab is what ✕ is for, and that
                    // now closes the window.
                    MenuAction::CloseAll => {
                        self.tabs().main_count() > usize::from(self.tabs().pinned_count() == 0)
                    }
                    MenuAction::RevealInExplore | MenuAction::Rename | MenuAction::MoveToTrash => {
                        document.is_some()
                    }
                    MenuAction::NewSocialPost => {
                        document.is_some_and(|id| self.social_source_title(id).is_some())
                    }
                    _ => false,
                }
            }
            // Computed live from the Manage tab's controller and this window
            // (`workspace_menu`): a workspace archived in another window while
            // the menu stands open dims what it can no longer do.
            MenuSubject::Workspace(id) => self.workspace_refusal(*id, action).is_none(),
            MenuSubject::AgentTree { project } => match action {
                MenuAction::NewShell
                | MenuAction::NewAgent
                | MenuAction::ActiveShells
                | MenuAction::ActiveAgents
                | MenuAction::OpenRunningSession(_) => true,
                // The same presence probe the `+` menu's rows read: a program
                // whose executable is not on this machine is dimmed with its
                // *Not installed* note, never hidden.
                MenuAction::TerminalProfile(index) => self.profile_probe.row_enabled(index),
                MenuAction::Rename => project.is_some(),
                // Koru Terminal's refusal, in this window's local answer: a
                // project with running sessions dims Delete and says why. The
                // host still refuses on the wire, and that refusal is
                // surfaced rather than swallowed.
                MenuAction::DeleteProject => {
                    project.is_some_and(|id| !self.explore.agent_project_has_running(id))
                }
                _ => false,
            },
            // Every row of a run pill's menu is one of its results, and a
            // result can always be opened: the tab says *the content is on
            // another device* itself rather than the row refusing (D27).
            // The *registered nothing* row is a statement, never an offer.
            MenuSubject::RunPill { .. } => matches!(action, MenuAction::OpenRunResult(_)),
            // A run that is still writing into a folder cannot be put away
            // reversibly; the row dims and says why. The worker refuses it
            // too — it is the party that knows — and this is the half that
            // stops the question being asked and then taken back.
            MenuSubject::AgentRuns { runs } => match action {
                MenuAction::DeleteRun => self.runs_still_working(runs).len() < runs.len(),
                _ => false,
            },
            MenuSubject::AiChat { .. } => matches!(action, MenuAction::DeleteAiChat),
            MenuSubject::AgentSchedules { schedules } => {
                let subjects = schedules
                    .iter()
                    .filter_map(|schedule| self.schedule_index.subject(*schedule))
                    .collect::<Vec<_>>();
                let complete = !subjects.is_empty() && subjects.len() == schedules.len();
                let local = complete
                    && subjects
                        .iter()
                        .all(|subject| Some(subject.owner) == self.this_device);
                match action {
                    MenuAction::ScheduleDelete => local,
                    MenuAction::ScheduleRename | MenuAction::ScheduleEdit => {
                        local
                            && subjects.len() == 1
                            && !matches!(
                                subjects[0].state,
                                koru_delegate_schedule::ScheduleState::HandoffPending { .. }
                                    | koru_delegate_schedule::ScheduleState::Retired
                            )
                    }
                    MenuAction::SchedulePause => {
                        local
                            && subjects.iter().all(|subject| {
                                matches!(
                                    subject.state,
                                    koru_delegate_schedule::ScheduleState::Active { .. }
                                )
                            })
                    }
                    MenuAction::ScheduleResume => {
                        local
                            && subjects.iter().all(|subject| {
                                matches!(
                                    subject.state,
                                    koru_delegate_schedule::ScheduleState::Paused { .. }
                                )
                            })
                    }
                    _ => false,
                }
            }
            MenuSubject::AgentSession { session } => match action {
                MenuAction::SessionOpenInNewTab
                | MenuAction::SessionMoveToNewWindow
                | MenuAction::SessionMoveToProject => true,
                // The project the session is already in is dimmed — the check
                // is the explanation (Koru Terminal's `destination_rows`).
                MenuAction::SessionAssignProject(target) => {
                    self.explore.agent_session_project_of(*session) != target
                }
                // Refused when the session exited or another window controls
                // it; the reason rides the snapshot the menu opened with.
                MenuAction::SessionTerminate => self.session_terminate_refusal(*session).is_none(),
                _ => false,
            },
            MenuSubject::Nodes(nodes) => {
                if action == MenuAction::NewSocialPost {
                    return nodes.len() == 1
                        && self
                            .social_source_title(koru_core::DocumentId::from_uuid(
                                nodes[0].as_uuid(),
                            ))
                            .is_some();
                }
                matches!(
                    action,
                    MenuAction::OpenInNewTab | MenuAction::OpenDocumentInNewWindow
                        if !nodes.is_empty()
                ) || matches!(action, MenuAction::Rename if nodes.len() == 1)
                    || matches!(
                        action,
                        MenuAction::Attributes
                            | MenuAction::AttributeTags
                            | MenuAction::AttributeColor
                            | MenuAction::AttributeDue
                            | MenuAction::AttributePriority
                            if nodes.len() == 1
                    )
                    || matches!(action, MenuAction::MoveTo if nodes.len() == 1)
                    || matches!(
                        action,
                        MenuAction::MoveNodesTo(count) if count > 1 && count == nodes.len()
                    )
                    || matches!(action, MenuAction::ExportAs | MenuAction::ExportFormat(_))
                    || matches!(action, MenuAction::MoveToTrash if nodes.len() == 1)
                    || matches!(
                        action,
                        MenuAction::MoveNodesToTrash(count)
                            if count > 1 && count == nodes.len()
                    )
            }
            MenuSubject::Files(files) => {
                (!files.is_empty()
                    && matches!(action, MenuAction::OpenFileInApp | MenuAction::SaveFileAs))
                    || matches!(action, MenuAction::Rename if files.len() == 1)
                    || (matches!(action, MenuAction::AddNote | MenuAction::OpenNote)
                        && files.len() == 1)
                    || matches!(action, MenuAction::MoveToTrash if files.len() == 1)
                    || matches!(
                        action,
                        MenuAction::MoveFilesToTrash(count)
                            if count > 1 && count == files.len()
                    )
            }
            MenuSubject::Trashed(nodes) => {
                !nodes.is_empty()
                    && matches!(action, MenuAction::Restore | MenuAction::DeletePermanently)
            }
            // Every row of a task's menu is always available: the menu was
            // built from the row it opened on, and a row that has since gone
            // refuses at the command rather than by being dimmed here.
            MenuSubject::Task { .. } => matches!(
                action,
                MenuAction::TaskGoToOrigin
                    | MenuAction::TaskGoToList
                    | MenuAction::TaskCreateDocument
                    | MenuAction::TaskComplete
                    | MenuAction::TaskReopen
                    | MenuAction::TaskPinToday
                    | MenuAction::TaskUnpinToday
                    | MenuAction::TaskDue
                    | MenuAction::TaskPriority
                    | MenuAction::TaskList
                    | MenuAction::TaskDelegate
                    | MenuAction::TaskRemove
            ),
            // A computed queue cannot go away while its menu is open, so its one
            // row needs no guard — which is the whole difference between it and
            // the list arm below.
            MenuSubject::RailTarget(_) => matches!(action, MenuAction::RailOpenInNewTab),
            // **Only while the list is still there.** The table is pushed from a
            // system document another window writes, so a list deleted next door
            // while this menu was open can no longer be renamed, recoloured or
            // deleted — the workspace menu's guard, one document along.
            //
            // *Open in new tab* is inside the guard for the same reason: a page
            // for a list that no longer exists is a page that would draw
            // "Nothing in this list yet." for ever.
            MenuSubject::List(id) => {
                self.list_table.iter().any(|entry| entry.id == *id)
                    && matches!(
                        action,
                        MenuAction::RailOpenInNewTab
                            | MenuAction::ListRename
                            | MenuAction::ListColor
                            | MenuAction::ListSwatch(_)
                            | MenuAction::ListDelete
                    )
            }
            // Emptying nothing is not an action, it is a no-op wearing a label.
            MenuSubject::Trash => {
                matches!(action, MenuAction::EmptyTrash) && self.explore.trash_count() > 0
            }
            // Every window action is always available — the rows were built from
            // this window's state when the menu opened, and a window can always
            // be minimised, resized and closed.
            MenuSubject::Window => match action {
                // Dim while *another* window holds the process's one
                // recording — the flag is the enforcement's visible half.
                MenuAction::HeaderStartRecording => {
                    !koru_recording::RECORDING_ACTIVE.load(std::sync::atomic::Ordering::SeqCst)
                }
                // A stop in flight is already on its way out.
                MenuAction::HeaderStopRecording => !matches!(
                    self.recording_lifecycle,
                    Some(recording::Lifecycle::Stopping)
                ),
                _ => matches!(
                    action,
                    MenuAction::WindowAlwaysOnTop
                        | MenuAction::WindowUnpinTop
                        | MenuAction::HeaderSupport
                        | MenuAction::HeaderSettings
                        | MenuAction::HeaderFeedback
                        | MenuAction::HeaderUpdates
                        // The row is never *chosen* — its controls are pressed —
                        // but this is what colours the word "Zoom", and a live
                        // stepper under a dimmed label would be a lie.
                        | MenuAction::HeaderZoom
                        | MenuAction::WindowSnapLeft
                        | MenuAction::WindowArrange
                        | MenuAction::WindowSnapRight
                        | MenuAction::WindowResetPosition
                ),
            },
        }
    }

    /// Run the highlighted item. The one place a context-menu choice takes
    /// effect, so the mapping from item to action stays in one spot.
    /// [`run_menu_action`](Self::run_menu_action), for a suite that wants the
    /// verb without building the menu that raises it.
    #[cfg(test)]
    pub(super) fn run_menu_action_for_test(
        &mut self,
        subject: MenuSubject,
        action: MenuAction,
    ) -> bool {
        self.run_menu_action(subject, action)
    }

    fn run_menu_action(&mut self, subject: MenuSubject, action: MenuAction) -> bool {
        // A row this build hides is never built ([`context_menu::shown`]);
        // this is the same answer at the one place a choice takes effect, so
        // a stale index or a test cannot reach one either (koru-features).
        if !action.shown_in_product() {
            return false;
        }
        match subject {
            MenuSubject::NewTab => match action {
                MenuAction::NewDocument => self.new_tab(),
                MenuAction::NewCanvas => self.new_canvas(),
                MenuAction::OpenRecentDocument(id) => self.open_recent_document(id),
                MenuAction::Documents
                | MenuAction::Canvases
                | MenuAction::NewShell
                | MenuAction::NewAgent
                | MenuAction::ActiveShells
                | MenuAction::ActiveAgents
                | MenuAction::Chats => false,
                MenuAction::OpenRunningSession(session) => {
                    self.open_session_tab_for_existing(session)
                }
                // The chosen profile opens as a real, running session tab in
                // this window (TERMINATAB W4) — the separate Koru Terminal app
                // is no longer this row's answer.
                MenuAction::TerminalProfile(index) => {
                    // The `+` menu's session has no placement: Unassigned.
                    self.open_session_tab_for_profile(index, None)
                }
                MenuAction::NewAiChat => self.open_ai_chat_tab(),
                MenuAction::Socials => self.open_socials_tab(),
                // The `+` menu knows nothing about a subject, so the modal
                // opens empty — one gesture, the same single path.
                MenuAction::Delegate => self.begin_delegation(delegate::DelegationOrigin::PlusMenu),
                MenuAction::OpenRecentChat(id) => self.open_ai_chat_tab_for(id),
                MenuAction::OpenWelcome => self.open_welcome_tab(),
                MenuAction::ClearRecents => self.clear_recents(),
                MenuAction::ImportFormat(format) => self.ask_pane(PaneRequest::ImportContent {
                    format,
                    source: koru_workspace_host::ImportSourceSelection::Picker,
                }),
                MenuAction::ImportAs => false,
                _ => false,
            },
            MenuSubject::Tab(tab) => match action {
                MenuAction::NewSocialPost => {
                    let Some(document) = self.tabs().get(tab).and_then(|tab| tab.document) else {
                        return false;
                    };
                    self.social_post_from_document(document)
                }
                MenuAction::PopOut => self.pop_out(tab),
                MenuAction::Close => {
                    if self.tabs().get(tab).is_some_and(|t| t.is_pinned()) {
                        self.close(tab)
                    } else {
                        self.close_requested(tab)
                    }
                }
                MenuAction::PinTab => self.pin_tab(tab),
                MenuAction::UnpinTab => self.unpin_tab(tab),
                MenuAction::ShowTabTitle => self.set_pinned_title(tab, true),
                MenuAction::HideTabTitle => self.set_pinned_title(tab, false),
                MenuAction::RevealInExplore => self.reveal_tab_in_explore(tab),
                MenuAction::CloseOthers => self.close_others(tab),
                MenuAction::CloseLeft => self.close_left(tab),
                MenuAction::CloseRight => self.close_right(tab),
                MenuAction::CloseAll => self.close_all(),
                MenuAction::Rename => {
                    let Some(document) = self.tabs().get(tab).and_then(|tab| tab.document) else {
                        return false;
                    };
                    self.pending_document_rename = Some((document, Self::gesture_deadline()));
                    let asked = self
                        .request_store_document_here(NodeId::from_uuid(document.as_uuid()), true);
                    if !asked {
                        self.pending_document_rename = None;
                    }
                    asked
                }
                MenuAction::MoveToTrash => {
                    let Some(document) = self.tabs().get(tab).and_then(|tab| tab.document) else {
                        return false;
                    };
                    self.queue_trash(vec![NodeId::from_uuid(document.as_uuid())])
                }
                _ => false,
            },
            MenuSubject::Workspace(id) => self.run_workspace_menu_action(id, action),
            // Every mutation is a typed backend verb; the tree only reflects
            // the `ProjectsChanged`/catalog that answers (ADR-264). *Shell*
            // and *Agents* act through their flyouts, so the parent rows
            // themselves do nothing.
            MenuSubject::AgentTree { project } => match action {
                MenuAction::TerminalProfile(index) => {
                    self.open_session_tab_for_profile(index, project)
                }
                MenuAction::Rename => {
                    project.is_some_and(|id| self.prompt_rename_agent_project(id))
                }
                MenuAction::DeleteProject => {
                    project.is_some_and(|id| self.confirm_delete_agent_project(id))
                }
                MenuAction::NewShell
                | MenuAction::NewAgent
                | MenuAction::ActiveShells
                | MenuAction::ActiveAgents => false,
                MenuAction::OpenRunningSession(session) => {
                    self.open_session_tab_for_existing(session)
                }
                _ => false,
            },
            // The session menu (Q-E7 P8). Open/focus is the same attach the
            // row's click does; assign is the verb P5's drag already sends;
            // move-to-window and terminate are their own paths in
            // `session_backend`. *Move to project ▸* is a flyout parent — its
            // rows carry the verb, so the parent itself does nothing.
            // **What the maintainers asked for**: right-click a run's crumb, see what
            // it produced, and open the one you pick as a tab (ADR-369).
            MenuSubject::RunPill { document } => match action {
                MenuAction::OpenRunResult(index) => self.open_run_pill_result(document, index),
                _ => false,
            },
            MenuSubject::AgentRuns { runs } => match action {
                MenuAction::DeleteRun => self.queue_run_trash(runs),
                _ => false,
            },
            MenuSubject::AiChat { chat } => match action {
                MenuAction::DeleteAiChat => self.ask_delete_chat(chat),
                _ => false,
            },
            MenuSubject::AgentSchedules { schedules } => match action {
                MenuAction::ScheduleRename => schedules
                    .first()
                    .copied()
                    .is_some_and(|schedule| self.start_schedule_rename(schedule)),
                MenuAction::ScheduleEdit => schedules
                    .first()
                    .copied()
                    .and_then(|schedule| self.schedule_index.subject(schedule))
                    .is_some_and(|subject| self.begin_schedule_edit(subject)),
                MenuAction::SchedulePause => {
                    let subjects = schedules
                        .iter()
                        .filter_map(|schedule| self.schedule_index.subject(*schedule))
                        .collect::<Vec<_>>();
                    let mut queued = false;
                    for subject in subjects {
                        queued |= self.schedule_index.pause(
                            koru_delegate_api::wire::PauseScheduleRequest {
                                schedule: subject.schedule,
                                workspace: subject.workspace,
                                expected_revision: subject.definition.revision,
                            },
                        );
                    }
                    queued
                }
                MenuAction::ScheduleResume => {
                    let subjects = schedules
                        .iter()
                        .filter_map(|schedule| self.schedule_index.subject(*schedule))
                        .collect::<Vec<_>>();
                    let mut queued = false;
                    for subject in subjects {
                        queued |= self.schedule_index.resume(
                            koru_delegate_api::wire::ResumeScheduleRequest {
                                schedule: subject.schedule,
                                workspace: subject.workspace,
                                expected_revision: subject.definition.revision,
                            },
                        );
                    }
                    queued
                }
                MenuAction::ScheduleDelete => self.confirm_delete_schedules(schedules),
                _ => false,
            },
            MenuSubject::AgentSession { session } => match action {
                MenuAction::SessionOpenInNewTab => self.open_session_tab_for_existing(session),
                MenuAction::SessionMoveToNewWindow => self.move_session_to_new_window(session),
                MenuAction::SessionAssignProject(project) => {
                    if let Some(sender) = self.session_verbs() {
                        sender.assign_project(session, project);
                    }
                    true
                }
                MenuAction::SessionTerminate => self.begin_terminate_session(session),
                MenuAction::SessionMoveToProject => false,
                _ => false,
            },
            // All four go through `pending_*`, because `run_menu_action` holds
            // no `&mut WorkspaceDoc` — and because routing every gesture through
            // one drain is what keeps the confirm and the toast unskippable.
            MenuSubject::Nodes(nodes) => match action {
                MenuAction::NewSocialPost if nodes.len() == 1 => self.social_post_from_document(
                    koru_core::DocumentId::from_uuid(nodes[0].as_uuid()),
                ),
                MenuAction::OpenInNewTab if !nodes.is_empty() => {
                    let mut any = false;
                    for node in nodes {
                        any |= self.request_new_tab_document(node);
                    }
                    any
                }
                MenuAction::OpenDocumentInNewWindow if !nodes.is_empty() => {
                    self.request_documents_in_new_window(nodes)
                }
                MenuAction::Rename if nodes.len() == 1 => {
                    self.pending_document_rename = Some((
                        koru_core::DocumentId::from_uuid(nodes[0].as_uuid()),
                        Self::gesture_deadline(),
                    ));
                    let asked = self.request_store_document_here(nodes[0], true);
                    if !asked {
                        self.pending_document_rename = None;
                    }
                    asked
                }
                // Same asynchronous swap as Rename: the picker needs the
                // document's root node, which exists only once the shell has
                // loaded the document and pushed it back — and the swap is
                // drained by [`finish_explore_gesture`](Self::finish_explore_gesture),
                // which is where the answer to *"and if it is already open?"*
                // is written down.
                MenuAction::AttributeTags
                | MenuAction::AttributeColor
                | MenuAction::AttributeDue
                | MenuAction::AttributePriority
                    if nodes.len() == 1 =>
                {
                    let kind = match action {
                        MenuAction::AttributeTags => AttrPickerKind::Tags,
                        MenuAction::AttributeColor => AttrPickerKind::Color,
                        MenuAction::AttributeDue => AttrPickerKind::Due,
                        _ => AttrPickerKind::Priority,
                    };
                    self.pending_document_attrs = Some((
                        koru_core::DocumentId::from_uuid(nodes[0].as_uuid()),
                        kind,
                        Self::gesture_deadline(),
                    ));
                    let asked = self.request_store_document_here(nodes[0], true);
                    if !asked {
                        self.pending_document_attrs = None;
                    }
                    asked
                }
                MenuAction::MoveTo | MenuAction::MoveNodesTo(_) if !nodes.is_empty() => {
                    let ids = nodes
                        .into_iter()
                        .map(|n| koru_core::DocumentId::from_uuid(n.as_uuid()))
                        .collect();
                    self.open_move_to(ids)
                }
                MenuAction::ExportFormat(format) => {
                    let (documents, _) = self.split_documents(nodes);
                    if documents.is_empty() {
                        return false;
                    }
                    self.ask_pane(PaneRequest::ExportDocuments {
                        ids: documents,
                        format,
                    })
                }
                MenuAction::MoveToTrash if nodes.len() == 1 => self.queue_trash(nodes),
                MenuAction::MoveNodesToTrash(count) if count > 1 && count == nodes.len() => {
                    self.queue_trash(nodes)
                }
                _ => false,
            },
            MenuSubject::Files(files) => {
                if matches!(action, MenuAction::MoveToTrash if files.len() == 1)
                    || matches!(
                        action,
                        MenuAction::MoveFilesToTrash(count)
                            if count > 1 && count == files.len()
                    )
                {
                    return self.queue_trash(files);
                }
                if action == MenuAction::Rename && files.len() == 1 {
                    return self.start_file_rename(files[0]);
                }
                let files: Vec<_> = files
                    .into_iter()
                    .map(|id| koru_core::DocumentId::from_uuid(id.as_uuid()))
                    .collect();
                if files.is_empty() {
                    return false;
                }
                match action {
                    // *Add* and *Open* are the same ask: the store is
                    // idempotent, so a note another window made between the
                    // menu opening and the choice is simply opened.
                    MenuAction::AddNote | MenuAction::OpenNote if files.len() == 1 => {
                        self.ask_pane(PaneRequest::OpenFileNote(files[0]))
                    }
                    MenuAction::OpenFileInApp if files.len() == 1 => {
                        self.ask_pane(PaneRequest::OpenFile(files[0]))
                    }
                    MenuAction::OpenFileInApp => self.ask_pane(PaneRequest::OpenFiles(files)),
                    MenuAction::SaveFileAs if files.len() == 1 => {
                        self.ask_pane(PaneRequest::SaveFile(files[0]))
                    }
                    MenuAction::SaveFileAs => self.ask_pane(PaneRequest::SaveFiles(files)),
                    _ => false,
                }
            }
            MenuSubject::Task { document, node } => match action {
                MenuAction::TaskGoToOrigin => self.open_task_node(document, node, true),
                MenuAction::TaskGoToList => self
                    .task_row(document, node)
                    .and_then(|row| row.list)
                    .is_some_and(|list| self.go_to_task_list(list)),
                MenuAction::TaskCreateDocument => {
                    self.queue_task_command(
                        koru_workspace_host::tasks::TaskCommand::CreateHome { document, node },
                        true,
                    );
                    true
                }
                MenuAction::TaskComplete | MenuAction::TaskReopen => {
                    // The row control's queue, so *Complete* from the menu and a
                    // click on the tick are one call rather than two that agree.
                    self.queue_task_command(
                        koru_workspace_host::tasks::TaskCommand::Complete {
                            document,
                            node,
                            completed: matches!(action, MenuAction::TaskComplete),
                        },
                        true,
                    );
                    true
                }
                // The seventh canonical command, through the one queue every
                // other Tasks write uses — so the row, the rail's numbers and
                // the wait all see it the way they see a tick.
                MenuAction::TaskPinToday | MenuAction::TaskUnpinToday => {
                    self.queue_task_command(
                        koru_workspace_host::tasks::TaskCommand::PinToday {
                            document,
                            node,
                            pinned: matches!(action, MenuAction::TaskPinToday),
                        },
                        true,
                    );
                    true
                }
                MenuAction::TaskDue => self.open_task_due_picker(document, node),
                MenuAction::TaskPriority => self.open_task_priority_picker(document, node),
                MenuAction::TaskList => self.open_task_list_picker(document, node),
                // The task's title becomes the objective's first line, and
                // the task's own row becomes the context.
                MenuAction::TaskDelegate => {
                    let title = self
                        .task_row(document, node)
                        .map(|row| row.title)
                        .unwrap_or_default();
                    self.begin_delegation(delegate::DelegationOrigin::TaskContextMenu {
                        task: delegate::TaskSubject {
                            document,
                            node,
                            title,
                        },
                    })
                }
                MenuAction::TaskRemove => self.ask_remove_from_tasks(document, node),
                _ => false,
            },
            // The rail's three verbs. Two of them are performed here and go up
            // to the shell on the next sweep; the third puts a question up and
            // performs nothing at all until it is answered.
            MenuSubject::RailTarget(target) => match action {
                MenuAction::RailOpenInNewTab => self.open_tasks_tab(target, true),
                _ => false,
            },
            MenuSubject::List(id) => match action {
                MenuAction::RailOpenInNewTab => {
                    self.open_tasks_tab(crate::panes::explore::rail::RailTarget::List(id), true)
                }
                MenuAction::ListRename => self.begin_list_rename(id),
                MenuAction::ListSwatch(token) => self.apply_tasks_rail(
                    crate::panes::explore::rail::TasksRailRequest::RecolorList {
                        id,
                        color: token.to_owned(),
                    },
                ),
                MenuAction::ListDelete => self.ask_delete_list(id),
                // Opening a submenu is not an action; the hover path does it.
                MenuAction::ListColor => false,
                _ => false,
            },
            MenuSubject::Trashed(nodes) => match action {
                MenuAction::Restore => {
                    self.pending_restore = Some(nodes);
                    true
                }
                MenuAction::DeletePermanently => {
                    self.pending_delete_forever = Some((nodes, false));
                    true
                }
                _ => false,
            },
            MenuSubject::Trash => match action {
                MenuAction::EmptyTrash => {
                    self.pending_empty_trash = Some(false);
                    true
                }
                _ => false,
            },
            // Straight to the shell, the same requests the four caption buttons
            // raised. A view holds no `Window`, so it asks and the shell performs
            // it inside this press — see [`ask_window`].
            MenuSubject::Window => match action {
                MenuAction::WindowAlwaysOnTop | MenuAction::WindowUnpinTop => {
                    self.ask_window(WindowRequest::TogglePin)
                }
                // Straight to the shell, which owns the transform, the step
                // and the bounds — the same line the Ctrl+= / Ctrl+- / Ctrl+0
                // chords reach.
                MenuAction::ZoomOut => self.ask_pane(PaneRequest::Zoom(ZoomIntent::Out)),
                MenuAction::ZoomIn => self.ask_pane(PaneRequest::Zoom(ZoomIntent::In)),
                MenuAction::ZoomActualSize => {
                    self.ask_pane(PaneRequest::Zoom(ZoomIntent::ActualSize))
                }
                MenuAction::HeaderSettings => self.open_settings_tab(),
                MenuAction::HeaderFeedback => self.open_send_feedback_tab(),
                MenuAction::HeaderSupport => {
                    self.ask_pane(PaneRequest::OpenSupport);
                    true
                }
                MenuAction::HeaderUpdates => {
                    self.updates_modal = true;
                    self.updates_modal_outside_press = false;
                    true
                }
                MenuAction::HeaderStartRecording => self.open_recording_modal(),
                // Stop is the same toggle F9 is: one path, two gestures.
                MenuAction::HeaderStopRecording => self.toggle_recording(),
                MenuAction::WindowSnapLeft => self.ask_window(WindowRequest::Place(
                    snap::Placement::Half(snap::Side::Left),
                )),
                MenuAction::WindowSnapRight => self.ask_window(WindowRequest::Place(
                    snap::Placement::Half(snap::Side::Right),
                )),
                MenuAction::WindowResetPosition => {
                    self.ask_window(WindowRequest::Place(snap::Placement::Reset))
                }
                // Opening a submenu is not an action; the hover path does it.
                MenuAction::WindowArrange => false,
                _ => false,
            },
        }
    }

    /// Open the window-menu button's menu, hanging from the button's bottom edge.
    ///
    /// [`MenuAnchor::ButtonRight`] rather than `Cursor`: the ⋮ button sits in
    /// the window's top-right corner, so a menu hanging right from it would
    /// immediately hit the edge and clamp to somewhere with no relationship to
    /// the button pressed. Its rows are built from this window's state now, and
    /// fixed — see `context_menu`'s header.
    fn open_window_menu(&mut self, button: Rect) -> bool {
        if self.support_only {
            // No menu in the support window; the slot is the pin, and the
            // press reaches it through `run_icon_action`. The hover path
            // lands here too and must open nothing.
            return false;
        }
        let mut m = ContextMenu::aligned(
            &mut self.engine,
            MenuSubject::Window,
            (button.x + button.w, button.y + button.h),
            context_menu::window_items(
                self.pinned,
                self.maximized,
                self.recording_lifecycle.is_some(),
            ),
            MenuAnchor::ButtonRight,
        );
        let enabled = self.enabled_items(&m);
        m.step(&enabled, 1);
        self.menu = Some(m);
        true
    }

    /// Keep an open window menu hanging off its `⋯` button after a zoom step.
    ///
    /// The anchor was captured in the viewport as it was, and zoom divides the
    /// viewport — so the button moves and a menu holding its old point walks
    /// away from it. Ordinarily that never happens while a menu is up; the
    /// menu's own zoom stepper is exactly the case where it does, on every
    /// press.
    fn follow_window_menu_button(&mut self) {
        if !self.window_menu_open() {
            return;
        }
        let Some(button) = self.window_menu_button().map(|control| control.rect()) else {
            return;
        };
        if let Some(menu) = self.menu.as_mut() {
            menu.set_anchor((button.x + button.w, button.y + button.h));
        }
    }

    /// Whether the caption's window-menu button is currently showing its menu.
    ///
    /// Asked by drawing (which glyph, and whether the hover is red) and by
    /// `mouse_down` (whether this press is a close rather than an open).
    fn window_menu_open(&self) -> bool {
        matches!(
            self.menu.as_ref().map(|m| &m.subject),
            Some(MenuSubject::Window)
        )
    }

    /// Open the submenu on row `i` of the open menu, if that row has one.
    ///
    /// **Prefer the right, fall back left.** Explore menus sit on the left of
    /// the window and need room to open outward; the caption window menu is
    /// often already against the right edge, where a right hang would clamp on
    /// top of the parent — so left is the fallback. See
    /// [`ContextMenu::as_submenu`].
    fn open_submenu(&mut self, i: usize) -> bool {
        if self.submenu.as_ref().is_some_and(|(at, _)| *at == i) {
            return false;
        }
        self.nested = None;
        let Some(parent) = self.menu.clone() else {
            return false;
        };
        let Some(items) = parent
            .items()
            .get(i)
            .copied()
            .and_then(context_menu::submenu)
        else {
            self.submenu = None;
            return true;
        };
        // Align the flyout with the row that owns it.  Aligning it with the
        // parent panel's top made a short submenu effectively unreachable when
        // it had to open to the left: the diagonal trip from a lower parent row
        // crossed an unrelated row first, which immediately closed the
        // flyout.  A row-aligned flyout has a continuous horizontal handoff on
        // either side of the parent.
        let row = parent.item_rect(self.viewport, i);
        // Same subject as the parent: the submenu only names the verb
        // (Markdown, Snap Left, …); the operands still live on the subject.
        let mut m = ContextMenu::as_submenu(
            &mut self.engine,
            parent.subject.clone(),
            row,
            items,
            self.viewport,
        );
        m.hover = None;
        self.submenu = Some((i, m));
        true
    }

    /// Open the nested flyout on row `i` of the open submenu, if that row has
    /// one — *Active ▸* under Shell or Agents.
    fn open_nested_submenu(&mut self, i: usize) -> bool {
        if self.nested.as_ref().is_some_and(|(at, _)| *at == i) {
            return false;
        }
        let Some((_, parent)) = self.submenu.clone() else {
            return false;
        };
        let Some(items) = parent
            .items()
            .get(i)
            .copied()
            .and_then(context_menu::submenu)
            .filter(|items| !items.is_empty())
        else {
            self.nested = None;
            return true;
        };
        let row = parent.item_rect(self.viewport, i);
        let mut m = ContextMenu::as_submenu(
            &mut self.engine,
            parent.subject.clone(),
            row,
            items,
            self.viewport,
        );
        m.hover = None;
        self.nested = Some((i, m));
        true
    }

    /// Whether a point is inside the open submenu's panel.
    fn in_submenu(&self, x: f32, y: f32) -> bool {
        self.submenu
            .as_ref()
            .is_some_and(|(_, m)| m.panel(self.viewport).contains(x, y))
    }

    /// Whether a point is inside the nested flyout hanging off the submenu.
    fn in_nested(&self, x: f32, y: f32) -> bool {
        self.nested
            .as_ref()
            .is_some_and(|(_, m)| m.panel(self.viewport).contains(x, y))
    }

    /// Whether nothing modal is up. **A hover must not open a menu over a dialog
    /// the user has been asked to answer** — a press cannot reach the button
    /// then, and hovering must not become the way round that.
    fn modal_free(&self) -> bool {
        self.error.is_none()
            && self.import_dialog.is_none()
            && self.confirm.is_none()
            && self.new_document.is_none()
            && self.snapshot_prompt.is_none()
            && self.link_device.is_none()
            && self.workspace_icon.is_none()
            && self.workspace_rename.is_none()
            && !self.update_newer_modal_open()
            && !self.updates_modal
            && self.recording_modal.is_none()
    }

    /// Open on hover, close when the pointer leaves both the button and the
    /// panel. Returns whether that changed anything.
    ///
    /// **The panel is not "outside".** It hangs directly under the button, so
    /// the walk from one to the other is continuous — a menu that closed the
    /// moment the pointer left the button would be unreachable.
    fn track_window_menu_hover(&mut self, x: f32, y: f32) -> bool {
        let button = self.window_menu_button();
        let on_button = button.is_some_and(|button| button.contains(x, y));
        // **The hover is recorded here, not by the general pass.** Both this
        // function and the "a menu is up, track its rows" branch return before
        // that pass runs, so leaving it to them meant the button never lit at
        // all — no neutral tint as a ⋮ button, and worse, **no red as an ✕**,
        // which is the one signal telling you the button now closes the window.
        let want = if on_button {
            Some(Hit::Window(CaptionAction::WindowMenu))
        } else if self.hover == Some(Hit::Window(CaptionAction::WindowMenu)) {
            // Onto a row, or away entirely: the ✕ stops promising a close that
            // is no longer what the pointer is standing on.
            None
        } else {
            self.hover
        };
        // Tracked as a change of its own, or the tint would not be painted
        // until something else happened to ask for a frame.
        let lit = self.hover != want;
        self.hover = want;
        if self.window_menu_open() {
            let panel = self.menu.as_ref().map(|m| m.panel(self.viewport));
            let in_panel = panel.is_some_and(|r| r.contains(x, y));
            // **The gap between the button and its panel is not "outside".**
            // The panel hangs a few pixels below the button, and crossing that
            // strip was closing the menu mid-reach — you had to be quick to get
            // there, which is not a thing an interface should ask. The corridor
            // spans both rects horizontally so a diagonal approach counts too.
            let in_corridor =
                panel
                    .zip(button.map(|button| button.rect()))
                    .is_some_and(|(p, b)| {
                        let left = p.x.min(b.x);
                        let right = (p.x + p.w).max(b.x + b.w);
                        let top = b.y + b.h;
                        x >= left && x <= right && y >= top && y <= p.y
                    });
            if !on_button
                && !in_panel
                && !in_corridor
                && !self.in_submenu(x, y)
                && !self.in_nested(x, y)
            {
                self.menu = None;
                self.submenu = None;
                self.nested = None;
                return true;
            }
            return lit;
        }
        if on_button && self.menu.is_none() && self.modal_free() {
            // **No pre-highlighted row.** `open_node_menu` steps to the first
            // enabled item because a keyboard user needs somewhere to start;
            // opening by hover, that would light a row the pointer is nowhere
            // near and hold it lit for as long as you stood still.
            self.open_window_menu(button.expect("on_button implies a button").rect());
            if let Some(m) = self.menu.as_mut() {
                m.hover = None;
            }
            return true;
        }
        lit
    }

    /// Open a library row's menu — from its `…`, or from a right-click.
    ///
    /// The `…` anchors the menu under the button; a right-click anchors it at
    /// the cursor. Same rule the workspace switcher's menu follows.
    fn open_node_menu(&mut self, nodes: Vec<NodeId>, at: (f32, f32), from_button: bool) -> bool {
        let align = if from_button {
            MenuAnchor::ButtonRight
        } else {
            MenuAnchor::Cursor
        };
        let count = nodes.len();
        let files = self.explore.navigator_mode() == crate::panes::explore::NavigatorMode::Files;
        // *Add note* versus *Open note* is decided now, once — the menu is
        // fixed at open time, so it reads the listing as it is this moment.
        // A multi-selection gets neither: a note attaches to one file.
        let note = if files && count == 1 {
            self.entries.as_ref().and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry.id.as_uuid() == nodes[0].as_uuid())
                    .map(|entry| entry.note.is_some())
            })
        } else {
            None
        };
        let mut m = ContextMenu::aligned(
            &mut self.engine,
            if files {
                MenuSubject::Files(nodes)
            } else {
                MenuSubject::Nodes(nodes)
            },
            at,
            if files {
                context_menu::file_items(count, note)
            } else {
                context_menu::node_items(count)
            },
            align,
        );
        let enabled = self.enabled_items(&m);
        m.step(&enabled, 1);
        self.menu = Some(m);
        true
    }

    /// Record one or many explicit Trash requests in tree order. Menus and
    /// drag-to-Trash share this queue so confirmation, undo, tab closing and
    /// toasts still have one execution path in `drain_trash_all`.
    /// Route *Move to trash* to whichever bin the subject belongs in.
    ///
    /// **An Explore row's `NodeId` is a `DocumentId`'s bytes**, so the id alone
    /// cannot say which — the *list* can, and does: a row is a document exactly
    /// when it is one of `entries`. Exact rather than heuristic, since a
    /// colliding id would have to be a v4 uuid minted for a document in this
    /// very workspace.
    ///
    /// Getting this wrong is what made *Move to trash* do nothing: the ids went
    /// to `ws.trash(node)`, which looks for a node in the *open document*, found
    /// none, and failed in silence.
    ///
    /// A document goes to the shell, which owns the manifest. A node keeps the
    /// existing path, confirm and toast included. A mixed selection is not a
    /// thing the explore column can produce — its rows are all documents — so it
    /// is split rather than refused, and each half goes where it belongs.
    /// Split ids into the documents this workspace holds and everything else.
    ///
    /// **The library *and* the bin**, because a Trash row names a document that
    /// is by definition not in the library — a split that only consulted
    /// `entries` would call every trashed row an ordinary node and send it down
    /// a path that cannot find it.
    ///
    /// Exact rather than heuristic: a colliding id would have to be a v4 uuid
    /// minted for a document in this very workspace.
    pub(super) fn split_documents(
        &self,
        nodes: Vec<NodeId>,
    ) -> (Vec<koru_core::DocumentId>, Vec<NodeId>) {
        let known = |n: &NodeId| {
            let here = |list: &Option<Vec<koru_core::explore::ExploreEntry>>| {
                list.as_ref()
                    .is_some_and(|l| l.iter().any(|e| e.id.as_uuid() == n.as_uuid()))
            };
            here(&self.entries) || here(&self.trashed_documents)
        };
        let (documents, rest): (Vec<NodeId>, Vec<NodeId>) = nodes.into_iter().partition(known);
        (
            documents
                .into_iter()
                .map(|n| koru_core::DocumentId::from_uuid(n.as_uuid()))
                .collect(),
            rest,
        )
    }

    /// How many documents the armed drag is carrying, from the manifest when
    /// there is one.
    ///
    /// The explore column counts through the document *tree*, which store-backed
    /// holds no documents at all — so it answered for a shape that is not there.
    /// The count and the confirm come from one function so they cannot quote
    /// different numbers at each other.
    fn refresh_drag_item_count(&mut self, ws: &WorkspaceDoc) {
        let Some(entries) = self.entries.clone() else {
            self.explore.refresh_drag_item_count(ws);
            return;
        };
        let (roots, _) = self.split_documents(self.explore.dragged_roots());
        self.explore
            .set_drag_item_count(koru_core::explore::documents_moved(&roots, &entries));
    }

    fn queue_trash(&mut self, nodes: Vec<NodeId>) -> bool {
        // **One queue, whatever the subject is.** Documents used to be sent
        // straight to the shell from here, which skipped `drain_trash` — and
        // with it the undo toast that lives there. AGENTS.md already records why
        // every gesture goes through one drain; this is that rule, re-learned.
        // `drain_trash` decides which bin an id belongs in, because it is the
        // one that has the workspace.
        //
        // **The confirm is here, though, and asks once about the whole
        // gesture.** Per document it asked four times for one section with four
        // pages under it, and never at all for four pages selected side by side.
        // What somebody is agreeing to is how many documents are about to move.
        self.confirm_then_queue(nodes)
    }

    /// Ask before binning more than [`CONFIRM_ABOVE`] documents, then queue.
    ///
    /// [`CONFIRM_ABOVE`]: crate::mainview::trash::CONFIRM_ABOVE
    fn confirm_then_queue(&mut self, nodes: Vec<NodeId>) -> bool {
        let Some(entries) = self.entries.clone() else {
            return self.queue_trash_nodes(nodes);
        };
        let (documents, _) = self.split_documents(nodes.clone());
        let moving = koru_core::explore::documents_moved(&documents, &entries);
        if moving > crate::mainview::trash::CONFIRM_ABOVE {
            self.confirm = Some((
                koru_ui::Confirm::new(
                    format!("Move {moving} items to Trash?"),
                    "They will be moved together and can be restored from Trash.".to_owned(),
                    "Move to Trash",
                ),
                crate::mainview::trash::ConfirmSubject::TrashDocuments(nodes),
            ));
            return true;
        }
        self.trash_now(nodes)
    }

    /// Bin these, the question already settled.
    ///
    /// Documents go as a **set**; nodes keep the one-at-a-time queue, because a
    /// node's confirm is about that node while a document's was about the whole
    /// gesture.
    pub(super) fn trash_now(&mut self, nodes: Vec<NodeId>) -> bool {
        let (documents, rest) = self.split_documents(nodes);
        let mut acted = false;
        if !documents.is_empty() {
            acted |= self.trash_documents_now(documents);
        }
        if !rest.is_empty() {
            acted |= self.queue_trash_nodes(rest);
        }
        acted
    }

    /// Bin a whole set of documents: one ask, one toast, one Undo.
    ///
    /// **One of each, because the gesture was one.** Doing this per document
    /// gave each its own toast — so nine of them left an Undo that took back the
    /// last and abandoned eight — and, before `ask_pane` became a queue, nine
    /// asks of which only the last survived.
    fn trash_documents_now(&mut self, documents: Vec<koru_core::DocumentId>) -> bool {
        let entries = self.entries.clone().unwrap_or_default();
        let moving = koru_core::explore::documents_moved(&documents, &entries);
        let name = documents
            .first()
            .and_then(|id| entries.iter().find(|e| e.id == *id))
            .map(|e| {
                if e.title.is_empty() {
                    "Untitled".to_owned()
                } else {
                    e.title.clone()
                }
            })
            .unwrap_or_else(|| "Untitled".to_owned());

        let rows: Vec<NodeId> = documents
            .iter()
            .map(|id| NodeId::from_uuid(id.as_uuid()))
            .collect();
        self.ask_pane(koru_workspace_host::PaneRequest::TrashDocuments(documents));
        for node in &rows {
            // Remembered so the *In Trash* notice does not appear in the window
            // that did the binning (ADR-70). It is for a peer, not for you.
            self.trashed_here.insert(*node);
            self.close_tabs_showing_document(*node);
        }
        self.explore_stale = true;
        self.toast = Some((
            koru_ui::Toast::new(
                crate::mainview::trash::trashed_batch_message(&name, rows.len(), moving),
                Some("Undo"),
                std::time::Instant::now(),
            ),
            // **Every root, so Undo takes back everything it offered.** Only the
            // roots: a child restored on its own would come back beside its
            // parent rather than inside it, and restoring the parent brings it
            // anyway.
            ToastPayload::Restore(rows),
        ));
        true
    }

    /// Close tabs over a binned **document**, by its id rather than by a node.
    fn close_tabs_showing_document(&mut self, node: NodeId) {
        let id = koru_core::DocumentId::from_uuid(node.as_uuid());
        let doomed: Vec<usize> = (0..self.tabs().len())
            .rev()
            .filter(|&i| self.tabs().get(i).and_then(|t| t.document) == Some(id))
            .collect();
        for i in doomed {
            self.close(i);
        }
    }

    fn queue_trash_nodes(&mut self, nodes: Vec<NodeId>) -> bool {
        let mut nodes = nodes.into_iter();
        self.pending_trash = nodes.next().map(|id| (id, false));
        self.trash_queue.extend(nodes);
        self.pending_trash.is_some()
    }

    /// The keys an open context menu answers to. Only these four are taken; any
    /// other key is ignored here (returns `true`) but still swallowed by the
    /// guard in [`key`](View::key), so typing does not reach the view behind the
    /// menu. `Esc` always closes; the arrows move between enabled items,
    /// skipping dimmed ones and wrapping; `Enter` runs the highlighted one.
    fn menu_key(&mut self, key: &Key) -> bool {
        use winit::keyboard::NamedKey;
        let Some(menu) = self.menu.clone() else {
            return false;
        };
        // **While a nested flyout is open the keys are its own.** Esc/Left
        // back out one level, the same rule the first submenu follows.
        if let Some((at, nested)) = self.nested.clone() {
            let enabled = self.enabled_items(&nested);
            return match key {
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::ArrowLeft) => {
                    self.nested = None;
                    if let Some((_, m)) = self.submenu.as_mut() {
                        m.hover = Some(at);
                    }
                    true
                }
                Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                    let delta = if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                        1
                    } else {
                        -1
                    };
                    if let Some((_, m)) = self.nested.as_mut() {
                        m.step(&enabled, delta);
                    }
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(i) = nested.hover.filter(|&i| enabled.get(i) == Some(&true)) {
                        let action = nested.items()[i];
                        self.nested = None;
                        self.submenu = None;
                        self.menu = None;
                        self.run_menu_action(nested.subject, action);
                    }
                    true
                }
                _ => true,
            };
        }
        // **While a submenu is open the keys are its own.** Anything else would
        // move the highlight in a list the pointer is no longer looking at.
        if let Some((at, sub)) = self.submenu.clone() {
            let enabled = self.enabled_items(&sub);
            return match key {
                // Esc and Left back out one level, leaving the parent standing
                // with its own row still highlighted — the same "one Esc, one
                // level" rule the menu already follows against the popover.
                Key::Named(NamedKey::Escape) | Key::Named(NamedKey::ArrowLeft) => {
                    self.nested = None;
                    self.submenu = None;
                    if let Some(m) = self.menu.as_mut() {
                        m.hover = Some(at);
                    }
                    true
                }
                Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                    let delta = if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                        1
                    } else {
                        -1
                    };
                    self.nested = None;
                    if let Some((_, m)) = self.submenu.as_mut() {
                        m.step(&enabled, delta);
                    }
                    true
                }
                Key::Named(NamedKey::ArrowRight) => {
                    if let Some(i) = sub.hover {
                        self.open_nested_submenu(i);
                        if let Some((_, m)) = self.nested.as_mut() {
                            let inner: Vec<bool> =
                                m.items().iter().map(|a| a.selectable()).collect();
                            m.step(&inner, 1);
                        }
                    }
                    true
                }
                Key::Named(NamedKey::Enter) => {
                    if let Some(i) = sub.hover.filter(|&i| enabled.get(i) == Some(&true)) {
                        let action = sub.items()[i];
                        if context_menu::submenu(action).is_some() {
                            self.open_nested_submenu(i);
                            if let Some((_, m)) = self.nested.as_mut() {
                                let inner: Vec<bool> =
                                    m.items().iter().map(|a| a.selectable()).collect();
                                m.step(&inner, 1);
                            }
                            return true;
                        }
                        self.nested = None;
                        self.submenu = None;
                        self.menu = None;
                        self.run_menu_action(sub.subject, action);
                    }
                    true
                }
                _ => true,
            };
        }
        let enabled = self.enabled_items(&menu);
        match key {
            Key::Named(NamedKey::Escape) => {
                // **Only the menu.** A menu opened over the switcher leaves it
                // standing, so the first Esc puts you back where you were rather
                // than dismissing both and losing your place in the list.
                self.menu = None;
                self.submenu = None;
                self.nested = None;
                true
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                let delta = if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                    1
                } else {
                    -1
                };
                if let Some(m) = self.menu.as_mut() {
                    m.step(&enabled, delta);
                }
                true
            }
            // Right enters the submenu on the highlighted row, landing on its
            // first item — the mirror of Left backing out of one.
            Key::Named(NamedKey::ArrowRight) => {
                if let Some(i) = menu.hover {
                    self.open_submenu(i);
                    if let Some((_, m)) = self.submenu.as_mut() {
                        let inner: Vec<bool> = m.items().iter().map(|a| a.selectable()).collect();
                        m.step(&inner, 1);
                    }
                }
                true
            }
            Key::Named(NamedKey::Enter) => {
                // Enter on a row that owns a submenu opens it rather than doing
                // nothing, which is what "it has no action" would otherwise mean.
                if let Some(i) = menu.hover.filter(|&i| enabled.get(i) == Some(&true)) {
                    let action = menu.items()[i];
                    if context_menu::submenu(action).is_some() {
                        self.open_submenu(i);
                        if let Some((_, m)) = self.submenu.as_mut() {
                            let inner: Vec<bool> =
                                m.items().iter().map(|a| a.selectable()).collect();
                            m.step(&inner, 1);
                        }
                        return true;
                    }
                    self.menu = None;
                    self.run_menu_action(menu.subject, action);
                }
                true
            }
            _ => true,
        }
    }

    /// The workspaces this window knows about, as the shell last described them.
    ///
    /// A **cache** of a document another process also writes, refreshed on a
    /// poll — which is why anything derived from it is keyed by `WorkspaceId`
    /// and re-read rather than remembered.
    fn workspaces(&self) -> &[WorkspaceEntry] {
        self.pane_context.as_ref().map_or(&[], |(_, _, w, _)| w)
    }

    fn profiles(&self) -> &[ProfileSummary] {
        self.pane_context.as_ref().map_or(&[], |(p, _, _, _)| p)
    }

    /// The artifacts column's own pass: which workspace it is about, the
    /// clock its relative times read against, and whatever its reader
    /// answered.
    ///
    /// **Only while it is showing.** A column nobody is looking at asks the
    /// gateway for nothing and polls nothing — which is what *hidden and
    /// idle surfaces do no timer-driven work* means for a surface that owns
    /// a reader.
    fn maintain_artifacts(&mut self) -> bool {
        if !self.explore.showing_artifacts() {
            return false;
        }
        self.explore.start_artifacts(crate::search::gateway_root());
        let workspace = self
            .current_workspace()
            .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()));
        let now = epoch_millis();
        let surface = self.explore.artifacts_mut();
        surface.set_now(now);
        let asked = surface.set_workspace(workspace);
        let answered = surface.poll();
        // **The column asks; this window opens** (ADR-369). Which tab strip
        // a document lands in was never a column's to decide, and the tab it
        // lands in is the one primitive every surface that names a result
        // reaches.
        let opened = match self.explore.artifacts_mut().take_wants_open() {
            Some(artifact) => self.open_artifact_tab(artifact),
            None => false,
        };
        asked || answered || opened
    }

    /// **The Chats view's own pass** (AGENTUX I5).
    ///
    /// Every chat surface in this window — the right-hand pane and each AI
    /// chat tab — says when the chat list changed (a chat made, renamed,
    /// deleted, re-pointed, answered). Draining them is a flag each, so it
    /// happens whether or not the view is showing, and the view is asked to
    /// refresh only when it is: **told, never polled**. The same pass hears a
    /// *Chats* button pressed inside a chat, which in this window means *show
    /// me the Chats view on this chat*.
    fn maintain_chats(&mut self) -> bool {
        let mut list_changed = self.ai_chat.take_chat_list_changed();
        let mut wants_view = self
            .ai_chat
            .take_wants_chats_view()
            .then(|| self.ai_chat.current_chat_id());
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::AiChat(view) = &mut tab.view {
                    list_changed |= view.take_chat_list_changed();
                    if view.take_wants_chats_view() {
                        wants_view = Some(view.chat_id());
                    }
                }
            }
        }
        let mut changed = false;
        if let Some(chat) = wants_view {
            changed |= self.show_chats_on(chat);
        }
        if !self.explore.showing_chats() {
            return changed;
        }
        self.explore.start_chats(crate::search::gateway_root());
        // The open chat wears the card: the AI chat tab in front, if one is.
        let current = match &self.tabs().active_tab().view {
            TabView::AiChat(view) => view.chat_id(),
            _ => None,
        };
        let surface = self.explore.chats_mut();
        surface.set_now(epoch_millis());
        changed |= surface.set_current(current);
        if list_changed {
            surface.refresh();
        }
        changed |= surface.poll();
        // A chat this view deleted is gone from every tab that showed it:
        // a tab over a deleted chat would answer its next message with an
        // error about something the person already chose to remove.
        for chat in self.explore.chats_mut().take_deleted_chats() {
            changed |= self.close_ai_chat_tabs(chat);
        }
        changed
    }

    /// Show the Chats view, on `chat` when there is one — the *Chats* button
    /// inside a chat, and `Ctrl+Shift+E` from an AI chat tab.
    fn show_chats_on(&mut self, chat: Option<koru_ai_ui::ChatId>) -> bool {
        match chat {
            Some(chat) => self.reveal_in_column(layout::ColumnPlace::Chat(chat)),
            None => self.show_navigator(crate::panes::explore::NavigatorMode::Chats),
        }
    }

    /// Close every AI chat tab in this window seated on `chat`. Back to
    /// front, so closing one does not renumber the next.
    fn close_ai_chat_tabs(&mut self, chat: koru_ai_ui::ChatId) -> bool {
        let doomed: Vec<usize> = (0..self.tabs().len())
            .rev()
            .filter(|&i| {
                matches!(&self.tabs().get(i).map(|tab| &tab.view),
                    Some(TabView::AiChat(view)) if view.chat_id() == Some(chat))
            })
            .collect();
        let closed = !doomed.is_empty();
        for i in doomed {
            self.close(i);
        }
        closed
    }

    /// Put the deletion question up for one chat. **Nothing is sent here** —
    /// the Chats view sends the delete on *yes*, through `settle_confirm`,
    /// which is what makes the dialog the one door. The title is the chat's
    /// own: shown, never logged.
    fn ask_delete_chat(&mut self, chat: koru_ai_ui::ChatId) -> bool {
        let lang = crate::format::lang();
        let title = self
            .explore
            .chats_mut()
            .chats()
            .iter()
            .find(|candidate| candidate.id == chat)
            .map(|candidate| koru_chats_ui::text::title(lang, candidate).to_owned())
            .unwrap_or_default();
        self.confirm = Some((
            koru_ui::Confirm::new(
                koru_i18n::t(lang, koru_i18n::Key::ChatsDeleteTitle),
                koru_i18n::fill(
                    koru_i18n::t(lang, koru_i18n::Key::ChatsDeleteBody),
                    &[&title],
                ),
                koru_i18n::t(lang, koru_i18n::Key::ChatsDelete),
            ),
            trash::ConfirmSubject::DeleteChat(chat),
        ));
        true
    }

    /// A chat row's menu: one verb, *Delete chat*.
    fn open_chat_menu(&mut self, chat: koru_ai_ui::ChatId, at: (f32, f32)) -> bool {
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::AiChat { chat },
            at,
            context_menu::ai_chat_items(),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// The workspace this window is showing.
    fn current_workspace(&self) -> Option<WorkspaceId> {
        self.pane_context.as_ref().and_then(|(_, _, _, id)| *id)
    }

    /// Whether `(x, y)` is inside the switcher popover.
    ///
    /// Three call sites needed this — `mouse_down`, `mouse_move` and
    /// `context_menu` — and the third was written without it, which is exactly
    /// the drift a fourth hand-rolled copy would repeat.
    fn popover_hit(&self, x: f32, y: f32) -> bool {
        self.explore.popover_open()
            && self
                .explore
                .popover_panel(self.explore_popover_area(), self.viewport)
                .is_some_and(|r| r.contains(x, y))
    }

    /// Begin renaming a workspace in place — over its Manage row, where the
    /// field goes (`workspace_rename_row`).
    fn start_workspace_rename(&mut self, id: WorkspaceId) -> bool {
        let Some(name) = self.workspace_facts(id).map(|w| w.name) else {
            return false;
        };
        self.workspace_rename = Some(WorkspaceRename::new(&mut self.engine, id, &name));
        true
    }

    fn start_profile_rename(&mut self, id: ProfileId) -> bool {
        let Some(name) = self
            .profiles()
            .iter()
            .find(|profile| profile.id == id)
            .map(|profile| profile.name.clone())
        else {
            return false;
        };
        self.workspace_rename = Some(WorkspaceRename::for_profile(&mut self.engine, id, &name));
        true
    }

    /// Where the field is drawn, which is the row's name column — **in whichever
    /// list is showing that workspace**.
    ///
    /// The switcher first, because it floats over the manager; then the manager
    /// tab's own row, translated out of the view's coordinates into the window's.
    /// One field serving both is what stops the manager growing a second inline
    /// editor with its own idea of what an acceptable name is.
    fn workspace_rename_row(&self) -> Option<Rect> {
        let target = self.workspace_rename.as_ref()?.target;
        match target {
            RenameTarget::Workspace(id) => {
                if let Some(r) = self.explore.popover_workspace_rect(
                    self.explore_popover_area(),
                    self.viewport,
                    id,
                ) {
                    return Some(r);
                }
            }
            RenameTarget::Profile(id) => {
                if let Some(r) = self.explore.popover_profile_rect(
                    self.explore_popover_area(),
                    self.viewport,
                    id,
                ) {
                    return Some(r);
                }
                // The Manage tab's Profile header, out of the view's
                // coordinates into the window's.
                let TabView::Manage(manage) = &self.tabs().active_tab().view else {
                    return None;
                };
                let local = manage.profile_name_rect(id)?;
                let view = self.focused_view_rect();
                return Some(Rect::new(
                    view.x + local.x,
                    view.y + local.y,
                    local.w,
                    local.h,
                ));
            }
            RenameTarget::File(id) => {
                return self
                    .explore
                    .row_label_rect(self.regions.sidebar, NodeId::from_uuid(id.as_uuid()));
            }
            RenameTarget::Schedule(schedule) => {
                if let Some(row) = self
                    .explore
                    .agent_schedule_label_rect(self.regions.sidebar, schedule)
                {
                    return Some(row);
                }
                // A rename invoked from a schedule pill stays where it was
                // invoked. The same editor and mutation path are reused; only
                // its geometric anchor falls back from the hidden Agents row
                // to the active document's live pill.
                let local = match &self.tabs().active_tab().view {
                    TabView::Leaf(leaf) => leaf.schedule_pill_rect(&schedule.to_storage()),
                    _ => None,
                }?;
                let view = self.focused_view_rect();
                return Some(Rect::new(
                    view.x + local.x,
                    view.y + local.y,
                    local.w,
                    local.h,
                ));
            }
        }
        let RenameTarget::Workspace(id) = target else {
            return None;
        };
        // A Profile page's row, out of the view's coordinates into the
        // window's.
        let TabView::Manage(manage) = &self.tabs().active_tab().view else {
            return None;
        };
        let local = manage.workspace_name_rect(id)?;
        let view = self.focused_view_rect();
        Some(Rect::new(
            view.x + local.x,
            view.y + local.y,
            local.w,
            local.h,
        ))
    }

    /// Every *other* workspace's name **in the same Profile**, so a duplicate
    /// can be refused. Renaming a workspace to what it is already called is not
    /// a clash. A workspace of a Profile that is not open is compared with its
    /// own Profile's workspaces from the catalog, not with the open one's.
    fn other_workspace_names(&self, id: WorkspaceId) -> Vec<String> {
        let siblings = if self.workspaces().iter().any(|w| w.id == id) {
            self.workspaces()
        } else if let Some((owner, _)) = self.explore.catalog_workspace(id) {
            self.explore.catalog_workspaces_of(owner)
        } else {
            self.workspaces()
        };
        siblings
            .iter()
            .filter(|w| w.id != id)
            .map(|w| w.name.clone())
            .collect()
    }

    fn settle_rename(&mut self, outcome: RenameOutcome) -> bool {
        match outcome {
            RenameOutcome::Keep => true,
            RenameOutcome::Cancel => {
                self.workspace_rename = None;
                true
            }
            RenameOutcome::Commit(name) => {
                let target = self.workspace_rename.as_ref().map(|r| r.target);
                match target {
                    Some(RenameTarget::Workspace(id)) => {
                        self.workspace_rename = None;
                        self.ask_pane(PaneRequest::RenameWorkspace(id, name))
                    }
                    Some(RenameTarget::Profile(id)) => {
                        self.workspace_rename = None;
                        self.ask_pane(PaneRequest::RenameProfile(id, name))
                    }
                    Some(RenameTarget::File(id)) => {
                        self.workspace_rename = None;
                        self.ask_pane(PaneRequest::RenameFile(id, name))
                    }
                    Some(RenameTarget::Schedule(schedule)) => {
                        let expected = self
                            .workspace_rename
                            .as_ref()
                            .and_then(WorkspaceRename::schedule_expected)
                            .cloned();
                        let subject = self.schedule_index.subject(schedule);
                        let name = koru_delegate_schedule::ScheduleName::new(name).ok();
                        let queued = match (expected, subject, name) {
                            (Some(expected_name), Some(subject), Some(name)) => self
                                .schedule_index
                                .rename(koru_delegate_api::wire::RenameScheduleRequest {
                                    schedule,
                                    workspace: subject.workspace,
                                    expected_name,
                                    name,
                                }),
                            _ => false,
                        };
                        if let Some(field) = self.workspace_rename.as_mut() {
                            if queued {
                                field.set_sending(true);
                            } else {
                                field.set_hint(koru_i18n::t(
                                    crate::format::lang(),
                                    koru_i18n::Key::ScheduleRenameFailed,
                                ));
                            }
                        }
                        true
                    }
                    None => true,
                }
            }
        }
    }

    /// Show this build's release notes after an update installed it.
    ///
    /// **One per window**, the rule the Trash and the workspace manager follow:
    /// there is one set of notes for one build, so a second tab would be the
    /// same page twice. The automatic path cannot ask twice anyway — the agent
    /// forgets the notice once a window has been told — so this is about the day
    /// somebody adds a menu row that opens it by hand.
    pub(crate) fn open_whats_new(&mut self, version: &str) -> bool {
        let existing = self.tabs().iter().position(|t| t.whats_new.is_some());
        if let Some(i) = existing {
            self.select(i);
            return true;
        }
        // The notes are read here rather than passed in because they are a fact
        // about this installation, not about the message: the agent says *which
        // build*, the file beside the exe says *what changed*.
        let notes = crate::whatsnew::notes_beside_the_exe();
        let Some(tab) = Tab::whats_new(version, notes.as_deref()) else {
            return false;
        };
        self.tabs_mut().push(tab);
        // Pushing changes every tab's *width*. A slide left mid-flight would
        // finish at the old pitch and overshoot, so the animation is dropped
        // rather than retargeted — what `open_trash_tab` does, and why.
        self.slides_mut().clear();
        self.relayout();
        self.select(self.tabs().len() - 1);
        true
    }

    /// Show a finished comparison in the diff tab — one per window at most, a
    /// new comparison replacing what the existing tab shows, on the same
    /// find-or-push shape as [`open_trash_tab`](Self::open_trash_tab).
    fn open_diff_tab(&mut self, comparison: crate::diff::Comparison) -> bool {
        // The diff reads at the compared document's own measure, so capture
        // its page mode and canvas before the tab switch, from the tab the
        // compare was asked in. Anything that is not a document keeps the
        // diff view's previous (or default) style.
        let source_style = match &mut self.tabs_mut().active_mut().view {
            TabView::Leaf(l) => {
                let view = l.view();
                Some((view.view_mode, view.canvas_bg))
            }
            _ => None,
        };
        let existing = self
            .tabs()
            .iter()
            .position(|t| matches!(t.view, TabView::Diff(_)));
        match existing {
            Some(i) => {
                self.select(i);
            }
            None => {
                self.tabs_mut()
                    .push(Tab::diff(crate::diff::DiffView::new(crate::format::lang())));
                self.slides_mut().clear();
                self.relayout();
            }
        }
        if let TabView::Diff(d) = &mut self.tabs_mut().active_mut().view {
            if let Some((mode, canvas)) = source_style {
                d.set_source_style(mode, canvas);
            }
            d.set_comparison(comparison);
        }
        self.size_visible_groups();
        true
    }

    /// Show the Trash, bringing an existing tab forward rather than opening a
    /// second: there is only one bin, so there is only ever one view of it.
    fn open_trash_tab(&mut self) -> bool {
        // Tell it who this window is before it lists anything, so a row this
        // window binned reads "You" rather than repeating your own name at you.
        let actor = self.actor.clone();
        let existing = self
            .tabs()
            .iter()
            .position(|t| matches!(t.view, TabView::Trash(_)));
        match existing {
            Some(i) => {
                self.select(i);
            }
            None => {
                self.tabs_mut().push(Tab::trash());
                self.slides_mut().clear();
                self.relayout();
            }
        }
        let documents = self.trashed_documents.clone();
        if let TabView::Trash(t) = &mut self.tabs_mut().active_mut().view {
            t.set_actor(actor);
            // A tab opened now missed the last push, and nothing pushes again
            // until the manifest changes — so it would sit reading "The Trash is
            // empty" over a bin with things in it.
            if let Some(documents) = documents {
                t.set_documents(documents);
            }
        }
        self.size_visible_groups();
        true
    }

    /// What the Trash tab asked for.
    ///
    /// **One path per verb**, so Restore from the button and Restore from the
    /// menu are the same call — and so the confirmations cannot be skipped by
    /// coming in the other way.
    pub(super) fn drain_trash_request(&mut self) -> bool {
        let request = match &mut self.tabs_mut().active_mut().view {
            TabView::Trash(t) => t.take_request(),
            _ => None,
        };
        let Some(request) = request else {
            return false;
        };
        // **A tab reports in its own coordinates.** The container passes the
        // content rect's *size* as the viewport and subtracts its *origin* from
        // every pointer coordinate before delegating, so anything a view hands
        // back has to come the other way before a menu — which is drawn in
        // window space — can use it. Without this the row menu opened a sidebar
        // and a caption away from the `…` that asked for it.
        let view = self.focused_view_rect();
        let to_window = |at: (f32, f32)| (at.0 + view.x, at.1 + view.y);
        match request {
            crate::trash::Request::Restore(nodes) => {
                self.pending_restore = Some(nodes);
            }
            crate::trash::Request::RowMenu { nodes, at } => {
                let (subject, items) = crate::trash::row_menu_at(nodes);
                self.open_subject_menu(subject, items, to_window(at));
            }
            crate::trash::Request::HeaderMenu { at } => {
                let (subject, items) = crate::trash::header_menu_at();
                self.open_subject_menu(subject, items, to_window(at));
            }
        }
        true
    }

    /// Open a menu anchored under a button. The shape `open_workspace_menu` and
    /// `open_node_menu` both take, hoisted so a third caller cannot invent a
    /// fourth spelling of it.
    pub(super) fn open_subject_menu(
        &mut self,
        subject: MenuSubject,
        items: Vec<MenuAction>,
        at: (f32, f32),
    ) -> bool {
        let mut m = ContextMenu::aligned(
            &mut self.engine,
            subject,
            at,
            items,
            MenuAnchor::ButtonRight,
        );
        let enabled = self.enabled_items(&m);
        m.step(&enabled, 1);
        self.menu = Some(m);
        true
    }

    /// The switcher's *New profile*: the Manage tab, on its New profile page.
    fn open_profile_creation(&mut self) -> bool {
        self.open_manage(crate::manage::Page::NewProfile)
    }

    /// Bring the Manage tab forward on `page`, opening it if there is none.
    ///
    /// A singleton: an existing one is selected rather than a second pushed,
    /// so only one `ProfilesController` is ever live — two would each answer
    /// the same account results and each drain their own Agent commands.
    pub(crate) fn open_manage(&mut self, page: crate::manage::Page) -> bool {
        let existing = self
            .tabs()
            .iter()
            .position(|tab| matches!(tab.view, TabView::Manage(_)));
        if let Some(index) = existing {
            self.select(index);
        } else {
            self.tabs_mut().push(Tab::manage());
            self.slides_mut().clear();
            self.relayout();
        }
        self.push_manage_profiles_context();
        if let TabView::Manage(view) = &mut self.tabs_mut().active_mut().view {
            view.show(page);
        }
        self.drain_manage_profiles_request();
        // A Profile's page lists its workspaces from the controller's link.
        self.arm_workspaces_poll();
        true
    }

    /// The switcher's *Manage profiles…*: Manage on the open Profile's page.
    /// Before the
    /// window knows its Profile, Koru Cloud — the account-wide page.
    fn open_manage_profiles(&mut self) -> bool {
        let page = self
            .pane_context
            .as_ref()
            .map_or(crate::manage::Page::Cloud, |(_, current, _, _)| {
                crate::manage::Page::Profile(*current)
            });
        self.open_manage(page)
    }

    /// Hand the Manage tab what the window knows: the Profiles, the open one
    /// and its Workspaces, the account, the retained and hidden Profiles, the
    /// Workspace counts and how many devices are linked.
    fn push_manage_profiles_context(&mut self) -> bool {
        let Some((profiles, current, workspaces, open_workspace)) = self.pane_context.clone()
        else {
            return false;
        };
        let counts = self.profile_workspace_counts.clone();
        let account_state = self.account_state.clone();
        let retained_profiles = self.retained_profiles.clone();
        let hidden_profile_count = self.hidden_profile_count;
        let linked_devices = self.linked_devices.clone();
        let push = |c: &mut crate::manage::profiles::ProfilesController| {
            c.set_account(account_state.clone());
            c.set_retained_profiles(retained_profiles.clone());
            c.set_hidden_profile_count(hidden_profile_count);
            c.set_profiles(profiles.clone(), current, &workspaces)
                | c.set_workspace_counts(counts.clone())
        };
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => {
                view.set_linked_devices(&linked_devices)
                    | view.set_current_workspace(open_workspace)
                    | view.set_workspace_profiles(&profiles)
                    | view.delegate(&push)
            }
            _ => false,
        })
    }

    /// Hand a Manage tab Agent's latest device snapshot, for its nav count
    /// and its *Linked devices* page.
    fn push_linked_devices(&mut self) -> bool {
        let devices = &self.linked_devices;
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Manage(view) = &mut tab.view {
                    changed |= view.set_linked_devices(devices);
                }
            }
        }
        changed
    }

    /// What the Manage tab asked of the window itself: the rename field over
    /// a Profile's name, or the icon and color panel. Neither is an account
    /// operation, so neither goes through the controller.
    fn drain_manage_requests(&mut self) -> bool {
        let requests: Vec<crate::manage::Request> = self
            .panes
            .iter_groups_mut()
            .flat_map(|(_, group)| group.tabs_mut().iter_mut())
            .filter_map(|tab| match &mut tab.view {
                TabView::Manage(view) => {
                    Some(std::iter::from_fn(|| view.take_request()).collect::<Vec<_>>())
                }
                _ => None,
            })
            .flatten()
            .collect();
        let mut changed = false;
        for request in requests {
            changed |= match request {
                crate::manage::Request::Rename(id) => self.start_profile_rename(id),
                crate::manage::Request::ChangeIconAndColor(id) => self.start_profile_icon(id),
                crate::manage::Request::NewWorkspace(profile) => self.new_workspace_for(profile),
                crate::manage::Request::ImportContent => self.open_import_menu(),
                crate::manage::Request::OpenWorkspace(id) => self.open_workspace(id),
                // The button's corner, out of the tab's coordinates into the
                // window's.
                crate::manage::Request::WorkspaceMenu { workspace, at } => {
                    let view = self.focused_view_rect();
                    self.open_workspace_menu(workspace, (view.x + at.0, view.y + at.1), true)
                }
                crate::manage::Request::Device(request) => self.settle_device_request(request),
            };
        }
        changed
    }

    /// Carry out the next thing the Manage tab's controller asked of the
    /// window. One effect per drain, in the order the controller raises
    /// them; the callers drain again on the next turn of the same chain.
    fn drain_manage_profiles_request(&mut self) -> bool {
        let asked = self.drain_manage_requests();
        for (_, group) in self.panes.iter_groups_mut() {
            let active = group.tabs().active();
            for (index, tab) in group.tabs_mut().iter_mut().enumerate() {
                // Manage reads Cloud only for a page that shows it.
                if let TabView::Manage(view) = &mut tab.view {
                    view.set_in_front(index == active);
                }
            }
        }
        let effect = self
            .panes
            .iter_groups_mut()
            .flat_map(|(_, group)| group.tabs_mut().iter_mut())
            .find_map(|tab| match &mut tab.view {
                TabView::Manage(view) => view.take_effect(),
                _ => None,
            });
        let Some(effect) = effect else {
            return asked;
        };
        self.ask_pane(profiles_effect_request(effect)) | asked
    }

    /// Open the emoji-and-colour modal for a workspace.
    fn start_workspace_icon(&mut self, id: WorkspaceId) -> bool {
        let Some(w) = self.workspace_facts(id) else {
            return false;
        };
        self.workspace_icon = Some(EmojiColorPanel::new(
            &mut self.engine,
            id,
            &w.name,
            w.icon,
            w.color,
        ));
        true
    }

    fn start_profile_icon(&mut self, id: ProfileId) -> bool {
        let Some(profile) = self
            .profiles()
            .iter()
            .find(|profile| profile.id == id)
            .cloned()
        else {
            return false;
        };
        self.workspace_icon = Some(EmojiColorPanel::for_profile(
            &mut self.engine,
            id,
            &profile.name,
            profile.icon,
            profile.color,
        ));
        true
    }

    fn settle_icon(&mut self, outcome: IconOutcome) -> bool {
        match outcome {
            IconOutcome::Keep => true,
            IconOutcome::Cancel => {
                self.workspace_icon = None;
                true
            }
            IconOutcome::Save { icon, color } => {
                let target = self.workspace_icon.take().map(|m| m.target);
                match target {
                    Some(IconTarget::Workspace(id)) => {
                        self.ask_pane(PaneRequest::SetWorkspaceIcon { id, icon, color })
                    }
                    Some(IconTarget::Profile(id)) => {
                        self.ask_pane(PaneRequest::SetProfileAppearance { id, icon, color })
                    }
                    Some(IconTarget::Draft | IconTarget::DraftProfile) => true,
                    None => true,
                }
            }
        }
    }

    /// The Link dialog, on its *New device* tab with a fresh code. The list
    /// of linked devices is Manage's page, not the dialog's, so nothing here
    /// asks for it.
    fn open_link_device(&mut self) -> bool {
        let mut dialog = LinkDevice::new(&mut self.engine);
        let problem = self.agent.identity_problem();
        // A device that cannot sync cannot pair either; the window shows the
        // problem and the way out instead of minting a code that would fail.
        if problem.is_none() {
            self.agent.start_pairing();
        }
        dialog.set_identity_problem(problem);
        self.link_device = Some(dialog);
        true
    }

    /// **Take the linked-device list whether or not anything is showing it.**
    ///
    /// The list is a single-consumer snapshot, so a window that drained it
    /// only while the Link-device dialog was open had no way to name the
    /// device a run pill points at (D16). This is the one drain; a Manage tab
    /// is handed the same answer for its *Linked devices* page.
    fn refresh_device_names(&mut self) {
        let Some((generation, this_device, devices, relay)) = self.agent.take_linked_devices()
        else {
            return;
        };
        self.device_names_acknowledged_generation = Some(generation);
        self.this_device = this_device
            .as_ref()
            .and_then(|device| koru_device::DeviceId::parse(&device.device_id));
        self.device_names = this_device
            .iter()
            .chain(devices.iter())
            .filter_map(|device| {
                koru_device::DeviceId::parse(&device.device_id)
                    .map(|id| (id, device.display_name.clone()))
            })
            .collect();
        self.linked_devices.set_linked(this_device, devices, relay);
        self.push_linked_devices();
        // A name that has just arrived changes what a pill says.
        self.recompose_run_pill_captions();
    }

    /// Ask for device identity once on each live Agent connection until that
    /// generation answers. A dropped writer advances the generation, so the
    /// reconnect wake reissues naturally without a timer or polling.
    fn request_device_names_if_needed(&mut self) {
        let Some(generation) = self.agent.connected_generation() else {
            return;
        };
        if self.device_names_acknowledged_generation == Some(generation)
            || self.device_names_requested_generation == Some(generation)
        {
            return;
        }
        self.device_names_requested_generation = Some(generation);
        self.agent.refresh_linked_devices();
    }

    /// Take what Agent answered about devices: the list (always), a page of
    /// one device's history (for Manage's page, which is the only thing that
    /// asks for one), and the pairing state while the Link dialog is up.
    fn refresh_link_device(&mut self) {
        // Taken whether or not the window is still open: once the Agent has
        // stopped for a reset, this process must relaunch regardless.
        match self.agent.take_identity_reset() {
            Some(crate::agent::IdentityReset::Relaunch) => {
                self.ask_pane(PaneRequest::Relaunch);
            }
            Some(crate::agent::IdentityReset::Refused(code)) => {
                tracing::warn!(code = %code, "the Agent refused to start over as a new device");
                if let Some(dialog) = self.link_device.as_mut() {
                    dialog.start_over_refused();
                }
            }
            _ => {}
        }
        self.refresh_device_names();
        if let Some((device_id, events, next_before)) = self.agent.take_device_history() {
            self.each_tab_mut(|tab| match &mut tab.view {
                TabView::Manage(view) => {
                    view.set_device_history(&device_id, events.clone(), next_before)
                }
                _ => false,
            });
        }
        let Some(dialog) = self.link_device.as_mut() else {
            return;
        };
        if let Some(pairing) = self.agent.take_pairing()
            && dialog.set_pairing(&mut self.engine, pairing)
        {
            self.agent.refresh_linked_devices();
        }
    }

    fn settle_link_device(&mut self, outcome: LinkDeviceOutcome) -> bool {
        match outcome {
            LinkDeviceOutcome::Keep => {}
            LinkDeviceOutcome::Close => {
                self.agent.cancel_pairing();
                self.link_device = None;
            }
            LinkDeviceOutcome::Copy(code) => {
                if koru_editor::clipboard::put_system_text(&code)
                    && let Some(dialog) = self.link_device.as_mut()
                {
                    dialog.mark_copied(Instant::now());
                }
            }
            LinkDeviceOutcome::Generate => self.agent.start_pairing(),
            LinkDeviceOutcome::Accept(code) => self.agent.accept_link_code(code),
            // maintainers' decision 12: the linked device is shown where it now lives.
            LinkDeviceOutcome::Done => {
                self.link_device = None;
                self.open_manage(crate::manage::Page::Devices);
            }
            LinkDeviceOutcome::StartOver => self.agent.reset_identity(),
        }
        true
    }

    /// What Manage's *Linked devices* page asked of Agent, or of the window.
    fn settle_device_request(&mut self, request: crate::manage::DeviceRequest) -> bool {
        use crate::manage::DeviceRequest;
        match request {
            DeviceRequest::Refresh => self.agent.refresh_linked_devices(),
            DeviceRequest::LoadHistory { device_id, before } => {
                self.agent.request_device_history(device_id, before);
            }
            DeviceRequest::Unlink(device_id) => {
                self.agent.unlink_device(device_id);
                self.agent.refresh_linked_devices();
            }
            // The response carries the refreshed list, so no extra refresh
            // is needed — and the presence refresh confirms it.
            DeviceRequest::RenameThisDevice(name) => self.agent.rename_device(name),
            DeviceRequest::SetRelay(enabled) => self.agent.set_relay_enabled(enabled),
            DeviceRequest::OpenDocument(document) => {
                self.record_jump();
                return self.ask_pane(PaneRequest::Navigate {
                    target: koru_workspace_host::NavigationTarget::Document {
                        document,
                        node: None,
                    },
                    match_text: None,
                    disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                });
            }
            DeviceRequest::LinkDevice => return self.open_link_device(),
        }
        true
    }

    /// Put an error in front of the user. Replaces one already showing: the
    /// newer failure is the one they are in the middle of.
    fn show_error(&mut self, report: koru_workspace_host::ErrorReport) -> bool {
        self.error = Some(ErrorDialog::new(report));
        true
    }

    fn settle_import(&mut self, outcome: ImportOutcome) -> bool {
        match outcome {
            ImportOutcome::Keep => true,
            ImportOutcome::Choose(choice) => {
                self.import_dialog = None;
                self.ask_pane(PaneRequest::FinishWorkspaceImport(choice))
            }
        }
    }

    fn error_key(&mut self, key: &Key) -> bool {
        let Some(dialog) = self.error.as_mut() else {
            return false;
        };
        let outcome = dialog.key(key);
        self.settle_error(outcome);
        true
    }

    fn error_mouse_down(&mut self, x: f32, y: f32) -> bool {
        let Some(dialog) = self.error.as_mut() else {
            return false;
        };
        let outcome = dialog.mouse_down(&mut self.engine, self.viewport, x, y);
        self.settle_error(outcome);
        true
    }

    /// What the dialog decided. Kept in one place so *Try Again* and Enter
    /// cannot drift into meaning different things.
    fn settle_error(&mut self, outcome: ErrorOutcome) -> bool {
        match outcome {
            ErrorOutcome::Keep => true,
            ErrorOutcome::Close => {
                self.error = None;
                true
            }
            ErrorOutcome::Copy(details) => {
                // Stays open: copying the error is something you do *before*
                // dismissing it, usually on the way to reporting it.
                koru_editor::clipboard::put_system_text(&details);
                true
            }
            ErrorOutcome::Act(action) => {
                self.error = None;
                match action {
                    koru_workspace_host::ErrorAction::Reveal(dir) => {
                        self.ask_pane(PaneRequest::RevealPath(dir))
                    }
                    koru_workspace_host::ErrorAction::Retry(req) => self.ask_pane(req),
                }
            }
        }
    }

    /// `+` opens **a page you can type on**, not a welcome screen.
    ///
    /// The document behind it is not created until the first keystroke — see
    /// [`Draft`] for why that is worth the machinery, and [`promote_draft`] for
    /// where it happens. A welcome tab is the fallback if Loro cannot make the
    /// private document at all, because a tab you cannot type in is worse than
    /// the screen this replaced.
    ///
    /// [`Draft`]: tab::Draft
    /// [`promote_draft`]: Self::promote_draft
    fn new_tab(&mut self) -> bool {
        // Capture before the draft becomes active — its root is not in the
        // library, and explore.selected would track that private id.
        let anchor = self.library_create_anchor_hint();
        self.record_jump();
        self.tabs_mut()
            .push(Tab::draft(anchor).unwrap_or_else(Tab::welcome));
        // Pushing renumbers nothing, but it does change every tab's *width*,
        // which is what a slide is measured in. Half a slide at the old pitch
        // finishing at the new one would overshoot.
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        true
    }

    /// Onboarding has created a Profile and Workspace: remember whom to greet
    /// and bring the Welcome tab forward.
    ///
    /// The owner's name is known only here — it is not stored on the Profile —
    /// so it is held for the rest of this window's life, **keyed by the
    /// Profile it was given for**: a window later switched to another Profile
    /// greets plainly rather than by somebody else's name.
    pub fn show_first_minutes(&mut self, owner: &str, profile: &str, workspace: &str) -> bool {
        // The manager is a utility used during creation. Once creation lands,
        // the Welcome page is the only onboarding destination it may leave
        // behind.
        self.tabs_mut()
            .retain_main(|tab| !matches!(tab.view, TabView::Manage(_)));
        self.welcome_owner =
            (!owner.trim().is_empty()).then(|| (profile.to_owned(), owner.trim().to_owned()));
        self.welcome_workspace_hint = Some(workspace.to_owned());
        let existing = self
            .tabs()
            .iter()
            .position(|tab| matches!(&tab.view, TabView::Welcome(_)) && tab.draft.is_none());
        if let Some(index) = existing {
            let _ = self.select(index);
        } else {
            self.record_jump();
            self.tabs_mut().push(Tab::first_minutes(owner, workspace));
        }
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.sync_welcome_lists();
        true
    }

    /// **+ → Welcome**: select the one Welcome tab, or open it.
    fn open_welcome_tab(&mut self) -> bool {
        let existing = self
            .tabs()
            .iter()
            .position(|t| matches!(t.view, TabView::Welcome(_)) && t.draft.is_none());
        if let Some(i) = existing {
            return self.select(i);
        }
        self.record_jump();
        self.tabs_mut().push(Tab::welcome());
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.sync_welcome_lists();
        true
    }

    /// Who the Welcome page greets, from what this window knows now.
    fn welcome_identity(&self) -> crate::welcome::WelcomeIdentity {
        let (profile, workspace) = match self.pane_context.as_ref() {
            Some((profiles, profile, workspaces, workspace)) => {
                let profile = profiles.iter().find(|p| p.id == *profile);
                (
                    profile.map(|p| p.name.clone()).unwrap_or_default(),
                    workspaces
                        .iter()
                        .find(|w| Some(w.id) == *workspace)
                        .map(|w| w.name.clone()),
                )
            }
            None => (String::new(), None),
        };
        let workspace = workspace
            .or_else(|| self.welcome_workspace_hint.clone())
            .unwrap_or_default();
        // Before the window's context arrives the Profile is unnamed; the
        // name onboarding just gave is the best answer there is.
        let profile = if profile.is_empty() {
            self.welcome_owner
                .as_ref()
                .map(|(profile, _)| profile.clone())
                .unwrap_or_default()
        } else {
            profile
        };
        let owner = self
            .welcome_owner
            .as_ref()
            .filter(|(named, _)| *named == profile)
            .map(|(_, owner)| owner.clone());
        crate::welcome::WelcomeIdentity {
            owner,
            workspace,
            profile,
        }
    }

    /// Settings uses the same proven preference view as Welcome, but opens in
    /// its own singleton tab so the entry point is explicit and repeat clicks
    /// return to the same surface.
    fn open_settings_tab(&mut self) -> bool {
        let existing = self
            .tabs()
            .iter()
            .position(|t| matches!(&t.view, TabView::Settings(_)) && t.draft.is_none());
        if let Some(i) = existing {
            return self.select(i);
        }
        self.record_jump();
        let mut tab = Tab::settings();
        // **Seed the values the page keeps a field of.** Most of what it draws
        // reads a process global, but these do not — and a Settings tab opened
        // an hour into a session would otherwise show the defaults rather than
        // what is stored. `restore` keeps this current.
        if let TabView::Settings(view) = &mut tab.view {
            View::adopt_preferences(view, &self.preferences);
        }
        self.tabs_mut().push(tab);
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        self.sync_reminder_settings_views();
        true
    }

    fn open_voice_settings_tab(&mut self) -> bool {
        let opened = self.open_settings_tab();
        let moved = match &mut self.tabs_mut().active_mut().view {
            TabView::Settings(view) => view.scroll_to_voice(),
            _ => false,
        };
        opened || moved
    }

    /// Open the one Settings tab, then target the profile field that the
    /// reminder editor says is missing. The destination supplies its own
    /// geometry, so this remains correct as sections above it change height.
    fn open_reminder_end_of_day_settings(&mut self) -> bool {
        let opened = self.open_settings_tab();
        let revealed = match &mut self.tabs_mut().active_mut().view {
            TabView::Settings(settings) => settings.reveal_reminder_end_of_day(),
            _ => false,
        };
        opened || revealed
    }

    pub(crate) fn open_send_feedback_tab(&mut self) -> bool {
        let existing = self
            .tabs()
            .iter()
            .position(|t| matches!(t.view, TabView::Feedback(_)) && t.draft.is_none());
        if let Some(i) = existing {
            let _ = self.select(i);
            return true;
        }
        self.record_jump();
        self.tabs_mut().push(Tab::feedback());
        self.slides_mut().clear();
        self.mark_tab_widths_stale();
        self.size_visible_groups();
        self.ensure_tab_visible(self.tabs().active());
        true
    }

    fn snapshot_plus_menu(&self) {
        use koru_session_protocol::{ProcessState, ProfileKind};
        let (canvases, recents) = self.split_canvas_recents(
            self.recent_documents()
                .into_iter()
                .take(context_menu::PLUS_RECENT_MAX)
                .map(|(title, id)| (id, title))
                .collect(),
        );
        // Kind comes from the launch profile, not from RawPty vs Managed:
        // Claude and OpenCode run as a PTY and would otherwise land under
        // Shell.
        let live = |want_agent: bool| {
            self.explore
                .agent_sessions()
                .iter()
                .filter(|session| {
                    matches!(
                        session.process,
                        ProcessState::Starting | ProcessState::Running
                    ) && match koru_session_profiles::resolve(&session.profile)
                        .map(|profile| profile.kind)
                    {
                        Ok(ProfileKind::Agent) => want_agent,
                        _ => !want_agent,
                    }
                })
                .map(|session| (session.id, session.display_name.clone()))
                .collect()
        };
        let shells = live(false);
        let agents = live(true);
        let chats = self.plus_recent_chats();
        context_menu::set_plus_snapshots(recents, shells, agents, chats);
        context_menu::set_plus_canvases(canvases);
    }

    /// Chats the `+` flyout can name. The right-hand pane is the usual source;
    /// a chat tab that has already booted is the fallback when the pane has
    /// never been opened.
    fn plus_recent_chats(&self) -> Vec<(koru_ai_ui::ChatId, String)> {
        let limit = context_menu::PLUS_RECENT_MAX;
        let pane = self.ai_chat.recent_chats(limit);
        if !pane.is_empty() {
            return pane;
        }
        self.panes
            .iter_groups()
            .find_map(|(_, group)| {
                group.tabs().iter().find_map(|tab| match &tab.view {
                    TabView::AiChat(view) => {
                        let chats = view.recent_chats(limit);
                        (!chats.is_empty()).then_some(chats)
                    }
                    _ => None,
                })
            })
            .unwrap_or_default()
    }

    /// Re-snapshot chats and rebuild the open Chats flyout if the list arrived
    /// while Add… was up. Rows are fixed at open, so a flyout built empty
    /// would otherwise stay empty until the next open.
    fn refresh_open_plus_chats(&mut self) {
        if !matches!(
            self.menu.as_ref().map(|m| &m.subject),
            Some(MenuSubject::NewTab)
        ) {
            return;
        }
        self.snapshot_plus_menu();
        self.rebuild_plus_chats_flyout();
    }

    fn rebuild_plus_chats_flyout(&mut self) {
        let Some(i) = self
            .menu
            .as_ref()
            .and_then(|m| m.items().iter().position(|a| *a == MenuAction::Chats))
        else {
            return;
        };
        if !self.submenu.as_ref().is_some_and(|(at, _)| *at == i) {
            return;
        }
        self.nested = None;
        self.submenu = None;
        let _ = self.open_submenu(i);
    }

    fn open_new_tab_menu(&mut self) -> bool {
        // Freshen the presence probe that dims missing programs — off the loop
        // thread, so the menu opens now on the last known answer and the
        // worker's wake re-dims the rows when the fresh one lands.
        self.profile_probe.refresh(self.agent.wake_proxy());
        // Start the pane surface so Boot can list chats. The flyout is empty
        // until that answer lands; [`refresh_open_plus_chats`] fills it. Not
        // when Koru AI is hidden: there is no Chats row to fill
        // (koru-features).
        if koru_features::ai() {
            self.ai_chat.activate();
        }
        self.snapshot_plus_menu();
        self.ensure_tab_widths();
        let button = tabs::new_tab(self.strip(), &self.tab_widths(), self.tab_scroll());
        let mut menu = ContextMenu::aligned(
            &mut self.engine,
            MenuSubject::NewTab,
            (button.x, button.y + button.h),
            context_menu::new_tab_items(),
            MenuAnchor::Cursor,
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// Open the format chooser used by the workspace-level *Import content…*
    /// entry. It shares the NewTab subject because those are the same import
    /// actions; only the parent menu's New document row is omitted here.
    fn open_import_menu(&mut self) -> bool {
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::NewTab,
            self.pointer,
            context_menu::import_items(),
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// The library background's context menu. It deliberately keeps the
    /// `Import…` parent row so the formats open as a flyout at the pointer.
    fn open_library_menu(&mut self, at: (f32, f32)) -> bool {
        let mut menu = ContextMenu::new(
            &mut self.engine,
            MenuSubject::NewTab,
            at,
            vec![MenuAction::ImportAs],
        );
        let enabled = self.enabled_items(&menu);
        menu.step(&enabled, 1);
        self.menu = Some(menu);
        true
    }

    /// Fulfil the entity picker's create row. Once somebody has typed a name,
    /// there is no useful second prompt: create that page directly through the
    /// shell. With no query this remains the lazy Ctrl+T draft, so merely
    /// opening the picker and choosing Add new page does not leave an empty
    /// document behind.
    fn new_page_from_entity(&mut self, title: String) -> bool {
        let title = title.trim();
        if title.is_empty() {
            return self.new_tab();
        }
        self.record_jump();
        self.ask_pane(koru_workspace_host::PaneRequest::CreateLinkedDocument(
            title.to_owned(),
        ));
        true
    }

    /// A workspace switch is a navigable jump, whether it came from the compact
    /// switcher, the manager, or a context menu.
    fn switch_workspace_here(&mut self, id: WorkspaceId) -> bool {
        self.record_jump();
        self.ask_pane(PaneRequest::Switch(id))
    }

    /// Best-effort library sibling anchor without a workspace check — used when
    /// opening a draft. Prefer the active real document, then explore highlight,
    /// then any other open real tab. Never a draft root.
    fn library_create_anchor_hint(&self) -> Option<NodeId> {
        if self.tabs().active_tab().draft.is_none() {
            if let Some(id) = self.document_root() {
                return Some(id);
            }
        }
        self.explore.selected().or_else(|| {
            self.tabs()
                .iter()
                .filter(|t| t.draft.is_none())
                .find_map(|t| t.view.root())
        })
    }

    // **`library_create_anchor` was here** — the anchor validated against the
    // live workspace, so a draft's private node id dropped out. Its only caller
    // was the branch of `promote_draft` that created the document itself, which
    // a store-backed workspace does not do: creation is the shell's, and where a
    // new document goes among its siblings is the manifest's business rather
    // than a node's. `library_create_anchor_hint` survives because the explore
    // pane still uses it.

    /// Move a tab along the strip, and start its neighbours sliding.
    ///
    /// The one place a reorder happens, so the order and the animation cannot
    /// disagree — and the drag follows the tab to its new index, or the next
    /// pointer move would swap a different one.
    fn move_tab(&mut self, from: usize, to: usize) -> bool {
        self.ensure_tab_widths();
        let strip = self.strip();
        let old_xs = tabs::slot_xs(strip, &self.tab_widths());
        // Widths travel with the tabs.
        if from < self.tab_widths().len() && to < self.tab_widths().len() {
            let w = self.tab_widths_mut().remove(from);
            self.tab_widths_mut().insert(to, w);
        }
        if !self.tabs_mut().move_tab(from, to) {
            // Undo width move if the tab set refused.
            if from < self.tab_widths().len() && to < self.tab_widths().len() {
                let w = self.tab_widths_mut().remove(to);
                self.tab_widths_mut().insert(from, w);
            }
            return false;
        }
        let new_xs = tabs::slot_xs(strip, &self.tab_widths());
        self.slides_mut()
            .moved(&old_xs, &new_xs, from, to, Instant::now());
        if let Some(d) = self.group_drag_mut() {
            d.index = to;
        }
        true
    }

    /// Where the carried tab's left edge is drawn, and how wide it is.
    ///
    /// Clamped to the strip: past either end there is nowhere to land, so a tab
    /// that kept following would only wander over the window buttons.
    fn carried(&mut self) -> Option<(usize, f32, f32)> {
        // A tear-off in progress is not a reorder: the tab stays in its slot and
        // a floating indicator follows the pointer instead.
        if self.drag_tearing() {
            return None;
        }
        self.ensure_tab_widths();
        let d = self.group_drag().as_ref()?;
        let strip = self.strip();
        let rect = tabs::layout(strip, &self.tab_widths(), self.tab_scroll())
            .get(d.index)
            .copied()?;
        let w = rect.body.w;
        let x = tabs::carry_x(strip, w, d.x - d.grab_dx);
        Some((d.index, x, w))
    }

    /// Whether the pointer has carried a tab far enough off the strip to tear it
    /// loose. Documents can leave the window entirely; shell and agent tabs tear
    /// loose for a move or a split but have nowhere to pop out *to*.
    fn drag_tearing(&self) -> bool {
        self.pane_drag.is_some()
            || self.group_drag().as_ref().is_some_and(|d| {
                self.is_pane_draggable(d.index) && (d.y - d.origin_y).abs() >= panes::TEAR_OFF_PX
            })
    }

    /// Which tab the pointer is carrying, and how far from its slot it is being
    /// drawn — the same number the reorder slide deals in, which is why
    /// `tabs::Tab` has one field for both.
    pub(super) fn carry_offset(&mut self) -> Option<(usize, f32)> {
        let (index, x, _) = self.carried()?;
        let slot = tabs::layout(self.strip(), &self.tab_widths(), self.tab_scroll())
            .get(index)?
            .body
            .x;
        Some((index, x - slot))
    }

    /// Perform what a piece of chrome asked for. The one place any of it
    /// happens, so the container's invariants stay in one file.
    fn apply(&mut self, ws: &WorkspaceDoc, out: Outcome) -> bool {
        match out {
            Outcome::Ignored => false,
            Outcome::Consumed => true,
            Outcome::SelectTab(i) => self.select(i),
            Outcome::CloseTab(i) => self.close_requested(i),
            Outcome::MoveTab { from, to } => self.move_tab(from, to),
            Outcome::NewTab => self.open_new_tab_menu(),
            Outcome::Search => self.open_search(),
            Outcome::OpenNode(id)
                if self.explore.navigator_mode() == crate::panes::explore::NavigatorMode::Files =>
            {
                self.ask_pane(PaneRequest::SaveFile(koru_core::DocumentId::from_uuid(
                    id.as_uuid(),
                )))
            }
            Outcome::OpenNode(id) => self.open_from_explore(ws, id),
            // A note is a document: the ordinary open path, seeded on first
            // open by the shell. It only looks like a file row because the
            // glyph that offered it sits on one.
            Outcome::OpenNote(id) => self.open_from_explore(ws, id),
            Outcome::OpenNodeInNewTab(_) => true, // retired: double-click no longer opens a tab
            // The Agents navigator: focus the tab already on this session,
            // else open one bound to it (never a launch).
            //
            // **A session a run is driving opens read-only** (ADR-361). The
            // run holds its controller, so a tab that attached would be
            // asking for a lease the orchestrator has; the tab watches
            // instead, and the control at the foot of the chat is how a
            // person asks for it.
            Outcome::OpenSession(id) => {
                if self.explore.session_belongs_to_a_run(id) {
                    self.open_session_tab_watching(id)
                } else {
                    self.open_session_tab_for_existing(id)
                }
            }
            // A run row: focus the tab already showing it, else open one.
            Outcome::OpenRun(run) => self.open_run_tab(run),
            // The Chats view: a row focuses the tab already showing that chat
            // or opens one; *New chat* opens a fresh one; Delete and the row
            // menu ask first, through the one confirmation.
            Outcome::OpenAiChat(chat) => self.open_ai_chat_tab_for(chat),
            Outcome::OpenSocials => {
                self.show_navigator(crate::panes::explore::NavigatorMode::Socials)
            }
            Outcome::NewAiChat => self.open_ai_chat_tab(),
            Outcome::DeleteAiChat(chat) => self.ask_delete_chat(chat),
            Outcome::AiChatMenu { chat, at } => self.open_chat_menu(chat, at),
            // The Agents tree's mutations: every one a typed verb to the
            // session host, with the tree reflecting the catalog that answers
            // — the container keeps no project truth (ADR-264, Q-E6).
            Outcome::NewAgentProject => {
                self.prompt_new_agent_project();
                true
            }
            Outcome::AgentTreeMenu { project, at } => self.open_agent_tree_menu(project, at),
            Outcome::AgentSessionMenu { session, at } => self.open_session_menu(session, at),
            Outcome::AgentRunMenu { runs, at } => self.open_run_menu(runs, at),
            Outcome::AgentScheduleMenu { schedules, at } => self.open_schedule_menu(schedules, at),
            // The drag's answer to the menu's Delete: same verb, same door,
            // same question.
            Outcome::TrashRuns(runs) => self.queue_run_trash(runs),
            Outcome::MoveSessionToProject { session, project } => {
                if let Some(sender) = self.session_verbs() {
                    sender.assign_project(session, project);
                }
                true
            }
            Outcome::DownloadFile(id) => {
                // Say so on the row before the request leaves: the fetch may wait
                // on a sleeping peer, and a press with no visible answer reads as
                // a press that missed.
                self.explore
                    .set_file_state(id, crate::panes::explore::FileState::Fetching);
                self.ask_pane(PaneRequest::DownloadFile(koru_core::DocumentId::from_uuid(
                    id.as_uuid(),
                )))
            }
            Outcome::OpenFile(id) => self.ask_pane(PaneRequest::OpenFile(
                koru_core::DocumentId::from_uuid(id.as_uuid()),
            )),
            Outcome::RenameImage(id) => self.rename_image(ws, id),
            Outcome::Tasks(request) => self.apply_tasks_rail(request),
            // A drag's commit needs `&mut ws`, which the release path holds; it
            // is handled directly in `mouse_up` rather than here, so this arm is
            // only here to keep the match exhaustive.
            Outcome::MoveNodes { .. } => false,
            Outcome::ToggleSidebar => self.toggle_sidebar(),
            Outcome::ToggleContext => self.toggle_context(),
            // Swapping the open document, picking a folder and starting a
            // process are all the shell's; a view has neither the data
            // directory nor a `Window`. So each is recorded and drained.
            Outcome::SwitchWorkspace(id) => self.switch_workspace_here(id),
            Outcome::SwitchToWorkspaceIn { profile, workspace } => {
                self.ask_pane(PaneRequest::SwitchToWorkspaceIn(profile, workspace))
            }
            Outcome::NewWorkspace => self.prompt_new_workspace(),
            Outcome::NewWorkspaceIn(profile) => self.prompt_new_workspace_in(profile),
            Outcome::NewProfile => self.open_profile_creation(),
            Outcome::SubscribeCloud => {
                self.ask_pane(PaneRequest::OpenWebsite(crate::website::page_url(
                    &crate::website::origin(),
                    crate::format::lang(),
                    crate::website::Page::Pricing,
                )))
            }
            Outcome::ImportContent => self.open_import_menu(),
            Outcome::ManageWorkspaces(profile) => self.open_manage_workspaces(profile),
            Outcome::ManageProfiles => self.open_manage_profiles(),
            Outcome::AddProfile => self.ask_pane(PaneRequest::AddProfile),
            Outcome::OpenProfile => self.ask_pane(PaneRequest::OpenProfile),
            Outcome::LinkDevice => self.open_link_device(),
            Outcome::NodeMenu {
                nodes,
                at,
                from_button,
            } => self.open_node_menu(nodes, at, from_button),
            Outcome::LibraryMenu { at } => self.open_library_menu(at),
            // **`queue_trash`, not `queue_trash_nodes`.** This is the drag onto
            // the Trash row — a second way in, and the one that carries several
            // rows at once. Routing it straight to the node path was why
            // *Move 2 items to trash* did nothing while the row menu worked:
            // the same fix has to answer every route, and there are two.
            Outcome::TrashNodes(ids) => self.queue_trash(ids),
            Outcome::OpenTrash => self.open_trash_tab(),
            Outcome::NewDocument => {
                // The same modal the taskbar's *New document* opens, rather than
                // a second way to make one. With no Store workspace the same
                // entry point asks for a workspace first; a document has
                // nowhere durable to belong yet.
                self.prompt_new_document();
                true
            }
            Outcome::NewCanvas => self.new_canvas(),
            Outcome::ImportFiles => self.ask_pane(PaneRequest::ImportFiles(
                koru_workspace_host::ImportSourceSelection::Picker,
            )),
            // **The task is in the document this column is about**, so this is
            // the ordinary navigation to a node, not an open — the same request
            // the Tasks tab's own row makes, through the same door.
            Outcome::RevealTask(node) => match self.active_document {
                Some(document) => self.open_task_node(document, node, false),
                None => false,
            },
            Outcome::SaveVersion => {
                // **The button is unchanged**, deliberately: it still takes the
                // unnamed snapshot it always took, and Ctrl+S is the way to name
                // one. Routing the button through the modal as well would put a
                // prompt in front of a control that has never had one — a
                // behavior change to something nobody asked to change. If the
                // two should agree, that is a decision to take on purpose.
                self.document_history.save_with_label(None);
                true
            }
            Outcome::CompareVersions(left, right) => {
                self.document_history.compare(left, right);
                true
            }
            Outcome::CompareWithCurrent(key) => {
                self.document_history.compare_with_current(key);
                true
            }
            // **The pane owns the text and the container owns the clipboard**,
            // the boundary every other copy in this app keeps — and the same
            // `put_system_text` the error dialog's *Copy* uses, so what lands
            // there pastes as plain text.
            Outcome::CopyCloudDetails => self.copy_cloud_details(),

            // A view cannot move or close its own window — it holds no `Window`
            // — so these are recorded and the shell drains them inside the same
            // press. Dragging in particular has to happen there: winit only
            // honours it while the button that asked is still down.
            Outcome::DragWindow => self.ask_window(WindowRequest::Drag),
            Outcome::Minimise => self.ask_window(WindowRequest::Minimise),
            Outcome::ToggleMaximise => self.ask_window(WindowRequest::ToggleMaximise),
            Outcome::CloseWindow => self.ask_window(WindowRequest::Close),
            Outcome::TogglePin => self.ask_window(WindowRequest::TogglePin),
            Outcome::HideToTray => self.ask_window(WindowRequest::HideToTray),
        }
    }

    fn ask_window(&mut self, req: WindowRequest) -> bool {
        self.window_request = Some(req);
        true
    }

    /// Commit an explore drag: move each carried node into a contiguous run at
    /// the resolved anchor, as one undo step.
    ///
    /// The cache already shows this landing — the drag animated to it — so this
    /// does not rebuild; [`refresh_explore`] holds the rebuild while the
    /// release slide plays out, then confirms it for free.
    ///
    /// Placing in increasing index order (`base`, `base + 1`, …) is what makes
    /// a multi-move land contiguous regardless of where each node came from:
    /// `mov_to` sets an absolute index, and a later insert at a higher index
    /// never disturbs an earlier one.
    fn move_nodes(
        &mut self,
        ws: &mut WorkspaceDoc,
        nodes: Vec<NodeId>,
        target: DropTarget,
    ) -> bool {
        // **An Explore row names a document**, so a drag there reorders the
        // *manifest*, not the tree of whatever document is open. Same reasoning
        // as opening one: the ids in these rows are `DocumentId`s, and the shell
        // owns the workspace.
        //
        if let Some(entries) = self.entries.clone() {
            let selected: HashSet<koru_core::DocumentId> = nodes
                .iter()
                .filter_map(|node| {
                    entries
                        .iter()
                        .find(|entry| entry.id.as_uuid() == node.as_uuid())
                        .map(|entry| entry.id)
                })
                .collect();
            // Only selected roots travel independently. A selected child of a
            // selected parent rides with that parent's manifest subtree.
            let roots: Vec<koru_core::DocumentId> = nodes
                .iter()
                .filter_map(|node| {
                    entries
                        .iter()
                        .find(|entry| entry.id.as_uuid() == node.as_uuid())
                        .map(|entry| entry.id)
                })
                .filter(|id| {
                    let mut parent = entries
                        .iter()
                        .find(|entry| entry.id == *id)
                        .and_then(|e| e.parent);
                    while let Some(ancestor) = parent {
                        if selected.contains(&ancestor) {
                            return false;
                        }
                        parent = entries
                            .iter()
                            .find(|entry| entry.id == ancestor)
                            .and_then(|entry| entry.parent);
                    }
                    true
                })
                .collect();
            if roots.is_empty() {
                return false;
            }

            let (parent, base, append) = match &target {
                // Onto the empty area below the tree: the top level, at the end.
                DropTarget::Library => (None, 0, true),
                DropTarget::Row { node, zone } if *zone == DropZone::Nest => (
                    entries
                        .iter()
                        .find(|e| e.id.as_uuid() == node.as_uuid())
                        .map(|e| e.id),
                    0,
                    true,
                ),
                DropTarget::Row { node, zone } => {
                    let onto = entries.iter().find(|e| e.id.as_uuid() == node.as_uuid());
                    let parent = onto.and_then(|e| e.parent);
                    // Count in the list after the carried roots have been
                    // lifted out, matching the manifest move coordinate.
                    let before = entries
                        .iter()
                        .filter(|entry| entry.parent == parent)
                        .filter(|entry| !roots.contains(&entry.id))
                        .take_while(|entry| Some(entry.id) != onto.map(|entry| entry.id))
                        .count();
                    let base = before + usize::from(*zone == DropZone::After);
                    (parent, base, false)
                }
            };
            // First gather the roots at the destination's end. Moving directly
            // to final indices is order-dependent when some selected rows sit
            // on each side of the target (A,C after D used to become A,D,C).
            // Once gathered, increasing final indices are stable and preserve
            // the visible selection order.
            for id in &roots {
                self.ask_pane(koru_workspace_host::PaneRequest::MoveDocument {
                    id: *id,
                    parent,
                    index: usize::MAX,
                });
            }
            if !append {
                for (offset, id) in roots.into_iter().enumerate() {
                    self.ask_pane(koru_workspace_host::PaneRequest::MoveDocument {
                        id,
                        parent,
                        index: base + offset,
                    });
                }
            }
            return true;
        }

        // A carried node nested under another carried one rides along with the
        // parent's subtree move; moving it again would fight that, so drop it.
        let carried: HashSet<NodeId> = nodes.iter().copied().collect();
        let roots: Vec<NodeId> = nodes
            .iter()
            .copied()
            .filter(|n| {
                !ws.ancestors(*n)
                    .map(|a| a.iter().any(|a| carried.contains(a)))
                    .unwrap_or(true)
            })
            .collect();

        // The anchor index is the target's place among its **non-carried**
        // siblings. `move_node` removes a node before inserting it, so an index
        // counted against the full list would be off by however many carried
        // nodes sat below it — counting only the non-carried is what makes a
        // multi-move land contiguous regardless of where each node came from.
        let (parent, base, append) = match target {
            DropTarget::Library => {
                let base = ws
                    .children(Parent::Root)
                    .unwrap_or_default()
                    .iter()
                    .filter(|n| !carried.contains(n))
                    .count();
                (Parent::Root, base, false)
            }
            DropTarget::Row {
                node,
                zone: DropZone::Nest,
            } => (Parent::Node(node), 0, true),
            DropTarget::Row { node, zone } => {
                let parent = match ws.parent(node) {
                    Ok(p) => p,
                    Err(_) => {
                        self.explore.clear_animation();
                        self.explore_stale = true;
                        return false;
                    }
                };
                let siblings = ws.children(parent).unwrap_or_default();
                let target_si = ws.sibling_index(node).unwrap_or(0);
                let non_carried_before = siblings[..target_si]
                    .iter()
                    .filter(|n| !carried.contains(n))
                    .count();
                let base = non_carried_before + usize::from(zone == DropZone::After);
                (parent, base, false)
            }
        };

        // One Ctrl+Z reverts the whole drag — the same bracket the editor uses
        // around a compound edit.
        ws.begin_group();
        for (k, node) in roots.iter().enumerate() {
            let parent_len = ws.children(parent).map(|c| c.len()).unwrap_or(0);
            // `move_node` errors on an out-of-range index, and a node already in
            // `parent` is removed before insert (so its reduced length is one
            // less). Account for both: append for Nest, clamp otherwise.
            let already_here = matches!(ws.parent(*node), Ok(p) if p == parent);
            let max_idx = if already_here {
                parent_len.saturating_sub(1)
            } else {
                parent_len
            };
            let idx = if append {
                max_idx
            } else {
                (base + k).min(max_idx)
            };
            let _ = ws.move_node(*node, parent, idx);
        }
        ws.checkpoint();
        true
    }

    /// Ask the shell for something.
    ///
    /// **Queued, not stored in a slot.** It was one `Option`, so a turn that
    /// asked twice kept only the *last* ask and dropped the rest without a word
    /// — six documents dragged to the Trash produced six requests and one
    /// binning. Anything that answers a gesture with several asks hits that, and
    /// it fails silently every time, so the container holds all of them and the
    /// shell drains them in order.
    fn ask_pane(&mut self, req: PaneRequest) -> bool {
        if matches!(req, PaneRequest::Switch(_) | PaneRequest::SwitchProfile(_))
            && self.socials_window_close_veto()
        {
            return true;
        }
        self.pane_requests.push_back(req);
        true
    }

    /// Turn a hit into what it means. Chrome reports; this decides.
    ///
    /// `(x, y)` is the press's own position. `x` is what the tab travels with
    /// once the pointer starts carrying it; `y` is where a tear-off is measured
    /// from.
    fn press(&mut self, hit: Hit, x: f32, y: f32, ctrl: bool) -> Outcome {
        // The offline chip is handled at the top of `mouse_down`, not here — it
        // is not a `Hit` variant, and dismissing it has to answer a press
        // anywhere at all rather than only the ones the caption claims.
        match hit {
            Hit::Tab(i) => {
                self.ensure_tab_widths();
                let slot = tabs::layout(self.strip(), &self.tab_widths(), self.tab_scroll())
                    .get(i)
                    .map_or(x, |t| t.body.x);
                *self.group_drag_mut() = Some(panes::TabDrag {
                    index: i,
                    grab_dx: x - slot,
                    x,
                    origin_y: y,
                    y,
                });
                Outcome::SelectTab(i)
            }
            Hit::TabClose(i) => Outcome::CloseTab(i),
            Hit::NewTab => Outcome::NewTab,
            Hit::TabScroll => {
                self.ensure_tab_widths();
                let strip = self.strip();
                if let Some(thumb) =
                    tabs::scroll_thumb(strip, &self.tab_widths(), self.tab_scroll())
                {
                    self.set_tab_scroll_drag(Some(x - thumb.x));
                } else {
                    self.set_tab_scroll(tabs::scroll_from_pointer(strip, &self.tab_widths(), x));
                }
                Outcome::Consumed
            }
            Hit::ToggleSidebar => {
                // The hint has been acted on; it must not sit there afterwards
                // re-explaining what just happened, with its verb flipped.
                self.toggle_tip_muted = true;
                Outcome::ToggleSidebar
            }
            Hit::ToggleContext => Outcome::ToggleContext,
            Hit::Search => {
                return if self.open_search() {
                    Outcome::Consumed
                } else {
                    Outcome::Ignored
                };
            }
            Hit::HostAction => Outcome::Consumed,
            Hit::Caption => Outcome::DragWindow,
            Hit::Window(CaptionAction::Minimise) => Outcome::Minimise,
            Hit::Window(CaptionAction::ToggleMaximise) => Outcome::ToggleMaximise,
            // **Ctrl and the ✕ puts Koru in the tray rather than quitting
            // it.** The plain ✕ still quits, so the ordinary gesture keeps its
            // ordinary meaning and going to the tray is something you have to
            // mean — the same modifier that means "somewhere else" everywhere
            // it already appears here.
            Hit::Window(CaptionAction::Close) if ctrl => Outcome::HideToTray,
            Hit::Window(CaptionAction::Close) => Outcome::CloseWindow,
            Hit::Window(CaptionAction::WindowMenu) => self
                .window_menu_button()
                .and_then(|button| button.action_at(x, y))
                .map_or(Outcome::Ignored, |action| {
                    self.run_icon_action(action, x, y)
                }),
            Hit::Sidebar | Hit::Context => Outcome::Consumed,
            Hit::Content => Outcome::Ignored,
        }
    }

    /// Bring the tab that is about to be drawn up to date with the document.
    ///
    /// Called from two places, and both are needed: `adopt`, so a change that
    /// has just landed is on screen in the frame it asks for; and `draw_frame`,
    /// so a tab that was in the background while the changes arrived settles
    /// the moment it is switched to. A no-op unless something is owed, which is
    /// almost every frame.
    ///
    /// A draft is skipped for the reason it is skipped in `adopt`: it is a view
    /// over its own private document, not this window's.
    ///
    /// **A leaf is not reflowed again here.** `TabView::catch_up` settles it
    /// through `Editing::catch_up`, which lays out as its fourth step — a caret
    /// re-anchored against the merged CRDT is only *queued* until it does, so
    /// the layout cannot be somebody else's to arrange. Reflowing again would be
    /// a second O(rows) pass on the path AGENTS.md's responsiveness rules are
    /// about ([ADR-343-overlaycatchup](../../../docs/decisions/README.md)).
    fn catch_up_active(&mut self, ws: &WorkspaceDoc) {
        let tab = self.tabs_mut().active_mut();
        if tab.draft.is_some() {
            return;
        }
        if let TabView::Leaf(l) = &tab.view
            && let Some(root) = l.body().root
            && ws.read(root).is_err()
        {
            crate::mainview::nav_debug::nav_debug!(
                "[koru:nav] catch-up skip root_in_ws=false rows={}",
                l.body().rows.len(),
            );
            return;
        }
        let rows_before = match &tab.view {
            TabView::Leaf(l) => l.body().rows.len(),
            _ => 0,
        };
        let structural = match &tab.view {
            TabView::Leaf(l) => l.body().is_stale(),
            _ => false,
        };
        if !tab.view.catch_up(ws) {
            return;
        }
        if let TabView::Leaf(l) = &tab.view {
            crate::mainview::nav_debug::nav_debug!(
                "[koru:nav] catch-up root_in_ws=true pending_structural={structural} rows_before={rows_before} rows_after={}",
                l.body().rows.len(),
            );
        }
        self.explore_stale = true;
    }

    /// **The active tab asked to be shown on the Tasks page.**
    ///
    /// Drained where the container already sweeps the active leaf after a
    /// dispatch, and only from the active one: the ask comes from a menu, and
    /// a menu is only ever open over the tab in front.
    pub(crate) fn drain_task_view_request(&mut self, ws: &WorkspaceDoc) -> bool {
        let node = match &mut self.tabs_mut().active_mut().view {
            TabView::Leaf(l) => View::take_wants_task_view(l),
            _ => None,
        };
        match node {
            Some(node) => self.reveal_task_in_tasks_view(ws, node),
            None => false,
        }
    }

    /// Take the task activity every open tab's editor owes, leaving none
    /// behind ([ADR-286]).
    ///
    /// **Every tab, not just the active one.** A tab that is not on screen is
    /// still a live `Body`, and a command applied from a menu or the explore
    /// column can land on a document another tab is rooted at; draining only
    /// what is in front would strand those intents until that tab happened to
    /// be clicked. The sweep costs a `mem::take` of an empty `Vec` per tab.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub fn take_task_activity(&mut self) -> Vec<koru_editor::tasks::TaskActivity> {
        let mut owed = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Leaf(leaf) = &mut tab.view {
                    owed.extend(Editing::body_mut(leaf).take_task_activity());
                }
            }
        }
        // **A canonical command ran in an editor**, which is the one signal that
        // covers `/due`, the checkbox and every menu row that acts on a document
        // this window holds. Noticed here because this is the sweep that already
        // visits every tab after every dispatched event.
        //
        // **All three surfaces, not one.** Until this call it invalidated the
        // context column alone, and the Tasks page and the rail's numbers were
        // left on their own half-minute deadline — so typing `/task` put a row
        // in a document, and the queue it belonged to went on saying *Nothing
        // waiting* while the number beside it had already moved. Two readings
        // of one workspace, disagreeing on screen, for up to thirty seconds.
        //
        // The refresh is still gated where it was: `tasks_changed` marks, and
        // each surface pays only if it is visible. What changes is that the
        // marking is caused by the edit rather than by a clock.
        // **The invalidation that used to be here moved to
        // [`note_task_activity`](Self::note_task_activity)**, which the
        // dispatcher calls with the written document in hand — so the mark
        // can carry the write's version rather than only the fact of it.
        owed
    }

    /// The tasks every open tab's editor has changed the text of, taken.
    ///
    /// **Every tab, for the same reason [`take_task_activity`] sweeps them
    /// all** — a menu row or an explore-column action can edit a document
    /// another tab is rooted at. The boundary sorts out what that means: a task
    /// edited where the caret is not ends immediately, since there is no caret
    /// there to leave it later.
    ///
    /// [`take_task_activity`]: Self::take_task_activity
    fn take_task_text_edits(&mut self) -> Vec<koru_core::EntityRef> {
        let mut edited = Vec::new();
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Leaf(leaf) = &mut tab.view {
                    edited.extend(Editing::body_mut(leaf).take_task_text_edits());
                }
            }
        }
        edited
    }

    /// **One dispatched event's worth of editing, resolved into ended bursts**
    /// ([ADR-286]). The caller emits them.
    ///
    /// The caret is the *active* tab's, because that is where a window's caret
    /// is, and it is read behind a closure so an event that edited no task and
    /// left no burst open costs a `Vec::is_empty` per tab and nothing else.
    ///
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    pub(crate) fn drain_content_bursts(&mut self, ws: &WorkspaceDoc) -> Vec<koru_core::EntityRef> {
        let edits = self.take_task_text_edits();
        let Self {
            panes, task_caret, ..
        } = self;
        task_caret.dispatched(edits, || match &panes.focused().tabs().active_tab().view {
            TabView::Leaf(leaf) => Editing::body(leaf).caret_task(ws),
            _ => None,
        })
    }

    /// **A close took the row this window's burst was open on** ([ADR-286]).
    ///
    /// Called from the tail every close path shares. It asks whether the task
    /// still has a row *anywhere* in the strip rather than whether the closed
    /// tab held it, because a document can be open in two tabs and the burst
    /// belongs to the caret, not to a tab index that has just been renumbered.
    ///
    /// At most one burst per window can be open (see [`TaskCaret`]), so this is
    /// a field test in the ordinary case and a walk of the remaining tabs' rows
    /// only when somebody was mid-edit as a tab went.
    ///
    /// [`TaskCaret`]: crate::tasks::TaskCaret
    /// [ADR-286]: ../../../docs/decisions/ADR-276-300.md#adr-286-taskboundary--six-canonical-commands-own-task-state-and-every-host-is-a-thin-wrapper-around-them
    fn flush_closed_tab_bursts(&mut self) {
        let Some(open) = self.task_caret.open() else {
            return;
        };
        let node = open.node_id();
        let still_shown = self.panes.iter_groups().any(|(_, g)| {
            g.iter_tabs().any(|tab| match &tab.view {
                TabView::Leaf(leaf) => Editing::body(leaf).row(node).is_some(),
                _ => false,
            })
        });
        if still_shown {
            return;
        }
        let ended = self.task_caret.gone();
        crate::tasks::content_changed(&ended);
    }

    /// `visible` is whether the column has any width to draw into. A shut
    /// sidebar still tracks that the tree is stale — it just does not pay to
    /// build rows nobody can see, and builds them when it opens.
    fn refresh_explore(&mut self, ws: &WorkspaceDoc, visible: bool) {
        // A structural change in the active tab — a row added, deleted or moved,
        // local or remote — changes what the explore tree shows. `rebuild` sets
        // the body's flag; drain it here so a deleted code block disappears from
        // the tree on the next frame rather than when something else happens.
        if let TabView::Leaf(l) = &mut self.tabs_mut().active_mut().view {
            if l.take_structure_dirty() {
                self.explore_stale = true;
                // A row deleted or moved changes which tasks this document
                // holds, and no task command ran to say so.
                self.context_tasks_stale = true;
            }
        }
        // Somebody else may have taken back something this window binned. Cheap
        // — the set is only what this session deleted — and this already runs
        // once per structural change.
        self.notice_restores(ws);
        if self.explore_stale
            && visible
            // While a drag is armed the cache is authoritative — rebuilding
            // would fight the in-flight reorder — and while the release slide
            // is settling the cache already matches the move, so a rebuild can
            // wait until the motion is done. Either way `explore_stale` stays
            // true so the rebuild happens the moment it can.
            && !self.explore.drag_armed()
            && !self.explore.slides_animating()
            // Held while a title is being typed, for the same reason the tab
            // label is: the tree shows that name too, and a row that changes
            // width and re-sorts on every keystroke is worse than one that
            // waits. The flag survives, so the rebuild lands the moment the
            // caret leaves.
            && self.naming.is_none()
        {
            // **From the manifest when there is one.** `rebuild(ws)` projects
            // from the document in hand, which store-backed is the *active
            // document* — so Explore listed that page's nested documents rather
            // than the workspace's, which is to say nothing at all.
            match &self.entries {
                Some(entries) => self.explore.rebuild_from_entries(entries),
                None => self.explore.rebuild(ws),
            }
            self.explore_stale = false;
        }
        // A row asked for by `ExploreView::reveal` — a search result's document
        // — brought into the band. Here rather than at the `reveal` call because
        // this is the first point after the rows are rebuilt, and the row it
        // wants may not have existed before that; the request survives a miss,
        // so it also covers the case where the column was shut or the window was
        // still swapping workspaces.
        if visible {
            self.explore.apply_reveal(self.regions.sidebar);
        }
        self.sync_taskbar_recents();
        // Never highlight a draft's private root in the library tree.
        let selected =
            if self.explore.navigator_mode() == crate::panes::explore::NavigatorMode::Agents {
                // The Agents rows are keyed by the session's UUID bytes, so the
                // row to mark is the active session tab's session — a document id
                // could never match one, and the reverse also holds.
                //
                // **And a run's row the same way** (ADR-379). A run row is
                // keyed by the run's own uuid, so the tab showing a run cards
                // its row exactly as a session tab cards its session's — the
                // the maintainers' *active selection style background*, from the painter
                // the library already uses. Without this arm the reveal's own
                // `set_selected` was undone by the next frame.
                match &self.tabs().active_tab().view {
                    TabView::Session(view) => view.session.map(|s| NodeId::from_uuid(s.as_uuid())),
                    TabView::Run(view) => view.run().map(|r| NodeId::from_uuid(r.as_uuid())),
                    _ => None,
                }
            } else if self.explore.navigator_mode() == crate::panes::explore::NavigatorMode::Files {
                // Files become active without opening a tab. Deriving this value
                // from the active document on every draw erased the file that
                // global search or an import toast had just selected.
                self.explore.selected()
            } else if self.tabs().active_tab().draft.is_some() {
                self.tabs()
                    .active_tab()
                    .draft
                    .as_ref()
                    .and_then(|d| d.library_anchor)
                    .filter(|&id| ws.read(id).is_ok())
            } else if self.entries.is_some() {
                // **From the manifest's coordinate system, matching the rows.**
                // `rebuild_from_entries` keys every row by `NodeId::from_uuid(
                // entry.id.as_uuid())`, so the row to mark is the *document's* id
                // spelled the same way. `document_root()` is a node inside that
                // document, which matches nothing here — the mismatch that left the
                // open document looking no different from the rest.
                self.active_document
                    .map(|id| NodeId::from_uuid(id.as_uuid()))
            } else {
                self.document_root()
            };
        // A pending Reveal wins over the active-document highlight once the
        // home workspace's tree actually holds the row — applying earlier
        // against the outgoing workspace would spend the pending and miss.
        let reveal_ready = self.pending_explore_reveal.is_some_and(|node| {
            self.entries.as_ref().is_some_and(|entries| {
                entries
                    .iter()
                    .any(|e| NodeId::from_uuid(e.id.as_uuid()) == node)
            })
        });
        if reveal_ready {
            self.apply_pending_explore_reveal();
            if visible {
                self.explore.apply_reveal(self.regions.sidebar);
            }
        } else {
            self.explore.set_selected(selected);
        }
        self.explore.set_footer_active(
            matches!(self.tabs().active_tab().view, TabView::Trash(_))
                .then_some(crate::panes::explore::FooterRow::Trash),
        );
        // The rail cards the queue the Tasks tab is on, exactly as the footer
        // cards Trash — **and this is the half that runs when the tab was not
        // pressed for**: a session restore, or Ctrl+Tab back to it. The rail
        // records its own selection on a press, so `tasks_selection` is asked
        // first and nothing is written when the two already agree.
        let tasks_target = match &self.tabs().active_tab().view {
            TabView::Tasks(t) => Some(t.target()),
            // A Reminders page cards the Reminders row, the same rule.
            TabView::Reminders(_) => Some(crate::panes::explore::rail::RailTarget::Reminders),
            _ => None,
        };
        if let Some(target) = tasks_target
            && self.tasks_selection() != Some(target)
        {
            self.explore.rail_mut().set_selected(Some(target));
        }
        if self.context_stale
            && let Some((profiles, current_profile, workspaces, current)) =
                self.pane_context.clone()
        {
            self.context_stale = false;
            self.explore
                .set_context(profiles, current_profile, workspaces, current);
        }
    }

    // ------------------------------------------------------- the tasks rail
    //
    // The rail is chrome: it reports a gesture and performs none of it. What
    // it needs from here is three things — the list table (names, swatches,
    // order), the counts, and somewhere for the selection to be read from —
    // and the container owes it one thing back: the inline field, because a
    // `TextInput` is the editor's and chrome cannot see one.

    /// What the rail is showing as selected.
    ///
    /// **The seam V-2 reads.** The Tasks tab does not exist yet; when it does,
    /// it is opened from here and pushes its own selection back with
    /// [`ExploreView::rail_mut`]`().set_selected(..)` — so the two never keep
    /// two answers to *which bucket am I looking at*.
    pub(crate) fn tasks_selection(&self) -> Option<crate::panes::explore::rail::RailTarget> {
        self.explore.rail().selected()
    }

    /// Turn the list table and the latest counts into the rail's rows.
    ///
    /// Names come from the workspace (the system document), counts from the
    /// store, and **a list with no group row counts zero** — the store omits a
    /// list nothing matches rather than sending it as a zero.
    fn refresh_rail_lists(&mut self) -> bool {
        let lists: Vec<crate::panes::explore::rail::RailList> = self
            .list_table
            .iter()
            .map(|entry| {
                let (count, overdue) = self.task_counts.group(entry.id);
                crate::panes::explore::rail::RailList {
                    id: entry.id,
                    name: entry.name.clone(),
                    color: entry.color.clone(),
                    count,
                    overdue,
                }
            })
            .collect();
        self.explore.rail_mut().set_lists(lists)
    }

    /// A press the rail reported.
    fn apply_tasks_rail(&mut self, request: crate::panes::explore::rail::TasksRailRequest) -> bool {
        use crate::panes::explore::rail::TasksRailRequest as Request;
        match request {
            // V-2 owns the content column. Until it exists the rail records
            // the selection and the window redraws with the row carded — the
            // seam is `tasks_selection`, deliberately live now so that the tab
            // is wired to something that already works.
            // The rail records the selection as it is pressed; this opens the
            // tab on it, or re-points the one that is already there.
            Request::OpenBucket { bucket, new_tab } => {
                self.close_list_field();
                self.open_tasks_tab(
                    crate::panes::explore::rail::RailTarget::Bucket(bucket),
                    new_tab,
                )
            }
            Request::OpenList { id, new_tab } => {
                self.close_list_field();
                self.open_tasks_tab(crate::panes::explore::rail::RailTarget::List(id), new_tab)
            }
            // The rail's Reminders row opens the All Reminders *page*: the
            // same surface the bell hosts, shown as a tab — so it re-points
            // (or opens) exactly like a queue. The bell's compact popover is
            // the other host, and this request never goes near it.
            Request::OpenReminders { new_tab } => {
                self.close_list_field();
                self.open_tasks_tab(crate::panes::explore::rail::RailTarget::Reminders, new_tab)
            }
            Request::BeginNewList => {
                let color = self.explore.rail().suggested_color().to_owned();
                self.close_list_field();
                self.explore.rail_mut().begin_new_list();
                self.new_list = Some(new_list::NewList::new(&mut self.engine, &color));
                true
            }
            Request::NewList { name, color } => {
                self.wants_create_list = Some((name, color));
                true
            }
            // The order the rows are already in on the rail, written to the
            // table so every window and every peer agrees with it.
            Request::ReorderList { id, to } => {
                let _ = (id, to);
                let order: Vec<koru_core::ids::ListId> =
                    self.explore.rail().list_order().into_iter().collect();
                self.list_edits
                    .push(koru_workspace_host::tasks::ListEdit::Reorder(order));
                true
            }
            Request::RowMenu { target, at } => self.open_rail_menu(target, at),
            Request::RenameList { id, name } => {
                self.list_edits
                    .push(koru_workspace_host::tasks::ListEdit::Rename { id, name });
                true
            }
            Request::RecolorList { id, color } => {
                self.list_edits
                    .push(koru_workspace_host::tasks::ListEdit::Recolor {
                        id,
                        color: Some(color),
                    });
                true
            }
            Request::DeleteList(id) => {
                self.list_edits
                    .push(koru_workspace_host::tasks::ListEdit::Delete(id));
                // The counts are about to be wrong in two ways — a list's row
                // goes, and the tasks that were in it move — so the standing
                // invalidation is asked rather than waiting for the coarse tick.
                self.task_counts_invalidate();
                true
            }
        }
    }

    /// Open a rail row's menu at the cursor.
    ///
    /// **A queue's menu is one row and a list's is four**, which is the whole
    /// difference between them said in the only place it shows: there is
    /// nothing about a computed queue to rename, recolour or delete, and
    /// *Open in new tab* is a thing you can do to either. The Reminders row
    /// wears the queue's one row, for the queue's reason.
    fn open_rail_menu(
        &mut self,
        target: crate::panes::explore::rail::RailTarget,
        at: (f32, f32),
    ) -> bool {
        match target {
            crate::panes::explore::rail::RailTarget::List(id) => self.open_list_menu(id, at),
            bucket => {
                self.close_list_field();
                let mut m = ContextMenu::new(
                    &mut self.engine,
                    MenuSubject::RailTarget(bucket),
                    at,
                    context_menu::rail_bucket_items(),
                );
                let enabled = self.enabled_items(&m);
                m.step(&enabled, 1);
                self.menu = Some(m);
                self.submenu = None;
                self.nested = None;
                true
            }
        }
    }

    /// Open a list row's menu at the cursor.
    ///
    /// The rows are fixed here, from the list as it is now — `context_menu`'s
    /// rule, and it bites harder on a list than on a tab: the table is a cache
    /// of a system document another window writes.
    fn open_list_menu(&mut self, id: koru_core::ids::ListId, at: (f32, f32)) -> bool {
        // A field left open under a menu about the same row would take the
        // keystrokes the menu's own arrows are for.
        self.close_list_field();
        let mut m = ContextMenu::new(
            &mut self.engine,
            MenuSubject::List(id),
            at,
            context_menu::list_items(),
        );
        let enabled = self.enabled_items(&m);
        m.step(&enabled, 1);
        self.menu = Some(m);
        self.submenu = None;
        self.nested = None;
        true
    }

    /// Open the rename field over a list's own row.
    ///
    /// Nothing is asked of the rail beyond reserving the row: the box, the
    /// keyboard and the duplicate check are the container's, exactly as they are
    /// for the `+` row — see [`new_list`].
    fn begin_list_rename(&mut self, id: koru_core::ids::ListId) -> bool {
        let Some(entry) = self.list_table.iter().find(|entry| entry.id == id) else {
            return false;
        };
        let (name, color) = (entry.name.clone(), entry.color.clone());
        self.close_list_field();
        self.explore.rail_mut().begin_rename(id);
        self.new_list = Some(new_list::NewList::renaming(
            &mut self.engine,
            id,
            &name,
            color.as_deref(),
        ));
        true
    }

    /// Put the deletion question up. **Nothing is queued here** — the edit is
    /// pushed by `settle_confirm` on *yes*, which is what makes the dialog the
    /// one door.
    ///
    /// The second sentence is the one that matters, and it is the reason there
    /// is a dialog at all: what a person is afraid of when they delete a list is
    /// losing the work in it, and the answer is that they do not. The list is
    /// what goes.
    fn ask_delete_list(&mut self, id: koru_core::ids::ListId) -> bool {
        let Some(entry) = self.list_table.iter().find(|entry| entry.id == id) else {
            return false;
        };
        self.confirm = Some((
            koru_ui::Confirm::new(
                "Delete this list?",
                format!(
                    "\u{201C}{}\u{201D} will be removed from the sidebar. Tasks in it are not \
                     deleted \u{2014} they just leave the list.",
                    entry.name
                ),
                "Delete list",
            ),
            trash::ConfirmSubject::DeleteList(id),
        ));
        true
    }

    /// Put away whichever inline list field is up — the `+` row's or a rename —
    /// and tell the rail, so the row it reserved comes back.
    ///
    /// **One function, because the two fields are one field.** Every path that
    /// used to clear `new_list` had to remember `end_new_list` beside it; a
    /// rename adds a third thing to remember, and a path that forgets leaves a
    /// row drawing no name at all.
    fn close_list_field(&mut self) -> bool {
        let had = self.new_list.is_some();
        self.new_list = None;
        self.explore.rail_mut().end_new_list();
        self.explore.rail_mut().end_rename();
        had
    }

    /// Every live list's name, so the field can refuse a duplicate without
    /// learning what a workspace is.
    ///
    /// **A rename does not clash with itself**: the list being renamed is left
    /// out, or pressing Enter on a name you have not changed would be refused as
    /// a duplicate of the row you are standing on.
    fn list_names(&self) -> Vec<String> {
        let mine = match self.new_list.as_ref().map(|field| field.target()) {
            Some(new_list::Target::Existing(id)) => Some(id),
            _ => None,
        };
        self.list_table
            .iter()
            .filter(|entry| Some(entry.id) != mine)
            .map(|entry| entry.name.clone())
            .collect()
    }

    /// Where the field is drawn and where its presses are measured — one
    /// rectangle, from the rail's own geometry.
    ///
    /// Which row it is over is the field's own `target`, so the box that is
    /// drawn and the box clicks are measured against cannot end up on two
    /// different rows.
    fn new_list_field_rect(&self) -> Option<Rect> {
        let area = self.regions.sidebar;
        let top = self.explore.rail_top(area);
        match self.new_list.as_ref()?.target() {
            new_list::Target::New => self.explore.rail().new_list_field_rect(area, top),
            new_list::Target::Existing(id) => self.explore.rail().list_field_rect(area, top, id),
        }
    }

    /// Close the field, keeping whatever it decided.
    fn finish_new_list(&mut self, outcome: new_list::Outcome) -> bool {
        match outcome {
            new_list::Outcome::Keep => true,
            new_list::Outcome::Cancel => {
                self.close_list_field();
                true
            }
            new_list::Outcome::Commit(name) => {
                let field = self.new_list.as_ref();
                let color = field.map(|field| field.color().to_owned());
                let target = field.map(|field| field.target());
                self.close_list_field();
                // Through the same door a press on the rail uses, so a list has
                // one vocabulary from the gesture to the row that changes.
                let request = match target {
                    Some(new_list::Target::Existing(id)) => {
                        crate::panes::explore::rail::TasksRailRequest::RenameList { id, name }
                    }
                    _ => crate::panes::explore::rail::TasksRailRequest::NewList { name, color },
                };
                self.apply_tasks_rail(request)
            }
        }
    }

    /// A key while the inline field is up. `None` means there is no field, and
    /// the press belongs to whatever would have had it.
    fn new_list_key(
        &mut self,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> Option<bool> {
        let taken = self.list_names();
        let field = self.new_list.as_mut()?;
        let outcome = field.key(&mut self.engine, key, text, mods, &taken);
        Some(self.finish_new_list(outcome))
    }

    /// Offer the counts a reason to be refreshed now rather than on the next
    /// coarse tick: something in this window changed a task.
    pub(crate) fn task_counts_invalidate(&mut self) {
        self.task_counts.invalidate();
    }

    /// Offer the recents to the taskbar, so its Recent list is the same list the
    /// welcome tab shows.
    ///
    /// Guarded by a flag rather than run per frame: the names have to be looked
    /// up in the live listing, and `set_recents` compares before it does
    /// anything — but building ten strings sixty times a second to hand them to
    /// a comparison is work nobody asked for.
    fn sync_taskbar_recents(&mut self) {
        if !self.recents_stale {
            return;
        }
        self.recents_stale = false;
        // One source, every surface: the same resolved list the welcome tab
        // shows, handed to Explorer as stable document ids.
        let entries = self.recent_documents();
        crate::taskbar::set_recents(entries);
        crate::taskbar::set_pinned(self.pinned_documents());
    }

    fn pinned_rows(&self) -> Vec<(String, koru_core::DocumentId, koru_core::WorkspaceId, bool)> {
        self.panes
            .iter_groups()
            .flat_map(|(_, group)| group.tabs().iter())
            .filter(|t| t.is_pinned())
            .filter_map(|t| {
                let id = t.document?;
                let workspace = t.pinned?;
                let name = match t.icon.as_deref() {
                    Some(icon) => format!("{icon} {}", t.label),
                    None => t.label.clone(),
                };
                Some((name, id, workspace, t.show_title))
            })
            .collect()
    }

    fn pinned_documents(&self) -> Vec<(String, koru_core::DocumentId)> {
        self.pinned_rows()
            .into_iter()
            .map(|(name, id, _, _)| (name, id))
            .collect()
    }

    /// The recently opened documents this container can name, most recent first
    /// and with anything deleted or outside the current workspace dropped.
    ///
    /// **The one resolver** the welcome tab and the Jump List both draw from —
    /// see [`set_recents`](crate::welcome::Welcome::set_recents). Resolved
    /// against the live store listing (`self.entries`), never against whichever
    /// document happens to be under the caret, so a list of document ids means
    /// a list of documents, not a single active one.
    fn recent_documents(&self) -> Vec<(String, koru_core::DocumentId)> {
        let Some(entries) = self.entries.as_ref() else {
            return Vec::new();
        };
        self.recents
            .iter()
            .filter_map(|&id| {
                entries
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| (entry.title.clone(), id))
            })
            .take(crate::taskbar::RECENT_MAX)
            .collect()
    }

    fn drain_editor_toast(&mut self) -> bool {
        let message = match self.active_view() {
            TabView::Leaf(leaf) => leaf.take_wants_toast(),
            _ => None,
        };
        let Some(message) = message else {
            return false;
        };
        self.toast = Some((
            koru_ui::Toast::new(message, None, Instant::now()),
            ToastPayload::None,
        ));
        true
    }

    /// The welcome page's *Apply to all* reaches every open document tab — the
    /// one place a tab can touch the others. Open tabs switch to the global
    /// mode now (the active one reflows; the rest reflow when shown); the
    /// per-document overrides are cleared at the next `capture` so documents
    /// not yet open follow it too.
    fn apply_layout_to_all_documents(&mut self) {
        let mode = theme::default_view_mode();
        let canvas = theme::default_canvas_bg();
        for (_, group) in self.panes.iter_groups_mut() {
            let active = group.active();
            for (i, t) in group.iter_tabs_mut().enumerate() {
                set_tab_layout(t, i == active, mode, canvas);
            }
        }
        self.clear_doc_view_prefs.set(true);
    }

    /// **+ → Documents → Clear recents.** The container owns the persisted
    /// list (`Settings::recents`), so the wipe happens here, and the Jump List
    /// follows on its next sync.
    fn clear_recents(&mut self) -> bool {
        if self.recents.is_empty() {
            return false;
        }
        self.recents.clear();
        self.recents_stale = true;
        true
    }

    /// Perform what the active Welcome tab asked for, and save the rows it
    /// ticked machine-wide. Every want is an existing command; the page only
    /// names it.
    fn drain_welcome(&mut self) {
        let (wants, ticked) = match &mut self.tabs_mut().active_mut().view {
            TabView::Welcome(view) => (view.take_wants(), view.take_newly_done()),
            _ => return,
        };
        if !ticked.is_empty() && self.welcome_progress.mark(ticked) {
            self.sync_welcome_lists();
        }
        use crate::welcome::WelcomeWant;
        for want in wants {
            match want {
                WelcomeWant::NewDocument => self.prompt_new_document(),
                WelcomeWant::VoiceSettings => {
                    self.open_voice_settings_tab();
                }
                WelcomeWant::Import(format) => {
                    self.ask_pane(PaneRequest::ImportContent {
                        format,
                        source: koru_workspace_host::ImportSourceSelection::Picker,
                    });
                }
                WelcomeWant::ImportFiles => {
                    self.ask_pane(PaneRequest::ImportFiles(
                        koru_workspace_host::ImportSourceSelection::Picker,
                    ));
                }
                WelcomeWant::Reminders => {
                    self.open_all_reminders();
                }
                WelcomeWant::Feedback => {
                    self.open_send_feedback_tab();
                }
                WelcomeWant::Email => {
                    self.ask_pane(PaneRequest::OpenUri(koru_ui::welcome::SUPPORT_MAILTO));
                }
                WelcomeWant::CopyMcpCommand => {
                    koru_editor::clipboard::put_system_text(&crate::welcome::mcp_command());
                }
                WelcomeWant::LinkDevice => {
                    self.open_link_device();
                }
                WelcomeWant::Settings => {
                    self.open_settings_tab();
                }
                // The tab Settings → AI's *Providers and models* row opens.
                WelcomeWant::AiSetup => {
                    self.perform_settings_request(koru_settings_ui::Request::OpenAiSetup);
                }
                WelcomeWant::Agents => {
                    self.open_agents_menu();
                }
            }
        }
    }

    /// **Perform what the Settings surface asked for.**
    ///
    /// Everything in this list is an effect that belongs to somebody else — the
    /// shell's live `DesktopPreferences`, `renderer.conf`, the voice worker's
    /// globals, a catalog and several hundred megabytes, every open document
    /// tab. The surface raises a request and this is where the vocabulary is
    /// translated back into Koru's. Each was its own `take_wants_*` before
    /// SP-7; one drain, because they arrive on one channel now.
    fn drain_settings_requests(&mut self) {
        let (requests, devices, reminder_intents) = match &mut self.tabs_mut().active_mut().view {
            TabView::Settings(s) => {
                let mut reminder_intents = Vec::new();
                while let Some(intent) = s.take_reminder_intent() {
                    reminder_intents.push(intent);
                }
                (
                    s.take_requests(),
                    s.take_wants_voice_devices(),
                    reminder_intents,
                )
            }
            _ => (Vec::new(), false, Vec::new()),
        };
        // It opens nothing, downloads nothing and starts no worker. What it
        // cannot be is a call from a view, because it blocks on a socket.
        if devices {
            self.ask_pane(PaneRequest::AskVoiceDevices);
        }
        for request in requests {
            self.perform_settings_request(request);
        }
        if !reminder_intents.is_empty() {
            for intent in reminder_intents {
                self.apply_reminder_settings_intent(intent);
            }
            self.sync_reminder_settings_views();
        }
    }

    /// **This device's telemetry answer, followed on the maintenance pass.**
    ///
    /// Every Settings tab's *Share anonymous usage data* switch shows what the
    /// shell's one reading of the record says
    /// (`koru_workspace_host::telemetry_consent`, which a worker thread keeps
    /// current — nothing here touches a socket), including after a write that
    /// did not land.
    ///
    /// **Nothing asks.** A device with no answer shares nothing, and the
    /// switch is where somebody opts in; the question that used to come up
    /// after Welcome on a first start is gone (maintainers, 2026-10-19).
    fn maintain_telemetry_consent(&mut self) -> bool {
        use koru_workspace_host::telemetry_consent;
        let reading = telemetry_consent::current().toggle();
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Settings(s) = &mut tab.view {
                    changed |= s.set_telemetry_consent(reading);
                }
            }
        }
        changed
    }

    /// Take whatever the feedback outbox finished with.
    ///
    /// **Success says nothing.** The thank-you page already thanked them, and
    /// a toast arriving seconds later — possibly over a different tab — would
    /// only tell them something they were entitled to assume. A failure has to
    /// speak, because it is the one outcome that page got wrong: the draft
    /// goes back so the form refills, and the toast offers it.
    fn drain_feedback_outbox(&mut self, now: Instant) -> bool {
        match crate::feedback::outbox::poll(now) {
            Some(crate::feedback::outbox::Outcome::Sent) => false,
            Some(crate::feedback::outbox::Outcome::Failed { draft }) => {
                crate::feedback::draft::save(*draft);
                self.toast = Some((
                    koru_ui::Toast::new(
                        "Your feedback couldn't be sent. It's been kept — reopen Feedback to try again.".to_string(),
                        Some("Reopen"),
                        now,
                    ),
                    ToastPayload::ReopenFeedback,
                ));
                true
            }
            None => false,
        }
    }

    fn drain_feedback(&mut self) {
        let TabView::Feedback(view) = &mut self.tabs_mut().active_mut().view else {
            return;
        };
        if view.take_wants_close() {
            let i = self.tabs().active();
            self.close(i);
            return;
        }
        if !view.take_wants_pick() {
            return;
        }
        let Some(paths) = koru_workspace_host::dialogs::pick_open_files(
            "Add screenshot",
            &[koru_workspace_host::dialogs::FileFilter {
                name: "Images",
                extensions: &["png", "jpg", "jpeg", "gif", "webp"],
            }],
            None,
            None,
        ) else {
            return;
        };
        for path in paths {
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("screenshot.png");
            if let TabView::Feedback(view) = &mut self.tabs_mut().active_mut().view {
                view.attach_bytes(name, &bytes);
            }
        }
    }

    fn perform_settings_request(&mut self, request: koru_settings_ui::Request) {
        use koru_settings_ui::Request;
        match request {
            Request::ApplyLayoutToAllDocuments => self.apply_layout_to_all_documents(),
            Request::CycleCanvasBackground => {
                let cycled = koru_document::docview::cycle_canvas_bg(theme::default_canvas_bg());
                theme::set_default_canvas_bg(cycled);
                if let TabView::Settings(s) = &mut self.tabs_mut().active_mut().view {
                    s.set_canvas_background(cycled);
                }
            }
            // It cannot be written here: the shell holds the live
            // `DesktopPreferences` and saves it when the process exits, so a
            // file written from a view would be overwritten on the way out.
            Request::SetCloseBehavior(choice) => {
                self.window_request = Some(WindowRequest::SetCloseBehavior(
                    crate::settings_view::close_behavior_of(choice),
                ));
            }
            Request::SetRenderer(token) => crate::settings_view::set_renderer_backend(&token),
            Request::SetVoiceLanguage(language) => {
                koru_workspace_host::voice_prefs::set_language(
                    crate::settings_view::language_from(&language),
                );
                self.push_voice_preferences();
            }
            Request::SetVoiceInput(input) => {
                koru_workspace_host::voice_prefs::set_input(crate::settings_view::input_from(
                    &input,
                ));
                self.push_voice_preferences();
            }
            Request::SetVoiceTier(tier) => {
                koru_workspace_host::voice_prefs::set_tier(crate::settings_view::tier_from(tier));
                self.push_voice_preferences();
            }
            Request::SetVoiceGesture(gesture) => {
                koru_workspace_host::voice_prefs::set_toggle(
                    gesture == koru_settings::DictationGesture::Toggle,
                );
                self.push_voice_preferences();
            }
            Request::SetSpellcheckEnabled(on) => {
                koru_workspace_host::spelling::set_enabled(on);
                self.push_spelling_preferences();
            }
            Request::SetSpellcheckLanguage(language) => {
                koru_workspace_host::spelling::set_preferred(
                    koru_workspace_host::spelling::parse_language(language.token()),
                );
                self.push_spelling_preferences();
            }
            // The download runs on its own thread; the Settings page follows
            // its progress through `maintain`.
            Request::DownloadDictionary => {
                if let Some(lang) = koru_workspace_host::spelling::effective_language() {
                    koru_workspace_host::spelling::download::start(lang);
                }
                self.push_spelling_preferences();
            }
            Request::ClearPersonalDictionary => {
                let lang = crate::format::lang();
                let label = koru_i18n::t(lang, koru_i18n::Key::SpellingClearPersonalDictionary);
                self.confirm = Some((
                    koru_ui::Confirm::new(
                        label,
                        koru_i18n::t(lang, koru_i18n::Key::SpellingClearPersonalDictionaryBody),
                        label,
                    ),
                    trash::ConfirmSubject::ClearPersonalDictionary,
                ));
            }
            // Neither is a view's to perform: *Install…* reaches a catalog and
            // then downloads several hundred megabytes, and *Remove* deletes
            // them. Both run on a worker thread in `koru-workspace-host`, and the
            // section takes a fresh reading when they finish.
            Request::InstallVoiceModels => {
                self.ask_pane(PaneRequest::SetUpVoice);
            }
            Request::RemoveVoiceModels => {
                self.ask_pane(PaneRequest::RemoveVoiceModels);
            }
            Request::AskVoiceDevices => {
                self.ask_pane(PaneRequest::AskVoiceDevices);
            }
            // Stopped short in SP-7: the record's directory is
            // `koru-workspace-host`'s and its reader is `pub(crate)` there, so
            // no Koru row raises this yet. See the packet report.
            Request::RevokeVoiceConsent => {}
            // Written off the event loop by the host's consent worker; the
            // record's own write deletes anything queued on a withdrawal.
            Request::SetTelemetryConsent(on) => {
                koru_workspace_host::telemetry_consent::set(on);
            }
            // **The way in, not a second copy of the rows.** The provider rows
            // are Koru AI's and stay there; Settings names the section and
            // opens the panel that owns them.
            // A tab of its own, beside Settings — not the side panel, which
            // is for chatting next to a document.
            Request::OpenAiSetup if !koru_features::ai() => {}
            Request::OpenAiSetup => {
                self.record_jump();
                self.tabs_mut().push(Tab::ai_setup());
                self.slides_mut().clear();
                self.mark_tab_widths_stale();
                self.size_visible_groups();
                self.ensure_tab_visible(self.tabs().active());
            }
        }
    }

    /// The voice globals moved; the Settings tabs' mirrors follow them rather
    /// than guessing what the write landed on.
    fn push_voice_preferences(&mut self) {
        for (_, group) in self.panes.iter_groups_mut() {
            for t in group.iter_tabs_mut() {
                if let TabView::Settings(s) = &mut t.view {
                    s.push_voice_preferences();
                }
            }
        }
    }

    /// The spelling globals moved — same rule as voice above.
    fn push_spelling_preferences(&mut self) {
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Settings(s) = &mut tab.view {
                    s.push_spelling_preferences();
                }
            }
        }
    }

    /// Take the finished name into the tab strip and the tree, if a title has
    /// just stopped being edited.
    ///
    /// Returns whether anything was refreshed, so a caller can ask for the
    /// frame that shows it (ADR-26).
    fn settle_naming(&mut self, ws: &WorkspaceDoc) -> bool {
        let now = self.tabs().active_tab().view.editing_title();
        if now == self.naming {
            return false;
        }
        let was = std::mem::replace(&mut self.naming, now);
        // Only *leaving* one is a commit. Entering a title changes no name.
        if was.is_none() {
            return false;
        }
        self.refresh_labels(ws);
        self.explore_stale = true;
        true
    }

    /// Refresh every tab's label from the document, and every welcome tab's
    /// recents. Renaming a document in one tab retitles it everywhere.
    /// Take each **background** document tab's label from the manifest.
    ///
    /// `refresh_labels` reads the *active* document, which knows nothing about
    /// the others — so a tab over another document would be named `Untitled`,
    /// or keep whatever it opened with while a peer renamed it. The manifest is
    /// where a document's title is listed without opening it
    /// ([ADR-91](../../../../docs/decisions/ADR-076-100.md#adr-91-perdocument--a-document-keeps-a-root-node-for-its-title-and-the-manifest-mirrors-it)), and it is the
    /// only source a window not in that document has.
    fn relabel_background_tabs(&mut self) {
        let Some(entries) = &self.entries else {
            return;
        };
        let active = self.active_document;
        // Names first, so the read of `entries` is finished before the tabs are
        // written and `mark_tab_widths_stale` can take `&mut self`.
        let names: Vec<(usize, String)> = self
            .tabs()
            .iter()
            .enumerate()
            .filter_map(|(i, t)| {
                let id = t.document.filter(|id| Some(*id) != active)?;
                let entry = entries.iter().find(|e| e.id == id)?;
                let title = if entry.title.is_empty() {
                    "Untitled".to_owned()
                } else {
                    entry.title.clone()
                };
                Some((i, title))
            })
            .collect();
        if names.is_empty() {
            return;
        }
        for (i, name) in names {
            if let Some(t) = self.tabs_mut().get_mut(i) {
                t.set_name(&name);
            }
        }
        self.mark_tab_widths_stale();
    }

    fn refresh_labels(&mut self, ws: &WorkspaceDoc) {
        let active_document = self.active_document;
        let naming = self.naming;
        for t in self.tabs_mut().iter_mut() {
            // **The Tasks tab is named for what it is showing**, and it is the
            // one tab whose name can change without anybody pressing anything:
            // a peer renaming the list it is on moves it. So it is refreshed
            // here with the document tabs rather than written once where the
            // queue is pressed. It has no root, so every gate below would skip
            // it.
            if matches!(t.view, TabView::Tasks(_) | TabView::Reminders(_)) {
                t.refresh_tasks_label();
                continue;
            }
            // A draft's root names a node in its own throwaway document, not in
            // this one, so `name_of` here would be a lookup in the wrong place.
            // Its label is `DRAFT_NAME` until it is promoted.
            if t.draft.is_some() {
                continue;
            }
            // **And the same is true of a tab over another document**, for the
            // same reason: `ws` is the *active* one, so `name_of` there finds
            // nothing and answers `Untitled` — which would rename every
            // background document tab to that the moment anything changed. Its
            // label follows the manifest instead, through `set_entries`.
            if t.document != active_document {
                continue;
            }
            // A title being typed into is held at the name it had when the
            // caret arrived — `settle_naming` releases it on the way out.
            if let Some(root) = t.view.root().filter(|r| Some(*r) != naming) {
                t.set_name(&koru_workspace::name_of(ws, root));
            }
        }
        self.mark_tab_widths_stale();
        self.sync_welcome_lists();
    }

    /// Push who the Welcome page greets and the machine-wide ticks to every
    /// Welcome tab in the window. Cheap: each tab compares before it
    /// re-measures.
    fn sync_welcome_lists(&mut self) {
        let identity = self.welcome_identity();
        let done: Vec<String> = self.welcome_progress.done().map(str::to_owned).collect();
        for (_, group) in self.panes.iter_groups_mut() {
            for t in group.iter_tabs_mut() {
                if let TabView::Welcome(w) = &mut t.view {
                    w.set_identity(identity.clone());
                    w.set_done(done.iter().map(String::as_str));
                }
            }
        }
    }
}
impl MainView {
    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        self.tooltip.clear();
        self.pointer = (x, y);
        // Keyboard focus also decides which surface receives the matching
        // release. Opening the AI pane gives it focus, but a later press in
        // Explore used to leave that bit set: Explore seeded a latent row drag
        // while mouse-up was routed to AI, so the tree kept following the
        // pointer until Escape cancelled it. Hand focus over at press time,
        // before any of the non-AI early-return paths can claim the click.
        let ai_hit = self.ai_visible() && self.ai_rect().contains(x, y);
        if !ai_hit {
            self.ai_focused = false;
        }
        // **The offline popover, before anything else.**
        //
        // Toggled by the chip, dismissed by a press anywhere else — and
        // "anywhere else" has to mean that literally, which is why it is here
        // rather than in `press`. That one is reached only by clicks the caption
        // hit test claims; a click in the sidebar, in the document, or on a
        // panel never arrives there, and the popover would sit open over
        // whatever happened next.
        //
        // The chip itself is checked here too, and for the same reason it is
        // checked before the shared hit test: it lives inside the caption band,
        // so letting the press through would drag the window.
        //
        // **On the inherent `mouse_down`, not the `View` one.** They resolve
        // differently — the inherent wins for every direct caller, tests
        // included — and putting this on the trait impl is exactly the mistake
        // AGENTS.md records about the pair of `key` methods. It was made here
        // first, and the popover simply never opened.
        if self.store_trouble.is_some() || self.offline_open {
            let on_chip = self.offline_chip().is_some_and(|c| c.contains(x, y));
            let was = self.offline_open;
            self.offline_open = on_chip && !was;
            if on_chip {
                return true;
            }
        }
        if self.error.is_some() {
            return self.error_mouse_down(x, y);
        }
        // The delegation modal claims every press while it stands, and a press
        // outside its panel does **not** dismiss it: it holds an objective
        // somebody typed, and losing that to a stray click is the failure
        // `Confirm` refuses for the same reason.
        if self.delegation_open() {
            return self.delegation_mouse_down(x, y);
        }
        // The recording modal claims every press while it stands, and — the
        // delegation rule again — an outside press dismisses nothing: the
        // size somebody typed is not lost to a stray click.
        if self.recording_modal.is_some() {
            return self.recording_mouse_down(x, y);
        }
        if let Some(dialog) = self.import_dialog.as_mut() {
            let outcome = dialog.mouse_down(&mut self.engine, self.viewport, x, y);
            return self.settle_import(outcome);
        }
        if self.updates_modal {
            return self.updates_modal_mouse_down(x, y);
        }
        if self.update_newer_mouse_down(x, y) {
            return true;
        }
        if self.update_mouse_down(x, y) {
            return true;
        }
        if let Some((dialog, _)) = self.confirm.as_mut() {
            let out = dialog.mouse_down(&mut self.engine, self.viewport, x, y);
            self.settle_confirm_outcome(out);
            self.drain_trash_all(ws);
            return true;
        }
        let has_optimistic = self.reminders.optimistic_creations().next().is_some();
        if self.reminder_creation.is_open() || has_optimistic {
            let outcome = self.reminder_creation.mouse_down(&mut self.engine, x, y);
            if outcome.handled {
                return self.settle_reminder_creation(ws, outcome);
            }
        }
        // The topmost non-modal thing in the window: a press on it must not
        // fall through to the document underneath.
        if self.toast_mouse_down(ws, x, y) {
            return true;
        }
        // The *In Trash* strip sits over the top of the content, so it is asked
        // before the tab it covers.
        if self.notice_mouse_down(ws, x, y) {
            self.drain_trash_all(ws);
            return true;
        }
        if let Some(dialog) = self.new_document.as_mut() {
            dialog.mouse_down(&mut self.engine, self.viewport, x, y);
            return true;
        }
        // A press outside dismisses nothing, in either direction — the same
        // rule the confirmation panel follows. The press is swallowed so it
        // cannot reach the document behind the scrim.
        if let Some(dialog) = self.snapshot_prompt.as_mut() {
            dialog.mouse_down(&mut self.engine, self.viewport, x, y);
            return true;
        }
        if let Some(dialog) = self.link_device.as_mut() {
            let outcome = dialog.mouse_down(&mut self.engine, self.viewport, x, y);
            return self.settle_link_device(outcome);
        }
        if self.workspace_icon.is_some() {
            let out = self
                .workspace_icon
                .as_mut()
                .map(|m| m.mouse_down(&mut self.engine, self.viewport, x, y));
            if let Some(out) = out {
                return self.settle_icon(out);
            }
        }
        // The active overflow button is checked before its modal menu so the
        // same control can dismiss that menu. A press elsewhere still follows
        // the ordinary modal dismissal path below.
        if self.window_menu_open()
            && self
                .window_menu_button()
                .is_some_and(|button| button.contains(x, y))
        {
            let out = self.press(Hit::Window(CaptionAction::WindowMenu), x, y, ctrl);
            return self.apply(ws, out);
        }
        // An open context menu is modal: the press either chooses an item or
        // dismisses it, and is consumed either way — it must not also hit
        // whatever happens to sit under the menu.
        // A press inside the nested flyout chooses from it, and takes every
        // menu down. A press on the submenu row that owns it is not a choice.
        if let Some((_, nested)) = self.nested.clone().filter(|_| self.in_nested(x, y)) {
            let hit = nested.item_at(self.viewport, x, y);
            let enabled = self.enabled_items(&nested);
            self.nested = None;
            self.submenu = None;
            self.menu = None;
            if let Some(i) = hit.filter(|&i| enabled.get(i) == Some(&true)) {
                let action = nested.items()[i];
                self.run_menu_action(nested.subject, action);
            }
            return true;
        }
        // A press inside the submenu chooses from it, and takes both menus down.
        if let Some((_, sub)) = self.submenu.clone().filter(|_| self.in_submenu(x, y)) {
            let hit = sub.item_at(self.viewport, x, y);
            let enabled = self.enabled_items(&sub);
            // Pressing the row that owns a nested flyout is not a choice — the
            // flyout is already open beside it.
            if hit.is_some_and(|i| {
                sub.items()
                    .get(i)
                    .copied()
                    .and_then(context_menu::submenu)
                    .is_some()
            }) {
                return true;
            }
            self.nested = None;
            self.submenu = None;
            self.menu = None;
            if let Some(i) = hit.filter(|&i| enabled.get(i) == Some(&true)) {
                let action = sub.items()[i];
                self.run_menu_action(sub.subject, action);
            }
            return true;
        }
        if let Some(menu) = self.menu.clone() {
            // **A stepper is pressed, not chosen, and the menu stays up.**
            // Zooming out from 100% to 70% is three presses of one button, so
            // dismissing on the first would make the row a worse Ctrl+-. The
            // dim end of a range answers here too, and swallows the press:
            // falling through would read as a click on the panel and take the
            // menu down for doing nothing.
            if menu.control_at(self.viewport, x, y).is_some() {
                let pressed =
                    menu.pressable_control_at(self.viewport, x, y)
                        .and_then(|(row, control)| {
                            context_menu::control_action(menu.items()[row], control)
                        });
                if let Some(action) = pressed {
                    self.run_menu_action(menu.subject, action);
                }
                return true;
            }
            let hit = menu.item_at(self.viewport, x, y);
            let enabled = self.enabled_items(&menu);
            // Pressing the row that owns a submenu is not a choice — the
            // submenu is already open beside it and the press must not take the
            // whole thing down on the way to it.
            if hit.is_some_and(|i| {
                menu.items()
                    .get(i)
                    .copied()
                    .and_then(context_menu::submenu)
                    .is_some()
            }) {
                return true;
            }
            self.nested = None;
            self.submenu = None;
            // A dismissing press only closes the menu. Choosing an action from
            // the workspace switcher's menu also closes the switcher, since the
            // action itself is the end of that interaction.
            self.menu = None;
            self.submenu = None;
            if let Some(i) = hit.filter(|&i| enabled.get(i) == Some(&true)) {
                let action = menu.items()[i];
                self.run_menu_action(menu.subject, action);
            }
            return true;
        }
        let bell = self.reminder_bell_rect();
        let (handled, intent) =
            self.reminder_shell
                .mouse_down(self.viewport, bell, x, y, Instant::now());
        if let Some(intent) = intent {
            self.apply_reminder_shell_intent(intent);
        }
        if handled {
            return true;
        }
        // The pane's resize grip, before the hit is resolved: it straddles the
        // pane's edge, so half of it is over the content and `regions::hit`
        // would answer for that half.
        if self.workspace_rename.is_none()
            && self.menu.is_none()
            && !self.popover_hit(x, y)
            && self.sidebar_grip().contains(x, y)
        {
            self.sidebar_drag = Some(x - self.sidebar.full());
            return true;
        }
        if self.workspace_rename.is_none()
            && self.menu.is_none()
            && !self.popover_hit(x, y)
            && self.ai_grip().contains(x, y)
        {
            self.ai_drag = Some(x - self.ai_rect().x);
            return true;
        }
        // A divider's grip, on the same footing as the two pane edges above: it
        // straddles the hairline, so half of it is over content a pane hit
        // would answer for.
        if self.workspace_rename.is_none()
            && self.menu.is_none()
            && !self.popover_hit(x, y)
            && let Some((path, axis, rect)) = self
                .panes
                .dividers(self.content())
                .into_iter()
                .find(|(_, axis, rect)| panes::divider_grip(*rect, *axis).contains(x, y))
        {
            let pointer = match axis {
                panes::SplitAxis::Vertical => x,
                panes::SplitAxis::Horizontal => y,
            };
            let origin = match axis {
                panes::SplitAxis::Vertical => rect.x,
                panes::SplitAxis::Horizontal => rect.y,
            };
            self.divider_drag = Some(panes::DividerDrag {
                path,
                axis,
                grab: pointer - origin,
                moved: false,
            });
            return true;
        }
        // A rename field is modal over the popover it sits in: a press either
        // moves its caret or finishes it, and never also chooses a row.
        if self.workspace_rename.is_some() {
            // A row that is no longer there — the popover closed underneath —
            // reads as "outside", which commits, the same as clicking away.
            let row = self
                .workspace_rename_row()
                .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            let taken = match self.workspace_rename.as_ref().map(|r| r.target) {
                Some(RenameTarget::Workspace(id)) => self.other_workspace_names(id),
                _ => Vec::new(),
            };
            let out = self
                .workspace_rename
                .as_mut()
                .map(|f| f.mouse_down(&mut self.engine, row, x, y, &taken));
            if let Some(out) = out {
                return self.settle_rename(out);
            }
        }
        // The rail's inline field, on the same footing as the rename field
        // above it: a press either moves its caret or finishes it, and a press
        // that finishes it does **not** also choose a rail row.
        if self.new_list.is_some() {
            let field = self
                .new_list_field_rect()
                .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            let taken = self.list_names();
            let out = self
                .new_list
                .as_mut()
                .map(|f| f.mouse_down(&mut self.engine, field, x, y, &taken));
            if let Some(out) = out {
                return self.finish_new_list(out);
            }
        }
        // **The popover first, before the hit is even resolved.** It floats
        // over the active view, so `regions::hit` would answer `Content` for a
        // point on one of its rows and the press would go to the document
        // underneath. Same position the tab menu already occupies.
        if self.popover_hit(x, y) {
            // Its rows have to be current before they are hit-tested. The
            // anchor may be virtual while Explore itself is shut.
            self.refresh_explore(ws, true);
            let area = self.explore_popover_area();
            let out = self.explore.mouse_down_with_modifiers(
                area,
                self.viewport,
                x,
                y,
                ctrl,
                self.mods.shift_key(),
            );
            self.refresh_drag_item_count(ws);
            self.explore_stale = true;
            return self.apply(ws, out);
        }
        // An open AI picker is not modal, but a click outside the AI surface
        // should still close it before the press reaches the document.
        if self.ai_visible() && !ai_hit {
            let dismissed = self.ai_chat.dismiss_popovers();
            if dismissed {
                return true;
            }
        }
        if ai_hit {
            self.ai_focused = true;
            let changed = self.ai_chat.mouse_down(x, y);
            return self.save_pending_ai_credential() || changed;
        }
        let hit = self.hit_at(x, y);
        inputdebug::write(format_args!(
            "[Koru][input] view press hit={hit:?} explore_focus={} ai_focused={}",
            self.explore.has_focus(),
            self.ai_focused,
        ));
        // A press anywhere else shuts the popover — but **does not eat the
        // click**. A panel hanging off a column is not a modal over the
        // document, so it must not cost you the press that dismissed it.
        // `handled` picks it up at the tail, so the frame still happens.
        let dismissed = hit != Hit::Sidebar && self.explore.close_popover();
        // **A press anywhere else gives the tree's keyboard back.** Without
        // this, clicking a row and then clicking into the document would leave
        // the arrows moving through the tree while the caret sat still — a
        // window that has quietly stopped answering the keys you are pressing,
        // with nothing on screen to say why.
        let unfocused = hit != Hit::Sidebar && self.explore.clear_focus();
        let dismissed = dismissed || unfocused;
        if hit == Hit::Sidebar {
            // Reached only by a press that landed in the column, so it is on
            // screen and its rows have to be current before they are hit-tested.
            self.refresh_explore(ws, true);
            let out = self.explore.mouse_down_with_modifiers(
                self.regions.sidebar,
                self.viewport,
                x,
                y,
                ctrl,
                self.mods.shift_key(),
            );
            self.refresh_drag_item_count(ws);
            // Expanding a row changes the tree, and the tree is a cache.
            self.explore_stale = true;
            return self.apply(ws, out);
        }
        if hit == Hit::Context {
            let out = self.context_view.mouse_down(self.regions.context, x, y);
            return self.apply(ws, out) || dismissed;
        }
        if hit != Hit::Content {
            self.focus_pane_at(x, y);
            let out = self.press(hit, x, y, ctrl);
            return self.apply(ws, out) || dismissed;
        }
        // A press in the document area focuses the pane it landed in before the
        // active view sees it. Single-group builds are unaffected; this is the
        // hook multi-pane input will need.
        self.focus_pane_at(x, y);
        if let Some(action) = self
            .document_ai_button()
            .and_then(|button| button.action_at(x, y))
            .or_else(|| {
                self.document_context_button()
                    .and_then(|button| button.action_at(x, y))
            })
        {
            let outcome = self.run_icon_action(action, x, y);
            return self.apply(ws, outcome) || dismissed;
        }
        let (lx, ly) = self.to_leaf(x, y);
        let page_icon_commit = self.tabs().active_tab().view.is_setting_page_icon();
        let (view, doc) = self.tabs_mut().active_mut().doc(ws);
        let handled = match view {
            TabView::Welcome(w) => w.mouse_down(doc, lx, ly, ctrl),
            TabView::Manage(w) => View::mouse_down(w, doc, lx, ly, ctrl),
            TabView::Trash(t) => View::mouse_down(t, doc, lx, ly, ctrl),
            TabView::Tasks(t) => View::mouse_down(t, doc, lx, ly, ctrl),
            TabView::Reminders(r) => View::mouse_down(r, doc, lx, ly, ctrl),
            TabView::Settings(s) => View::mouse_down(s, doc, lx, ly, ctrl),
            TabView::Feedback(f) => View::mouse_down(f, doc, lx, ly, ctrl),
            TabView::Diff(d) => View::mouse_down(d, doc, lx, ly, ctrl),
            TabView::AiChat(a) => View::mouse_down(a, doc, lx, ly, ctrl),
            TabView::Socials(a) => View::mouse_down(a, doc, lx, ly, ctrl),
            TabView::Session(s) => View::mouse_down(s, doc, lx, ly, ctrl),
            TabView::Run(r) => View::mouse_down(r, doc, lx, ly, ctrl),
            TabView::Result(r) => View::mouse_down(r, doc, lx, ly, ctrl),
            TabView::Canvas(c) => View::mouse_down(c, doc, lx, ly, ctrl),
            TabView::Leaf(l) => Leaf::mouse_down(l, doc, lx, ly, ctrl),
        };
        let handled = handled || dismissed;
        if matches!(hit, Hit::Content) {
            self.trace_input_state("press-content");
            // A press that reached the document's content is the v1 set's
            // node click — the same observation, reclassified.
            if matches!(self.tabs().active_tab().view, TabView::Leaf(_)) {
                self.observe_recording_node_click();
            }
        }
        if handled && page_icon_commit && !self.tabs().active_tab().view.is_setting_page_icon() {
            self.explore_stale = true;
        }
        // Document-pill clicks set `wants_open` / `wants_entity_open` here.
        let opened = self.drain_wants_open(ws);
        if opened {
            inputdebug::write(format_args!("[Koru][input] view press drained=wants-open"));
        }
        let toasted = self.drain_editor_toast();
        let entity_changed = self.sync_entity_picker();
        let asked = self.drain_manage_profiles_request();
        let binned = self.drain_trash_all(ws);
        self.drain_welcome();
        self.drain_settings_requests();
        self.drain_feedback();
        handled || opened || toasted || asked || binned || entity_changed
    }
    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if self.updates_modal {
            // The release of an outside press is where the modal closes, so
            // both halves of the click are consumed here and neither reaches
            // the document underneath (ADR-150).
            if std::mem::take(&mut self.updates_modal_outside_press) {
                self.updates_modal = false;
            }
            return true;
        }
        if self.error.is_some() {
            return true;
        }
        if self.import_dialog.is_some() {
            return true;
        }
        if self.confirm.is_some() {
            return true;
        }
        if self.reminder_creation.is_open() {
            let _ = self.reminder_creation.mouse_up();
            return true;
        }
        if let Some(dialog) = self.link_device.as_mut() {
            dialog.mouse_up();
            return true;
        }
        if let Some(m) = self.workspace_icon.as_mut() {
            let outcome = m.mouse_up();
            return self.settle_icon(outcome);
        }
        if let Some(f) = self.workspace_rename.as_mut() {
            f.mouse_up();
            return true;
        }
        if self.delegation_open() {
            return self.delegation_mouse_up();
        }
        if self.recording_modal.is_some() {
            return self.recording_mouse_up();
        }
        let reminder_blocked = self.reminder_shell.overlay_live();
        if self.reminder_shell.mouse_up() || reminder_blocked {
            return true;
        }
        if let Some(f) = self.new_list.as_mut() {
            f.mouse_up();
            return true;
        }
        if self.sidebar_drag.take().is_some() {
            // One reflow for the whole drag, here.
            // The width reaches disk on its own: `capture_geometry` runs in
            // `about_to_wait` and goes through `capture_session`, which records
            // it per workspace. Nothing to flag.
            self.commit_sidebar_width();
            return true;
        }
        if self.ai_drag.take().is_some() {
            self.commit_ai_width();
            return true;
        }
        if let Some(drag) = self.divider_drag.take() {
            // One reflow for the whole drag, like the pane's own width. The
            // ratio reaches disk on its own through the session capture.
            if drag.moved {
                self.relayout();
            }
            return true;
        }
        if self.ai_visible() && self.ai_focused {
            return self.ai_chat.mouse_up();
        }
        // A read accessor returns a *copy* of the Option, so `.take()` on it
        // clears nothing — the field must go through the setter.
        if self.tab_scroll_drag().is_some() {
            self.set_tab_scroll_drag(None);
            return true;
        }
        if let Some(dialog) = self.new_document.as_mut() {
            return dialog.mouse_up();
        }
        if let Some(dialog) = self.snapshot_prompt.as_mut() {
            return dialog.mouse_up();
        }
        // A cross-pane drag commits on release: split, drop into another group,
        // or pop out if the pointer is outside the content area.
        if self.pane_drag.is_some() {
            let (x, y) = self.pointer;
            return self.commit_pane_drag(ws, x, y);
        }
        // A tear-off releases straight into a pop-out: the tab leaves the strip
        // for its own window rather than dropping back into a slot.
        let tearing = self.drag_tearing();
        let torn = tearing
            .then(|| self.group_drag().as_ref().map(|d| d.index))
            .flatten();
        let landing = self.carry_offset();
        let was_dragging_tab = self.group_drag_mut().take().is_some();
        if let Some(i) = torn {
            self.pop_out(i);
            return true;
        }
        // Let go of the tab where the pointer left it, and let it drop into its
        // slot rather than teleporting there — the same easing its neighbours
        // have been using, so the gesture ends the way it ran.
        if let Some((i, dx)) = landing {
            let tab_count = self.tabs().len();
            self.slides_mut()
                .settle(tab_count, i, dx, std::time::Instant::now());
        }
        // The context column's scrollbar drag ends on any release.
        if self.context_view.mouse_up() {
            return true;
        }
        // An explore drag ends wherever the pointer released — including over
        // the chrome or outside the column, since it follows the pointer out.
        // It returns `MoveNodes` (the container is the one that can mutate the
        // document) or `OpenNode` for a click that never armed.
        if self.explore.pointer_active() {
            let row_drag = self.explore.drag_active();
            // A document carried out of the column and released over a pane
            // opens there — the slot, the split, or the group — rather than
            // moving in the tree. The tree's move is an answer only inside
            // the column, and a multi-select drop keeps it.
            let roots = self.explore.dragged_roots();
            if row_drag && roots.len() == 1 {
                let (x, y) = self.pointer;
                if self.pane_at(x, y).is_some()
                    && (self.content().contains(x, y) || self.drop_slot.is_some())
                {
                    let slot = self.drop_slot.take();
                    let intent = self.split_intent.take();
                    self.explore.cancel_drag();
                    // The cache still shows the drag's folded view; rebuild it.
                    self.explore_stale = true;
                    self.drop_document_into_pane(ws, roots[0], x, y, slot, intent);
                    return true;
                }
            }
            let out = self.explore.mouse_up();
            // A drag leaves the cache in its docs-only, folded view; clearing
            // `docs_only` (inside `mouse_up`) does not by itself rebuild, so the
            // full tree would stay hidden until something else touched the pane.
            // Mark it stale and let the release slide gate lift, then the next
            // frame rebuilds the whole library.
            self.explore_stale |= row_drag;
            return match out {
                Outcome::MoveNodes { nodes, target } => {
                    self.move_nodes(ws, nodes, target);
                    true
                }
                // `apply` handles `OpenNode` (and the no-op arms) the same way a
                // press would.
                other => self.apply(ws, other),
            };
        }
        // Always delegated: a drag has to end wherever it was released, even if
        // that is over the chrome or outside the window entirely.
        let (view, doc) = self.tabs_mut().active_mut().doc(ws);
        let handled = match view {
            TabView::Welcome(w) => w.mouse_up(doc),
            TabView::Manage(w) => View::mouse_up(w, doc),
            TabView::Trash(t) => View::mouse_up(t, doc),
            TabView::Tasks(t) => View::mouse_up(t, doc),
            TabView::Reminders(r) => View::mouse_up(r, doc),
            TabView::Settings(s) => View::mouse_up(s, doc),
            TabView::Feedback(f) => View::mouse_up(f, doc),
            TabView::Diff(d) => View::mouse_up(d, doc),
            TabView::AiChat(a) => View::mouse_up(a, doc),
            TabView::Socials(a) => View::mouse_up(a, doc),
            TabView::Session(s) => View::mouse_up(s, doc),
            TabView::Run(r) => View::mouse_up(r, doc),
            TabView::Result(r) => View::mouse_up(r, doc),
            TabView::Canvas(c) => View::mouse_up(c, doc),
            TabView::Leaf(l) => Leaf::mouse_up(l, doc),
        };
        handled || was_dragging_tab
    }
}

/// Move one open document tab to `mode` and `canvas`; a tab that is not a
/// document, or already there, is left alone.
///
/// Only the shown tab reflows now: the others have their fields set and their
/// rows invalidated, and reflow when they are next shown. The invalidation is
/// not optional — the rows were laid out at the old mode's wrap width, and
/// without it the next `set_viewport` would see the new mode already in place,
/// compute the same effective width, skip its own invalidation and reflow
/// without rebuilding, leaving long lines overflowing (or, going to Full,
/// under-wrapped). `invalidate_size` then forces that reflow on show.
fn set_tab_layout(t: &mut Tab, shown: bool, mode: theme::ViewMode, canvas: Option<[u8; 4]>) {
    let TabView::Leaf(l) = &mut t.view else {
        return;
    };
    {
        let v = Editing::view(l);
        if v.view_mode == mode && v.canvas_bg == canvas {
            return;
        }
    }
    if shown {
        Editing::set_view_mode(l, mode);
        Editing::view_mut(l).canvas_bg = canvas;
        return;
    }
    let (was, is) = {
        let v = Editing::view_mut(l);
        let was = v.view_mode.effective_w(v.viewport.0);
        v.view_mode = mode;
        v.content_x = mode.content_x(v.viewport.0);
        v.canvas_bg = canvas;
        (was, mode.effective_w(v.viewport.0))
    };
    if (was - is).abs() > 0.5 {
        Editing::body_mut(l).invalidate();
    }
    t.invalidate_size();
}

impl View for MainView {
    /// **A preference changed elsewhere.** Deliberately *not* `restore`, which
    /// reopens the session: a theme change in another window is not a reason to
    /// rebuild a tab strip. What it does reach is the emoji picker's two pieces
    /// of remembered state and every Settings tab in this window, which are the
    /// only things here holding a copy rather than reading a global.
    fn adopt_preferences(&mut self, settings: &Settings) {
        koru_editor::emoji::restore_usage(&settings.emoji_recents);
        koru_editor::emoji::set_preferred_skin_tone(koru_editor::emoji::parse_skin_tone(
            &settings.emoji_skin_tone,
        ));
        // Held as well as fanned out, because a Settings tab opened *later*
        // has to start from the same place the open ones were just moved to.
        self.preferences = settings.clone();
        self.each_tab_mut(|tab| {
            match &mut tab.view {
                TabView::Welcome(w) => View::adopt_preferences(w, settings),
                TabView::Settings(s) => View::adopt_preferences(s, settings),
                _ => {}
            }
            false
        });
    }

    /// **The authority's first answer.** `restore` laid every document out
    /// against the registry's default width, because the person's had not
    /// arrived yet. Resolve each document again by the rule `restore` uses —
    /// `Settings::doc_layout` — so one that follows the default now shows the
    /// person's default and one with an override of its own keeps it. Done
    /// here rather than through each leaf's own hook so that only the shown
    /// tab pays for a reflow.
    fn adopt_startup_preferences(&mut self, settings: &Settings) {
        self.adopt_preferences(settings);
        for (_, group) in self.panes.iter_groups_mut() {
            let active = group.active();
            for (i, t) in group.iter_tabs_mut().enumerate() {
                // A draft or What's New page is rooted in a private workspace:
                // no override can be filed for it, so it follows the defaults
                // rather than risk a lookup that collides with a real node —
                // the same exclusion `restore` makes.
                let root = (t.draft.is_none() && t.whats_new.is_none())
                    .then(|| t.view.root())
                    .flatten();
                let (mode, canvas) = settings.doc_layout(root);
                set_tab_layout(t, i == active, mode, canvas);
            }
        }
    }

    fn open_all_reminders(&mut self) -> bool {
        // The tray's *All Reminders* and the Welcome row both come here; a
        // build that hides reminders opens nothing (koru-features).
        if !koru_features::reminders() {
            return false;
        }
        self.apply_reminder_shell_intent(reminder_shell::Intent::OpenAllReminders)
    }

    fn dictate(&mut self, ws: &mut WorkspaceDoc, action: &koru_dictation::DictationAction) -> bool {
        MainView::dictate(self, ws, action)
    }

    fn dictation_status(&mut self, status: Option<koru_dictation::Status>) -> bool {
        self.set_dictation_status(status)
    }

    fn dictation_hit(&self, x: f32, y: f32) -> bool {
        self.dictation_pressed(x, y)
    }

    fn dictation_refused(&mut self, refusal: koru_dictation::Refusal) -> bool {
        // **The settings section is told as well as the toast.** A refusal that
        // says there is no worker is the one thing that page cannot ask for
        // itself — asking means starting one — so the answer arrives here or
        // not at all.
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.iter_tabs_mut() {
                if let TabView::Welcome(w) = &mut tab.view {
                    changed |= w.dictation_refused(refusal.clone());
                }
            }
        }
        MainView::report_dictation_refusal(self, refusal) || changed
    }

    /// A model was installed or deleted, so every Settings tab in this window
    /// takes another reading of the cache.
    fn voice_models_changed(&mut self) -> bool {
        let mut changed = false;
        for tab in self.tabs_mut().iter_mut() {
            if let TabView::Welcome(w) = &mut tab.view {
                changed |= w.voice_models_changed();
            }
        }
        changed
    }

    /// The workspace under this container was **replaced**.
    ///
    /// Every tab names a `NodeId` in the document that has just closed, and a
    /// `NodeId` is only meaningful inside the document that minted it — so they
    /// all go, and the window is left on a welcome tab.
    ///
    /// Required rather than defaulted (see [`View::reseat`]): forgetting it
    /// would leave every tab subscribed to a document nobody holds, rendering
    /// once and then going quiet with nothing to show for it.
    fn reseat(&mut self, ws: &WorkspaceDoc) {
        self.tabs_mut().reset_to_welcome();
        if !self.tabs().active_tab().is_pinned() {
            self.tabs_mut().active_view().reseat(ws);
        }
        // **Recents survive a workspace switch.** They are store document ids,
        // so a list wiped here would throw away every document opened in the
        // workspace just left; unresolved ids are dropped on display instead.
        // History is window navigation, not workspace-owned session state. Its
        // spots carry their workspace route and survive this reseat so Back can
        // return from a cross-workspace pill or switcher jump.
        self.explore_stale = true;
        self.explore.close_popover();
        let _ = self.reminders.cancel_creation();
        self.reminder_shell.reset_profile();
        self.reminder_creation.reset_profile();
        self.menu = None;
        self.new_document = None;
        self.snapshot_prompt = None;
        self.error = None;
        self.workspace_rename = None;
        self.workspace_icon = None;
        self.link_device = None;
        self.sidebar_drag = None;
        self.pane_context = None;
        self.relayout();
    }

    // ---- the three that mutate the document
    //
    // **Thin wrappers over inherent methods of the same name**, the shape `Leaf`
    // uses. The bodies have ten early `return`s between them — a menu choice
    // returns the moment it runs an action, a tree key returns the moment it is
    // applied — so a drain written at their tails is a drain most gestures never
    // reach. That is not a hypothetical: it is how *every* route to the Trash
    // except the chord silently did nothing. Draining here instead makes "the
    // request is always served" a property of the shape rather than of
    // remembering.

    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        if let Some(support) = self.support.as_mut()
            && support.key(key)
        {
            return true;
        }
        // **The two surfaces the wrapper claims**, taken from the same table
        // the inherent `key` walks below — see `mainview::routing`. They are
        // here rather than in the container's own chain because they float
        // over the whole window: the inherent method is what the container's
        // tests drive, and it has never seen either.
        match self.blockers() {
            Blockers::Prompt(PromptKind::MoveTo) => {
                return self.move_to_key(key, text, mods);
            }
            Blockers::Prompt(PromptKind::Search) => {
                if let Some(search) = self.search.as_mut() {
                    let handled = View::key(search, ws, key, text, mods);
                    self.service_search_outputs();
                    return handled;
                }
            }
            _ => {}
        }
        let handled = MainView::key(self, ws, key, text, mods);
        let acted = self.drain_trash_all(ws);
        if handled || acted {}
        handled || acted
    }

    fn mouse_down(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32, ctrl: bool) -> bool {
        // The recorder sees every press, with its position — a no-op unless a
        // capture is live. A press that reaches the document's content is
        // reclassified a node click at the leaf dispatch below.
        self.observe_recording_click(x, y);
        if self.move_to.is_some() {
            return self.move_to_mouse_down(x, y);
        }
        // The support window: the dialog owns every press while it is up;
        // otherwise the chat area is the support view's and the caption stays
        // the container's (buttons, drag).
        if let Some(fab) = self.fab.as_mut() {
            let handled = fab.mouse_down(x, y);
            self.gather_support_intents();
            return handled;
        }
        let in_content = self.in_support_content(x, y);
        if self.support_only
            && let Some(support) = self.support.as_mut()
            && (support.dialog_open() || in_content)
        {
            let viewport = self.viewport;
            let handled = support.mouse_down(&mut self.engine, viewport, x, y);
            self.gather_support_intents();
            return handled;
        }
        if self.search.is_some() {
            let bounds = self.search_bounds();
            if !self.search_only && !bounds.contains(x, y) {
                self.search = None;
                return true;
            }
            let (x, y) = self.search_point(x, y);
            let handled = View::mouse_down(self.search.as_mut().unwrap(), ws, x, y, ctrl);
            self.service_search_outputs();
            return handled;
        }
        let handled = MainView::mouse_down(self, ws, x, y, ctrl);
        let acted = self.drain_trash_all(ws);
        if handled || acted {}
        handled || acted
    }

    fn mouse_up(&mut self, ws: &mut WorkspaceDoc) -> bool {
        if let Some(fab) = self.fab.as_mut() {
            let changed = fab.mouse_up();
            self.gather_support_intents();
            return changed;
        }
        if let Some(search) = self.search.as_mut() {
            return View::mouse_up(search, ws);
        }
        let handled = MainView::mouse_up(self, ws);
        let acted = self.drain_trash_all(ws);
        if handled || acted {}
        handled || acted
    }

    fn context_menu(&mut self, ws: &mut WorkspaceDoc, x: f32, y: f32) -> bool {
        if let Some(fab) = self.fab.as_mut() {
            return fab.context_menu();
        }
        self.pointer = (x, y);
        if self.error.is_some()
            || self.import_dialog.is_some()
            || self.confirm.is_some()
            || self.new_document.is_some()
            || self.snapshot_prompt.is_some()
            || self.link_device.is_some()
            || self.workspace_icon.is_some()
            || self.updates_modal
            || self.recording_modal.is_some()
        {
            return true;
        }
        // A context press outside the reminder editor dismisses it, then keeps
        // travelling so the object under the pointer can open its own menu.
        // Ordinary pointer dismissal consumes the press; doing that here was
        // why a reminder pill opened the document formatting menu only after a
        // second right-click.
        if self.reminder_creation.is_open() {
            if self.reminder_creation.contains(x, y) {
                return true;
            }
            if self.reminders.creation_owner().is_some() {
                if !self.reminders.cancel_creation() {
                    return true;
                }
            } else {
                self.reminders.close_management();
            }
            self.reminder_creation.close_context();
        }
        let bell = self.reminder_bell_rect();
        if self
            .reminder_shell
            .context_press(self.viewport, bell, x, y, Instant::now())
        {
            return true;
        }
        // **The popover first, before the hit is resolved** — the same guard
        // `mouse_down` and `mouse_move` carry, and for the same reason: a
        // switcher row that overhangs the content is answered `Hit::Content` by
        // `regions::hit`. Without this, a right-click on a workspace row fell
        // into the arm below and did nothing at all, not even close the popover.
        if self.popover_hit(x, y) {
            self.refresh_explore(ws, true);
            // A right-click in the switcher is not a gesture it has — workspace
            // and Profile actions live in their managers. It is swallowed
            // rather than falling through to the view underneath.
            return true;
        }
        if self.ai_visible() && self.ai_rect().contains(x, y) {
            self.ai_focused = true;
            self.menu = None;
            self.submenu = None;
            self.nested = None;
            return self.ai_chat.context_menu(x, y);
        }
        let hit = self.hit_at(x, y);
        match hit {
            // A right-click on a tab (its close box is part of it) opens a menu
            // for that tab. It does **not** select the tab: the menu acts on the
            // tab under the cursor regardless of which is active.
            Hit::Tab(i) | Hit::TabClose(i) => {
                self.focus_pane_at(x, y);
                let tab = self.tabs().get(i);
                let document = tab.is_some_and(|tab| tab.document.is_some());
                let pinnable = tab.is_some_and(|tab| tab.view.is_pinnable());
                let pinned = tab.is_some_and(|tab| tab.is_pinned());
                let show_title = tab.is_some_and(|tab| tab.show_title);
                let mut m = ContextMenu::for_tab(
                    &mut self.engine,
                    i,
                    document,
                    pinnable,
                    pinned,
                    show_title,
                    (x, y),
                );
                // Open with the first item already highlighted, the way a native
                // menu does, so the keyboard can drive it from the first press.
                let enabled = self.enabled_items(&m);
                m.step(&enabled, 1);
                self.menu = Some(m);
                true
            }
            // A library row's menu — the same one its `…` opens, anchored at
            // the cursor rather than under a button.
            Hit::Sidebar => {
                self.refresh_explore(ws, true);
                let out = self.explore.context_menu(self.regions.sidebar, x, y);
                self.apply(ws, out)
            }
            Hit::Content => {
                self.menu = None;
                self.submenu = None;
                self.nested = None;
                self.focus_pane_at(x, y);
                let (lx, ly) = self.to_leaf(x, y);
                // The manager's rows are workspace rows too. Resolve the
                // subject there, then open the container-owned menu used by the
                // switcher so the actions and enablement cannot drift apart.
                let manager_target = match &mut self.tabs_mut().active_mut().view {
                    // A Profile page's workspace row; anywhere else on the
                    // page falls through to the tab.
                    TabView::Manage(manage) => manage.workspace_menu_target_at(lx, ly).map(Some),
                    _ => None,
                };
                if let Some(target) = manager_target {
                    return target.is_some_and(|id| self.open_workspace_menu(id, (x, y), false));
                }
                let (view, doc) = self.tabs_mut().active_mut().doc(ws);
                let handled = match view {
                    TabView::Welcome(_) => false,
                    TabView::Manage(w) => View::context_menu(w, doc, lx, ly),
                    // Settings opens no context menu: every row is one press.
                    TabView::Settings(_) | TabView::Feedback(_) => false,
                    TabView::Diff(_) => false,
                    // The AI surface draws and handles its own popovers.
                    TabView::AiChat(_) => false,
                    TabView::Socials(_) => false,
                    // A session grid opens no context menu in W4.1.
                    TabView::Session(_) => false,
                    // Every verb a run has is a control on its own surface,
                    // with the sentence saying what it would do beside it, and
                    // a result tab reads a version it cannot change.
                    TabView::Run(_) | TabView::Result(_) => false,
                    // The canvas's own menu (Add card, Insert image…) is the
                    // surface's; the placeholder offers none.
                    TabView::Canvas(c) => View::context_menu(c, doc, lx, ly),
                    TabView::Trash(t) => View::context_menu(t, doc, lx, ly),
                    TabView::Tasks(t) => View::context_menu(t, doc, lx, ly),
                    TabView::Reminders(r) => View::context_menu(r, doc, lx, ly),
                    TabView::Leaf(l) => View::context_menu(l, doc, lx, ly),
                };
                // **A right-click is a tail too.** The Tasks and Trash tabs
                // answer a context menu by *asking* for one — the row is theirs,
                // the menu is the container's — and that ask sits in the view
                // until somebody drains it. `mouse_down`, `mouse_up` and `key`
                // all end in this call and this one did not, so a right-click on
                // a task row set the request and drew nothing; the menu appeared
                // on whatever key or click came next. Same call, same place as
                // the other three, for the reason `drain_trash_all` exists.
                //
                // **A run pill asks the same way** (ADR-369): the pill is the
                // document's, the menu is this window's, because what a run
                // produced is not something an editor can name.
                handled
                    | self.drain_trash_all(ws)
                    | self.drain_run_pill_menu((x, y))
                    | self.drain_schedule_pill_menu((x, y))
            }
            // A right-click anywhere else dismisses a menu that was open, but
            // opens none.
            _ => {
                let was_open = self.menu.is_some();
                self.menu = None;
                self.submenu = None;
                self.nested = None;
                was_open
            }
        }
    }

    /// **The caption's window-menu button opens on hover**, and its menu closes when the
    /// pointer leaves both the button and the panel.
    ///
    /// It wraps the rest of the move rather than living inside it because a
    /// close has to happen *before* the "a menu is up, so track its rows" branch
    /// below — otherwise the menu would follow the pointer out of the window and
    /// never let go — while the redraw it earns has to survive whichever of that
    /// branch's several exits the move ends up taking.
    ///
    /// The pointer entering the panel is not "leaving": the panel hangs directly
    /// under the button, so the walk from one to the other is continuous, which
    /// is what makes a hover menu usable at all.
    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        self.pointer = (x, y);
        if let Some(fab) = self.fab.as_mut() {
            let viewport = self.viewport;
            let changed = fab.mouse_move(x, y, viewport);
            self.gather_support_intents();
            return changed;
        }
        let in_content = self.in_support_content(x, y);
        if self.support_only
            && let Some(support) = self.support.as_mut()
            && (support.dialog_open() || in_content)
        {
            let viewport = self.viewport;
            return support.mouse_move(&mut self.engine, viewport, x, y);
        }
        if self.updates_modal {
            self.tooltip.set_hover(None, Instant::now());
            return true;
        }
        // The delegation modal owns the pointer while it stands, for the same
        // reason every other modal here does: chrome under the scrim must not
        // light up or change the cursor.
        if self.delegation_open() {
            self.tooltip.set_hover(None, Instant::now());
            return self.delegation_mouse_move(x, y);
        }
        // The recording modal owns the pointer while it stands, for the same
        // reason: chrome under the scrim must not light up or change the
        // cursor.
        if self.recording_modal.is_some() {
            self.tooltip.set_hover(None, Instant::now());
            return self.recording_mouse_move(x, y);
        }
        // Modal: own the pointer so chrome under the scrim does not light up or
        // change the cursor (I-beam, sidebar ColResize).
        if self.move_to.is_some() {
            self.tooltip.set_hover(None, Instant::now());
            return true;
        }
        if self.search.is_some() {
            let bounds = self.search_bounds();
            let point = if self.search_only || bounds.contains(x, y) {
                self.search_point(x, y)
            } else {
                (-1.0, -1.0)
            };
            return View::mouse_move(self.search.as_mut().unwrap(), point.0, point.1);
        }
        // **Before the "a menu is up, track its rows" branch below**, or the
        // window menu would follow the pointer out of the caption and never let
        // go.
        let tracked = self.track_window_menu_hover(x, y);
        if tracked && self.hover == Some(Hit::Window(CaptionAction::WindowMenu)) {
            // The pointer is standing on the button. Nothing underneath needs a
            // hover update, and the branch below would only clear the row
            // highlight the menu has already replaced.
            self.pointer = (x, y);
            self.tooltip.set_hover(None, Instant::now());
            return true;
        }
        // Otherwise fall through, so a row or the leaf under it still gets its
        // hover — but keep the frame the tracker earned, whichever of the
        // several exits below this move ends up taking.
        if tracked {
            self.mouse_move_rest(x, y);
            return true;
        }
        self.mouse_move_rest(x, y)
    }

    fn mouse_double_click(&mut self, ws: &WorkspaceDoc, x: f32, y: f32) -> bool {
        if self.updates_modal {
            return true;
        }
        if self.recording_modal.is_some() {
            return true;
        }
        if self.move_to.is_some() {
            let bounds = self.move_to_bounds();
            if !bounds.contains(x, y) {
                return true;
            }
            let Some(surface) = self.move_to.as_mut() else {
                return true;
            };
            let action = surface.mouse_double_click(&mut self.engine, x - bounds.x, y - bounds.y);
            return self.apply_move_to_action(action);
        }
        if self.search.is_some() {
            let bounds = self.search_bounds();
            if !self.search_only && !bounds.contains(x, y) {
                return true;
            }
            let (x, y) = self.search_point(x, y);
            return View::mouse_double_click(self.search.as_mut().unwrap(), ws, x, y);
        }
        if self.link_device.is_some() {
            return true;
        }
        if self.error.is_some()
            || self.import_dialog.is_some()
            || self.workspace_icon.is_some()
            || self.update_newer_modal_open()
        {
            return true;
        }
        let bell = self.reminder_bell_rect();
        if bell.contains(x, y)
            || self
                .reminder_shell
                .blocks_pointer(self.viewport, bell, x, y)
        {
            return true;
        }
        if self.workspace_rename.is_some() {
            // A row that is no longer there — the popover closed underneath —
            // reads as "outside", which commits, the same as clicking away.
            let row = self
                .workspace_rename_row()
                .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            if let Some(f) = self.workspace_rename.as_mut() {
                f.double_click(&mut self.engine, row, x, y);
            }
            return true;
        }
        if let Some(dialog) = self.new_document.as_mut() {
            dialog.double_click(&mut self.engine, self.viewport, x, y);
            return true;
        }
        if let Some(dialog) = self.snapshot_prompt.as_mut() {
            dialog.double_click(&mut self.engine, self.viewport, x, y);
            return true;
        }
        if self.ai_visible() && self.ai_rect().contains(x, y) {
            self.ai_focused = true;
            return self.ai_chat.mouse_double_click(x, y);
        }
        let hit = self.hit_at(x, y);
        if hit == Hit::Sidebar {
            // Reached only by a press that landed in the column, so it is on
            // screen and its rows have to be current before they are hit-tested.
            self.refresh_explore(ws, true);
            let out = self
                .explore
                .mouse_double_click(self.regions.sidebar, self.viewport, x, y);
            return self.apply(ws, out);
        }
        if hit != Hit::Content {
            return false;
        }
        self.focus_pane_at(x, y);
        let (lx, ly) = self.to_leaf(x, y);
        let (view, doc) = self.tabs_mut().active_mut().doc_ref(ws);
        let handled = match view {
            TabView::Welcome(w) => w.mouse_double_click(doc, lx, ly),
            TabView::Manage(w) => View::mouse_double_click(w, doc, lx, ly),
            TabView::Trash(t) => View::mouse_double_click(t, doc, lx, ly),
            TabView::Tasks(t) => View::mouse_double_click(t, doc, lx, ly),
            TabView::Reminders(r) => View::mouse_double_click(r, doc, lx, ly),
            TabView::Settings(s) => View::mouse_double_click(s, doc, lx, ly),
            TabView::Feedback(f) => View::mouse_double_click(f, doc, lx, ly),
            TabView::Diff(d) => View::mouse_double_click(d, doc, lx, ly),
            TabView::AiChat(a) => View::mouse_double_click(a, doc, lx, ly),
            TabView::Socials(a) => View::mouse_double_click(a, doc, lx, ly),
            TabView::Session(s) => View::mouse_double_click(s, doc, lx, ly),
            TabView::Run(r) => View::mouse_double_click(r, doc, lx, ly),
            TabView::Result(r) => View::mouse_double_click(r, doc, lx, ly),
            TabView::Canvas(c) => View::mouse_double_click(c, doc, lx, ly),
            TabView::Leaf(l) => Leaf::mouse_double_click(l, doc, lx, ly),
        };
        // A Profile page's double-clicked workspace opens through the window.
        handled | self.drain_manage_requests()
    }

    fn scroll_by(&mut self, delta: MouseScrollDelta) {
        if self.updates_modal || self.support_only || self.fab_only {
            return;
        }
        // Nothing in the setup modal scrolls; the wheel goes no further.
        if self.recording_modal.is_some() {
            return;
        }
        if self.delegation_scroll(-Self::wheel_lines(delta) * 32.0) {
            return;
        }
        if self.reminder_creation.is_open() {
            let management = self.reminders.management.clone();
            self.reminder_creation.scroll_by(
                self.viewport,
                management.as_ref(),
                Self::wheel_lines(delta),
            );
            return;
        }
        if self.reminder_shell.overlay_live() {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => -y * 32.0,
                MouseScrollDelta::PixelDelta(p) => -p.y as f32,
            };
            self.reminder_shell.scroll_by(self.viewport, dy);
            return;
        }
        if let Some(surface) = self.move_to.as_mut() {
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => -y * 32.0,
                MouseScrollDelta::PixelDelta(p) => -p.y as f32,
            };
            surface.scroll_by(dy);
            return;
        }
        if self.search.is_some() {
            if self.search_only
                || self
                    .search_bounds()
                    .contains(self.pointer.0, self.pointer.1)
            {
                View::scroll_by(self.search.as_mut().unwrap(), delta);
            }
            return;
        }
        if let Some(dialog) = self.error.as_mut() {
            // The one thing a wheel can mean over a modal error: move the
            // details text inside its well.
            dialog.scroll_by(&mut self.engine, self.viewport, -Self::wheel_lines(delta));
            return;
        }
        if let Some(dialog) = self.workspace_icon.as_mut() {
            dialog.scroll_by(delta);
            return;
        }
        if self.link_device.is_some() {
            return;
        }
        if self.confirm.is_some() || self.new_document.is_some() || self.snapshot_prompt.is_some() {
            return;
        }
        let bell = self.reminder_bell_rect();
        if self
            .reminder_shell
            .blocks_pointer(self.viewport, bell, self.pointer.0, self.pointer.1)
        {
            return;
        }
        if self.update_contains_pointer() {
            return;
        }
        if let Some(dialog) = self.import_dialog.as_mut() {
            if dialog.preview_contains(self.viewport, self.pointer.0, self.pointer.1) {
                dialog.scroll_by(delta, self.viewport);
            }
            return;
        }
        // The wheel arrives without coordinates, so it goes wherever the pointer
        // was last seen. This is the only container state the shell already has
        // and does not pass on.
        let (px, py) = self.pointer;
        if self.popover_hit(px, py) {
            let dy = Self::wheel_lines(delta);
            self.explore
                .scroll_popover(self.explore_popover_area(), self.viewport, dy);
            return;
        }
        if self.regions.sidebar.w > 0.0 && self.regions.sidebar.contains(px, py) {
            let dy = Self::wheel_lines(delta);
            self.explore.scroll_by(self.regions.sidebar, dy);
            return;
        }
        if self.ai_visible() && self.ai_rect().contains(px, py) {
            self.ai_chat.scroll_by(delta);
            return;
        }
        if self.context.is_open() && self.regions.context.contains(px, py) {
            let dy = Self::wheel_lines(delta);
            self.context_view.scroll_by(self.regions.context, dy);
            return;
        }
        // Over the caption strip, vertical wheel scrolls tabs horizontally —
        // the browser convention. Shift+wheel / trackpad x also count.
        if self.regions.caption.contains(px, py) {
            self.ensure_tab_widths();
            let dx = match delta {
                MouseScrollDelta::LineDelta(x, y) => {
                    if x.abs() > y.abs() {
                        x * theme::NAV_ROW_H
                    } else {
                        y * theme::NAV_ROW_H
                    }
                }
                MouseScrollDelta::PixelDelta(p) => {
                    if p.x.abs() > p.y.abs() {
                        p.x as f32
                    } else {
                        p.y as f32
                    }
                }
            };
            // Positive wheel-up → scroll content left (reveal leftward tabs).
            self.set_tab_scroll(tabs::clamp_scroll(
                self.strip(),
                &self.tab_widths(),
                self.tab_scroll() - dx,
            ));
            return;
        }
        if !self.content().contains(px, py) {
            return;
        }
        // The pane under the pointer, not the focused one: a split must not
        // keep scrolling the pane last clicked. See `wheel_pane` for why this
        // does not move focus with it.
        let path = self.wheel_pane(px, py);
        let local = self.pane_local(&path, px, py);
        let Some(group) = self.panes.group_mut(&path) else {
            return;
        };
        match group.active_view() {
            TabView::Welcome(w) => w.scroll_by(delta),
            TabView::Manage(w) => View::scroll_by(w, delta),
            TabView::Trash(t) => View::scroll_by(t, delta),
            TabView::Tasks(t) => View::scroll_by(t, delta),
            TabView::Reminders(r) => View::scroll_by(r, delta),
            TabView::Settings(s) => View::scroll_by(s, delta),
            TabView::Feedback(f) => View::scroll_by(f, delta),
            TabView::Diff(d) => View::scroll_by(d, delta),
            TabView::AiChat(a) => View::scroll_by(a, delta),
            TabView::Socials(a) => View::scroll_by(a, delta),
            TabView::Session(s) => View::scroll_by(s, delta),
            TabView::Run(r) => View::scroll_by(r, delta),
            TabView::Result(r) => View::scroll_by(r, delta),
            TabView::Canvas(c) => c.wheel(delta, local.0, local.1),
            TabView::Leaf(l) => Leaf::scroll_by(l, delta),
        }
    }

    fn zoom_wheel(&mut self, delta: MouseScrollDelta, x: f32, y: f32) -> bool {
        self.canvas_zoom_wheel(delta, x, y)
    }

    fn zoom_key(&mut self, intent: ZoomIntent) -> bool {
        self.canvas_zoom_key(intent)
    }

    fn key_up(&mut self, ws: &mut WorkspaceDoc, key: &Key) -> bool {
        self.canvas_key_up(ws, key)
    }

    fn set_viewport(&mut self, w: f32, h: f32) {
        self.viewport = (w, h);
        let bounds = self.search_bounds();
        if let Some(search) = self.search.as_mut() {
            View::set_viewport(search, bounds.w, bounds.h);
        }
        // Only the tab being looked at lays out; see `relayout`.
        self.relayout();
        let bell = self.reminder_bell_rect();
        self.reminder_shell.maintain_geometry(self.viewport, bell);
    }

    fn set_maximized(&mut self, maximized: bool) {
        self.maximized = maximized;
    }

    fn set_pinned(&mut self, pinned: bool) {
        self.pinned = pinned;
    }

    fn set_mods(&mut self, mods: ModifiersState) {
        self.mods = mods;
        match self.active_view() {
            TabView::Trash(t) => View::set_mods(t, mods),
            TabView::Tasks(t) => View::set_mods(t, mods),
            TabView::Reminders(r) => View::set_mods(r, mods),
            TabView::Leaf(l) => View::set_mods(l, mods),
            TabView::Canvas(c) => View::set_mods(c, mods),
            _ => {}
        }
    }

    fn cursor_icon(&self) -> CursorIcon {
        if self.updates_modal {
            return CursorIcon::Default;
        }
        if self.in_support_content(self.pointer.0, self.pointer.1)
            && let Some(support) = self.support.as_ref()
        {
            return support.cursor_icon();
        }
        // The delegation modal owns the pointer while it stands — `mouse_move`
        // answers it before `move_to` and `search`, so theirs is stale — and it
        // draws a scrim, so the tab underneath must not be asked. Without this
        // the cursor fell through to the document and the row behind the
        // *Cancel* / *Delegate* pair made the pointer an I-beam.
        if let Some(modal) = &self.delegation {
            return modal.cursor();
        }
        // The recording modal's own table: text over a field, press over a
        // control, and never the document's cursor leaking through the scrim.
        if let Some(modal) = &self.recording_modal {
            return modal.cursor_icon();
        }
        if self.move_to.is_some() {
            return CursorIcon::Default;
        }
        if let Some(search) = &self.search {
            return if self.search_only
                || self
                    .search_bounds()
                    .contains(self.pointer.0, self.pointer.1)
            {
                View::cursor_icon(search)
            } else {
                CursorIcon::Default
            };
        }
        if let Some(dialog) = &self.error {
            return dialog.cursor();
        }
        if let Some(dialog) = &self.import_dialog {
            return dialog.cursor();
        }
        if let Some((dialog, _)) = &self.confirm {
            return dialog.cursor();
        }
        if let Some(icon) = self.update_newer_cursor() {
            return icon;
        }
        if let Some(icon) = self.update_cursor() {
            return icon;
        }
        if self
            .document_ai_button()
            .into_iter()
            .chain(self.document_context_button())
            .any(|button| button.contains(self.pointer.0, self.pointer.1))
        {
            return koru_ui::cursor::PRESS;
        }
        if let Some(dialog) = &self.new_document {
            return dialog.cursor(self.viewport, self.pointer.0, self.pointer.1);
        }
        if let Some(dialog) = &self.snapshot_prompt {
            return dialog.cursor(self.viewport, self.pointer.0, self.pointer.1);
        }
        if let Some(dialog) = &self.link_device {
            return dialog.cursor();
        }
        if let Some(m) = &self.workspace_icon {
            return m.cursor();
        }
        if let Some(menu) = &self.menu {
            let i = menu.item_at(self.viewport, self.pointer.0, self.pointer.1);
            let enabled = self.enabled_items(menu);
            return match i {
                Some(i) if enabled.get(i) == Some(&true) => koru_ui::cursor::PRESS,
                _ => CursorIcon::Default,
            };
        }
        // The toast sits over the document: the plain arrow across the whole
        // chip — never the I-beam the row underneath would earn.
        if let Some((toast, _)) = &self.toast
            && toast.over_chip()
        {
            return toast.cursor();
        }
        // The *In Trash* band sits over the document, so without this the
        // editor underneath answers and the pointer becomes an I-beam over a
        // strip there is nothing to type in.
        if self.notice_showing && self.notice_rect().contains(self.pointer.0, self.pointer.1) {
            let on_button = self
                .notice_buttons
                .iter()
                .any(|b| b.contains(self.pointer.0, self.pointer.1));
            return if on_button {
                koru_ui::cursor::PRESS
            } else {
                CursorIcon::Default
            };
        }
        if let Some(cursor) = self
            .reminder_creation
            .cursor(self.pointer.0, self.pointer.1)
        {
            return cursor;
        }
        if self.reminder_creation.is_open() {
            return CursorIcon::Default;
        }
        let bell = self.reminder_bell_rect();
        if self
            .reminder_shell
            .blocks_pointer(self.viewport, bell, self.pointer.0, self.pointer.1)
        {
            return if self.reminder_shell.cursor_press(
                self.viewport,
                bell,
                self.pointer.0,
                self.pointer.1,
            ) {
                koru_ui::cursor::PRESS
            } else {
                CursorIcon::Default
            };
        }
        // The workspace switcher can overlap the pane's resize grip. It is a
        // floating surface above the pane, so its row/pin/menu cursor owns that
        // overlap rather than the column edge underneath it.
        if self.popover_hit(self.pointer.0, self.pointer.1) {
            return self.explore.cursor_icon();
        }
        // A library thumb keeps its vertical-resize cursor while captured,
        // including when the pointer has left the sidebar's narrow hit region.
        if self.explore.pointer_active() && !self.explore.drag_active() {
            return self.explore.cursor_icon();
        }
        if self.sidebar_drag.is_some()
            || self.sidebar_grip().contains(self.pointer.0, self.pointer.1)
        {
            return CursorIcon::ColResize;
        }
        if self.ai_drag.is_some() || self.ai_grip().contains(self.pointer.0, self.pointer.1) {
            return CursorIcon::ColResize;
        }
        let divider_axis = match self.divider_drag.as_ref() {
            Some(d) => Some(d.axis),
            // A tab drag owns the pointer wherever it goes; the resize cursor
            // must not surface under it.
            None if self.pane_drag.is_some() || self.group_drag().is_some() => None,
            None => self
                .panes
                .dividers(self.content())
                .into_iter()
                .find(|(_, axis, rect)| {
                    panes::divider_grip(*rect, *axis).contains(self.pointer.0, self.pointer.1)
                })
                .map(|(_, axis, _)| axis),
        };
        if let Some(axis) = divider_axis {
            return match axis {
                panes::SplitAxis::Vertical => CursorIcon::ColResize,
                panes::SplitAxis::Horizontal => CursorIcon::RowResize,
            };
        }
        if self.ai_visible() && self.ai_rect().contains(self.pointer.0, self.pointer.1) {
            return self.ai_chat.cursor_icon();
        }
        // A tree drag in flight is grabbing wherever the pointer is — it
        // follows the pointer out of the column, so the cursor must too.
        if self.explore.drag_armed() {
            return CursorIcon::Grabbing;
        }
        match self.hover {
            Some(Hit::Content) | None => match &self.tabs().active_tab().view {
                TabView::Welcome(w) => w.cursor_icon(),
                TabView::Manage(w) => View::cursor_icon(w),
                TabView::Trash(t) => View::cursor_icon(t),
                TabView::Tasks(t) => View::cursor_icon(t),
                TabView::Reminders(r) => View::cursor_icon(r),
                TabView::Settings(s) => View::cursor_icon(s),
                TabView::Feedback(f) => View::cursor_icon(f),
                TabView::Diff(d) => View::cursor_icon(d),
                TabView::AiChat(a) => View::cursor_icon(a),
                TabView::Socials(a) => View::cursor_icon(a),
                TabView::Session(s) => View::cursor_icon(s),
                TabView::Run(r) => View::cursor_icon(r),
                TabView::Result(r) => View::cursor_icon(r),
                TabView::Canvas(c) => View::cursor_icon(c),
                TabView::Leaf(l) => View::cursor_icon(l),
            },
            Some(Hit::Sidebar) => self.explore.cursor_icon(),
            Some(Hit::Context) => self.context_view.cursor_icon(),
            // Over any chrome the pointer is never the I-beam the row scrolled
            // behind it would otherwise earn. Beyond that it does not change:
            // Koru does not use the hand cursor (`docs/features/design/DESIGN.md`), so a tab, a
            // window button and a column toggle all keep the plain arrow and
            // say they are pressable by lighting up under the pointer. The
            // arms are kept apart because they are the register of what is
            // pressable here, not because they answer differently.
            Some(Hit::TabScroll) => CursorIcon::Default,
            Some(Hit::Tab(_))
            | Some(Hit::TabClose(_))
            | Some(Hit::NewTab)
            | Some(Hit::ToggleSidebar)
            | Some(Hit::ToggleContext)
            | Some(Hit::Search)
            | Some(Hit::HostAction)
            | Some(Hit::Window(_)) => koru_ui::cursor::PRESS,
            // The caption is the part with no furniture on it: you drag the
            // window by it rather than pressing it.
            Some(Hit::Caption) => CursorIcon::Default,
        }
    }

    fn adopt(&mut self, ws: &WorkspaceDoc) {
        // Every tab drains its pending changes — a tab that never adopts shows
        // deleted rows the moment you switch to it, and its Loro subscription
        // queues without bound. But only the active one *acts on* them: see
        // `TabView::adopt_cheap`.
        let active = self.tabs().active();
        for (i, t) in self.tabs_mut().iter_mut().enumerate() {
            // A draft drains its *own* document. Handing it the workspace here
            // would apply the window's changes to a leaf that is not over the
            // window's document at all.
            if t.draft.is_none() {
                t.view.adopt_cheap(ws);
            }
            if i != active {
                t.invalidate_size();
            }
        }
        // ...and the active one settles what it just noted, then lays out.
        self.catch_up_active(ws);
        if let TabView::Leaf(l) = self.active_view() {
            l.reflow();
        }
        let active_document = self.active_document;
        if self.tabs_mut().retain_live(ws, active_document) {
            // A tab deleted in another window renumbers the strip, so any slide
            // in flight now belongs to the wrong tab.
            self.slides_mut().clear();
            self.size_visible_groups();
        }
        if let Some(entries) = self.entries.clone() {
            let current = self.current_workspace();
            let foreign: std::collections::HashSet<_> = self
                .tabs()
                .iter()
                .filter_map(|t| {
                    let id = t.document?;
                    let home = t.pinned?;
                    (Some(home) != current).then_some(id)
                })
                .collect();
            let live: std::collections::HashSet<_> = entries.iter().map(|e| e.id).collect();
            if self
                .tabs_mut()
                .retain_pinned(|id| live.contains(&id) || foreign.contains(&id))
            {
                self.slides_mut().clear();
                self.size_visible_groups();
            }
        }
        self.refresh_labels(ws);
        // **A search target waiting for its document.** A result in a document
        // this window did not have open asks the shell for that document and
        // leaves the node here; this is the moment the document arrives, so it
        // is the moment to look for it.
        //
        // **It waits rather than being dropped on the first look**, which is
        // the bug this shape exists to prevent: the target used to be `take`n
        // and *then* asked whether it resolved, so the first `adopt` after the
        // request threw it away — and a workspace switch fires several before
        // the document is anywhere near open. The result was a hit in another
        // workspace opening nothing at all, reproducibly. The deadline is what
        // keeps "waits" from meaning "forever".
        if let Some(mut target) = self.deferred_search_target.take() {
            // **Ask again if the first ask could not be made.** `open_node`
            // answers `false` while the manifest has not caught up with a
            // workspace just switched to, and a target waiting for a document
            // nobody requested waits for ever. Asking once *successfully* is
            // what `asked` bounds — not asking once.
            if !target.asked
                && let Some(document) = target.document
            {
                // No history: the pill/search gesture that deferred this already
                // recorded the place the person left.
                target.asked = self.request_store_document(document, false)
                    || self.land_on(ws, document, false, None);
            }
            let expired = target.until.is_some_and(|until| Instant::now() >= until);
            match target.node {
                Some(node) if ws.read(node).is_ok() => {
                    View::open_search_target(
                        self,
                        ws,
                        target.document,
                        Some(node),
                        target.trash_root,
                        target.match_text.as_deref(),
                    );
                }
                // A title hit: it lands when the document it names is the one
                // showing, and the row to land on is that document's root —
                // its title is row 0 of its own body. Handled here rather than
                // by re-entering `open_search_target`, which would take the
                // no-node arm again and defer it a second time, for ever.
                None if target.document.is_some()
                    && self.workspace_document_is_active(target.document) =>
                {
                    if let Some(root) = koru_workspace::document_root(ws) {
                        self.open_node_with_centering(ws, root, true, target.match_text.as_deref());
                    }
                    if let Some(document) = target.document
                        && self.explore.reveal(document)
                    {
                        self.explore_stale = true;
                    }
                    self.retained_search = self.search.take();
                }
                _ if !expired => self.deferred_search_target = Some(target),
                _ => {}
            }
        }
        // The tree is a cache over the library, so a node added or renamed
        // anywhere invalidates it. Rebuilt lazily, at the next draw.
        self.explore_stale = true;
    }

    /// True only while a column or a tab is mid-slide, or the active view is
    /// smooth-scrolling, so the shell's fast wake-up is the exception it is
    /// meant to be. Each must return to `false` on its own (ADR-26).
    fn animating(&self) -> bool {
        // **Only while the toast is fading or showing indeterminate progress.**
        // Holding a message or a determinate bar still is not an animation; see
        // `koru_ui::toast`.
        if self.toast_animating(Instant::now()) {
            return true;
        }
        // **A deletion verb is in flight.** Its answer comes back on the run
        // index's own thread, which cannot ask for a frame; without this the
        // receipt would wait for the pointer to move. It stops the moment the
        // answer lands.
        if self.run_index.awaiting_deletion() {
            return true;
        }
        self.tooltip.animating(Instant::now())
            || self.reminder_shell.animating()
            || self.sidebar.animating()
            || self.ai_panel.animating()
            || (self.ai_visible() && self.ai_chat.animating())
            || self.context.animating()
            || self.slides().animating()
            || self.zoom_label.animating()
            || self.explore.animating()
            || self.workspace_icon.as_ref().is_some_and(|m| m.animating())
            || crate::taskbar::new_document_pending()
            || crate::taskbar::open_pending()
            || crate::taskbar::open_document_pending()
            || self.tabs().active_tab().view.is_animating()
    }

    fn invalidate(&mut self) {
        if let Some(search) = self.search.as_mut() {
            View::invalidate(search);
        }
        self.ai_chat.invalidate();
        self.engine.clear_pill_cache();
        self.mark_tab_widths_stale();
        self.reminder_shell.invalidate_metrics();
        // The indicator's widths are measured once, when its word changes — so
        // a theme, font-role or density change is the one other thing that has
        // to re-measure them, or the chip keeps the size of a face nobody is
        // drawing in any more.
        self.remeasure_dictation();
        for t in self.tabs_mut().iter_mut() {
            match &mut t.view {
                TabView::Welcome(w) => w.invalidate(),
                TabView::Manage(w) => View::invalidate(w),
                TabView::Trash(t) => View::invalidate(t),
                TabView::Tasks(t) => View::invalidate(t),
                TabView::Reminders(r) => View::invalidate(r),
                TabView::Settings(s) => View::invalidate(s),
                TabView::Feedback(f) => View::invalidate(f),
                TabView::Diff(d) => View::invalidate(d),
                TabView::AiChat(a) => View::invalidate(a),
                TabView::Socials(a) => View::invalidate(a),
                TabView::Session(s) => View::invalidate(s),
                TabView::Run(r) => View::invalidate(r),
                TabView::Result(r) => View::invalidate(r),
                TabView::Canvas(c) => View::invalidate(c),
                TabView::Leaf(l) => View::invalidate(l),
            }
        }
    }

    // ------------------------------------------- accessibility (1a.2, 1b.4)
    //
    // Seven small methods, each one line of delegation into
    // `mainview/accessibility`. The subsystem lives there; this file is a call
    // site (§7.10).

    fn seat_accessibility(
        &mut self,
        window: koru_workspace_host::accessibility::WindowSlot,
        _view: koru_workspace_host::accessibility::ViewSlot,
    ) {
        MainView::seat_accessibility(self, window);
    }

    fn semantics(
        &self,
        view: koru_workspace_host::accessibility::ViewSlot,
        scope: koru_workspace_host::accessibility::Scope,
    ) -> Vec<koru_workspace_host::accessibility::SemanticNode> {
        MainView::semantics(self, view, scope)
    }

    fn accessibility_retired(&self) -> Vec<koru_workspace_host::accessibility::Occurrence> {
        MainView::accessibility_retired(self)
    }

    fn accessibility_focus(
        &self,
        view: koru_workspace_host::accessibility::ViewSlot,
    ) -> Option<koru_workspace_host::accessibility::Occurrence> {
        MainView::accessibility_focus(self, view)
    }

    fn window_command_bounds(&self) -> koru_workspace_host::accessibility::CaptionGeometry {
        MainView::window_command_bounds(self)
    }

    fn semantics_revision(&self) -> u64 {
        MainView::semantics_revision(self)
    }

    fn set_durability(&mut self, durability: &koru_workspace::durability::Durability) {
        self.save_status.observe(durability);
    }

    fn accessibility_action(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: koru_workspace_host::accessibility::Occurrence,
        action: accesskit::Action,
        data: Option<&accesskit::ActionData>,
    ) -> bool {
        let handled = MainView::accessibility_action(self, ws, target, action, data);
        // **A request from a screen reader is a gesture, and every gesture ends
        // here.** `key`, `mouse_down` and `mouse_up` each finish with this call,
        // and `drain_trash_all` exists so that a new gesture cannot be wired to
        // some of the drains and quietly miss the rest. This one was wired to
        // none of them, which nothing noticed until the Tasks page gave an AT
        // action something to ask the container for.
        let acted = self.drain_trash_all(ws);
        handled || acted
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        workspace: &Workspace,
        images: &ImageCache,
        root: Affine,
        caret_on: bool,
        pointer: (f32, f32),
    ) {
        if self.fab_only {
            let viewport = self.viewport;
            if let Some(fab) = self.fab.as_mut() {
                fab.draw(scene, root, &mut self.engine, viewport);
            }
            return;
        }
        if self.support_only {
            self.draw_support_frame(scene, root);
            return;
        }
        if self.search_only {
            if let Some(search) = self.search.as_mut() {
                View::draw(search, scene, workspace, images, root, caret_on, pointer);
            }
            // A dedicated search window is still a place somebody dictates
            // into, and a chip that silently did not appear there would be
            // worse than one drawn over a full-window surface.
            self.draw_dictation(scene, root);
            return;
        }
        self.draw_frame(scene, workspace, images, root, caret_on, pointer);
        self.draw_update_banner(scene, root);
        if self.search.is_some() {
            let bounds = self.search_bounds();
            draw_scene::fill(
                scene,
                root,
                theme::p().scrim,
                &draw_scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
            );
            draw_scene::floating_panel(
                scene,
                root,
                bounds.x,
                bounds.y,
                bounds.w,
                bounds.h,
                &draw_scene::Panel::popover(crate::search::SURFACE_RADIUS),
            );
            let local_pointer = (pointer.0 - bounds.x, pointer.1 - bounds.y);
            let modal_root = root * Affine::translate((f64::from(bounds.x), f64::from(bounds.y)));
            scene.push_clip_layer(
                Fill::NonZero,
                modal_root,
                &draw_scene::rrect(0.0, 0.0, bounds.w, bounds.h, crate::search::SURFACE_RADIUS),
            );
            View::draw(
                self.search.as_mut().unwrap(),
                scene,
                workspace,
                images,
                modal_root,
                caret_on,
                local_pointer,
            );
            scene.pop_layer();
            draw_scene::stroke(
                scene,
                root,
                theme::p().panel_border,
                theme::BORDER_W,
                &draw_scene::rrect(
                    bounds.x,
                    bounds.y,
                    bounds.w,
                    bounds.h,
                    crate::search::SURFACE_RADIUS,
                ),
            );
        }
        if self.move_to.is_some() {
            let bounds = self.move_to_bounds();
            draw_scene::fill(
                scene,
                root,
                theme::p().scrim,
                &draw_scene::rect(0.0, 0.0, self.viewport.0, self.viewport.1),
            );
            draw_scene::floating_panel(
                scene,
                root,
                bounds.x,
                bounds.y,
                bounds.w,
                bounds.h,
                &draw_scene::Panel::popover(move_to::SURFACE_RADIUS),
            );
            let modal_root = root * Affine::translate((f64::from(bounds.x), f64::from(bounds.y)));
            scene.push_clip_layer(
                Fill::NonZero,
                modal_root,
                &draw_scene::rrect(0.0, 0.0, bounds.w, bounds.h, move_to::SURFACE_RADIUS),
            );
            if let Some(surface) = self.move_to.as_mut() {
                surface.draw(scene, &mut self.engine, modal_root);
            }
            scene.pop_layer();
            draw_scene::stroke(
                scene,
                root,
                theme::p().panel_border,
                theme::BORDER_W,
                &draw_scene::rrect(
                    bounds.x,
                    bounds.y,
                    bounds.w,
                    bounds.h,
                    move_to::SURFACE_RADIUS,
                ),
            );
        }
        self.reminder_creation
            .draw(scene, root, &mut self.engine, crate::format::lang());
        // **Last, and above the two modals this method draws.** The composition
        // it describes may be *inside* one of them — the search box, the *Move
        // to…* filter — so an indicator painted earlier would be hidden by the
        // thing it is about. It exists only while somebody is speaking.
        self.draw_dictation(scene, root);
    }

    fn document_root(&self) -> Option<NodeId> {
        self.tabs().active_tab().view.root()
    }

    fn delegate_folder_chosen(&mut self, chosen: koru_workspace_host::DelegateFolder) -> bool {
        MainView::delegate_folder_chosen(self, chosen)
    }

    fn ai_chat_folder_chosen(&mut self, chosen: std::path::PathBuf) -> bool {
        let Some((source, chat)) = self.ai_chat_folder_ask.take() else {
            return false;
        };
        match source {
            AiChatFolderSource::Pane => self.ai_chat.working_folder_chosen(chat, chosen),
            AiChatFolderSource::AiChatTab => {
                if let TabView::AiChat(view) = self.tabs_mut().active_view() {
                    view.working_folder_chosen(chat, chosen)
                } else {
                    false
                }
            }
        }
    }

    fn recording_path_chosen(&mut self, chosen: std::path::PathBuf) -> bool {
        self.recording_path_chosen_impl(chosen)
    }

    fn recording_event(&mut self, event: koru_workspace_host::RecordingEvent) -> bool {
        self.recording_event_impl(event)
    }

    fn take_recording_telemetry(&mut self) -> Option<koru_recording::SessionTelemetry> {
        self.take_recording_telemetry_impl()
    }

    fn tab_presence(&self, document: koru_core::DocumentId) -> koru_workspace_host::TabPresence {
        match self.tabs().showing_document(document) {
            Some(i) if self.tabs().get(i).is_some_and(|t| t.is_pinned()) => {
                koru_workspace_host::TabPresence::Pinned
            }
            Some(_) => koru_workspace_host::TabPresence::Open,
            None => koru_workspace_host::TabPresence::Absent,
        }
    }

    fn unpin_document(&mut self, document: koru_core::DocumentId) -> bool {
        self.unpin_live_document(document)
    }

    fn pinned_search_rows(
        &self,
    ) -> Vec<(String, koru_core::DocumentId, koru_core::WorkspaceId, bool)> {
        self.pinned_rows()
    }

    /// Always `None`: a document opens as a **tab** here, drained in
    /// `mouse_down` and `key` where a document is in hand. Only Ctrl+N, which
    /// the shell owns, still opens a window.
    fn take_wants_open(&mut self) -> Option<NodeId> {
        None
    }

    fn set_window_focused(&mut self, focused: bool) {
        if !focused {
            self.canvas_focus_lost();
        }
        if focused {
            match &mut self.tabs_mut().active_mut().view {
                TabView::Manage(view) => view.refresh_cloud(),
                _ => {}
            }
            self.drain_manage_profiles_request();
        }
    }
    fn take_wants_stay_focused(&mut self) -> bool {
        match self.active_view() {
            TabView::Leaf(leaf) => leaf.take_wants_stay_focused(),
            _ => false,
        }
    }

    fn open_background_tab(
        &mut self,
        ws: &WorkspaceDoc,
        document: koru_core::DocumentId,
        home: Option<koru_core::WorkspaceId>,
    ) -> bool {
        if self.canvas_hidden(Some(document)) {
            return false;
        }
        let home = home.or_else(|| self.current_workspace());
        if let Some(i) = self.tabs().showing_document(document).or_else(|| {
            self.tabs()
                .showing(koru_core::NodeId::from_uuid(document.as_uuid()))
        }) {
            inputdebug::write(format_args!(
                "[Koru][input] view open-background-tab already-showing i={i}"
            ));
            if let Some(tab) = self.tabs_mut().get_mut(i) {
                if tab.home.is_none() {
                    tab.home = home;
                }
            }
            self.ensure_tab_visible(i);
            return true;
        }
        let Some(root) = koru_workspace::document_root(ws) else {
            return false;
        };
        let name = koru_workspace::name_of(ws, root);
        let view = self.document_view(ws, root, Some(document));
        let tab = Tab::document_at(view, &name, Some(document), home);
        let i = self.tabs_mut().insert_inactive(tab);
        inputdebug::write(format_args!(
            "[Koru][input] view open-background-tab inserted i={i} pinned_showing={}",
            self.tabs().active_tab().is_pinned(),
        ));
        self.mark_tab_widths_stale();
        self.ensure_tab_visible(i);
        true
    }

    /// Alt+Arrow and the mouse's thumb buttons, resolved by the shell into a
    /// direction. The container is the one thing here with somewhere to go back
    /// *to*, which is why no other view implements this.
    fn navigate_history(&mut self, ws: &WorkspaceDoc, delta: i32) -> bool {
        self.step_history(ws, delta)
    }

    fn take_window_request(&mut self) -> Option<WindowRequest> {
        self.window_request.take()
    }

    fn take_pane_request(&mut self) -> Option<PaneRequest> {
        self.pane_requests.pop_front()
    }

    fn create_document_failed(&mut self) {
        // The draft's promotion latch. `promote_draft` asks exactly once per
        // latch, so a failed create must hand the latch back or the draft
        // types forever and never becomes real — found live, against a store
        // that went away between the app opening and the first keystroke.
        self.promoting = false;
    }

    fn request_close(&mut self) -> bool {
        // **A window never dies holding a capture.** The veto drives the
        // lifecycle to its end — stop and finalize a recording, abort a
        // countdown — and the close is re-asked when it lands.
        if self.recording_lifecycle.is_some() {
            return self.recording_close_veto();
        }
        if self.socials_window_close_veto() {
            return true;
        }
        if self.fab_only {
            // The icon's ✕ does not exist; a close from the OS just hides it.
            self.background_intents.push_back(
                koru_workspace_host::native_surface::BackgroundIntent::SetVisible(false),
            );
            return true;
        }
        if let Some(support) = self.support.as_mut() {
            return support.request_close();
        }
        if self.import_dialog.is_some() {
            self.import_dialog = None;
            self.ask_pane(PaneRequest::FinishWorkspaceImport(
                koru_workspace_host::ImportChoice::Cancel,
            ));
            return true;
        }
        false
    }

    fn is_global_search(&self) -> bool {
        self.search_only
    }

    fn is_feedback_window(&self) -> bool {
        self.feedback_only
    }

    fn is_support_window(&self) -> bool {
        self.support_only
    }

    fn is_auxiliary(&self) -> bool {
        self.support_only || self.fab_only
    }

    fn floating_window(&self) -> bool {
        self.fab_only
    }

    fn preferred_window_size(&self) -> Option<((f32, f32), (f32, f32))> {
        if self.fab_only {
            let s = crate::support::fab::SIZE;
            return Some(((s, s), (s, s)));
        }
        // The Messenger's own width, the way the web draws it; the minimum
        // width is the width because `fixed_width` holds it there.
        self.support_only
            .then_some(((440.0, 720.0), (440.0, 480.0)))
    }

    fn fixed_width(&self) -> bool {
        self.support_only
    }

    fn attach_surface_host(&mut self, host: koru_workspace_host::native_surface::SurfaceHost) {
        if let Some(support) = self.support.as_mut() {
            support.attach_host(host.clone());
        }
        if let Some(fab) = self.fab.as_mut() {
            fab.attach_host(host);
        }
    }

    fn background_deadline(&self) -> Option<Instant> {
        self.support.as_ref().and_then(|s| s.maintenance_deadline())
    }

    fn background_maintain(&mut self, now: Instant, app_hidden: bool) -> bool {
        let mut changed = false;
        if let Some(support) = self.support.as_mut() {
            changed |= support.background_maintain(now, app_hidden);
        }
        if let Some(fab) = self.fab.as_mut() {
            changed |= fab.background_maintain(now, app_hidden);
        }
        self.gather_support_intents();
        changed
    }

    fn visible_after_restore(&self) -> bool {
        if let Some(support) = self.support.as_ref() {
            return support.visible_after_restore();
        }
        if let Some(fab) = self.fab.as_ref() {
            return fab.visible_after_restore();
        }
        true
    }

    fn window_shown(&mut self) {
        if let Some(support) = self.support.as_mut() {
            support.shown();
        }
    }

    fn popup_chosen(&mut self, id: &str) -> bool {
        let claimed = self.fab.as_mut().is_some_and(|fab| fab.popup_chosen(id));
        if claimed {
            self.gather_support_intents();
        }
        claimed
    }

    fn show_support_reply(&mut self) -> bool {
        if self.support_only || self.fab_only {
            return false;
        }
        let lang = crate::format::lang();
        self.toast = Some((
            koru_ui::Toast::new(
                koru_i18n::t(lang, koru_i18n::Key::SupportReplied).to_string(),
                Some(koru_i18n::t(lang, koru_i18n::Key::SupportOpen)),
                Instant::now(),
            ),
            ToastPayload::OpenSupport,
        ));
        true
    }

    fn native_surface(
        &mut self,
    ) -> Option<(
        koru_workspace_host::native_surface::SharedSurface,
        koru_workspace_host::native_surface::Placement,
    )> {
        let content = self.support_content();
        self.support.as_mut()?.native_surface(content)
    }

    fn native_surface_failed(&mut self, error: koru_workspace_host::native_surface::SurfaceError) {
        if let Some(support) = self.support.as_mut() {
            support.native_surface_failed(error);
        }
    }

    fn take_background_intent(
        &mut self,
    ) -> Option<koru_workspace_host::native_surface::BackgroundIntent> {
        self.background_intents.pop_front()
    }

    fn open_feedback_tab(&mut self) -> bool {
        if self.search_only {
            return false;
        }
        self.open_send_feedback_tab()
    }

    fn close_feedback_tab(&mut self) -> bool {
        let Some(i) = self
            .tabs()
            .iter()
            .position(|t| matches!(t.view, TabView::Feedback(_)))
        else {
            return false;
        };
        self.close(i)
    }

    /// **Refused by a search-only window**, the same answer
    /// [`Self::open_search_target`] gives it and for the same reason: it hosts
    /// no tabs, so there is nowhere for a page to go.
    fn open_whats_new(&mut self, version: &str) -> bool {
        if self.search_only {
            return false;
        }
        MainView::open_whats_new(self, version)
    }

    fn open_search_target(
        &mut self,
        ws: &WorkspaceDoc,
        document: Option<NodeId>,
        node: Option<NodeId>,
        trash_root: Option<NodeId>,
        match_text: Option<&str>,
    ) -> bool {
        let started = Instant::now();
        tracing::info!(
            target: "koru::navigation",
            document = %trace_navigation_node(document),
            node = %trace_navigation_node(node),
            active_document = self.active_document.map(|id| id.to_storage()).as_deref().unwrap_or("none"),
            "document view began applying a routed target"
        );
        if self.search_only {
            tracing::warn!(
                target: "koru::navigation",
                document = %trace_navigation_node(document),
                node = %trace_navigation_node(node),
                "search-only window declined the routed target"
            );
            return false;
        }
        // A page result belongs to Documents even if search was opened while
        // the navigator was showing Files. Opaque files arrive through the
        // dedicated `open_file_search_target` entry point instead.
        if trash_root.is_none() {
            self.explore.show_documents();
        }
        let completing_history_route = self.history_pending_routed && self.restoring;
        if completing_history_route && self.history_pending.is_none() {
            // `show_active_document` already restored the spot. Landing again
            // rebuilds the leaf and drops the caret and scroll.
            self.history_pending_routed = false;
            self.restoring = false;
            return true;
        }
        // A document pill carries a stable document route, not a snapshot of
        // lifecycle state. Once the target workspace is loaded its manifest is
        // authoritative: if the document has moved to Trash, open the Trash tab
        // even when the source pill had not learned that yet.
        let trash_root = trash_root.or_else(|| {
            document.filter(|document| {
                self.trashed_documents.as_ref().is_some_and(|entries| {
                    entries
                        .iter()
                        .any(|entry| entry.id.as_uuid() == document.as_uuid())
                })
            })
        });
        let opened = if let Some(root) = trash_root {
            self.open_trash_tab();
            if let TabView::Trash(trash) = &mut self.tabs_mut().active_mut().view {
                trash.reveal(root);
            }
            true
        } else {
            match node {
                Some(node) => {
                    // **The node may be in a document this window does not have
                    // open.** One `LoroDoc` is one document, so `ws` is the
                    // active one and `read` answers no for anything else —
                    // which used to make a result from elsewhere silently open
                    // nothing at all. Ask for its document first, and let the
                    // deferred target find the node once the swap lands.
                    if ws.read(node).is_err() {
                        match document {
                            Some(document) if document != node => {
                                self.defer_until_document_arrives(
                                    document,
                                    Some(node),
                                    match_text.map(str::to_owned),
                                );
                                // **Asked for either way.** `open_node` answers
                                // `false` when the manifest has not caught up
                                // with the document yet — a workspace this
                                // window switched to a moment ago — and that is
                                // precisely when the deferred target matters.
                                // Reporting it as a failure would tell the
                                // surface nothing happened, when what is
                                // happening is a round trip.
                                // History already recorded at the pill/search
                                // gesture — do not record the workspace's
                                // intermediate active document as a stop.
                                let asked = self.request_store_document(document, false)
                                    || self.land_on(ws, document, false, None);
                                if let Some(target) = self.deferred_search_target.as_mut() {
                                    target.asked = asked;
                                }
                                true
                            }
                            _ => false,
                        }
                    } else if self.refuse_stale_task_open(ws, node) {
                        // **The one landing that is refused rather than made**
                        // (SPEC §3): a Tasks row whose task was unmarked since
                        // the page was fetched. Everything else about the
                        // navigation is unchanged, which is why the check sits
                        // here — this is the line that would open the paragraph.
                        false
                    } else {
                        // Gesture already recorded (pill / search / Back).
                        self.land_on(ws, node, true, match_text)
                    }
                }
                // **A document's own title is a hit with no node**, and that is
                // not the same thing as a workspace hit. The index stores a
                // title as a fragment with a null `node_id` — it comes from the
                // manifest's listing, not from a node in anybody's tree — so a
                // title match arrived here indistinguishable from a
                // workspace-level one and was answered "nothing more to do".
                // Searching a page by its name found it and refused to open it,
                // while the same page found by a word in its body opened.
                //
                // The landing is deferred like the body case, because there is
                // no node to aim at until the document is open: its title *is*
                // row 0 of its own body, so the row to flash is the root, and
                // the root is only knowable from the opened document.
                None => match document {
                    Some(document) => {
                        if self.workspace_document_is_active(Some(document)) {
                            // `active_document` says which Store room the
                            // workspace has loaded, not which tab is in front.
                            // A cold restore may deliberately put Welcome over
                            // that already-loaded room. Asking the active tab
                            // for its root then declines the first Explore
                            // click, and the shell misreports the valid document
                            // as unavailable. The loaded workspace is the
                            // authority for the root we are about to show.
                            let Some(root) = koru_workspace::document_root(ws) else {
                                return false;
                            };
                            self.land_on(ws, root, true, match_text)
                        } else {
                            self.defer_title_landing(document, match_text.map(str::to_owned));
                            let asked = self.request_store_document(document, false)
                                || self.land_on(ws, document, false, None);
                            if let Some(target) = self.deferred_search_target.as_mut() {
                                target.asked = asked;
                            }
                            true
                        }
                    }
                    // A workspace-level hit names neither: opening the workspace
                    // is the whole of what was asked for.
                    None => true,
                },
            }
        };
        if opened {
            self.retained_search = self.search.take();
            // **Show the result where it lives, not just in a tab.** A hit names
            // a document anywhere in the library and the branch above it is very
            // often shut, so the tree highlighted a row that was three collapsed
            // carets from anything on screen — which reads as the tree ignoring
            // the result. Binned hits are excluded: their rows are in the Trash
            // tab, and the library does not list them at all.
            //
            // `document` is already the row's key. `rebuild_from_entries` keys
            // every row `NodeId::from_uuid(entry.id.as_uuid())`, and that is what
            // the search surface puts in this field.
            if trash_root.is_none()
                && let Some(document) = document
            {
                if self.explore.reveal(document) {
                    // A fold changed, so the rows are wrong until they are
                    // walked again; the scroll waits for that walk.
                    self.explore_stale = true;
                }
            }
        }
        let caret = match &self.tabs().active_tab().view {
            TabView::Leaf(leaf) => leaf.caret_spot(),
            _ => None,
        };
        tracing::info!(
            target: "koru::navigation",
            document = %trace_navigation_node(document),
            node = %trace_navigation_node(node),
            opened,
            deferred = self.deferred_search_target.is_some(),
            caret_row = ?caret.map(|spot| spot.0),
            caret_offset = ?caret.map(|spot| spot.1),
            elapsed_ms = started.elapsed().as_millis(),
            "document view finished applying the routed target"
        );
        // **The only announcement an already-active document gets.** The shell
        // skips `show_active_document` when `workspace.active()` does not move,
        // so this is where a *Rename* or an *Attributes* row aimed at the
        // document already in front is completed.
        //
        // Not gated on `opened`, for the same reason `show_active_document`
        // does not gate it: the question is *which document is in front*, not
        // *did anything change* — and a landing this window could not take is
        // the one case where a pending request has to be dropped rather than
        // left for a later arrival, because with
        // [`SourceWindow`](koru_workspace_host::SearchTargetDisposition::SourceWindow)
        // no other window is going to be offered it.
        self.finish_explore_gesture(ws);
        if completing_history_route {
            self.history_pending_routed = false;
            self.restoring = false;
        }
        opened
    }

    fn open_file_search_target(&mut self, file: NodeId) -> bool {
        if self.search_only {
            return false;
        }
        if !self.sidebar.is_open() {
            self.toggle_sidebar();
        }
        self.explore.show_files();
        self.explore.reveal(file);
        self.explore.set_selected(Some(file));
        self.retained_search = self.search.take();
        true
    }

    /// Tell the pane which profile and workspaces it is showing.
    ///
    /// Pushed by the shell, which owns the profile document — a view holds no
    /// such thing. Idempotent, and it reports whether anything changed so an
    /// unchanged call costs no frame (ADR-26).
    fn show_active_document(
        &mut self,
        ws: &WorkspaceDoc,
        active: Option<koru_core::DocumentId>,
    ) -> bool {
        // **Which document `ws` now is, before anything asks.**
        // `open_node_with_centering` decides "is this already in a tab?" from
        // this field, and it used to arrive later, through `set_entries` — so
        // the first click on a document matched the tab of the *previous* one,
        // selected that, and opened nothing. The second click worked, because
        // by then the push had landed.
        self.active_document = active;
        // **Recents are recorded here**, the one place the shell tells the
        // container which document is active — store ids that survive a
        // workspace switch, not nodes minted in whichever document happens to
        // be under the caret.
        if active.is_some() {
            self.remember();
        }
        // The document's own root node — its title row, and what a tab is over.
        // A document with none is one the store made and nothing has seeded, so
        // there is nothing to show and no tab to open.
        let Some(root) = ws
            .children(koru_core::schema::Parent::Root)
            .ok()
            .and_then(|kids| {
                kids.into_iter()
                    .find(|&id| ws.kind_of(id) == Some(koru_core::Kind::Document))
            })
        else {
            self.history_pending = None;
            self.history_pending_routed = false;
            self.restoring = false;
            return false;
        };
        self.explore_stale = true;

        if let Some(spot) = self.history_pending {
            // A document may have moved since this history stop was recorded.
            // Once its stable id is active, the Store-resolved destination is
            // authoritative; the remembered profile/workspace is display
            // history, not a condition for completing the landing.
            let route_arrived = spot.document.is_some()
                || spot.profile.zip(spot.workspace).is_none_or(|route| {
                    self.pane_context
                        .as_ref()
                        .is_some_and(|(_, profile, _, workspace)| {
                            workspace.is_some_and(|workspace| route == (*profile, workspace))
                        })
                });
            let arrived = route_arrived
                && match spot.document {
                    Some(document) => active == Some(document),
                    None => active.is_some_and(|document| document.as_uuid() == spot.doc.as_uuid()),
                };
            if arrived {
                self.history_pending = None;
                let routed = self.history_pending_routed;
                // No history: Back is what asked for this landing.
                let opened = self.land_on(ws, root, false, None);
                if opened {
                    self.apply_spot_view(spot);
                }
                // A routed history request resumes in the shell after the
                // workspace switch. Keep suppression until that duplicate
                // `open_search_target` phase has run, or it records this arrival
                // as a new jump and drops Forward.
                if !routed {
                    self.restoring = false;
                }
                return opened;
            }
            // The workspace default (or another document) landed first.
            // Keep the pending spot — scroll and caret included — until the
            // document we asked for arrives. Dropping it here is how Back
            // across a workspace forgot where you were.
            return false;
        }

        // **A waiting draft becomes this document rather than sitting beside
        // it.** Its text is captured now — at the swap — so everything typed
        // during the round trip is kept.
        if self.promoting && self.tabs().active_tab().draft.is_some() {
            self.promoting = false;
            let captured = self.tabs().active_tab().draft.as_ref().and_then(|draft| {
                let d = &draft.workspace.ws;
                let droot = d
                    .children(koru_core::schema::Parent::Root)
                    .ok()?
                    .into_iter()
                    .find(|&n| d.kind_of(n) == Some(koru_core::Kind::Document))?;
                let title = d
                    .read(droot)
                    .ok()
                    .and_then(|n| n.content.as_text().map(str::to_owned))
                    .unwrap_or_default();
                let body: Vec<String> = d
                    .children(koru_core::schema::Parent::Node(droot))
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|n| d.read(n).ok())
                    .filter_map(|n| n.content.as_text().map(str::to_owned))
                    .collect();
                Some((title, body))
            });
            // **Where the caret is, not merely whether it is in the title.**
            // The row alone is not enough: the swap rebuilds every row, and a
            // rebuilt row's caret is 0.
            let caret = match &self.tabs().active_tab().view {
                TabView::Leaf(l) => l.caret_spot(),
                _ => None,
            };
            if let Some((title, body)) = captured {
                self.pending_draft_text = Some((root, title, body, caret));
                return true;
            }
        }
        // Shell announcement, not a user gesture — history was recorded when
        // the person asked to leave where they were (explore click, pill, Back).
        let opened = self.land_on(ws, root, false, None);
        if opened {
            self.apply_tab_pending(ws);
            // A drop dragged out of the explore column may have parked its
            // split or lane slot waiting for exactly this document.
            self.settle_pending_pane_drop();
        }
        // Self-guarding, so it is not gated on `opened`: see
        // [`finish_explore_gesture`](Self::finish_explore_gesture) for why the
        // question it asks is *which document is in front*, not *did anything
        // change*.
        self.finish_explore_gesture(ws);
        opened
    }

    fn set_store_trouble(&mut self, trouble: Option<koru_workspace::StoreTrouble>) -> bool {
        // **Compared on what is drawn, not on the whole value.** `since` is an
        // `Instant` that never stops moving, so comparing the struct would
        // report a change on every tick and redraw the window forever.
        let shown = |t: &Option<koru_workspace::StoreTrouble>| {
            t.as_ref().map(|t| (t.owed, t.fatal, t.unconfirmed.clone()))
        };
        let changed = shown(&self.store_trouble) != shown(&trouble);
        let offline = trouble.is_some();
        self.store_trouble = trouble;

        // **Every tab, every tick — outside the changed-check on purpose.** An
        // image's bytes go to the store, so with it unreachable `/image` and an
        // image paste have nowhere to put them; they go dim with a reason rather
        // than failing after the fact. Pushing this only on a *change* would
        // leave any tab opened since the last one believing images were fine,
        // and a tab is opened far more often than the store goes away. A few
        // bools a tick is not a cost worth being clever about.
        //
        // Background tabs too, because one is typed into the moment it is shown
        // and nothing pushes this again in between. A draft needs no arm of its
        // own: `Draft` holds the private *workspace* its leaf edits, and the
        // leaf itself is the tab's ordinary view.
        for tab in self.tabs_mut().iter_mut() {
            if let TabView::Leaf(l) = &mut tab.view {
                l.set_images_offline(offline);
            }
        }

        if !changed {
            return false;
        }
        // The tab strip is narrower or wider now, so the tabs have to be laid
        // out again against it.
        self.mark_tab_widths_stale();
        self.relayout();
        true
    }

    fn set_trashed(&mut self, trashed: Vec<koru_core::explore::ExploreEntry>) -> bool {
        // The count under the pinned Trash row, and the listing the Trash tab
        // draws. Kept here as well as pushed, because a Trash tab opened *later*
        // has to be given it and nothing pushes again in between.
        let changed = self.trashed_documents.as_ref() != Some(&trashed);
        // Rebuild from the current trash list — do not accumulate. A permanent
        // delete leaves the list without the id; leaving it in `entity_trashed`
        // kept the pill looking "in trash" and offered Restore against a
        // document the manifest no longer holds.
        let mut next_trashed = std::collections::HashSet::new();
        for entry in &trashed {
            let key = entry.id.to_storage();
            self.entity_titles.insert(key.clone(), entry.title.clone());
            next_trashed.insert(key);
        }
        self.entity_trashed = next_trashed;
        self.trashed_documents = Some(trashed.clone());
        self.recompose_deep_link_captions();
        let live = self.entity_live();
        let entity_titles = self.entity_titles.clone();
        let entity_trashed = self.entity_trashed.clone();
        let mut acted = false;
        for tab in self.tabs_mut().iter_mut() {
            if let TabView::Trash(t) = &mut tab.view {
                acted |= t.set_documents(trashed.clone());
            }
            if let TabView::Leaf(leaf) = &mut tab.view {
                acted |= leaf.set_entity_titles(&entity_titles, &entity_trashed, &live);
            }
        }
        if changed {
            self.sync_trash_count();
            acted = true;
        }
        acted
    }

    fn set_remote_files(&mut self, remote: Vec<koru_core::DocumentId>) -> bool {
        let states = remote
            .into_iter()
            .map(|id| {
                (
                    koru_core::NodeId::from_uuid(id.as_uuid()),
                    crate::panes::explore::FileState::Remote,
                )
            })
            .collect();
        self.explore.set_file_states(states)
    }

    fn set_file_fetching(&mut self, file: koru_core::DocumentId, fetching: bool) {
        let node = koru_core::NodeId::from_uuid(file.as_uuid());
        // A finished fetch falls back to `Remote` rather than `Present`: the
        // sweep that follows is what promotes it, and a failed fetch has to land
        // back on the placeholder it started from.
        let state = if fetching {
            crate::panes::explore::FileState::Fetching
        } else {
            crate::panes::explore::FileState::Remote
        };
        self.explore.set_file_state(node, state);
    }

    fn set_entries(
        &mut self,
        entries: Vec<koru_core::explore::ExploreEntry>,
        active: Option<koru_core::DocumentId>,
    ) -> bool {
        if self.entries.as_ref() == Some(&entries) && self.active_document == active {
            return false;
        }
        self.active_document = active;
        for entry in &entries {
            let key = entry.id.to_storage();
            self.entity_titles.insert(key.clone(), entry.title.clone());
            self.entity_trashed.remove(&key);
        }
        self.entries = Some(entries);
        self.adopt_canvas_kinds();
        self.recompose_deep_link_captions();
        // Which documents are notes decides a tab's glyph, and the glyph is
        // part of the tab's width — so a change here is a remeasure, not just
        // a repaint.
        let notes: std::collections::HashSet<koru_core::DocumentId> = self
            .entries
            .as_ref()
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| entry.kind == koru_core::explore::KIND_NOTE)
                    .map(|entry| entry.id)
                    .collect()
            })
            .unwrap_or_default();
        if notes != self.note_documents {
            self.note_documents = notes;
            self.mark_tab_widths_stale();
        }
        let live = self.entity_live();
        let entity_titles = self.entity_titles.clone();
        let entity_trashed = self.entity_trashed.clone();
        for tab in self.tabs_mut().iter_mut() {
            if let TabView::Leaf(leaf) = &mut tab.view {
                leaf.set_entity_titles(&entity_titles, &entity_trashed, &live);
            }
        }
        self.relabel_background_tabs();
        // **The parent chains now, the rows at the next draw.** A reveal — a
        // search result unfolding the branch it lives in — runs *before* that
        // rebuild, so a pane whose parent map is a rebuild behind is a pane
        // that unfolds nothing on the switch that needed it most. Pushing it
        // here costs one pass over a list already in hand.
        if let Some(entries) = &self.entries {
            self.explore.note_parents(entries);
        }
        // The tree is what changed, so the tree is what rebuilds — the flag the
        // draw path already consults, rather than a rebuild from here where
        // there is no `WorkspaceDoc` in hand.
        self.explore_stale = true;
        // Welcome may have been restored before the Store listing arrived.
        // The ids were already present, but could not be resolved to titles;
        // waiting for a tab switch to call this made Recent appear late.
        self.sync_welcome_lists();
        true
    }

    fn complete_new_page_link(
        &mut self,
        ws: &mut WorkspaceDoc,
        _profile: ProfileId,
        _workspace: WorkspaceId,
        document: koru_core::DocumentId,
        title: String,
    ) -> bool {
        let TabView::Leaf(leaf) = self.active_view() else {
            return false;
        };
        leaf.complete_new_page_link(
            ws,
            title,
            koru_editor::trigger::DocumentTarget {
                document,
                node: None,
            },
        )
    }

    fn complete_selection_move(
        &mut self,
        ws: &mut WorkspaceDoc,
        document: koru_core::DocumentId,
        node: Option<koru_core::NodeId>,
        title: String,
    ) -> bool {
        let TabView::Leaf(leaf) = self.active_view() else {
            return false;
        };
        let toast = title.clone();
        let done = leaf.complete_selection_move(
            ws,
            title,
            koru_editor::trigger::DocumentTarget { document, node },
        );
        if done {
            self.show_notice(format!("Moved to {toast}"));
        }
        done
    }

    fn cancel_selection_move(&mut self) -> bool {
        let TabView::Leaf(leaf) = self.active_view() else {
            return false;
        };
        leaf.cancel_selection_move()
    }

    fn set_pane_context(
        &mut self,
        profiles: Vec<ProfileSummary>,
        current_profile: ProfileId,
        workspaces: Vec<WorkspaceEntry>,
        current: Option<WorkspaceId>,
    ) -> bool {
        let next = (profiles, current_profile, workspaces, current);
        let changed = self.pane_context.as_ref() != Some(&next);
        if changed {
            // A profile switch in place invalidates the store-terms recents:
            // a recents row from the old profile would offer a destination
            // the shell must refuse (a move never crosses a profile).
            if self
                .pane_context
                .as_ref()
                .is_some_and(|(_, profile, _, _)| *profile != next.1)
            {
                self.recent_documents.clear();
                self.reminder_shell.reset_profile();
                self.reminder_creation.reset_profile();
            }
            self.pane_context = Some(next);
            self.reminder_preferences
                .select_profile(koru_store_core::ids::ProfileId::from_uuid(
                    current_profile.as_uuid(),
                ));
            // Who this window is, for anything it writes into a document. The
            // profile is where a person has already told us their name, and this
            // is the only place the container hears about it.
            self.actor = self
                .pane_context
                .as_ref()
                .and_then(|(profiles, id, _, _)| profiles.iter().find(|p| p.id == *id))
                .and_then(|p| koru_workspace::actor_label(Some(&p.name), Some(&p.email), None));
            // The explore column keeps its own copy, and `refresh_explore` only
            // hands one over when this says there is a new one to hand.
            self.context_stale = true;
            self.push_manage_profiles_context();
        }
        changed
    }

    /// Hand the measurements to whichever tabs are showing them.
    ///
    /// The shell pushes to the *pane*, which is this container — so a container
    /// that did not forward this would leave the manager showing a dash for
    /// everything, which is exactly what it did the first time.
    fn set_reminder_startup(&mut self, facts: koru_window::ReminderStartupFacts) -> bool {
        self.reminder_preferences
            .set_startup_facts(crate::reminder_settings::StartupFacts {
                start_on_login: facts.start_on_login,
                reminder_explained: facts.explained,
            })
    }

    fn reminder_startup_result(
        &mut self,
        result: koru_workspace_host::ReminderStartupResult,
    ) -> bool {
        self.reminder_preferences.finish_startup(result)
    }

    fn reminder_snooze(&mut self, intent: koru_workspace_host::ReminderSnoozeIntent) -> bool {
        tracing::info!(
            target: "koru::notification_activation",
            profile = %intent.profile.to_storage(),
            workspace = %intent.entity.workspace().to_storage(),
            document = %intent.entity.document().to_storage(),
            node = %intent.entity.node(),
            reminder = %intent.reminder.to_storage(),
            occurrence = %intent.occurrence.to_storage(),
            "Koru reminder controller began native Snooze management"
        );
        self.reminders.open_snooze(
            reminders::Target {
                profile: intent.profile,
                entity: intent.entity,
            },
            intent.reminder,
            intent.occurrence,
        );
        true
    }

    fn set_profile_workspace_counts(
        &mut self,
        counts: std::collections::HashMap<ProfileId, usize>,
    ) -> bool {
        self.profile_workspace_counts = counts.clone();
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.set_workspace_counts(counts.clone())),
            _ => false,
        })
    }
    fn set_profile_workspaces(
        &mut self,
        workspaces: std::collections::HashMap<ProfileId, Vec<koru_core::WorkspaceEntry>>,
    ) -> bool {
        self.explore.set_profile_workspaces(workspaces)
    }
    fn show_first_minutes(&mut self, owner: &str, profile: &str, workspace: &str) -> bool {
        MainView::show_first_minutes(self, owner, profile, workspace)
    }

    fn set_retained_profiles(&mut self, profiles: Vec<ProfileSummary>) -> bool {
        if self.retained_profiles == profiles {
            return false;
        }
        self.retained_profiles = profiles;
        self.push_manage_profiles_context();
        true
    }

    fn set_hidden_profile_count(&mut self, count: usize) -> bool {
        if self.hidden_profile_count == count {
            return false;
        }
        self.hidden_profile_count = count;
        self.push_manage_profiles_context();
        true
    }

    fn account_scoped_result(
        &mut self,
        owner_scope: u64,
        generation: u64,
        result: koru_agent_wire::AccountOperationResult,
    ) -> bool {
        // The first local Profile the reconciliation says was deleted, which
        // is the one the controller blocks on: its page carries the choice.
        let deleted_local = match &result {
            koru_agent_wire::AccountOperationResult::ProfileLifecycleReconciliation {
                profiles,
            } => self.pane_context.as_ref().and_then(|(local, _, _, _)| {
                profiles.iter().find_map(|lifecycle| {
                    if lifecycle.lifecycle_state
                        != koru_agent_wire::AccountProfileLifecycleState::Deleted
                    {
                        return None;
                    }
                    local
                        .iter()
                        .find(|profile| {
                            profile.id.to_storage() == lifecycle.profile_id
                                && profile
                                    .account
                                    .as_deref()
                                    .is_some_and(|account| account.starts_with("koru:"))
                        })
                        .map(|profile| profile.id)
                })
            }),
            _ => None,
        };
        let opened = owner_scope == self.account_scope
            && deleted_local
                .is_some_and(|profile| self.open_manage(crate::manage::Page::Profile(profile)));
        let changed = self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => {
                view.delegate(|c| c.complete_scoped(owner_scope, generation, result.clone()))
            }
            _ => false,
        });
        // Targeted results intentionally chain (Status -> Inspect -> Profiles
        // -> local CAS). Each link leaves through the ordinary PaneRequest
        // queue in this same event-loop turn, never through an idle poll.
        opened | changed | self.drain_manage_profiles_request()
    }

    fn account_command_failed(
        &mut self,
        failure: koru_workspace_host::account_ui::CommandFailure,
    ) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.account_command_failed(failure)),
            _ => false,
        })
    }

    fn account_command_admitted(
        &mut self,
        admission: koru_workspace_host::account_ui::CommandAdmission,
    ) -> bool {
        let changed = self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.account_command_admitted(admission)),
            _ => false,
        });
        changed | self.drain_manage_profiles_request()
    }

    fn connected_profile_connection_paused(
        &mut self,
        owner_scope: u64,
        profile: ProfileId,
        retry: koru_agent_wire::AccountCommand,
    ) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => {
                view.delegate(|c| c.connection_paused(owner_scope, profile, retry.clone()))
            }
            _ => false,
        })
    }

    fn connected_profile_ready(&mut self, owner_scope: u64, profile: ProfileId) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.connection_ready(owner_scope, profile)),
            _ => false,
        })
    }

    fn set_account_state(&mut self, state: koru_agent_wire::AccountState) -> bool {
        if self.account_state == state {
            return false;
        }
        self.account_state = state.clone();
        let explore_changed = self.explore.set_account_state(&state);
        let scope_changed = self.account_scope != state.active_scope;
        self.account_scope = state.active_scope;
        if scope_changed {
            self.profile_lifecycle_reconciled_generation = None;
        }
        let should_reconcile = state.stage == koru_agent_wire::AccountStage::SignedIn
            && self.profile_lifecycle_reconciled_generation.is_none();
        // A command result advances generation too. Reconciliation must not
        // enqueue another reconciliation in response to its own completion.
        if !matches!(
            state.stage,
            koru_agent_wire::AccountStage::SignedIn
                | koru_agent_wire::AccountStage::Refreshing
                | koru_agent_wire::AccountStage::Restoring
        ) {
            self.profile_lifecycle_reconciled_generation = None;
        } else if should_reconcile {
            self.profile_lifecycle_reconciled_generation = Some(state.generation);
        }
        let changed = self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.set_account(state.clone())),
            _ => false,
        });
        // Account state can enqueue a targeted session inspection. Promote it
        // immediately so the maintenance pass that delivered the state also
        // drains the request; no extra click or key press is required.
        let reconciled = should_reconcile
            && self.ask_pane(PaneRequest::AccountOwned {
                owner_scope: self.account_scope,
                command: koru_agent_wire::AccountCommand::ReconcileProfileLifecycles,
            });
        changed | explore_changed | reconciled | self.drain_manage_profiles_request()
    }

    fn account_result(
        &mut self,
        generation: u64,
        result: koru_agent_wire::AccountOperationResult,
    ) -> bool {
        self.account_scoped_result(self.account_scope, generation, result)
    }
    fn local_profile_account_bound(&mut self, profile: ProfileId) -> bool {
        let changed = self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.local_profile_account_bound(profile)),
            _ => false,
        });
        changed | self.drain_manage_profiles_request()
    }

    fn connected_profile_creation_failed(&mut self, message: &str) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => {
                view.delegate(|c| c.connected_profile_creation_failed(message))
            }
            _ => false,
        })
    }

    fn local_profile_restored(&mut self, profile: ProfileId) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.local_profile_restored(profile)),
            _ => false,
        })
    }

    fn local_profile_restore_failed(&mut self, message: &str) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.local_profile_restore_failed(message)),
            _ => false,
        })
    }

    fn local_profile_account_bind_failed(&mut self, profile: ProfileId, code: &str) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => {
                view.delegate(|c| c.local_profile_account_bind_failed(profile, code))
            }
            _ => false,
        })
    }

    fn local_profile_removed(&mut self, profile: ProfileId) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => view.delegate(|c| c.local_profile_removed(profile)),
            _ => false,
        })
    }

    fn local_profile_remove_failed(&mut self, profile: ProfileId, code: &str) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => {
                view.delegate(|c| c.local_profile_remove_failed_for(profile, code))
            }
            _ => false,
        })
    }

    fn deleted_profile_continued_offline(&mut self, profile: ProfileId) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => {
                view.delegate(|c| c.deleted_profile_continued_offline(profile))
            }
            _ => false,
        })
    }

    fn deleted_profile_continue_offline_failed(&mut self, profile: ProfileId) -> bool {
        self.each_tab_mut(|tab| match &mut tab.view {
            TabView::Manage(view) => {
                view.delegate(|c| c.deleted_profile_continue_offline_failed(profile))
            }
            _ => false,
        })
    }
    /// **Nothing shows these any more.**
    ///
    /// The manager's table carried per-workspace documents, files, storage,
    /// backups and index figures, and the shell pushed them here in a
    /// profile-wide batch. The shared surface draws none of them: a
    /// `WorkspaceStat` is `koru-workspace-host`'s type, and a surface Koru
    /// Terminal and Koru AI also host may not name it (SP-8b). The hook stays
    /// because the shell's side of it does; it forwards to nobody, and the
    /// container stops asking for a refresh.
    fn set_workspace_stats(&mut self, _stats: Vec<koru_workspace_host::WorkspaceStat>) -> bool {
        false
    }

    fn report_error(&mut self, error: koru_workspace_host::ErrorReport) -> bool {
        self.show_error(error)
    }

    fn show_notice(&mut self, message: String) -> bool {
        self.toast = Some((
            koru_ui::Toast::new(message, None, Instant::now()),
            ToastPayload::None,
        ));
        true
    }

    fn show_notice_open_another_profile(&mut self, message: String) -> bool {
        self.toast = Some((
            koru_ui::Toast::new(message, Some("Open another Profile"), Instant::now()),
            ToastPayload::OpenProfiles,
        ));
        true
    }

    fn show_notice_reveal(&mut self, message: String, path: PathBuf) -> bool {
        let action = koru_workspace_host::reveal::is_supported().then_some("Show");
        self.toast = Some((
            koru_ui::Toast::new(message, action, Instant::now()),
            if action.is_some() {
                ToastPayload::Reveal(path)
            } else {
                ToastPayload::None
            },
        ));
        true
    }

    /// **The consent step**, and the one place a download can be agreed to.
    ///
    /// A `Confirm` rather than a fourth copy of the panel recipe, and
    /// `Confirm::asking` rather than `Confirm::new` because downloading a model
    /// destroys nothing — what it keeps from that type is the half that
    /// matters here: **Enter is Cancel**, so a modal appearing under somebody's
    /// hands cannot be agreed to by a keystroke they had already begun.
    ///
    /// The sentence names the size, the licence and who to credit, and all
    /// three come off the *signed* descriptor by way of
    /// `koru_voice_consent::Terms` — so what is on the screen is exactly what
    /// is written into the record afterwards.
    fn show_voice_consent(
        &mut self,
        offer: koru_workspace_host::voice_install::VoiceOffer,
    ) -> bool {
        // A question already on the screen is not replaced by another: the one
        // that is up is the one somebody is reading.
        if let Some((_, subject)) = self.confirm.as_ref() {
            // **Name the dialog that is in the way.** "A question is already
            // up" was true and useless: the whole difficulty of a stuck
            // confirm is that nothing on screen says which one it is.
            tracing::info!(
                target: "koru::voice",
                subject = ?subject,
                "a voice consent step was refused by a question already up"
            );
            return false;
        }
        let lang = crate::format::lang();
        let megabytes = offer.megabytes().to_string();
        let licences = offer.licences();
        let attributions = offer.attributions();
        self.confirm = Some((
            koru_ui::Confirm::asking(
                koru_i18n::t(lang, koru_i18n::Key::VoiceConsentTitle),
                koru_i18n::fill(
                    koru_i18n::t(lang, koru_i18n::Key::VoiceConsentBody),
                    &[&megabytes, &licences, &attributions],
                ),
                koru_i18n::t(lang, koru_i18n::Key::VoiceConsentAccept),
                koru_i18n::t(lang, koru_i18n::Key::VoiceConsentCancel),
            ),
            ConfirmSubject::VoiceConsent,
        ));
        // **Success was silent, and that hid the shape of the failure.** Two
        // offers arriving a millisecond apart look identical in the log to one
        // offer meeting a dialog somebody left open, and only the first is a
        // bug in this code.
        tracing::info!(
            target: "koru::voice",
            "a voice consent step is now on screen"
        );
        true
    }

    fn show_voice_download(&mut self, message: String, fraction: Option<f32>) -> bool {
        self.toast = Some((
            koru_ui::Toast::new(
                message,
                Some(koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::VoiceStop,
                )),
                Instant::now(),
            )
            .with_progress(voice_progress(fraction)),
            ToastPayload::VoiceDownload,
        ));
        true
    }

    fn update_voice_download(&mut self, message: String, fraction: Option<f32>) -> bool {
        if let Some((toast, ToastPayload::VoiceDownload)) = self.toast.as_mut() {
            toast.set_progress(message, voice_progress(fraction), Instant::now());
        }
        // Consumed even when the toast is gone: this container owns the
        // download's feedback, and recreating a surface somebody closed would
        // make the × mean nothing.
        true
    }

    fn cancel_voice_download(&mut self, message: String) -> bool {
        if let Some((toast, ToastPayload::VoiceDownload)) = self.toast.as_mut() {
            toast.set_progress(
                message,
                koru_ui::ProgressMode::Indeterminate,
                Instant::now(),
            );
            // **The action goes immediately**, which is DESIGN's contract: an
            // accepted Stop cannot be asked for twice.
            toast.set_action(None);
        }
        true
    }

    fn clear_voice_download(&mut self) -> bool {
        if matches!(
            self.toast.as_ref().map(|(_, payload)| payload),
            Some(ToastPayload::VoiceDownload)
        ) {
            self.toast = None;
            return true;
        }
        false
    }

    fn show_import_progress(
        &mut self,
        message: String,
        progress: koru_workspace_host::ImportNoticeProgress,
    ) -> bool {
        let progress = match progress {
            koru_workspace_host::ImportNoticeProgress::Indeterminate => {
                koru_ui::ProgressMode::Indeterminate
            }
            koru_workspace_host::ImportNoticeProgress::Determinate(fraction) => {
                koru_ui::ProgressMode::Determinate(fraction)
            }
        };
        self.toast = Some((
            koru_ui::Toast::new(message, Some("Stop"), Instant::now()).with_progress(progress),
            ToastPayload::ImportProgress,
        ));
        true
    }

    fn cancel_import_progress(&mut self, message: String) -> bool {
        if let Some((toast, ToastPayload::ImportProgress)) = self.toast.as_mut() {
            toast.set_progress(
                message,
                koru_ui::ProgressMode::Indeterminate,
                Instant::now(),
            );
            toast.set_action(None);
        }
        true
    }

    fn update_import_progress(
        &mut self,
        message: String,
        progress: koru_workspace_host::ImportNoticeProgress,
    ) -> bool {
        let progress = match progress {
            koru_workspace_host::ImportNoticeProgress::Indeterminate => {
                koru_ui::ProgressMode::Indeterminate
            }
            koru_workspace_host::ImportNoticeProgress::Determinate(fraction) => {
                koru_ui::ProgressMode::Determinate(fraction)
            }
        };
        if let Some((toast, ToastPayload::ImportProgress)) = self.toast.as_mut() {
            toast.set_progress(message, progress, Instant::now());
        }
        // This container owns import feedback even when the person dismissed
        // it. Consuming an update without recreating the toast is intentional.
        true
    }

    fn clear_import_progress(&mut self) -> bool {
        if matches!(
            self.toast.as_ref().map(|(_, payload)| payload),
            Some(ToastPayload::ImportProgress)
        ) {
            self.toast = None;
            return true;
        }
        false
    }

    fn show_import_notice(
        &mut self,
        message: String,
        target: koru_workspace_host::ImportNoticeTarget,
    ) -> bool {
        let (action, payload) = match target {
            koru_workspace_host::ImportNoticeTarget::Notebook(document) => {
                ("View Notebook", ToastPayload::Open(document))
            }
            koru_workspace_host::ImportNoticeTarget::Document(document) => {
                ("View file", ToastPayload::Open(document))
            }
            koru_workspace_host::ImportNoticeTarget::File(file) => {
                ("View file", ToastPayload::ShowFile(file))
            }
        };
        self.toast = Some((
            koru_ui::Toast::new(message, Some(action), Instant::now()),
            payload,
        ));
        true
    }

    fn show_import_preview(&mut self, preview: koru_workspace_host::ImportPreview) -> bool {
        self.import_dialog = Some(ImportDialog::new(preview));
        true
    }

    /// A document tab asked to pop out into its own window. Drained by the
    /// shell, which is the only thing that can open one.
    fn take_wants_popout(&mut self) -> Option<NodeId> {
        self.wants_popout.take()
    }

    /// A session the Agents navigator asked to move to a new window (Q-E7 P8).
    /// Drained by the shell, which duplicates a container and seats it there.
    fn take_wants_session_popout(&mut self) -> Option<koru_session_protocol::SessionId> {
        self.wants_session_popout.take()
    }

    /// Seat `session` as a tab in this (freshly duplicated) container — the
    /// receiving end of another window's *Move to new window*, and of the
    /// notification area's Agents rows. Attaches by id, never launches, the
    /// same open-or-focus the navigator's click takes.
    fn open_session_tab(&mut self, session: koru_session_protocol::SessionId) -> bool {
        self.open_session_tab_for_existing(session)
    }

    /// Whether a tab here is bound to `session` — the shell's routing fact
    /// for a tray Agents click: the pane already showing it is the one asked.
    fn shows_session(&self, session: koru_session_protocol::SessionId) -> bool {
        self.session_tab_for(session).is_some()
    }

    /// The tray's Agents ▸ New: the same `+` menu the strip's button opens,
    /// where the agent profiles live — which one to start stays the person's
    /// choice.
    fn open_new_session_chooser(&mut self) -> bool {
        self.open_new_tab_menu()
    }

    /// Open `doc` as a tab — the receiving end of a popped-out window's "restore
    /// as a tab". The same path a document pill takes, so a restored tab lands
    /// exactly where opening it fresh would.
    fn restore_tab(&mut self, ws: &WorkspaceDoc, doc: NodeId) {
        self.open_node(ws, doc);
    }

    /// This is the tab container — the thing a popped-out window restores into.
    fn is_container(&self) -> bool {
        true
    }

    fn take_wants_sharing_toggle(&mut self) -> bool {
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_sharing_toggle(l),
            _ => false,
        }
    }

    fn take_wants_image(&mut self) -> Option<NodeId> {
        if let Some(root) = self.canvas_wants_image() {
            return Some(root);
        }
        let active = self.tabs().active();
        match self.active_view() {
            TabView::Leaf(l) => {
                let want = View::take_wants_image(l);
                if want.is_some() {
                    // Remembered so the file the shell comes back with lands in
                    // the tab that asked, not wherever the focus went while the
                    // picker was open — it is a blocking dialog.
                    self.image_request = Some(active);
                }
                want
            }
            _ => None,
        }
    }

    fn take_wants_attachment(&mut self) -> Option<NodeId> {
        let active = self.tabs().active();
        match self.active_view() {
            TabView::Leaf(l) => {
                let want = View::take_wants_attachment(l);
                if want.is_some() {
                    koru_workspace_host::attach_debug!(
                        "mainview take_wants_attachment tab={active} node={want:?}"
                    );
                    self.attachment_request = Some(active);
                }
                want
            }
            _ => None,
        }
    }

    fn take_wants_create_tag(&mut self) -> Option<(Vec<NodeId>, String)> {
        // Only the active tab can have asked — the chooser is modal to it.
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_create_tag(l),
            _ => None,
        }
    }

    fn take_wants_join_new_list(&mut self) -> Option<(Vec<NodeId>, String, Option<String>)> {
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_join_new_list(l),
            _ => None,
        }
    }

    fn take_wants_workspace_word(&mut self) -> Option<(String, String)> {
        // Likewise: the context menu that raised it was the active tab's.
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_workspace_word(l),
            _ => None,
        }
    }

    /// **No tab is remembered**, unlike the image and attachment requests
    /// above. Those cross a blocking file dialog, so the tab that asked has to
    /// be recorded before the focus can move; a dictation is begun in the same
    /// pass and routed by [`View::dictate`], which asks where a typed character
    /// would go *now*.
    fn take_wants_dictation(&mut self) -> bool {
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_dictation(l),
            _ => false,
        }
    }

    fn set_tag_table(&mut self, tags: &[koru_core::system::TagEntry]) -> bool {
        // Kept for tabs opened later, and forwarded to every leaf on every
        // push — each leaf compares against its own copy, so an unchanged
        // push costs a Vec compare per tab and no work. Forwarding only on a
        // container-level change was the tempting shape, and it starves a tab
        // opened after the last change.
        self.tag_table = tags.to_vec();
        let mut changed = false;
        for tab in self.tabs_mut().iter_mut() {
            if let TabView::Leaf(l) = &mut tab.view {
                changed |= View::set_tag_table(l, tags);
            }
        }
        changed
    }

    /// The list table, pushed into the rail **and** into the leaves.
    ///
    /// It used to stop at the rail, on the reasoning that a list is only ever
    /// drawn there while a tag resolves inside a document. The due chip is what
    /// changed that: a task in a list wears its list's colour, so a document
    /// now has to resolve a `ListId` too, and the leaves are forwarded to on
    /// every push for the reason [`set_tag_table`](Self::set_tag_table) gives
    /// — a tab opened since the last change still has to be told, and a leaf
    /// that already has the table answers the compare and no work.
    fn set_list_table(&mut self, lists: &[koru_core::system::ListEntry]) -> bool {
        let moved = self.list_table != lists;
        if moved {
            self.list_table = lists.to_vec();
        }
        let mut changed = false;
        for tab in self.tabs_mut().iter_mut() {
            if let TabView::Leaf(l) = &mut tab.view {
                changed |= View::set_list_table(l, lists);
            }
        }
        // The Tasks tab needs the table too — a membership chip draws the
        // list's *name* — but it is given it in `refresh_tasks_context`, which
        // runs whether or not the table moved. A tab opened since the last
        // change would never be told from here.
        if moved {
            changed |= self.refresh_rail_lists();
        }
        changed
    }

    /// The rail's "+ New list", on its way to the workspace's system document.
    /// Taken rather than read, the tag request's contract exactly.
    fn take_wants_create_list(&mut self) -> Option<(String, Option<String>)> {
        self.wants_create_list.take()
    }

    /// The rail's renames, recolours and deletions, on the same journey. Taken
    /// rather than read, and drained whole: two of them can have happened
    /// between two turns.
    fn take_wants_list_edits(&mut self) -> Vec<koru_workspace_host::tasks::ListEdit> {
        std::mem::take(&mut self.list_edits)
    }

    /// The Tasks tab's ticks, removals and captures, on their way to the
    /// workspace. The list request's contract exactly, one layer along.
    fn take_wants_run_pills(&mut self) -> Vec<koru_workspace_host::runpill::RunPill> {
        std::mem::take(&mut self.pending_run_pills)
    }

    fn take_wants_schedule_pills(
        &mut self,
    ) -> Vec<koru_workspace_host::schedulepill::SchedulePill> {
        std::mem::take(&mut self.pending_schedule_pills)
    }

    fn take_wants_task_commands(&mut self) -> Vec<koru_workspace_host::tasks::TaskCommand> {
        MainView::take_wants_task_commands(self)
    }

    fn task_command_failed(
        &mut self,
        command: &koru_workspace_host::tasks::TaskCommand,
        error: &str,
    ) -> bool {
        MainView::task_command_failed(self, command, error)
    }

    fn task_command_landed(&mut self, landed: &koru_workspace_host::tasks::Landed) -> bool {
        MainView::task_command_landed(self, landed)
    }

    /// Forwarded to every leaf on every push, exactly as the tag table is — a
    /// tab opened after the last workspace switch has to be told too, or a
    /// copy made in it would say it came from nowhere.
    fn set_workspace(&mut self, workspace: Option<koru_core::WorkspaceId>) -> bool {
        let mut changed = false;
        for (_, group) in self.panes.iter_groups_mut() {
            for tab in group.tabs_mut().iter_mut() {
                if let TabView::Leaf(l) = &mut tab.view {
                    changed |= View::set_workspace(l, workspace);
                }
            }
        }
        changed
    }

    fn insert_attachment(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        files: &[(koru_core::DocumentId, String)],
    ) {
        let i = self
            .attachment_request
            .take()
            .unwrap_or(self.tabs().active());
        koru_workspace_host::attach_debug!(
            "mainview insert_attachment tab={i} node={node:?} n={}",
            files.len()
        );
        if let Some(TabView::Leaf(l)) = self.tabs_mut().get_mut(i).map(|tab| &mut tab.view) {
            View::insert_attachment(l, ws, node, files);
        }
    }

    fn attachment_cancelled(&mut self, ws: &mut WorkspaceDoc, node: NodeId) {
        let i = self
            .attachment_request
            .take()
            .unwrap_or(self.tabs().active());
        koru_workspace_host::attach_debug!("mainview attachment_cancelled tab={i} node={node:?}");
        if let Some(TabView::Leaf(l)) = self.tabs_mut().get_mut(i).map(|tab| &mut tab.view) {
            View::attachment_cancelled(l, ws, node);
        }
    }

    fn take_wants_save_diagram_as(
        &mut self,
        ws: &WorkspaceDoc,
    ) -> Option<koru_workspace_host::SaveDiagram> {
        let request = match self.active_view() {
            TabView::Leaf(l) => View::take_wants_save_diagram_as(l, ws),
            _ => None,
        }?;
        // The route a file's Save takes: a pane request the shell serves
        // against this window, whose device rasterises the PNG. Converted
        // here and `None` returned, so the shell's own drain finds nothing.
        self.ask_pane(PaneRequest::SaveDiagram(request));
        None
    }

    fn take_wants_file_action(
        &mut self,
    ) -> Option<(koru_core::DocumentId, koru_workspace_host::FileAction)> {
        let action = match self.active_view() {
            TabView::Leaf(l) => View::take_wants_file_action(l),
            _ => None,
        };
        // Handle every action here and return None so the shell's service_images
        // path does not also try. Open/Save use the same PaneRequest path as the
        // Explore Files menu (which already works); Reveal stays local.
        match action {
            Some((file, koru_workspace_host::FileAction::Open)) => {
                koru_workspace_host::attach_debug!(
                    "mainview Open → PaneRequest::OpenFile file={}",
                    file.to_storage()
                );
                self.ask_pane(PaneRequest::OpenFile(file));
                None
            }
            Some((file, koru_workspace_host::FileAction::Save)) => {
                koru_workspace_host::attach_debug!(
                    "mainview Save → PaneRequest::SaveFile file={}",
                    file.to_storage()
                );
                self.ask_pane(PaneRequest::SaveFile(file));
                None
            }
            Some((file, koru_workspace_host::FileAction::Reveal)) => {
                koru_workspace_host::attach_debug!(
                    "mainview Reveal → explore Files file={}",
                    file.to_storage()
                );
                self.reveal_file_in_explorer(file);
                None
            }
            Some((file, koru_workspace_host::FileAction::Restore)) => {
                koru_workspace_host::attach_debug!(
                    "mainview Restore → PaneRequest::RestoreDocuments file={}",
                    file.to_storage()
                );
                self.ask_pane(PaneRequest::RestoreDocuments(vec![file]));
                None
            }
            None => None,
        }
    }

    fn take_wants_copy_image(&mut self) -> Option<String> {
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_copy_image(l),
            _ => None,
        }
    }

    fn take_wants_copy_html(&mut self) -> Option<(String, String)> {
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_copy_html(l),
            _ => None,
        }
    }

    fn apply_pending_image_cut(&mut self, ws: &mut WorkspaceDoc) -> bool {
        match self.active_view() {
            TabView::Leaf(l) => View::apply_pending_image_cut(l, ws),
            _ => false,
        }
    }

    fn take_wants_paste_image(&mut self) -> Option<koru_workspace_host::PasteTarget> {
        if let Some(target) = self.canvas_wants_paste() {
            return Some(target);
        }
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_paste_image(l),
            _ => None,
        }
    }

    fn take_wants_paste_rich(&mut self) -> Option<(koru_workspace_host::PasteTarget, String)> {
        match self.active_view() {
            TabView::Leaf(l) => View::take_wants_paste_rich(l),
            _ => None,
        }
    }

    fn insert_image(
        &mut self,
        ws: &mut WorkspaceDoc,
        node: NodeId,
        hash: &str,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        if self.canvas_insert_image(ws, node, (hash, mime), width, height) {
            return;
        }
        let i = self.image_request.take().unwrap_or(self.tabs().active());
        if let Some(TabView::Leaf(l)) = self.tabs_mut().get_mut(i).map(|t| &mut t.view) {
            View::insert_image(l, ws, node, hash, mime, width, height);
        }
    }

    fn append_image(
        &mut self,
        ws: &mut WorkspaceDoc,
        hash: &str,
        mime: &str,
        width: Option<u32>,
        height: Option<u32>,
    ) {
        if self.canvas_append_image(ws, (hash, mime), width, height) {
            return;
        }
        if let TabView::Leaf(l) = self.active_view() {
            View::append_image(l, ws, hash, mime, width, height);
        }
    }

    fn paste_images(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<koru_workspace_host::PasteTarget>,
        images: &[koru_workspace_host::PastedImage],
    ) {
        if self.canvas_insert_images(ws, images) {
            return;
        }
        if let TabView::Leaf(l) = self.active_view() {
            View::paste_images(l, ws, target, images);
            // New image children just landed under the active document — the
            // explore column's cache of the library is stale. The per-frame
            // `adopt` would catch it next frame; this makes it immediate.
            self.explore_stale = true;
        }
    }

    fn paste_text(&mut self, ws: &mut WorkspaceDoc, text: &str) {
        if let TabView::Leaf(l) = self.active_view() {
            View::paste_text(l, ws, text);
        }
    }

    fn paste_rich(
        &mut self,
        ws: &mut WorkspaceDoc,
        target: Option<koru_workspace_host::PasteTarget>,
        blocks: &[koru_workspace_host::RichBlock],
    ) {
        if let TabView::Leaf(l) = self.active_view() {
            View::paste_rich(l, ws, target, blocks);
            // Like `paste_images`: image blocks landed under the active document,
            // so the explore column's library cache is stale.
            self.explore_stale = true;
        }
    }

    fn open_move_to(&mut self) -> bool {
        self.open_move_to_here()
    }

    fn set_move_to_workspace_entries(
        &mut self,
        workspace: WorkspaceId,
        entries: Vec<koru_core::explore::ExploreEntry>,
    ) -> bool {
        let Some(surface) = self.move_to.as_mut() else {
            return false;
        };
        surface.set_workspace_entries(workspace, entries);
        true
    }

    fn transfer_failed(&mut self) -> bool {
        let had = self.move_undo.is_some() || self.toast.is_some();
        self.move_undo = None;
        self.toast = None;
        had
    }

    fn cycle_tab(&mut self, delta: i32) -> bool {
        if !self.tabs_mut().cycle(delta) {
            return false;
        }
        self.ensure_tab_visible(self.tabs().active());
        self.size_visible_groups();
        self.wake_workspaces_tab();
        true
    }

    fn restore(&mut self, ws: &Workspace, settings: &Settings) {
        koru_editor::emoji::restore_usage(&settings.emoji_recents);
        koru_editor::emoji::set_preferred_skin_tone(koru_editor::emoji::parse_skin_tone(
            &settings.emoji_skin_tone,
        ));
        self.preferences = settings.clone();
        // A token, so a third mode is a name rather than a second flag. One
        // this build does not know reopens on Documents.
        match crate::panes::explore::NavigatorMode::parse(&settings.navigator_mode) {
            Some(crate::panes::explore::NavigatorMode::Files) => self.explore.show_files(),
            Some(crate::panes::explore::NavigatorMode::Tasks) => self.explore.show_tasks(),
            Some(crate::panes::explore::NavigatorMode::Artifacts) => self.explore.show_artifacts(),
            Some(crate::panes::explore::NavigatorMode::Chats) => self.explore.show_chats(),
            Some(crate::panes::explore::NavigatorMode::Canvases) => self.explore.show_canvases(),
            Some(crate::panes::explore::NavigatorMode::Socials) => self.explore.show_socials(),
            _ => self.explore.show_documents(),
        }
        self.restore_session(ws, settings);
        if let Some((document, node, match_text)) = self.deferred_navigation.take() {
            self.pane_requests.push_back(PaneRequest::Navigate {
                target: koru_workspace_host::NavigationTarget::Document { document, node },
                match_text,
                disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
            });
        }
        // After the session tabs, not before: a cold `--open`, pinned jump-list,
        // or global-search target must win over the last active session tab.
        if let Some(target) = self.deferred_search_target.take() {
            View::open_search_target(
                self,
                &ws.ws,
                target.document,
                target.node,
                target.trash_root,
                target.match_text.as_deref(),
            );
        }
        self.wake_workspaces_tab();
    }

    fn on_workspace_switched(&mut self, ws: &Workspace, settings: &Settings) {
        // Explore width / collapsed are per workspace; the strip is not.
        if let Some(w) = settings.workspace.and_then(|id| settings.sidebar_w_in(id)) {
            self.sidebar
                .set_full(w.clamp(layout::SIDEBAR_MIN_W, layout::SIDEBAR_MAX_W));
        }
        let collapsed = settings
            .workspace
            .map(|id| settings.collapsed_in(id).to_vec())
            .unwrap_or_default();
        self.explore.set_collapsed(collapsed);
        self.explore_stale = true;
        self.entries = None;
        // Reseat each open leaf against its document room when it is still
        // parked; the active tab is reseated by the shell before this call.
        for t in self.tabs_mut().iter_mut() {
            if t.draft.is_some() || t.whats_new.is_some() {
                continue;
            }
            if let Some(doc_id) = t.document
                && let Some(doc) = ws.document(doc_id)
            {
                t.view.reseat(doc);
                continue;
            }
            t.view.reseat(&ws.ws);
        }
        self.relayout();
        self.wake_workspaces_tab();
    }

    fn capture(&self, settings: &mut Settings) {
        settings.emoji_recents = koru_editor::emoji::recent_usage();
        settings.emoji_skin_tone =
            koru_editor::emoji::skin_tone_token(koru_editor::emoji::preferred_skin_tone())
                .to_owned();
        settings.navigator_mode = self.explore.navigator_mode().token().to_owned();
        self.capture_session(settings);
    }

    fn maintenance_deadline(&self) -> Option<Instant> {
        [
            self.support.as_ref().and_then(|s| s.maintenance_deadline()),
            self.tooltip.maintenance_deadline(),
            self.entity_search
                .pending()
                .then(|| Instant::now() + std::time::Duration::from_millis(16)),
            self.search
                .as_ref()
                .and_then(koru_workspace_host::View::maintenance_deadline),
            self.tabs().active_tab().view.maintenance_deadline(),
            self.socials_navigation_deadline(),
            self.document_history.deadline(),
            // Only while the Cloud tab is the section showing — a hidden one
            // has no clock, which is the same bargain History keeps.
            self.cloud_sync.deadline(),
            self.task_counts.deadline(),
            self.welcome_progress.deadline(),
            // Only the tab in front has one: a Tasks tab in the background
            // contributes no clock and therefore no wake.
            match &self.tabs().active_tab().view {
                TabView::Tasks(t) => t.panel().deadline(),
                _ => None,
            },
            self.reminders.deadline(),
            self.reminder_shell.maintenance_deadline(),
            if self.ai_visible() {
                self.ai_chat.maintenance_deadline()
            } else {
                self.ai_chat.listing_deadline()
            },
            self.link_device
                .as_ref()
                .and_then(LinkDevice::maintenance_deadline),
            self.workspaces_poll_due,
            self.run_pill_deadline(),
            // **Only while an answer is owed** to a Chats view that is
            // showing: an idle list is never a reason to wake.
            (self.explore.showing_chats() && self.explore.chats_awaiting())
                .then(|| Instant::now() + std::time::Duration::from_millis(50)),
            // **Not `tabs().active_tab()`, deliberately.** A feedback report is
            // sent by a worker that outlives the tab that wrote it, so the
            // window keeps ticking for it whatever is in front — otherwise a
            // failure would wait for the next click to be noticed, which is
            // exactly what the old in-view receiver did.
            crate::feedback::outbox::deadline(),
            // **The meter's 10 Hz tick, and `None` whenever nothing is being
            // said.** Deliberately here and not in `animating`: that one is the
            // shell's 8 ms wake, and a level meter does not need 125 frames a
            // second — see `mainview::dictation`.
            self.dictation_deadline(),
            // **The countdown's four wakes, and `None` everywhere else** — an
            // idle window is never woken by the recorder.
            self.recording_deadline(),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn maintain(&mut self, ws: &mut WorkspaceDoc, now: Instant) -> bool {
        if self.support.is_some() || self.fab.is_some() {
            // Serviced by the background pass, which runs in both loop paths.
            return false;
        }
        self.reminder_environment = reminder_environment();
        poll_terminal_prefs();
        // **Where a delegated run's own folder goes** (ADR-365). Polled here
        // rather than read when the modal opens, because opening the modal may
        // touch no disk and this is the only thread that could.
        let delegate_folder = self.maintain_delegate_folder();
        let delegation_requested = self.drain_delegate_request();
        let delegation_answered = self.maintain_delegation();
        self.refresh_device_names();
        let welcome_ticks = self.welcome_progress.maintain(now);
        if welcome_ticks {
            self.sync_welcome_lists();
        }
        let run_pills = self.maintain_run_pills(now)
            | self.maintain_canvas_runs()
            | self.maintain_canvas(ws)
            | self.drain_run_pill_open()
            | self.drain_schedule_pill_open();
        let reminder_requested = self.drain_reminder_request();
        let reminder_management_requested = self.drain_reminder_management_request(ws);
        let reminder_context_requested = self.drain_reminder_context_request(ws);
        let reminder_profile = self.ensure_reminder_profile();
        let reminder_global_handoff = self.drain_global_reminder_handoff(ws);
        let reminder_inspector_labels = self.context_view.set_reminder_label(
            koru_i18n::t(crate::format::lang(), koru_i18n::Key::Reminder).to_owned(),
        );
        let reminder_context_lost = self
            .reminders
            .creation_owner()
            .is_some_and(|(target, identity)| !self.reminder_target_is_active(&target, identity))
            && self.reminders.cancel_creation();
        let reminder_surface_context_lost = self.reminder_creation.is_open()
            && self.reminders.creation_owner().is_none()
            && self
                .reminders
                .current_target()
                .zip(self.reminder_creation.node())
                .is_none_or(|(target, node)| !self.reminder_target_node_is_active(target, node));
        if reminder_surface_context_lost {
            self.reminders.close_management();
            self.reminder_creation.close_context();
        }
        let reminder_due_changed =
            self.reminders
                .creation_owner()
                .is_some_and(|(target, identity)| {
                    if !self.reminder_target_is_active(&target, identity) {
                        return false;
                    }
                    let due = koru_editor::reminders::due_anchor(ws, identity.node);
                    self.reminders.refresh_due_anchor(&target, identity, due)
                });
        let reminder_changed = self.reminders.maintain();
        if self
            .reminders
            .take_settings_change(self.reminder_preferences.model.profile())
        {
            self.reminder_preferences.refresh_selected_settings();
        }
        let reminder_surface_changed = self.sync_reminder_creation_surface(ws);
        let reminder_presence = self.reminders.has_reminders();
        let reminder_presence_changed =
            reminder_presence.is_some_and(|any| self.observe_reminders(any));
        while let Some(resolution) = self.reminders.take_submitted_request() {
            self.pending_reminder_submissions.push_back(resolution);
        }
        // Submission is the document-mutating maintenance event. The exact
        // command text leaves immediately; Store completion updates only the
        // temporary pill carrying this request token.
        let reminder_submitted = self.apply_pending_reminder_submissions(ws);
        while let Some(resolution) = self.reminders.take_cancelled_request() {
            self.pending_reminder_cancels.push_back(resolution);
        }
        let reminder_cancelled = self.apply_pending_reminder_cancels(ws);
        // **The Manage tab's Workspaces tick.** Take what its controller's
        // link answered, and re-arm while Manage is still the tab in front.
        // Dropped the moment it is not: a window looking at a document does no
        // work for a list nobody can see. The 120 ms pass is also the
        // post-activation settle: a just-finished Store mutation reaches its
        // indexed usage row within a poll or two, without a second mechanism.
        let mut refresh_stats = false;
        if self.workspaces_poll_due.is_some_and(|due| now >= due) {
            self.workspaces_poll_due = None;
            if matches!(self.tabs().active_tab().view, TabView::Manage(_)) {
                refresh_stats |= self.poll_manage_workspaces();
                self.workspaces_poll_due = Some(now + WORKSPACES_POLL);
            }
        }
        // A deep-link caption follows a heading nobody here is editing, and
        // nothing announces that. Two seconds is the same tick the pane context
        // already runs on, and the work is one indexed query per linked
        // document -- which is a handful, not a workspace.
        if self
            .deep_links_polled
            .is_none_or(|last| now.duration_since(last) >= DEEP_LINK_POLL)
        {
            self.deep_links_polled = Some(now);
            self.refresh_deep_link_outlines();
        }
        let feedback_failed = self.drain_feedback_outbox(now);
        let telemetry_changed = self.maintain_telemetry_consent();
        let tooltip_changed = self.tooltip.maintain(now);
        let link_device_changed = self
            .link_device
            .as_mut()
            .is_some_and(|dialog| dialog.maintain(now));
        let search_changed = self
            .search
            .as_mut()
            .is_some_and(|search| search.maintain(ws, now));
        // Move to… and the @ entity picker share EntitySearch; only one is
        // open at a time, and both must not drain the same poll.
        let entity_changed = if self.move_to.is_some() {
            self.poll_move_to_search()
        } else {
            // Both, and in this order: an outline is a separate answer on the
            // same reader, and the picker's query answer must not swallow the
            // maintenance pass that would have delivered it.
            let headings = self.poll_entity_headings();
            self.poll_entity_picker() || headings
        };
        let history_target = self.history_target();
        let history_visible =
            self.context.is_open() && self.context_view.section() == Section::History;
        let history_changed = self
            .document_history
            .maintain(history_target, history_visible, now);
        if history_changed {
            self.context_view
                .set_history(self.document_history.rows(), self.document_history.status());
        }
        // **The Cloud tab, on the same terms as History**: one target, visible
        // only while its own section is showing, and a push into chrome only
        // when the lines actually moved.
        let cloud_visible =
            self.context.is_open() && self.context_view.section() == Section::CloudSync;
        let cloud_context = self.cloud_context(cloud_visible);
        let cloud_changed =
            self.cloud_sync
                .maintain(history_target, cloud_visible, &cloud_context, now);
        if cloud_changed {
            // The sweep is owed again whenever the lines moved: that is the
            // tick the poll landed on, and it is cheaper than a second clock.
            self.cloud_images_due |= cloud_visible;
            self.push_cloud_lines();
        }
        // **Midnight, once, before either panel is asked anything.** One clock
        // read on the tick this window already runs, compared against the day
        // the watch last saw; on a move, both surfaces are invalidated through
        // the door a mutation already uses, so the next refresh recomputes
        // `windows_now()` and every bucket edge with it. A tab in the background
        // is invalidated by the visibility change when it comes forward, which
        // is what `TaskListPanel::maintain` has always done.
        let rolled = self.day_rolled(chrono::Local::now().date_naive());
        // The rail's numbers. Visible means *the tasks mode is on screen*: a
        // sidebar showing documents, or shut, makes no task traffic at all.
        let rail_visible = self.sidebar.is_open() && self.explore.showing_tasks();
        let counts_changed = self
            .task_counts
            .maintain(self.current_workspace(), rail_visible, now);
        if counts_changed {
            // **The rail's numbers go to the rail and nowhere else.** The Tasks
            // page used to be handed them too, for the teaching state's gate;
            // with that state gone the page is drawn entirely from its own
            // answer, which is what stops it disagreeing with the number beside
            // the queue it is showing.
            self.explore
                .rail_mut()
                .set_counts(self.task_counts.counts());
            self.refresh_rail_lists();
        }
        let context_tasks_changed = self.refresh_context_tasks(ws);
        // The Tasks tab's own page. **Visible means the tab in front**: a Tasks
        // tab in the background has no deadline and makes no request at all,
        // which is the other half of "a background Tasks tab costs a bool".
        let workspace = self.current_workspace();
        let showing = matches!(self.tabs().active_tab().view, TabView::Tasks(_));
        let mut page_changed = false;
        let active_path = self.panes.focused_path().clone();
        for (path, group) in self.panes.iter_groups_mut() {
            let group_active = group.active();
            for (i, tab) in group.tabs_mut().iter_mut().enumerate() {
                if let TabView::Tasks(t) = &mut tab.view {
                    let target = t.target();
                    let in_front = showing && path == active_path && i == group_active;
                    if t.panel_mut()
                        .maintain(workspace, Some(target), in_front, now)
                    {
                        t.mark_stale();
                        page_changed = true;
                    }
                }
            }
        }
        // A finished comparison opens (or refreshes) the diff tab, once.
        let mut history_extras = false;
        if let Some(comparison) = self.document_history.take_new_comparison() {
            self.open_diff_tab(comparison);
            history_extras = true;
        }
        // An "unchanged" probe answer turns the open snapshot prompt into a
        // rename of the entry nothing diverged from. A prompt already closed
        // (or a "changed" answer) leaves everything as it is.
        if let Some(probe) = self.document_history.take_probe()
            && let Some(target) = probe
            && let Some(prompt) = self.snapshot_prompt.as_mut()
        {
            prompt.become_rename(&mut self.engine, target.key, target.label);
            history_extras = true;
        }
        // The session tabs' pass: reconcile which tab holds the controller,
        // then drain what the backend queued (`Wake::Session` woke this loop;
        // nothing here touches the session socket).
        let session_changed = self.maintain_sessions();
        // The run tabs' pass: tell each run view the things only a window
        // knows, take what its worker said, and perform the verbs it cannot
        // send itself — the session handover, *Re-run*, and opening a result
        // as a tab. Nothing here reaches a run: that is another thread's,
        // and it is the only one there is. The result tabs are folded in on
        // the same pass, for the same reason.
        let runs_changed = self.maintain_runs();
        // Schedule invalidations are pushed by the gateway watch. This pass
        // joins their fired RunIds against the run index just maintained above;
        // it starts no poll and performs no I/O on the window thread.
        let schedules_changed = self.maintain_schedules(runs_changed);
        // The artifacts column's reader answered: fold it in on the pass
        // that is already running, never on a timer of its own.
        let artifacts_changed = self.maintain_artifacts();
        // The presence probe answered since the last pass: the `+` menu's
        // profile rows may dim (or undim) without the pointer moving.
        let probe_changed = self.profile_probe.take_dirty();
        // **Only while Add… is standing.** This snapshot exists to notice the
        // chat list arriving under an open flyout, and `recent_chats` clones
        // its titles — a tick with no menu up must not pay for an answer
        // nobody is waiting on. `refresh_open_plus_chats` asks the same
        // question again; it is the precondition of the operation, this is the
        // guard on the work.
        let plus_menu_open = matches!(
            self.menu.as_ref().map(|m| &m.subject),
            Some(MenuSubject::NewTab)
        );
        let chats_before =
            plus_menu_open.then(|| self.ai_chat.recent_chats(context_menu::PLUS_RECENT_MAX));
        let ai_changed = if self.ai_visible() {
            self.ai_chat.maintain(now)
        } else {
            self.ai_chat.drain_updates()
        };
        if ai_changed
            && let Some(before) = chats_before
            && before != self.ai_chat.recent_chats(context_menu::PLUS_RECENT_MAX)
        {
            self.refresh_open_plus_chats();
        }
        let ai_credential_saved = self.save_pending_ai_credential();
        // The Chats view's pass, **after** the pane's updates were drained so
        // news it just heard is acted on this pass: told by every chat surface
        // when the list changed, and folding in its reader's answer — never
        // on a timer.
        let chats_changed = self.maintain_chats();
        let socials_changed = self.maintain_socials_navigation(now);
        let ai_document_opened = self.open_pending_ai_document();
        self.ask_pending_ai_chat_folder();
        // The level meter's tick. `false` the moment the session ends, because
        // there is no `Listening` left to ask.
        let meter_changed = self.tick_dictation(now);
        // The recorder's clock: the countdown's numerals, the clean-present
        // handshake, and the abort path's close re-ask.
        let recording_changed = self.maintain_recording(now);
        // The recorder's menu-open observation is a rising edge, not fourteen
        // hooks: one detector covers every menu and every gesture.
        let menu_open = self.menu.is_some();
        if menu_open && !self.recording_menu_was_open {
            self.observe_recording_menu_open();
        }
        self.recording_menu_was_open = menu_open;
        let reminder_diagnostics_changed =
            self.agent
                .take_reminder_diagnostics()
                .is_some_and(|diagnostics| {
                    self.reminder_preferences
                        .set_pushed_diagnostics(diagnostics)
                });
        let reminder_preferences_changed = {
            let agent = self.agent.clone();
            self.reminder_preferences.maintain(&agent)
        };
        let reminder_settings_view_changed = self.sync_reminder_settings_views();
        let reminder_health_changed = self.reminder_shell.set_health(
            self.reminder_preferences
                .health_presentation(crate::format::lang()),
        );
        let reminder_failure_changed = self
            .reminder_shell
            .set_mutation_failure(self.reminders.failed_owner().is_some());
        let reminder_attention_changed = self
            .reminders
            .set_global_attention(self.reminder_preferences.attention());
        // Publish pills after both Store rows and device attention have settled
        // so this frame does not need another event to reflect either update.
        let chip_changed = self.push_reminder_chip();
        let reminder_shell_changed = self.reminder_shell.set_global(
            self.reminders.global(),
            reminder_shell::PresentationEnvironment::new(
                self.reminder_environment.locale.clone(),
                self.reminder_environment.display_rules.clone(),
            ),
        );
        let reminder_action_geometry_changed = self
            .reminder_shell
            .prepare_action_widths(&mut self.engine, crate::format::lang());
        // The rail's Reminders row carries the bell badge's number. Pushed
        // every turn rather than only when the shell reports a change — the
        // rail compares inside, the same contract `set_counts` keeps.
        let rail_reminders_changed = self
            .explore
            .rail_mut()
            .set_reminder_attention(self.reminder_shell.attention_count());
        let pending_snooze = self.reminders.pending_snooze().cloned();
        let reminder_snooze_opened = self
            .reminder_shell
            .open_pending_snooze(pending_snooze.as_ref(), now);
        let reminder_bell = self.reminder_bell_rect();
        let reminder_geometry_changed = self
            .reminder_shell
            .maintain_geometry(self.viewport, reminder_bell);
        let reminder_clock_changed = self
            .reminder_shell
            .maintain_clock(now, chrono::Utc::now().timestamp_millis());
        // **The Reminders page is fed beside the shell**, with the same
        // inputs in the same order — but only while its tab is in front.
        // Visible means in front: a background Reminders tab makes no request
        // and answers no clock, the other half of the rule the Tasks page
        // keeps above. The geometry rebuild itself runs on the tab's ordinary
        // `maintain` below, after this block.
        let reminders_page_changed =
            if matches!(self.tabs().active_tab().view, TabView::Reminders(_)) {
                let health = self
                    .reminder_preferences
                    .health_presentation(crate::format::lang());
                let failed = self.reminders.failed_owner().is_some();
                // Borrow the settled projection across the disjoint `panes`
                // mutation. Cloning here made every idle settle copy every
                // reminder before `RemindersView::set_global` could discover
                // that nothing had changed.
                let global = self.reminders.global();
                let environment = reminder_shell::PresentationEnvironment::new(
                    self.reminder_environment.locale.clone(),
                    self.reminder_environment.display_rules.clone(),
                );
                let now_ms = chrono::Utc::now().timestamp_millis();
                // Field-disjoint borrows: the tab strip lives under `panes`, the
                // measuring engine beside it.
                let panes = &mut self.panes;
                let engine = &mut self.engine;
                let TabView::Reminders(view) =
                    &mut panes.focused_mut().tabs_mut().active_mut().view
                else {
                    unreachable!("the matches! above just answered Reminders");
                };
                let mut changed = view.set_health(health);
                changed |= view.set_mutation_failure(failed);
                changed |= view.set_global(global, environment);
                changed |= view.prepare_action_widths(engine, crate::format::lang());
                changed |= view.maintain_clock(now, now_ms);
                changed
            } else {
                false
            };
        while let Some(request) = self.reminder_preferences.take_startup_request() {
            self.ask_pane(PaneRequest::AnswerReminderStartup(request));
        }
        let view_changed = self.tabs_mut().active_view().maintain(ws, now);
        // The Manage tab's *Linked devices* page re-asks for presence on its
        // deadline; the ask leaves as a request for Agent.
        let manage_asked = matches!(self.tabs().active_tab().view, TabView::Manage(_))
            && self.drain_manage_requests();
        view_changed
            || manage_asked
            || session_changed
            || runs_changed
            || schedules_changed
            || artifacts_changed
            || chats_changed
            || socials_changed
            || probe_changed
            || search_changed
            || entity_changed
            || history_changed
            || counts_changed
            || context_tasks_changed
            || rolled
            || page_changed
            || history_extras
            || ai_changed
            || ai_credential_saved
            || ai_document_opened
            || link_device_changed
            || tooltip_changed
            || meter_changed
            || recording_changed
            || reminder_preferences_changed
            || reminder_settings_view_changed
            || welcome_ticks
            || reminder_clock_changed
            || reminder_health_changed
            || reminder_failure_changed
            || reminder_diagnostics_changed
            || reminder_attention_changed
            || reminder_snooze_opened
            || reminder_geometry_changed
            || delegate_folder
            || delegation_requested
            || delegation_answered
            || run_pills
            || reminder_requested
            || reminder_management_requested
            || reminder_context_requested
            || reminder_global_handoff
            || reminder_profile
            || reminder_inspector_labels
            || reminder_context_lost
            || reminder_surface_context_lost
            || reminder_due_changed
            || reminder_changed
            || reminder_surface_changed
            || reminder_shell_changed
            || reminder_action_geometry_changed
            || rail_reminders_changed
            || reminders_page_changed
            || reminder_presence_changed
            || reminder_submitted
            || reminder_cancelled
            || chip_changed
            || feedback_failed
            || telemetry_changed
            || refresh_stats
    }
}
impl MainView {
    fn key(
        &mut self,
        ws: &mut WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        // **A promoted draft's text lands here.** The first point after the
        // swap where the document is available mutably, and the *inherent*
        // `key` rather than the trait one because that is what both the shell
        // and a direct caller reach — an inherent method wins name resolution,
        // so putting it on the trait impl left the tests going round it.
        self.flush_promoted_draft(ws);
        self.trace_input_state("key");
        // **The blocking prefix, in an order this function no longer owns.**
        //
        // Every arm below is a surface that takes the key and lets nothing
        // reach the view underneath; which of them wins is
        // [`routing::Layer`]'s answer, and it is the same table
        // [`MainView::editable_target`] walks. Two chains — one for keys and
        // one for dictation — would drift the first time a panel was added,
        // and the drift is silent: a dictation landing behind a modal, or in
        // the document under a prompt.
        //
        // It starts at `Layer::Error` because the `View::key` wrapper above
        // has already claimed *Move to…* and the search overlay. Those two
        // arms are therefore unreachable from here, and are spelled out rather
        // than caught by a wildcard so that adding a layer is a compile error
        // in this match.
        match self.blockers_from(routing::Layer::Error) {
            Blockers::Modal(ModalKind::Error) => return self.error_key(key),
            Blockers::Modal(ModalKind::Import) => {
                if let Some(dialog) = self.import_dialog.as_mut() {
                    let outcome = dialog.key(key, self.viewport);
                    return self.settle_import(outcome);
                }
            }
            Blockers::Modal(ModalKind::Updates) => return self.updates_modal_key(key),
            Blockers::Modal(ModalKind::UpdateNewer) => return self.update_newer_key(key),
            // Modal, and it swallows everything: a dialog that let keystrokes
            // through would take dictation into the document behind it.
            Blockers::Modal(ModalKind::Confirm) => {
                if let Some((dialog, _)) = self.confirm.as_mut() {
                    let out = dialog.key(key);
                    self.settle_confirm_outcome(out);
                    self.drain_trash_all(ws);
                    // **Always handled**, whatever the key was: a modal that
                    // let keystrokes through would take dictation into the
                    // document behind it. The dialog's own `Outcome::Keep`
                    // says the same thing.
                    return true;
                }
            }
            Blockers::Prompt(PromptKind::NewDocument) => {
                return self.new_document_key(ws, key, text, mods);
            }
            Blockers::Prompt(PromptKind::Snapshot) => {
                return self.snapshot_key(key, text, mods);
            }
            Blockers::Prompt(PromptKind::LinkDevice) => {
                if let Some(dialog) = self.link_device.as_mut() {
                    let outcome = dialog.key(&mut self.engine, key, text, mods);
                    return self.settle_link_device(outcome);
                }
            }
            Blockers::Prompt(PromptKind::Appearance) => {
                if let Some(m) = self.workspace_icon.as_mut() {
                    let out = m.key(&mut self.engine, key, text, mods);
                    return self.settle_icon(out);
                }
            }
            // A rename field owns the keyboard while it is open — including Esc
            // and Enter, which the popover under it would otherwise take.
            Blockers::Prompt(PromptKind::Rename) => {
                if let Some(field) = self.workspace_rename.as_mut() {
                    let taken = self
                        .pane_context
                        .as_ref()
                        .map(|(_, _, w, _)| match field.target {
                            RenameTarget::Workspace(id) => w
                                .iter()
                                .filter(|w| w.id != id)
                                .map(|w| w.name.clone())
                                .collect::<Vec<_>>(),
                            RenameTarget::Profile(_)
                            | RenameTarget::File(_)
                            | RenameTarget::Schedule(_) => Vec::new(),
                        })
                        .unwrap_or_default();
                    let out = field.key(&mut self.engine, key, text, mods, &taken);
                    return self.settle_rename(out);
                }
            }
            // The rail's inline "+ New list" field owns the keyboard for the
            // same reason the rename field above it does: Enter and Esc are
            // its, and the column under it would otherwise take them.
            Blockers::Prompt(PromptKind::NewList) => {
                return self.new_list_key(key, text, mods).unwrap_or(false);
            }
            // The delegation modal answers **every** key while it stands: it
            // holds an objective somebody is typing, and a chord reaching the
            // document under it would edit a page nobody can see.
            Blockers::Prompt(PromptKind::Delegate) => {
                return self.delegation_key(key, text, mods);
            }
            // The recording setup modal answers **every** key while it stands,
            // the delegation rule: a chord reaching the document under it
            // would edit a page nobody can see.
            Blockers::Modal(ModalKind::Recording) => {
                return self.recording_key(key, text, mods);
            }
            // An open context menu takes the keys it cares about (Esc, the
            // arrows, Enter) and lets nothing else through — a chord must not,
            // say, close a tab under a menu that is itself about to dismiss.
            Blockers::Modal(ModalKind::ContextMenu) => return self.menu_key(key),
            Blockers::Modal(ModalKind::ReminderOverlay) => {
                let bell = self.reminder_bell_rect();
                let (handled, intent) =
                    self.reminder_shell
                        .key(self.viewport, bell, key, Instant::now());
                if let Some(intent) = intent {
                    self.apply_reminder_shell_intent(intent);
                }
                return handled;
            }
            Blockers::Modal(ModalKind::ReminderManagement)
            | Blockers::Prompt(PromptKind::ReminderCreation) => {
                return self.reminder_creation_key(ws, key, text, mods);
            }
            // Claimed by `View::key`, above; see `blockers_from`.
            Blockers::Prompt(PromptKind::MoveTo | PromptKind::Search) => {}
            Blockers::None => {}
        }
        // **Escape during the countdown aborts.** Every other key keeps
        // working — the window is about to be recorded exactly as it is used
        // — but Escape is the take-back the modal taught, and it must not
        // leak to the draft-close or the tab drag underneath while the
        // countdown stands. (F9 needs no arm: it is a chord, and the toggle
        // knows the lifecycle.)
        if matches!(
            self.recording_lifecycle,
            Some(recording::Lifecycle::Countdown { .. } | recording::Lifecycle::CleanPresent)
        ) && matches!(key, Key::Named(winit::keyboard::NamedKey::Escape))
        {
            return self.toggle_recording();
        }
        // Escape cancels an in-flight tab drag before anything else claims it.
        if matches!(key, Key::Named(winit::keyboard::NamedKey::Escape))
            && (self.pane_drag.is_some() || self.group_drag().is_some())
        {
            self.pane_drag = None;
            *self.group_drag_mut() = None;
            self.split_intent = None;
            self.drop_slot = None;
            return true;
        }
        // **A recovery pill that `F6` has landed on**, asked beside the bell
        // because it is the same kind of thing: window chrome that owns no
        // blocker, standing while the document still has everything else. The
        // gate is the focus itself, so an unfocused pill claims nothing at all
        // — and the surface answers only Escape, Tab, the arrows and an
        // activation, letting every other key fall through the way the bell
        // does. Reached through `reminder_creation_key`, so a Try again from
        // the keyboard settles exactly as a press on it settles.
        if self.reminder_creation.pill_focused() && self.reminder_creation_key(ws, key, text, mods)
        {
            return true;
        }
        let bell = self.reminder_bell_rect();
        let (handled, intent) = self
            .reminder_shell
            .key(self.viewport, bell, key, Instant::now());
        if let Some(intent) = intent {
            self.apply_reminder_shell_intent(intent);
        }
        if handled {
            return true;
        }
        // An open popover takes Esc, the arrows and Enter — and **lets
        // everything else through**. A panel hanging off a column is not a
        // modal over the document, so Ctrl+E must still shut the pane under it
        // and typing must still reach the view.
        if self.explore.popover_open()
            && let Some(out) = self.explore.popover_key(key)
        {
            let area = self.explore_popover_area();
            self.explore.reveal_popover_hover(area, self.viewport);
            return self.apply(ws, out);
        }
        // Esc cancels an explore drag before anything else claims the key —
        // including nav_key, which only clears focus and would leave the cache
        // reordered until mouse-up committed the move.
        if matches!(key, Key::Named(winit::keyboard::NamedKey::Escape))
            && self.explore.pointer_active()
        {
            let row_drag = self.explore.drag_active();
            let _ = self.explore.cancel_pointer();
            self.explore_stale |= row_drag;
            return true;
        }
        // Question navigation belongs to an open AI pane, even when the
        // document or Explore last held focus. Route it before Explore's
        // modifier-agnostic arrow handling can claim the key.
        if self.ai_visible() && ChatSurface::is_question_navigation_chord(key, mods) {
            self.ai_focused = true;
            let _ = self.ai_chat.key(key, text, mods);
            let _ = self.save_pending_ai_credential();
            // The open pane owns this chord even when it has no authored
            // questions yet (including first-run setup). Reporting it as
            // unhandled lets the shell/OS reinterpret a shortcut whose
            // destination is already unambiguous.
            return true;
        }
        // **The tree, while it holds the keyboard.** Only the navigation keys —
        // everything else falls through, so typing still reaches the document
        // you are writing in. It is checked before the chords only in the sense
        // of being checked at all: `keys::resolve` claims no arrow, so Ctrl+E
        // still shuts the pane out from under it.
        if self.sidebar.is_open()
            && self.explore.has_focus()
            // ...but never over a prompt. A panel asking a question owns Enter,
            // whoever else has the arrows.
            && !self.tabs().active_tab().view.is_prompting()
            && let Some(out) = self.explore.nav_key_with_modifiers(self.regions.sidebar, key, mods)
        {
            inputdebug::write(format_args!("[Koru][input] view key claimed=explore-nav"));
            // An expand or a collapse changed which rows there are.
            self.explore_stale = true;
            return self.apply(ws, out);
        }
        // The editor owns the prompt and its write, while the container owns two
        // caches of the resulting name: the **tab's label** and the explore
        // row. Remembered before routing Enter, because by the time the key has
        // been handled the prompt has closed and there is nothing left to ask.
        //
        // A *local* edit never comes back round through `adopt` the way a peer's
        // does, so nothing else would notice. This was already done for an image
        // rename and not for a document's title, which is why one updated
        // immediately and the other did not update until something unrelated
        // happened to invalidate the cache.
        let rename_commit = matches!(key, Key::Named(winit::keyboard::NamedKey::Enter))
            && self.tabs().active_tab().view.is_renaming();
        let page_icon_commit = matches!(key, Key::Named(winit::keyboard::NamedKey::Enter))
            && self.tabs().active_tab().view.is_setting_page_icon();
        // **Ctrl+Z, while a delete is still offering to be taken back.** Before
        // the chords and before the tab, because the tab that would have handled
        // it is the one the delete just closed. See the method's own note.
        if self.undo_key_takes_back_a_trash(key, mods) {
            return true;
        }
        // Container chords first: a tab is not the active view's to close.
        if let Some(chord) = keys::resolve(key, text, mods) {
            return match chord {
                Chord::NewTab => self.new_tab(),
                Chord::AddContent => self.open_new_tab_menu(),
                Chord::CloseTab => self.close_requested(self.tabs().active()),
                Chord::CloseAllTabs => self.close_all(),
                Chord::Search => self.open_search(),
                Chord::SelectTab(i) => self.select(i),
                Chord::ToggleSidebar => self.focus_or_toggle_sidebar(ws),
                Chord::ToggleContext => self.toggle_context(),
                Chord::ToggleAi => self.toggle_ai(),
                Chord::OpenSettings => self.open_settings_tab(),
                Chord::Rename => self.rename_here(ws),
                Chord::ShowNavigator(mode) => self.show_navigator(mode),
                Chord::ManageWorkspaces => self.open_manage_workspaces(None),
                Chord::WorkspacesPopover => self.open_workspaces_popover(),
                Chord::ProfilesPopover => self.open_profiles_popover(),
                Chord::NextContextSection => self.next_context_section(),
                Chord::CopyCloudDetails => self.copy_cloud_details(),
                Chord::SaveSnapshot => {
                    if let TabView::Socials(view) = self.tabs_mut().active_view() {
                        view.save_draft()
                    } else {
                        self.prompt_save_snapshot()
                    }
                }
                Chord::FocusChrome => self.focus_chrome(),
                Chord::ToggleRecording => self.toggle_recording(),
                Chord::Reminders => self.open_reminders_here(ws),
            };
        }
        if self.ai_visible() && self.ai_focused {
            inputdebug::write(format_args!("[Koru][input] view key claimed=ai"));
            let changed = self.ai_chat.key(key, text, mods);
            return self.save_pending_ai_credential() || changed;
        }
        // **Escape on a draft is "nothing happened".** Nothing was ever
        // written, so closing the tab is the whole of it — the trip Back
        // (Alt+Left, mouse back) would make if a draft had a history entry to
        // leave. Guarded against a prompt: an open panel owns Escape first.
        if matches!(key, Key::Named(winit::keyboard::NamedKey::Escape))
            && self.tabs().active_tab().draft.is_some()
            && !self.tabs().active_tab().view.is_prompting()
        {
            return self.close(self.tabs().active());
        }
        // **The keystroke that makes a draft real.** It only *asks*: creating a
        // document is a round trip to the store, so this key and the next few
        // still land in the draft, and `flush_promoted_draft` carries their text
        // and the caret across at the swap. The comment here used to say the
        // draft never accumulated content, which stopped being true the day
        // promotion stopped being synchronous.
        if would_type(key, text, mods) {
            self.promote_draft();
            // The recorder hears that a character landed, never which one —
            // the kind is the whole observation.
            self.observe_recording_text_input();
        }
        // A draft is talked to through its own document, never the window's —
        // see `Tab::doc`. Everything below this line is the same for both.
        let (view, doc) = self.tabs_mut().active_mut().doc(ws);
        let handled = match view {
            TabView::Welcome(w) => w.key(doc, key, text, mods),
            TabView::Manage(w) => View::key(w, doc, key, text, mods),
            TabView::Trash(t) => View::key(t, doc, key, text, mods),
            TabView::Tasks(t) => View::key(t, doc, key, text, mods),
            TabView::Reminders(r) => View::key(r, doc, key, text, mods),
            TabView::Settings(s) => View::key(s, doc, key, text, mods),
            TabView::Feedback(f) => View::key(f, doc, key, text, mods),
            TabView::Diff(d) => View::key(d, doc, key, text, mods),
            TabView::AiChat(a) => View::key(a, doc, key, text, mods),
            TabView::Socials(a) => View::key(a, doc, key, text, mods),
            TabView::Session(s) => View::key(s, doc, key, text, mods),
            TabView::Run(r) => View::key(r, doc, key, text, mods),
            TabView::Result(r) => View::key(r, doc, key, text, mods),
            TabView::Canvas(c) => View::key(c, doc, key, text, mods),
            TabView::Leaf(l) => Leaf::key(l, doc, key, text, mods),
        };
        inputdebug::write(format_args!("[Koru][input] view key tab handled={handled}"));
        if handled {
            let entity_changed = self.sync_entity_picker();
            if rename_commit {
                self.explore_stale = true;
                // The tab strip is not rebuilt from the document per frame —
                // the label is a cache — so it is refreshed here rather than
                // waiting for whatever would have done it next.
                self.refresh_labels(ws);
            }
            if page_icon_commit {
                self.explore_stale = true;
            }
            // Drained here rather than by the shell: `take_wants_open` has no
            // document, and this is the nearest point that does.
            self.drain_wants_open(ws);
            self.drain_manage_profiles_request();
            self.drain_trash_all(ws);
            self.drain_welcome();
            self.drain_settings_requests();
            self.drain_feedback();
            return handled || entity_changed;
        }
        handled
    }
}

impl MainView {
    fn reminder_target(&self, node: NodeId) -> Option<reminders::Target> {
        let root = self.document_root();
        let document = self.active_document?;
        let (_, profile, _, workspace) = self.pane_context.as_ref()?;
        let workspace = (*workspace)?;
        let entity = koru_store_core::EntityRef::new(
            if root == Some(node) {
                "document"
            } else {
                "block"
            },
            koru_store_core::WorkspaceId::from_uuid(workspace.as_uuid()),
            koru_store_core::DocumentId::from_uuid(document.as_uuid()),
            node.as_uuid(),
        )
        .ok()?;
        Some(reminders::Target {
            profile: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            entity,
        })
    }

    fn reminder_anchor(&mut self, node: NodeId, reminder_id: Option<&str>) -> Option<Rect> {
        let local = match &mut self.tabs_mut().active_mut().view {
            TabView::Leaf(leaf) => leaf.reminder_anchor(node, reminder_id),
            _ => None,
        }?;
        let content = self.content();
        Some(Rect::new(
            local.x + content.x,
            local.y + content.y,
            local.w,
            local.h,
        ))
    }

    fn reminder_context(
        &self,
        ws: &WorkspaceDoc,
        node: NodeId,
    ) -> koru_editor::utils::when::ResolutionContext {
        let mut context = self.reminder_environment.resolution.clone();
        context.due_anchor = koru_editor::reminders::due_anchor(ws, node);
        context
    }

    /// **The item a reminder set from the keyboard is about** — the row the
    /// caret is in.
    ///
    /// `Body::focus` is the row the caret is drawn in and the row
    /// `document_focus` publishes to a screen reader, so the three cannot
    /// disagree about which item a person means. `None` for a tab that is not a
    /// document, which is what makes the shortcut answer nothing there rather
    /// than reaching for a neighbour.
    fn reminder_subject(&self) -> Option<NodeId> {
        match &self.tabs().active_tab().view {
            TabView::Leaf(leaf) => leaf.caret_node(),
            _ => None,
        }
    }

    /// `Ctrl+Shift+R` — the reminders on the item the caret is in.
    ///
    /// **Reopen what is saved, or start what is not**, and the first half is
    /// the point: reminder *creation* already had a keyboard route through the
    /// `/remind` command, and *revisiting a saved one* had none at all — a
    /// pill hanging off a row is opened by pressing it, and `press` was the
    /// only caller that ever asked. This is that route, and it performs the
    /// same request the press performs.
    ///
    /// **What counts as "saved" is `Controller::global`'s rows**, which are the
    /// reconciliation snapshot the Store answered with. A submission that has
    /// not turned up among them yet is not in that list, so this cannot reopen
    /// an optimistic pill and call it the schedule; it opens a new reminder
    /// instead, which is the honest offer while nothing is stored.
    ///
    /// **The first reminder on the item**, when there is more than one. Which
    /// of several a chord should mean is a question with no obvious answer and
    /// nobody has asked it; the pill's own press picks the exact one, and the
    /// *Reminders on this item* group reads them all. If somebody asks for a
    /// way to cycle, that is the thing to build rather than a second chord.
    fn open_reminders_here(&mut self, ws: &WorkspaceDoc) -> bool {
        let Some(node) = self.reminder_subject() else {
            return false;
        };
        let saved = self
            .reminder_target(node)
            .map(|target| target.entity.to_storage())
            .and_then(|key| {
                self.reminders
                    .global()
                    .rows
                    .iter()
                    .find(|row| row.target == key)
                    .map(|row| row.reminder_id.clone())
            });
        match saved {
            Some(id) => self.open_reminder_edit(ws, node, &id),
            None => self.open_reminder_creation(ws, node),
        }
    }

    fn open_reminder_creation(&mut self, ws: &WorkspaceDoc, node: NodeId) -> bool {
        let Some(target) = self.reminder_target(node) else {
            return false;
        };
        let context = self.reminder_context(ws, node);
        self.reminders.open(
            target,
            koru_editor::reminders::Request {
                identity: koru_editor::reminders::RequestIdentity {
                    token: koru_editor::reminders::RequestToken::new(),
                    node,
                },
                phrase: String::new(),
                due_anchor: context.due_anchor.clone(),
            },
            context,
        );
        true
    }

    fn open_reminder_recreate(
        &mut self,
        ws: &WorkspaceDoc,
        node: NodeId,
        source: koru_editor::reminders::Draft,
    ) -> bool {
        let Some(target) = self.reminder_target(node) else {
            return false;
        };
        let context = self.reminder_context(ws, node);
        self.reminders.open_recreate(target, node, source, context);
        true
    }

    fn settle_reminder_creation(
        &mut self,
        ws: &WorkspaceDoc,
        outcome: reminder_creation::Outcome,
    ) -> bool {
        use reminder_creation::Action;
        let changed = match outcome.action {
            Some(Action::Cancel) => self.reminders.cancel_creation(),
            Some(Action::Confirm(draft)) => {
                self.reminders.confirm_creation(draft);
                // The Store projection is deliberately asynchronous. Publish
                // the controller-owned optimistic reminder in this input turn
                // so closing the creation surface cannot leave a bare row
                // until that projection catches up.
                self.push_reminder_chip();
                true
            }
            Some(Action::Phrase(phrase)) => self.reminders.update_creation_phrase(phrase),
            Some(Action::Schedule(schedule)) => self.reminders.replace_creation_schedule(schedule),
            Some(Action::Add(node)) => self.open_reminder_creation(ws, node),
            Some(Action::Recreate(source)) => self
                .reminder_creation
                .node()
                .is_some_and(|node| self.open_reminder_recreate(ws, node, source)),
            Some(Action::Management(intent)) => {
                let updates_chip = matches!(
                    &intent,
                    koru_editor::reminders::ManagementIntent::Snooze { .. }
                );
                self.reminders.apply_management(intent);
                if updates_chip {
                    self.push_reminder_chip();
                }
                true
            }
            Some(Action::ToggleHistory) => {
                if let Some(management) = self.reminders.management.as_mut() {
                    management.toggle_history();
                    true
                } else {
                    false
                }
            }
            Some(Action::Retry(token)) => self.reminders.retry_creation(token),
            Some(Action::EditFailed(token)) => self.reminders.edit_failed_creation(token),
            Some(Action::Dismiss(token)) => self.reminders.dismiss_failed_creation(token),
            Some(Action::RetryCurrent) => self
                .reminders
                .failed_owner()
                .is_some_and(|(owner, _)| self.reminders.retry_failed(&owner)),
            Some(Action::DiscardCurrent) => self
                .reminders
                .failed_owner()
                .is_some_and(|(owner, _)| self.reminders.discard_failed(&owner)),
            Some(Action::OpenEndOfDaySettings) => {
                let cancelled = self.reminders.cancel_creation();
                let opened = self.open_reminder_end_of_day_settings();
                cancelled || opened
            }
            None => false,
        };
        outcome.handled || changed
    }

    fn reminder_creation_key(
        &mut self,
        ws: &WorkspaceDoc,
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
    ) -> bool {
        let outcome = self
            .reminder_creation
            .key(&mut self.engine, key, text, mods);
        self.settle_reminder_creation(ws, outcome)
    }

    /// **`F6` — move the keyboard into window chrome**, one stop per press.
    ///
    /// It has always meant this and has always had one stop: the reminder bell
    /// (`Shell::focus_bell`). It is a *rotation* now because a second surface
    /// needed reaching and the alternative was a chord of its own — a shortcut
    /// nobody discovers for a state that is rare. So the recovery pill a
    /// refused reminder save leaves floating joins the convention rather than
    /// inventing a rule for surfaces that own no keyboard (Q-R1, the maintainers'
    /// answer on 2026-09-11).
    ///
    /// **The rotation is what is standing, never a fixed list.** A pill is a
    /// stop only while there is one — `Surface::has_failed_pill` reads the same
    /// geometry that draws it — so with nothing failed this is exactly what it
    /// was before: `F6` lands on the bell, and a second `F6` leaves it there
    /// (`focus_bell` answers `false` and the key falls through, claiming
    /// nothing). A stop that is not there is never a dead press.
    ///
    /// **The pill comes first** because it is the transient one: it is standing
    /// because something a person asked for did not happen, and the bell will
    /// still be there on the next press. From the pill, `F6` moves on to the
    /// bell; from the bell it wraps round to the pill if one is still standing.
    /// Escape on the pill leaves chrome outright, which
    /// `accessibility_focus` reads as the content region — the same place a
    /// character would go.
    ///
    /// Whichever stop is taken, the other is cleared, so two rings can never be
    /// painted at once.
    fn focus_chrome(&mut self) -> bool {
        // Both stops are reminder chrome; with reminders hidden F6 has
        // nowhere to land and falls through (koru-features).
        if !koru_features::reminders() {
            return false;
        }
        // Anywhere but the pill goes to the pill, if one is standing; the pill
        // goes on to the bell. Two stops, so the whole rotation is this line.
        if self.reminder_creation.has_failed_pill() && !self.reminder_creation.pill_focused() {
            let moved = self.reminder_creation.focus_first_failed_pill();
            return self.reminder_shell.blur_bell() || moved;
        }
        let moved = self.reminder_shell.focus_bell();
        self.reminder_creation.blur_pill() || moved
    }

    fn drain_reminder_management_request(&mut self, ws: &WorkspaceDoc) -> bool {
        let request = match self.active_view() {
            TabView::Leaf(leaf) => leaf.take_reminder_management_request(),
            _ => None,
        };
        let Some((node, reminder_id)) = request else {
            return false;
        };
        if !koru_features::reminders() {
            return false;
        }
        self.open_reminder_edit(ws, node, &reminder_id)
    }

    fn open_reminder_edit(&mut self, ws: &WorkspaceDoc, node: NodeId, reminder_id: &str) -> bool {
        let Some(target) = self.reminder_target(node) else {
            return false;
        };
        let Some(anchor) = self.reminder_anchor(node, Some(reminder_id)) else {
            return false;
        };
        let Some(row) = self
            .reminders
            .global()
            .rows
            .iter()
            .find(|row| row.reminder_id == reminder_id && row.target == target.entity.to_storage())
            .cloned()
        else {
            return false;
        };
        let context = self.reminder_context(ws, node);
        self.reminders.open_management(target, context.clone());
        let (context, default_tone) = self.reminders.editor_context(context);
        self.reminder_creation.update_context(
            context,
            self.reminder_environment.locale.clone(),
            default_tone,
        );
        self.reminder_creation.open_existing_edit(
            &mut self.engine,
            node,
            anchor,
            row.reminder_id,
            row.draft,
            row.rule_paused,
        );
        true
    }

    fn drain_reminder_context_request(&mut self, ws: &WorkspaceDoc) -> bool {
        let request = match self.active_view() {
            TabView::Leaf(leaf) => leaf.take_reminder_context_request(),
            _ => None,
        };
        let Some(request) = request else {
            return false;
        };
        if !koru_features::reminders() {
            return false;
        }
        if request.action == koru_editor::reminders::ContextAction::Edit {
            return self.open_reminder_edit(ws, request.node, &request.reminder_id);
        }
        let Some(target) = self.reminder_target(request.node) else {
            return false;
        };
        let Some(row) = self
            .reminders
            .global()
            .rows
            .iter()
            .find(|row| {
                row.reminder_id == request.reminder_id && row.target == target.entity.to_storage()
            })
            .cloned()
        else {
            return false;
        };
        let action = match request.action {
            koru_editor::reminders::ContextAction::Pause => reminders::GlobalAction::Pause,
            koru_editor::reminders::ContextAction::Resume => reminders::GlobalAction::Resume,
            koru_editor::reminders::ContextAction::Delete => reminders::GlobalAction::Delete,
            koru_editor::reminders::ContextAction::Edit => unreachable!(),
        };
        self.reminders.apply_global(&row, action)
    }

    fn global_reminder_landing(&mut self, target: &reminders::Target) -> Option<(NodeId, Rect)> {
        let node = NodeId::from_uuid(target.entity.node());
        (self.reminder_target(node).as_ref() == Some(target))
            .then(|| {
                self.reminder_anchor(node, None)
                    .map(|anchor| (node, anchor))
            })
            .flatten()
    }

    /// Consume a global Edit/Recreate handoff only after navigation has landed
    /// on its exact profile/workspace/document/kind/node and layout has
    /// published the target anchor. Until then the controller retains it.
    fn drain_global_reminder_handoff(&mut self, ws: &WorkspaceDoc) -> bool {
        if let Some(target) = self.reminders.pending_global_edit_target().cloned()
            && let Some((node, anchor)) = self.global_reminder_landing(&target)
            && let Some(request) = self.reminders.take_global_edit()
        {
            let context = self.reminder_context(ws, node);
            self.reminders
                .open_management(request.target, context.clone());
            let (context, default_tone) = self.reminders.editor_context(context);
            self.reminder_creation.update_context(
                context,
                self.reminder_environment.locale.clone(),
                default_tone,
            );
            self.reminder_creation.open_existing_edit(
                &mut self.engine,
                node,
                anchor,
                request.row.reminder_id,
                request.row.draft,
                request.row.rule_paused,
            );
            return true;
        }
        if let Some(target) = self.reminders.pending_global_recreate_target().cloned()
            && let Some((node, _)) = self.global_reminder_landing(&target)
            && let Some(request) = self.reminders.take_global_recreate()
        {
            let context = self.reminder_context(ws, node);
            self.reminders
                .open_recreate(request.target, node, request.source, context);
            return true;
        }
        false
    }

    fn sync_reminder_creation_surface(&mut self, ws: &WorkspaceDoc) -> bool {
        let node = self
            .reminders
            .creation
            .as_ref()
            .map(|creation| creation.request.identity.node)
            .or_else(|| self.reminder_creation.node());
        let anchor = node.and_then(|node| self.reminder_anchor(node, None));
        if let Some(node) = node {
            let context = self.reminder_context(ws, node);
            let (context, default_tone) = self.reminders.editor_context(context);
            self.reminders
                .refresh_creation_context(context.clone(), default_tone);
            self.reminder_creation.update_context(
                context,
                self.reminder_environment.locale.clone(),
                default_tone,
            );
        }
        let creation = self.reminders.creation.clone();
        let management = self.reminders.management.clone();
        let optimistic = self
            .reminders
            .optimistic_creations()
            .cloned()
            .collect::<Vec<_>>();
        // **Where the announcement is decided.** An announcement is a claim
        // about a *change*, and `View::semantics` is handed `&self`; this is
        // the pass that has both `&mut self` and the facts, and the list it
        // reads is the one it just built. See `accessibility/reminders.rs`.
        let submissions: Vec<_> = optimistic.iter().map(|item| item.state).collect();
        self.reminder_news.observe(&submissions);
        let mut changed =
            self.reminder_creation
                .sync_creation(&mut self.engine, creation.as_ref(), anchor);
        changed |= self.reminder_creation.maintain_projection(
            &mut self.engine,
            self.viewport,
            crate::format::lang(),
            creation.as_ref(),
            management.as_ref(),
            &optimistic,
        );
        if let Some(schedule) = self.reminder_creation.take_pending_schedule() {
            changed |= self.settle_reminder_creation(
                ws,
                reminder_creation::Outcome {
                    handled: true,
                    action: Some(reminder_creation::Action::Schedule(schedule)),
                },
            );
        }
        changed
    }

    fn apply_reminder_shell_intent(&mut self, intent: reminder_shell::Intent) -> bool {
        // Every reminders surface — the bell's popover, the All Reminders
        // page — speaks through here; none is shown with reminders hidden
        // (koru-features).
        if !koru_features::reminders() {
            return false;
        }
        let now = Instant::now();
        let updates_chip = matches!(&intent, reminder_shell::Intent::Snooze(_, _));
        let changed = match &intent {
            reminder_shell::Intent::OpenAllReminders => true,
            reminder_shell::Intent::SetFilter(filter) => self.reminders.set_global_filter(*filter),
            reminder_shell::Intent::Open(row) => koru_store_core::EntityRef::parse(&row.target)
                .is_some_and(|entity| {
                    self.ask_pane(PaneRequest::Navigate {
                        target: koru_workspace_host::NavigationTarget::Document {
                            document: koru_core::DocumentId::from_uuid(entity.document().as_uuid()),
                            node: Some(NodeId::from_uuid(entity.node())),
                        },
                        match_text: None,
                        disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                    })
                }),
            reminder_shell::Intent::Edit(row) => {
                let handed_off = self.reminders.begin_global_edit(row);
                handed_off
                    && koru_store_core::EntityRef::parse(&row.target).is_some_and(|entity| {
                        self.ask_pane(PaneRequest::Navigate {
                            target: koru_workspace_host::NavigationTarget::Document {
                                document: koru_core::DocumentId::from_uuid(
                                    entity.document().as_uuid(),
                                ),
                                node: Some(NodeId::from_uuid(entity.node())),
                            },
                            match_text: None,
                            disposition:
                                koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                        })
                    })
            }
            reminder_shell::Intent::Recreate(row) => {
                let handed_off = self.reminders.begin_global_recreate(row);
                handed_off
                    && koru_store_core::EntityRef::parse(&row.target).is_some_and(|entity| {
                        self.ask_pane(PaneRequest::Navigate {
                            target: koru_workspace_host::NavigationTarget::Document {
                                document: koru_core::DocumentId::from_uuid(
                                    entity.document().as_uuid(),
                                ),
                                node: Some(NodeId::from_uuid(entity.node())),
                            },
                            match_text: None,
                            disposition:
                                koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                        })
                    })
            }
            reminder_shell::Intent::Snooze(row, until_ms) => self.reminders.apply_global(
                row,
                reminders::GlobalAction::Snooze {
                    until_ms: *until_ms,
                },
            ),
            reminder_shell::Intent::Handled(row) => self
                .reminders
                .apply_global(row, reminders::GlobalAction::Handled),
            reminder_shell::Intent::Pause(row) => self
                .reminders
                .apply_global(row, reminders::GlobalAction::Pause),
            reminder_shell::Intent::Resume(row) => self
                .reminders
                .apply_global(row, reminders::GlobalAction::Resume),
            reminder_shell::Intent::Delete(row) => self
                .reminders
                .apply_global(row, reminders::GlobalAction::Delete),
            reminder_shell::Intent::ClearAllHistory => {
                self.reminders.clear_all_history();
                true
            }
            reminder_shell::Intent::CancelSnooze => self.reminders.cancel_snooze(),
            reminder_shell::Intent::Health(action) => {
                match action {
                    crate::reminder_settings::HealthAction::Refresh => {
                        self.reminder_preferences.refresh_notification_status()
                    }
                    crate::reminder_settings::HealthAction::Repair => {
                        self.reminder_preferences.repair_delivery()
                    }
                    crate::reminder_settings::HealthAction::OpenNotificationSettings => {
                        self.reminder_preferences.open_notification_settings()
                    }
                    crate::reminder_settings::HealthAction::EnableDelivery => {
                        self.reminder_preferences.set_delivery_enabled(true)
                    }
                }
                true
            }
            reminder_shell::Intent::RetryFailed => self
                .reminders
                .failed_owner()
                .map(|(owner, _)| self.reminders.retry_failed(&owner))
                .unwrap_or(false),
            reminder_shell::Intent::DiscardFailed => self
                .reminders
                .failed_owner()
                .map(|(owner, _)| self.reminders.discard_failed(&owner))
                .unwrap_or(false),
        };
        let shell_changed = updates_chip
            && self.reminder_shell.set_global(
                self.reminders.global(),
                reminder_shell::PresentationEnvironment::new(
                    self.reminder_environment.locale.clone(),
                    self.reminder_environment.display_rules.clone(),
                ),
            );
        self.reminder_shell.apply(intent, now);
        changed | shell_changed | (updates_chip && self.push_reminder_chip())
    }

    /// Integration hook for the expanded reminder editor branch. Until that
    /// host registers, the shell does not expose Edit as an enabled action.
    #[allow(dead_code)]
    pub(crate) fn set_global_reminder_edit_host_available(&mut self, available: bool) -> bool {
        let shell = self.reminder_shell.set_edit_available(available);
        let page = match &mut self.tabs_mut().active_mut().view {
            TabView::Reminders(r) => r.set_edit_available(available),
            _ => false,
        };
        shell || page
    }

    /// Consume exactly one durable edit handoff after navigation has landed.
    /// The target and projected draft remain paired until this call.
    #[allow(dead_code)]
    pub(crate) fn take_global_reminder_edit(&mut self) -> Option<reminders::GlobalEditRequest> {
        self.reminders.take_global_edit()
    }

    /// Register the shared creation host only when it can resolve an old
    /// History schedule through the ordinary passed-time proposal/editor flow.
    /// Until then Recreate is absent rather than an enabled immediate insert.
    #[allow(dead_code)]
    pub(crate) fn set_global_reminder_recreate_host_available(&mut self, available: bool) -> bool {
        let shell = self.reminder_shell.set_recreate_available(available);
        let page = match &mut self.tabs_mut().active_mut().view {
            TabView::Reminders(r) => r.set_recreate_available(available),
            _ => false,
        };
        shell || page
    }

    /// Consume exactly one durable History handoff after navigation. The
    /// original draft is still unmodified and must be resolved by creation.
    #[allow(dead_code)]
    pub(crate) fn take_global_reminder_recreate(
        &mut self,
    ) -> Option<reminders::GlobalRecreateRequest> {
        self.reminders.take_global_recreate()
    }

    fn ensure_reminder_profile(&mut self) -> bool {
        let Some(root) = self.document_root() else {
            return false;
        };
        let Some(document) = self.active_document else {
            return false;
        };
        let Some((_, profile, _, Some(workspace))) = self.pane_context.as_ref() else {
            return false;
        };
        let Ok(entity) = koru_store_core::EntityRef::new(
            "document",
            koru_store_core::WorkspaceId::from_uuid(workspace.as_uuid()),
            koru_store_core::DocumentId::from_uuid(document.as_uuid()),
            root.as_uuid(),
        ) else {
            return false;
        };
        self.reminders.ensure_profile(reminders::Target {
            profile: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
            entity,
        })
    }

    /// Remove exact submitted text during the submission maintenance pass.
    fn apply_pending_reminder_submissions(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let mut changed = false;
        let mut keep = std::collections::VecDeque::new();
        while let Some((target, identity)) = self.pending_reminder_submissions.pop_front() {
            if self.reminder_target_is_active(&target, identity) {
                match self.active_view() {
                    TabView::Leaf(leaf) => changed |= leaf.reminder_submitted(ws, identity),
                    _ => keep.push_back((target, identity)),
                }
            } else {
                keep.push_back((target, identity));
            }
        }
        self.pending_reminder_submissions = keep;
        changed
    }

    fn apply_pending_reminder_cancels(&mut self, ws: &mut WorkspaceDoc) -> bool {
        let mut changed = false;
        let mut keep = std::collections::VecDeque::new();
        while let Some((target, identity)) = self.pending_reminder_cancels.pop_front() {
            if self.reminder_target_is_active(&target, identity) {
                match self.active_view() {
                    TabView::Leaf(leaf) => changed |= leaf.cancel_reminder(ws, identity),
                    _ => keep.push_back((target, identity)),
                }
            } else {
                keep.push_back((target, identity));
            }
        }
        self.pending_reminder_cancels = keep;
        changed
    }

    fn reminder_target_is_active(
        &self,
        target: &reminders::Target,
        identity: koru_editor::reminders::RequestIdentity,
    ) -> bool {
        self.reminder_target_node_is_active(target, identity.node)
    }

    fn reminder_target_node_is_active(&self, target: &reminders::Target, node: NodeId) -> bool {
        let Some((_, profile, _, Some(workspace))) = self.pane_context.as_ref() else {
            return false;
        };
        self.active_document.is_some_and(|document| {
            target.profile.as_uuid() == profile.as_uuid()
                && target.entity.workspace().as_uuid() == workspace.as_uuid()
                && target.entity.document().as_uuid() == document.as_uuid()
                && target.entity.node() == node.as_uuid()
        })
    }

    /// Drain `/remind` after an input gesture without doing Store work. One
    /// background reconciliation later supplies both settings and entity rows.
    fn drain_reminder_request(&mut self) -> bool {
        let root = self.document_root();
        let request = match self.active_view() {
            TabView::Leaf(leaf) => leaf.take_reminder_request(),
            _ => None,
        };
        let Some(request) = request else {
            return false;
        };
        // `/remind` is not offered with reminders hidden; a request that
        // arrives anyway is dropped here rather than written (koru-features).
        if !koru_features::reminders() {
            return false;
        }
        let Some((_, profile, _, Some(workspace))) = self.pane_context.as_ref() else {
            return false;
        };
        let Some(document) = self.active_document else {
            return false;
        };
        let entity = koru_store_core::EntityRef::new(
            if root == Some(request.identity.node) {
                "document"
            } else {
                "block"
            },
            koru_store_core::WorkspaceId::from_uuid(workspace.as_uuid()),
            koru_store_core::DocumentId::from_uuid(document.as_uuid()),
            request.identity.node.as_uuid(),
        );
        let Ok(entity) = entity else {
            return false;
        };
        let mut context = self.reminder_environment.resolution.clone();
        context.due_anchor = request.due_anchor.clone();
        self.reminders.open(
            reminders::Target {
                profile: koru_store_core::ProfileId::from_uuid(profile.as_uuid()),
                entity,
            },
            request,
            context,
        );
        true
    }

    fn push_reminder_chip(&mut self) -> bool {
        let Some(document) = self.active_document else {
            return false;
        };
        let Some(workspace) = self.current_workspace() else {
            return false;
        };
        let store_document = koru_store_core::DocumentId::from_uuid(document.as_uuid());
        let store_workspace = koru_store_core::WorkspaceId::from_uuid(workspace.as_uuid());
        let lang = crate::format::lang();
        let mut reminders = HashMap::<NodeId, Vec<koru_editor::reminders::InlineChip>>::new();
        let mut authoritative_ids = HashSet::new();
        for row in &self.reminders.global().rows {
            let Some(entity) = koru_store_core::EntityRef::parse(&row.target) else {
                continue;
            };
            if entity.workspace() != store_workspace || entity.document() != store_document {
                continue;
            }
            authoritative_ids.insert(row.reminder_id.clone());
            let snoozed = row.snoozed;
            let label = if snoozed {
                row.sort_at_ms.map_or_else(
                    || {
                        reminder_creation::format_draft_label_during_maintenance(
                            lang,
                            &self.reminder_environment.locale,
                            &self.reminder_environment.resolution.current_zone,
                            &row.draft,
                        )
                    },
                    |until_ms| {
                        reminder_creation::format_instant_label_during_maintenance(
                            &self.reminder_environment.locale,
                            &self.reminder_environment.display_rules,
                            until_ms,
                        )
                    },
                )
            } else {
                reminder_creation::format_draft_label_during_maintenance(
                    lang,
                    &self.reminder_environment.locale,
                    &self.reminder_environment.resolution.current_zone,
                    &row.draft,
                )
            };
            reminders
                .entry(NodeId::from_uuid(entity.node()))
                .or_default()
                .push(koru_editor::reminders::InlineChip {
                    reminder_id: row.reminder_id.clone(),
                    label,
                    paused: row.rule_paused,
                    snoozed,
                    attention: row.state == koru_editor::reminders::GlobalItemState::NeedsAttention,
                    handled: row.state == koru_editor::reminders::GlobalItemState::History,
                });
        }
        // A successful create reaches the Store before the projection snapshot
        // necessarily contains it. Pending and acknowledged records therefore
        // remain the inline source of truth until their client-minted ids show
        // up authoritatively. Failed records stay in the recovery surface,
        // where Retry/Edit/Dismiss are available, rather than masquerading as
        // live reminders in the document.
        for optimistic in self.reminders.optimistic_creations() {
            if matches!(
                optimistic.state,
                koru_editor::reminders::OptimisticState::Failed { .. }
            ) || authoritative_ids.contains(&optimistic.reminder_id)
            {
                continue;
            }
            let Some(entity) = koru_store_core::EntityRef::parse(&optimistic.target) else {
                continue;
            };
            if entity.workspace() != store_workspace || entity.document() != store_document {
                continue;
            }
            reminders
                .entry(NodeId::from_uuid(entity.node()))
                .or_default()
                .push(koru_editor::reminders::InlineChip {
                    reminder_id: optimistic.reminder_id.clone(),
                    label: reminder_creation::format_draft_label_during_maintenance(
                        lang,
                        &self.reminder_environment.locale,
                        &self.reminder_environment.resolution.current_zone,
                        &optimistic.draft,
                    ),
                    paused: false,
                    snoozed: false,
                    attention: false,
                    handled: false,
                });
        }
        // A build that hides reminders draws no reminder pills in the
        // document either (koru-features): an empty map clears any standing.
        if !koru_features::reminders() {
            reminders.clear();
        }
        match self.active_view() {
            TabView::Leaf(leaf) => leaf.set_reminder_chips(reminders),
            _ => false,
        }
    }

    fn ai_visible(&self) -> bool {
        self.ai_panel.is_open()
    }

    /// A document chip in the AI chat — in its composer, or in a message it
    /// sent — was clicked, in the pane or in an AI chat tab: open the document
    /// as a tab, the way following a document pill does, or focus the tab
    /// already showing it.
    fn open_pending_ai_document(&mut self) -> bool {
        let link = self.ai_chat.take_open_document_request().or_else(|| {
            match self.tabs_mut().active_view() {
                TabView::AiChat(view) => view.take_open_document_request(),
                _ => None,
            }
        });
        let Some(koru_link::KoruLink::Document { document, node }) = link else {
            return false;
        };
        let document = koru_core::DocumentId::from_uuid(document);
        let node = node.map(koru_core::NodeId::from_uuid);
        if self.focus_showing_document(document) {
            return true;
        }
        self.pending_lands.push_back((document, OpenWhere::NewTab));
        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
            target: koru_workspace_host::NavigationTarget::Document { document, node },
            match_text: None,
            disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
        });
        true
    }

    /// Consume the one secret-bearing handoff immediately into the provider's
    /// zeroizing vault type. No gateway request, log, or persisted UI setting
    /// ever carries the key.
    ///
    /// An agent chat asked for its working folder: hand the native picker to
    /// the shell and remember who asked, so the answer returns to the same
    /// surface. Either the pane's surface or the active AI chat tab may ask.
    fn ask_pending_ai_chat_folder(&mut self) {
        let (request, source) = if let Some(request) = self.ai_chat.take_working_folder_request() {
            (Some(request), AiChatFolderSource::Pane)
        } else if let TabView::AiChat(view) = self.tabs_mut().active_view() {
            (
                view.take_working_folder_request(),
                AiChatFolderSource::AiChatTab,
            )
        } else {
            (None, AiChatFolderSource::Pane)
        };
        let Some(request) = request else {
            return;
        };
        self.ai_chat_folder_ask = Some((source, request.chat));
        self.ask_pane(koru_workspace_host::PaneRequest::ChooseAiChatFolder {
            title: koru_i18n::t(crate::format::lang(), koru_i18n::Key::AiAgentChooseFolder)
                .to_owned(),
            start: request.start,
        });
    }

    /// The handoff can come from either the right-hand pane's shared surface or
    /// from an active Koru AI tab; both use the same vault entry.
    fn save_pending_ai_credential(&mut self) -> bool {
        enum Source {
            Pane,
            AiChatTab,
        }
        let (request, source) = if let Some(request) = self.ai_chat.take_openai_credential_request()
        {
            (Some(request), Source::Pane)
        } else if let TabView::AiChat(view) = self.tabs_mut().active_view() {
            (view.take_openai_credential_request(), Source::AiChatTab)
        } else {
            (None, Source::Pane)
        };
        let Some(request) = request else {
            return false;
        };
        let account = format!("koru-{}", koru_channel::segment());
        let result = match request.into_secret() {
            Some(secret) => SecretString::new(secret)
                .and_then(|secret| OsCredentialStore.save_openai(&account, secret)),
            None => OsCredentialStore.remove_openai(&account),
        }
        .map_err(|error| error.public_message);
        match source {
            Source::Pane => self.ai_chat.openai_credential_saved(result),
            Source::AiChatTab => {
                if let TabView::AiChat(view) = self.tabs_mut().active_view() {
                    view.openai_credential_saved(result)
                } else {
                    false
                }
            }
        }
    }
}

/// Whether this key is somebody **starting to write**, which is the moment a
/// draft becomes a real document.
///
/// Deliberately narrow. Everything a person does while deciding *whether* to
/// write — arrows, Escape, Home, a click, the wheel, even Ctrl+B on an empty
/// line — leaves the draft a draft, because none of it puts a character
/// anywhere. What counts is a printable character, Enter, or a paste.
///
/// **Control codes are not characters.** Ctrl+letter arrives as one on Windows
/// (Ctrl+K is `0x0B`, not `Character("k")`), so `is_control` is what keeps every
/// chord in the key map from creating a document as a side effect.
fn would_type(key: &Key, text: Option<&str>, mods: ModifiersState) -> bool {
    use winit::keyboard::NamedKey;
    if matches!(key, Key::Named(NamedKey::Enter)) {
        return true;
    }
    // Paste, in both spellings — the character never arrives, so nothing below
    // would catch it.
    if koru_keys::has_primary(mods)
        && koru_editor::input::keymap::ctrl_letter(key, text) == Some('v')
    {
        return true;
    }
    if koru_keys::has_primary(mods) || koru_keys::has_foreign_primary(mods) || mods.alt_key() {
        return false;
    }
    text.is_some_and(|t| t.chars().any(|c| !c.is_control()))
}

#[cfg(test)]
mod account_startup_tests;
#[cfg(test)]
mod manage_tests;
#[cfg(test)]
mod product_flags_tests;
#[cfg(test)]
mod tests;

/// The real dispatch for the three entry points that can mutate the document.
///
/// Inherent rather than on the trait, and **named the same**, so `impl View`
/// can delegate and the drain that follows every one of them cannot be skipped
/// by an early return. See the note on the trait impl.
impl MainView {
    /// Document ids still reachable in this workspace — live entries and the
    /// trash. A file pill whose entity is absent from this set is gone.
    fn entity_live(&self) -> std::collections::HashSet<String> {
        let mut live = self.entity_trashed.clone();
        if let Some(entries) = &self.entries {
            for entry in entries {
                live.insert(entry.id.to_storage());
            }
        }
        live
    }

    /// Open a document leaf and apply the explore caption/trash/live cache.
    ///
    /// Unlabelled `AttrValue::Document` pills (Notion MD import) paint as
    /// "unavailable" until `set_entity_titles` runs. `set_entries` only pushes
    /// when the listing *changes*, so a tab opened after entries are stable
    /// would keep that fallback forever without this.
    pub(super) fn document_leaf(&self, ws: &WorkspaceDoc, root: NodeId) -> Leaf {
        let mut leaf = Leaf::document(ws, root);
        let live = self.entity_live();
        leaf.set_entity_titles(&self.entity_titles, &self.entity_trashed, &live);
        leaf.set_schedule_pills(&self.schedule_pill_presentations());
        leaf
    }

    /// Switch the explore pane to Files mode, select the file, and scroll it into view.
    fn reveal_file_in_explorer(&mut self, file: koru_core::DocumentId) {
        if !self.sidebar.is_open() {
            koru_workspace_host::attach_debug!("reveal_file_in_explorer opening sidebar");
            self.toggle_sidebar();
        }
        let node = koru_core::NodeId::from_uuid(file.as_uuid());
        self.explore.show_files();
        self.explore_stale |= self.explore.reveal(node);
        self.explore.set_selected(Some(node));
        koru_workspace_host::attach_debug!(
            "reveal_file_in_explorer done file={} node={} mode={:?} selected={:?}",
            file.to_storage(),
            node.to_storage(),
            self.explore.navigator_mode(),
            self.explore.selected().map(|n| n.to_storage())
        );
    }

    /// Rebuild every composite caption from its two live halves.
    ///
    /// Cheap by construction: it walks `entity_headings`, which holds one entry
    /// per distinct deep link on screen rather than one per document in the
    /// workspace. A composite key whose document half is not known yet is
    /// skipped rather than half-rendered — the pill keeps the caption it was
    /// authored with, which already reads `page#heading`.
    fn recompose_deep_link_captions(&mut self) {
        let composed: Vec<(String, String)> = self
            .entity_headings
            .iter()
            .filter_map(|(key, heading)| {
                let title = self
                    .entity_titles
                    .get(koru_editor::pillinfo::document_of(key))?;
                Some((key.clone(), format!("{title}#{heading}")))
            })
            .collect();
        for (key, caption) in composed {
            self.entity_titles.insert(key, caption);
        }
    }

    /// Ask for an outline for every document an open tab links into.
    ///
    /// On the two-second context tick rather than on a change notification,
    /// because there is no notification to have: the app hears about manifest
    /// changes, not about a heading being retyped, and a target in another
    /// workspace says nothing at all. One indexed query per linked document is
    /// the price of the caption being right.
    fn refresh_deep_link_outlines(&mut self) {
        let mut wanted = std::collections::HashSet::new();
        for tab in self.tabs_mut().iter_mut() {
            if let TabView::Leaf(leaf) = &mut tab.view {
                wanted.extend(leaf.deep_link_documents());
            }
        }
        self.deep_link_documents = wanted;
        for document in &self.deep_link_documents {
            self.entity_search
                .request_outline(koru_store_core::ids::DocumentId::from_uuid(
                    document.as_uuid(),
                ));
        }
    }

    /// Push context into an open `@` picker and send its newest query to the
    /// shared search worker. Called after every editor gesture; it is a no-op
    /// when no entity picker is open.
    fn sync_entity_picker(&mut self) -> bool {
        let Some((_, _, _, current)) = self.pane_context.as_ref() else {
            return false;
        };
        let Some(current) = *current else {
            return false;
        };
        let workspaces = self.entity_workspaces.clone();
        // What a bare `@#` links into: this page's own headings. Named here
        // rather than inside the picker because the picker knows nothing about
        // tabs or manifests.
        let here = self.active_document.map(|document| {
            koru_editor::components::entity_picker::DocumentChoice {
                target: koru_editor::trigger::DocumentTarget {
                    document,
                    node: None,
                },
                title: self
                    .entries
                    .as_ref()
                    .and_then(|entries| entries.iter().find(|entry| entry.id == document))
                    .map(|entry| entry.title.clone())
                    .unwrap_or_default(),
                breadcrumb: String::new(),
                is_file: false,
            }
        });
        let (request, outline, new_page, open, move_selection, changed) = match self.active_view() {
            TabView::Leaf(leaf) => {
                let mut changed = leaf.set_entity_context(current, workspaces);
                changed |= leaf.set_entity_current_document(here);
                let request = leaf.take_entity_search_request();
                let outline = leaf.take_heading_request();
                let new_page = leaf.take_wants_new_page();
                let open = leaf.take_wants_entity_open();
                let move_selection = leaf.take_wants_move_selection();
                (request, outline, new_page, open, move_selection, changed)
            }
            _ => (None, None, None, None, None, false),
        };
        let moved = move_selection.is_some();
        if let Some((destination, payload)) = move_selection {
            self.request_selection_move(destination, payload);
        }
        if let Some(document) = outline {
            self.entity_search
                .request_outline(koru_store_core::ids::DocumentId::from_uuid(
                    document.as_uuid(),
                ));
        }
        let opened = open.is_some();
        if let Some((target, where_)) = open {
            let match_text = target.node.map(|_| String::new());
            tracing::info!(
                target: "koru::document_link",
                document = %target.document.to_storage(),
                node = ?target.node.map(|node| node.to_storage()),
                disposition = ?where_,
                route = "store-document-location",
                "document link opened"
            );
            match where_ {
                OpenWhere::BackgroundTab => {
                    if let Some(i) = self.tabs().showing_document(target.document).or_else(|| {
                        self.tabs()
                            .showing(koru_core::NodeId::from_uuid(target.document.as_uuid()))
                    }) {
                        inputdebug::write(format_args!(
                            "[Koru][input] view entity background-tab already-showing i={i}"
                        ));
                        self.ensure_tab_visible(i);
                    } else {
                        inputdebug::write(format_args!(
                            "[Koru][input] view entity background-tab open-background"
                        ));
                        self.ask_pane(koru_workspace_host::PaneRequest::OpenBackground {
                            document: target.document,
                            node: target.node,
                        });
                    }
                }
                OpenWhere::NewWindow => {
                    self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
                        target: koru_workspace_host::NavigationTarget::Document {
                            document: target.document,
                            node: target.node,
                        },
                        match_text,
                        disposition: koru_workspace_host::SearchTargetDisposition::NewWindow,
                    });
                }
                OpenWhere::SameTab | OpenWhere::NewTab => {
                    let land = if where_ == OpenWhere::SameTab
                        && !self.tabs().active_tab().is_reusable()
                    {
                        if self.focus_showing_document(target.document) {
                            inputdebug::write(format_args!(
                                "[Koru][input] view entity same-tab already-showing"
                            ));
                            None
                        } else {
                            inputdebug::write(format_args!(
                                "[Koru][input] view entity same-tab pin-redirect=NewTab"
                            ));
                            Some(OpenWhere::NewTab)
                        }
                    } else {
                        Some(where_)
                    };
                    if let Some(land) = land {
                        self.pending_lands.push_back((target.document, land));
                        self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
                            target: koru_workspace_host::NavigationTarget::Document {
                                document: target.document,
                                node: target.node,
                            },
                            match_text,
                            disposition:
                                koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                        });
                    }
                }
                OpenWhere::Reuse => {
                    self.record_jump();
                    self.ask_pane(koru_workspace_host::PaneRequest::Navigate {
                        target: koru_workspace_host::NavigationTarget::Document {
                            document: target.document,
                            node: target.node,
                        },
                        match_text,
                        disposition: koru_workspace_host::SearchTargetDisposition::CurrentWindow,
                    });
                }
            }
        }
        // A move picker with nothing typed lists this window's recents, not
        // the search service's idea of them — the service only remembers
        // search-driven navigations, so its empty-query answer is not what a
        // person means by "recent". Its request is dropped; the container's
        // own list is the answer.
        let recents_mode = matches!(self.active_view(), TabView::Leaf(leaf) if {
            let leaf: &Leaf = leaf;
            leaf.entity_wants_recents()
        });
        let mut fed = false;
        if recents_mode {
            let hits = self.recent_document_choices();
            if let TabView::Leaf(leaf) = self.active_view() {
                // Compare-first, or this per-gesture sync would report a
                // change every time and pin the redraw loop.
                fed = leaf.set_entity_recents(hits);
            }
        }
        let sent = request.is_some();
        if let Some(request) = request {
            if !(recents_mode && request.query.trim().is_empty()) {
                let scope = request
                    .workspaces
                    .into_iter()
                    .map(|id| koru_store_core::ids::WorkspaceId::from_uuid(id.as_uuid()))
                    .collect();
                self.entity_search.request(request.query, scope);
            }
        }
        if let Some(title) = new_page {
            return self.new_page_from_entity(title) || changed;
        }
        changed || sent || opened || moved || fed
    }

    /// The window's recents as picker rows — most recent first, without the
    /// document the picker is open over (moving a selection into the page it
    /// is already on is a reorder, not a move worth offering first).
    fn recent_document_choices(
        &self,
    ) -> Vec<koru_editor::components::entity_picker::DocumentChoice> {
        self.recent_documents
            .iter()
            .filter(|(id, _)| Some(*id) != self.active_document)
            .map(
                |(id, title)| koru_editor::components::entity_picker::DocumentChoice {
                    target: koru_editor::trigger::DocumentTarget {
                        document: *id,
                        node: None,
                    },
                    title: title.clone(),
                    breadcrumb: String::new(),
                    is_file: false,
                },
            )
            .collect()
    }

    /// Forward a committed selection move to the shell, in koru-core terms.
    fn request_selection_move(
        &mut self,
        destination: koru_editor::view_state::MoveSelectionDestination,
        payload: koru_editor::view_state::MoveSelectionPayload,
    ) {
        use koru_editor::view_state::{MoveSelectionDestination, MoveSelectionPayload};
        let payload = match payload {
            MoveSelectionPayload::Rows(ids) => koru_workspace_host::MoveSelectionPayload::Rows(ids),
            MoveSelectionPayload::Inline { runs, attrs } => {
                koru_workspace_host::MoveSelectionPayload::Inline { runs, attrs }
            }
        };
        match destination {
            MoveSelectionDestination::NewPage {
                title,
                follow,
                child,
            } => {
                // The shell will land this window on the new page, and a
                // shell-announced landing records nothing — so the step Back
                // returns to is taken here, at the gesture, the same moment
                // `new_page_from_entity` takes its. The spot's caret row is
                // about to be moved away with the selection; `restore_spot`
                // still lands on the document and reapplies the scroll.
                // A Ctrl+Enter move stays put, so there is no jump to record.
                if follow {
                    self.record_jump();
                }
                self.ask_pane(
                    koru_workspace_host::PaneRequest::MoveSelectionToNewDocument {
                        title,
                        payload,
                        follow,
                        child,
                    },
                );
            }
            MoveSelectionDestination::Existing { target, title } => {
                self.ask_pane(koru_workspace_host::PaneRequest::MoveSelectionToDocument {
                    document: target.document,
                    node: target.node,
                    title,
                    payload,
                });
            }
        }
    }

    /// Hand any arrived outline to the picker waiting for it.
    fn poll_entity_headings(&mut self) -> bool {
        let mut changed = false;
        while let Some(outline) = self.entity_search.poll_outline() {
            let document = koru_core::DocumentId::from_uuid(outline.document.as_uuid());
            let headings: Vec<koru_editor::components::entity_picker::HeadingChoice> = outline
                .headings
                .into_iter()
                .filter_map(|heading| {
                    Some(koru_editor::components::entity_picker::HeadingChoice {
                        // A node id the app cannot parse names nothing it can
                        // land on, so it is dropped rather than offered.
                        node: koru_core::NodeId::parse(&heading.node)?,
                        level: heading.level,
                        text: heading.text,
                    })
                })
                .collect();
            // The cache first: an outline asked for by the caption refresh has
            // no picker waiting for it, and the picker's answer is equally good
            // for the captions.
            let document_key = document.to_storage();
            let mut fresh: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            for heading in &headings {
                fresh.insert(
                    koru_editor::pillinfo::entity_key(
                        &document_key,
                        Some(&heading.node.to_storage()),
                    ),
                    heading.text.clone(),
                );
            }
            // Drop what this document no longer has, so a deleted heading falls
            // back to its authored caption rather than staying stale forever.
            self.entity_headings.retain(|key, _| {
                koru_editor::pillinfo::document_of(key) != document_key || fresh.contains_key(key)
            });
            let moved = fresh.iter().any(|(key, text)| {
                self.entity_headings.get(key).map(String::as_str) != Some(text.as_str())
            });
            self.entity_headings.extend(fresh);
            if moved {
                self.recompose_deep_link_captions();
                let live = self.entity_live();
                let entity_titles = self.entity_titles.clone();
                let entity_trashed = self.entity_trashed.clone();
                for tab in self.tabs_mut().iter_mut() {
                    if let TabView::Leaf(leaf) = &mut tab.view {
                        changed |= leaf.set_entity_titles(&entity_titles, &entity_trashed, &live);
                    }
                }
            }
            if let TabView::Leaf(leaf) = self.active_view() {
                changed |= leaf.set_headings(document, headings, outline.projected);
            }
        }
        changed
    }

    /// Apply the newest versioned entity-search answer to the active picker.
    fn poll_entity_picker(&mut self) -> bool {
        let Some(update) = self.entity_search.poll() else {
            return false;
        };
        self.entity_workspaces = update
            .workspaces
            .into_iter()
            .map(
                |workspace| koru_editor::components::entity_picker::WorkspaceChoice {
                    profile: koru_core::ProfileId::from_uuid(workspace.profile.as_uuid()),
                    profile_name: workspace.profile_name,
                    id: koru_core::WorkspaceId::from_uuid(workspace.id.as_uuid()),
                    name: workspace.name,
                },
            )
            .collect();
        for document in &update.documents {
            let key = document.document.to_storage();
            self.entity_titles
                .insert(key.clone(), document.title.clone());
            self.entity_trashed.remove(&key);
        }
        let hits = update
            .documents
            .into_iter()
            .filter(|document| {
                self.active_document
                    != Some(koru_core::DocumentId::from_uuid(
                        document.document.as_uuid(),
                    ))
            })
            .map(
                |document| koru_editor::components::entity_picker::DocumentChoice {
                    target: koru_editor::trigger::DocumentTarget {
                        document: koru_core::DocumentId::from_uuid(document.document.as_uuid()),
                        node: None,
                    },
                    title: document.title,
                    breadcrumb: document.breadcrumb,
                    is_file: document.kind == "file"
                        || document.kind == koru_core::explore::KIND_FILE,
                },
            )
            .collect();
        let current = self
            .pane_context
            .as_ref()
            .and_then(|(_, _, _, current)| *current);
        let choices = self.entity_workspaces.clone();
        let applied = match self.active_view() {
            TabView::Leaf(leaf) => {
                let context =
                    current.is_some_and(|current| leaf.set_entity_context(current, choices));
                leaf.set_entity_search_results(hits, update.unavailable) || context
            }
            _ => false,
        };
        let titles = self.entity_titles.clone();
        let trashed = self.entity_trashed.clone();
        let live = self.entity_live();
        for tab in self.tabs_mut().iter_mut() {
            if let TabView::Leaf(leaf) = &mut tab.view {
                leaf.set_entity_titles(&titles, &trashed, &live);
            }
        }
        // A catalog may have arrived with the result and initialized the
        // default scope; send the resulting request if so.
        self.sync_entity_picker() || applied
    }

    /// The caption buttons' chips, each teaching the chord that does the same
    /// thing.
    ///
    /// **Every chord here comes from the registry**, through
    /// `koru_keys::show_hint`. They were four `"Ctrl+…"` literals, which is the
    /// mistake the document's menus had already been swept for: on macOS all
    /// four taught a modifier this app does not use. Going through the seam also
    /// means tooltips can stop teaching chords without touching these rows.
    fn caption_tooltip(&self, hit: Hit) -> Option<TooltipContent> {
        if self.group_drag().is_some() || self.sidebar.animating() {
            return None;
        }
        let cap = self.drawn_caption();
        let hint = |id| koru_keys::show_hint(id, koru_keys::Section::Tooltip);
        match hit {
            Hit::ToggleSidebar if !self.toggle_tip_muted => {
                let label = if self.sidebar.is_open() {
                    "Hide sidebar"
                } else {
                    "Show sidebar"
                };
                Some(
                    TooltipContent::new(1, koru_shell_chrome::caption::sidebar_toggle(cap), label)
                        .with_optional_shortcut(hint(koru_keys::ShortcutId::ToggleExplore)),
                )
            }
            Hit::ToggleContext if CONTEXT_TOGGLE => Some(
                TooltipContent::new(
                    2,
                    koru_shell_chrome::caption::context_toggle_with_host_action(
                        cap,
                        WINDOW_BUTTONS,
                        CONTEXT_TOGGLE,
                        reminder_bell(),
                    ),
                    "Toggle context",
                )
                .with_optional_shortcut(hint(koru_keys::ShortcutId::ToggleContext)),
            ),
            Hit::Search => Some(
                TooltipContent::new(
                    3,
                    koru_shell_chrome::caption::search_button_with_host_action(
                        cap,
                        WINDOW_BUTTONS,
                        CONTEXT_TOGGLE,
                        reminder_bell(),
                    ),
                    "Search",
                )
                .with_optional_shortcut(hint(koru_keys::ShortcutId::Search)),
            ),
            Hit::HostAction if reminder_bell() => Some(TooltipContent::new(
                6,
                koru_shell_chrome::caption::host_action_with_context(
                    cap,
                    WINDOW_BUTTONS,
                    CONTEXT_TOGGLE,
                    reminder_bell(),
                ),
                koru_i18n::t(crate::format::lang(), koru_i18n::Key::ReminderAttention),
            )),
            Hit::NewTab => {
                let slot = tabs::new_tab(self.strip(), &self.tab_widths(), self.tab_scroll());
                (slot.w > 0.0).then(|| {
                    TooltipContent::new(4, slot, "Add…")
                        .with_shortcut(koru_keys::label_of(koru_keys::ShortcutId::AddContent))
                })
            }
            // A hovered history row answers with the exact moment behind its
            // relative time. Keys sit past the caption buttons' 1..4 so a
            // pointer sliding between rows re-anchors while staying warm.
            Hit::Context => self
                .context_view
                .history_tooltip(self.regions.context)
                .map(|(row, anchor, detail)| TooltipContent::new(100 + row as u64, anchor, detail)),
            _ => None,
        }
    }

    /// The tooltip for the view icon under the pointer, if one is: its name
    /// and its `Alt+digit`, from the registry. Keys past the caption's and
    /// well short of the history rows' 100+.
    fn navigator_mode_tooltip(&self, x: f32, y: f32) -> Option<TooltipContent> {
        if self.group_drag().is_some() || self.sidebar.animating() {
            return None;
        }
        self.explore
            .navigator_mode_rects(self.regions.sidebar)
            .into_iter()
            .enumerate()
            .find(|(_, (_, rect))| rect.w > 0.0 && rect.contains(x, y))
            .map(|(i, (mode, rect))| {
                TooltipContent::new(40 + i as u64, rect, mode.label()).with_optional_shortcut(
                    koru_keys::show_hint(mode.shortcut(), koru_keys::Section::Tooltip),
                )
            })
    }

    /// `Alt+1..8`: show one of the explore column's views, opening the column
    /// when it is closed — the view was asked for by name.
    pub(super) fn show_navigator(&mut self, mode: crate::panes::explore::NavigatorMode) -> bool {
        if !self.sidebar.is_open() {
            self.toggle_sidebar();
        }
        match mode {
            crate::panes::explore::NavigatorMode::Documents => self.explore.show_documents(),
            crate::panes::explore::NavigatorMode::Files => self.explore.show_files(),
            crate::panes::explore::NavigatorMode::Tasks => {
                self.explore.show_tasks();
                self.explore.focus_tree(self.regions.sidebar);
            }
            // **Focus enters on open**: the chords that show a list put the
            // keyboard in it, on the chat in front of you when it is there.
            crate::panes::explore::NavigatorMode::Chats => {
                self.explore.show_chats();
                self.explore.focus_tree(self.regions.sidebar);
            }
            crate::panes::explore::NavigatorMode::Agents => self.explore.show_agents(),
            crate::panes::explore::NavigatorMode::Artifacts => self.explore.show_artifacts(),
            crate::panes::explore::NavigatorMode::Canvases => self.explore.show_canvases(),
            crate::panes::explore::NavigatorMode::Socials => {
                self.open_socials_tab();
                self.explore.focus_tree(self.regions.sidebar);
            }
        }
        self.explore_stale = true;
        true
    }

    fn reminder_bell_rect(&self) -> Rect {
        koru_shell_chrome::caption::host_action_with_context(
            self.drawn_caption(),
            WINDOW_BUTTONS,
            CONTEXT_TOGGLE,
            reminder_bell(),
        )
    }

    fn mouse_move_rest(&mut self, x: f32, y: f32) -> bool {
        // Tab overflow thumb darkens only on the pill — entering/leaving it is a
        // paint change even when `Hit` stays Tab/TabScroll.
        let thumb_tone = self.tab_scrollbar_active_at(self.pointer.0, self.pointer.1)
            != self.tab_scrollbar_active_at(x, y);
        self.pointer = (x, y);
        if let Some(dialog) = self.error.as_mut() {
            return dialog.mouse_move(&mut self.engine, self.viewport, x, y);
        }
        if let Some(dialog) = self.import_dialog.as_mut() {
            return dialog.mouse_move(&mut self.engine, self.viewport, x, y);
        }
        if let Some((dialog, _)) = self.confirm.as_mut() {
            // The scrim is modal over the entire window. Update button hover,
            // then claim even its empty area so the editor underneath neither
            // keeps an I-beam nor receives a hover/mouse transition.
            dialog.mouse_move(&mut self.engine, self.viewport, x, y);
            return true;
        }
        // Before content: the chip sits over the document, and without this the
        // leaf keeps an I-beam and its own hover under a surface you cannot type
        // in. Same reason the *In Trash* band claims the pointer below.
        if self.toast_mouse_move(x, y) {
            return true;
        }
        if self.update_newer_modal_open() {
            return self.update_newer_mouse_move(x, y);
        }
        if self.update_mouse_move(x, y) {
            return true;
        }
        if let Some(dialog) = self.new_document.as_mut() {
            return dialog.mouse_move(&mut self.engine, self.viewport, x);
        }
        if let Some(dialog) = self.snapshot_prompt.as_mut() {
            return dialog.mouse_move(&mut self.engine, self.viewport, x);
        }
        if let Some(dialog) = self.link_device.as_mut() {
            return dialog.mouse_move(&mut self.engine, self.viewport, x, y);
        }
        if let Some(m) = self.workspace_icon.as_mut() {
            return m.mouse_move(&mut self.engine, self.viewport, x, y);
        }
        // **`set_sidebar_width` only — never `relayout`.** That calls
        // `invalidate_size` on every other tab, and per mouse-move it is exactly
        // the O(tabs) cost the responsiveness rules forbid. The content catches
        // up once, on release.
        if let Some(grab) = self.sidebar_drag {
            return self.set_sidebar_width(x - grab);
        }
        if let Some(grab) = self.ai_drag {
            return self.set_ai_width(self.viewport.0 - (x - grab));
        }
        // The same contract as the two pane edges above: the ratio is cheap
        // geometry, so the hairline and the strips follow the pointer live;
        // the views keep their layout until mouse-up pays one reflow.
        if self.divider_drag.is_some() {
            let content = self.content();
            let d = self.divider_drag.as_mut().unwrap();
            let pos = match d.axis {
                panes::SplitAxis::Vertical => x - d.grab,
                panes::SplitAxis::Horizontal => y - d.grab,
            };
            let moved = self.panes.drag_divider(content, &d.path, pos);
            d.moved |= moved;
            return true;
        }
        if self.workspace_rename.is_some() {
            // A row that is no longer there — the popover closed underneath —
            // reads as "outside", which commits, the same as clicking away.
            let row = self
                .workspace_rename_row()
                .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            if let Some(f) = self.workspace_rename.as_mut()
                && f.mouse_move(&mut self.engine, row, x)
            {
                return true;
            }
        }
        if self.new_list.is_some() {
            let field = self
                .new_list_field_rect()
                .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            if let Some(f) = self.new_list.as_mut()
                && f.mouse_move(&mut self.engine, field, x)
            {
                return true;
            }
        }
        // **Innermost flyout first.** Nested overlaps the submenu, the submenu
        // overlaps the parent; asking the parent first would track a row under
        // the flyout and close it by hovering the row that opened it.
        if self.in_nested(x, y) {
            let vp = self.viewport;
            if let Some((_, m)) = self.nested.as_mut() {
                let new = m.item_at(vp, x, y);
                let changed = new != m.hover;
                m.hover = new;
                return changed;
            }
        }
        if self.in_submenu(x, y) {
            let vp = self.viewport;
            let new = self
                .submenu
                .as_ref()
                .map(|(_, m)| m.item_at(vp, x, y))
                .unwrap_or(None);
            let changed = self.submenu.as_ref().is_some_and(|(_, m)| new != m.hover);
            if let Some((_, m)) = self.submenu.as_mut() {
                m.hover = new;
            }
            let opened = match new {
                Some(i) => self.open_nested_submenu(i),
                None => self.nested.take().is_some(),
            };
            return changed || opened;
        }
        // While the context menu is up it tracks its own hover; nothing under it
        // is hovered, so a row does not stay lit behind the menu.
        if let Some(menu) = self.menu.as_mut() {
            let new = menu.item_at(self.viewport, x, y);
            // A stepper's cells light one at a time, so the control under the
            // pointer is tracked beside the row under it rather than instead
            // of it: the row a control belongs to is never itself hoverable.
            let control = menu.control_at(self.viewport, x, y);
            let changed = new != menu.hover || control != menu.control_hover;
            menu.hover = new;
            menu.control_hover = control;
            // A glyph is a good button and a poor label: the chip says what the
            // control does and which chord does the same thing. Built here,
            // while `menu` is still borrowed, so the rest of this arm is free to
            // reach for `self` again.
            let tip = control.and_then(|(row, c)| {
                let action = *menu.items().get(row)?;
                let (label, chord) = context_menu::control_tooltip(action, c)?;
                let anchor = menu.control_rect(self.viewport, row, c)?;
                Some(
                    TooltipContent::new(MENU_CONTROL_TIP + c as u64, anchor, label)
                        .with_optional_shortcut(chord),
                )
            });
            let row = new;
            // Also what takes the `⋯` button's own chip down as the pointer
            // crosses into the menu it opened — this arm used to return without
            // touching the tooltip at all, leaving that one hanging under the
            // panel.
            let tip_changed = self.tooltip.set_hover(tip, Instant::now());
            let changed = changed || tip_changed;
            // Hovering a row with a submenu opens it; hovering any other row
            // closes whatever was open. A menu where the flyout lingered over an
            // unrelated row would cover the row you moved to.
            self.nested = None;
            let opened = match row {
                Some(i) => self.open_submenu(i),
                None => self.submenu.take().is_some(),
            };
            return changed || opened;
        }
        let _ = self.reminder_creation.pointer_move(&mut self.engine, x, y);
        if self.reminder_creation.is_open() || self.reminder_creation.cursor(x, y).is_some() {
            self.active_view().clear_hover();
            self.pointer_in_content = false;
            self.tooltip.clear();
            return true;
        }
        let bell = self.reminder_bell_rect();
        let reminder_moved = self.reminder_shell.pointer_move(self.viewport, bell, x, y);
        if bell.contains(x, y) {
            self.active_view().clear_hover();
            self.pointer_in_content = false;
            let changed = self.hover != Some(Hit::HostAction);
            self.hover = Some(Hit::HostAction);
            let tooltip = self.caption_tooltip(Hit::HostAction);
            let tooltip_changed = self.tooltip.set_hover(tooltip, Instant::now());
            return changed || tooltip_changed || reminder_moved;
        }
        if self
            .reminder_shell
            .blocks_pointer(self.viewport, bell, x, y)
        {
            self.active_view().clear_hover();
            self.pointer_in_content = false;
            self.hover = if bell.contains(x, y) {
                Some(Hit::HostAction)
            } else {
                None
            };
            self.tooltip.clear();
            return true;
        }
        // A tree drag follows the pointer wherever it goes — out of the column,
        // the way a leaf row-drag does — so it is settled before anything that
        // would move a highlight under it.
        if self.explore.pointer_active() {
            // A document carried into the pane area gets the pane answers a
            // dragged tab would: the slot marker over a lane, the split
            // preview at an edge.
            self.update_explore_drop(x, y);
            return reminder_moved
                || self
                    .explore
                    .mouse_move(self.regions.sidebar, self.viewport, x, y);
        }
        // **The popover first, for the same reason `mouse_down` asks first.** It
        // is wider than the column it hangs off, so `regions::hit` answers
        // `Content` for its right-hand half — and the branch below would then
        // call `explore.clear_hover()` and drop the highlight off the row the
        // pointer is actually on. That is what made a profile's remove box
        // vanish as you moved towards it: the ✕ only draws on the hovered row,
        // and the hover was being cleared by the very move that approached it.
        if self.explore.popover_open()
            && self
                .explore
                .popover_panel(self.explore_popover_area(), self.viewport)
                .is_some_and(|r| r.contains(x, y))
        {
            // The leaf under it must let go too, or a row stays lit behind a
            // menu — the same rule the tab menu follows a few lines above.
            let left_content = self.pointer_in_content;
            if left_content {
                self.active_view().clear_hover();
                self.pointer_in_content = false;
            }
            // The popover belongs to the pane wherever it happens to overhang,
            // so the cursor comes from the pane rather than from the document
            // it is floating over.
            self.hover = Some(Hit::Sidebar);
            let area = self.explore_popover_area();
            let moved = self.explore.mouse_move(area, self.viewport, x, y);
            return moved || left_content || reminder_moved;
        }

        let hit = self.hit_at(x, y);
        let hover_changed = self.hover != Some(hit);
        self.hover = Some(hit);
        // Leaving the toggle re-arms its hint: pointing at it again is a fresh
        // question, and deserves a fresh answer.
        if hit != Hit::ToggleSidebar {
            self.toggle_tip_muted = false;
        }
        // The carried tab travels with the pointer, and swaps once *it* — not
        // the pointer — is past the middle of a neighbour. The two are only the
        // same when the tab happened to be grabbed dead centre, and it is the
        // tab the eye is judging against.
        if self.group_drag().is_some() {
            if let Some(d) = self.group_drag_mut().as_mut() {
                d.x = x;
                d.y = y;
            }
            // A tear-off freezes the reorder: the tab is on its way out, not to
            // a new slot, so the neighbours stop shuffling around it.
            if !self.drag_tearing() {
                let from = self.group_drag().as_ref().unwrap().index;
                if let Some((_, cx, w)) = self.carried() {
                    let to = tabs::drop_index(
                        self.strip(),
                        &self.tab_widths(),
                        self.tab_scroll(),
                        from,
                        cx + w * 0.5,
                    );
                    if to != from {
                        self.move_tab(from, to);
                    }
                }
            }
            // Once a draggable tab has torn far enough off its strip, switch to
            // the cross-pane drag mode that can split or — documents only —
            // pop out.
            if self.group_drag().as_ref().is_some_and(|d| {
                self.is_pane_draggable(d.index) && (d.y - d.origin_y).abs() >= panes::TEAR_OFF_PX
            }) {
                if let Some(d) = self.group_drag().clone() {
                    let source = self.panes.focused_path().clone();
                    let tab_id = self.tabs().get(d.index).map(|t| t.id);
                    if let Some(tab_id) = tab_id {
                        *self.group_drag_mut() = None;
                        self.pane_drag = Some(panes::PaneDrag {
                            source,
                            tab_id,
                            grab_dx: d.grab_dx,
                            origin_y: d.origin_y,
                            x,
                            y,
                        });
                        let _ = self.update_split_intent(x, y);
                    }
                }
            }
            // Always a repaint: the tab moved, or the tear indicator follows.
            return true;
        }

        if let Some(d) = self.pane_drag.as_mut() {
            d.x = x;
            d.y = y;
            let _ = self.update_split_intent(x, y);
            self.update_drop_slot(x, y);
            return true;
        }

        if let Some(grab) = self.tab_scroll_drag() {
            self.ensure_tab_widths();
            let strip = self.strip();
            if let Some(thumb) = tabs::scroll_thumb(strip, &self.tab_widths(), self.tab_scroll()) {
                let pointer = x - grab + thumb.w * 0.5;
                self.set_tab_scroll(tabs::scroll_from_pointer(
                    strip,
                    &self.tab_widths(),
                    pointer,
                ));
            }
            return true;
        }

        // Each column has its own hover, and a pointer that has left one must
        // not leave a row or a section lit behind.
        let explore_moved = if hit == Hit::Sidebar {
            self.explore
                .mouse_move(self.regions.sidebar, self.viewport, x, y)
        } else {
            self.explore.clear_hover()
        };
        // A document dragged out of the column gets the pane answers a dragged
        // tab would: the slot marker over a lane, the split preview at an edge.
        self.update_explore_drop(x, y);
        let ai_moved = if self.ai_visible() && self.ai_rect().contains(x, y) {
            self.ai_chat.mouse_move(x, y)
        } else {
            false
        };
        // A live scrollbar drag follows the pointer out of the column, the
        // way every other drag in the app does.
        let context_moved = if hit == Hit::Context || self.context_view.is_dragging() {
            self.context_view.mouse_move(self.regions.context, x, y)
        } else {
            self.context_view.clear_hover()
        };
        let chrome_moved = explore_moved || ai_moved || context_moved;

        // After the chrome hovers, deliberately: a history row's tooltip is
        // read from the context column's fresh hover, and asking before the
        // update would anchor it one row behind the pointer.
        // The page-furniture context toggle is not a caption hit — check it
        // before the caption table, or a pointer on the title icon is
        // `Hit::Content` and teaches nothing.
        let tooltip = if let Some(content) = self.document_ai_tooltip() {
            Some(content)
        } else if let Some(content) = self.document_context_tooltip() {
            Some(content)
        } else if hit == Hit::Sidebar {
            // The Explore header's search button, and the three view icons
            // beside it — icon-only controls, so each carries its name and
            // the chord that does the same (`DESIGN.md` §8: one interaction
            // clock, flat chip).
            let button = self.explore.search_button(self.regions.sidebar);
            if button.w > 0.0 && button.contains(x, y) {
                Some(
                    TooltipContent::new(3, button, "Search").with_optional_shortcut(
                        koru_keys::show_hint(
                            koru_keys::ShortcutId::Search,
                            koru_keys::Section::Tooltip,
                        ),
                    ),
                )
            } else {
                self.navigator_mode_tooltip(x, y)
            }
        } else {
            self.caption_tooltip(hit)
        };
        let tooltip_changed = self.tooltip.set_hover(tooltip, Instant::now());

        let inside = hit == Hit::Content && !self.ai_rect().contains(x, y);
        let dragging = self.tabs().active_tab().view.is_dragging();
        // Leaving the content rect drops the leaf's hover exactly once — its
        // `hover_at` ignores `x`, so a row would otherwise stay lit under a
        // pointer parked over the chrome.
        if !inside && self.pointer_in_content && !dragging {
            self.active_view().clear_hover();
            self.pointer_in_content = false;
            return true;
        }
        self.pointer_in_content = inside;

        // A drag deliberately follows the pointer outside the band: a text
        // selection, a row reorder, a thumb and an image resize all do.
        if !inside && !dragging {
            return reminder_moved
                || hover_changed
                || chrome_moved
                || thumb_tone
                || tooltip_changed;
        }
        let (lx, ly) = self.to_leaf(x, y);
        let moved = match self.active_view() {
            TabView::Welcome(w) => w.mouse_move(lx, ly),
            TabView::Manage(w) => View::mouse_move(w, lx, ly),
            TabView::Trash(t) => View::mouse_move(t, lx, ly),
            TabView::Tasks(t) => View::mouse_move(t, lx, ly),
            TabView::Reminders(r) => View::mouse_move(r, lx, ly),
            TabView::Settings(s) => View::mouse_move(s, lx, ly),
            TabView::Feedback(f) => View::mouse_move(f, lx, ly),
            TabView::Diff(d) => View::mouse_move(d, lx, ly),
            TabView::AiChat(a) => View::mouse_move(a, lx, ly),
            TabView::Socials(a) => View::mouse_move(a, lx, ly),
            TabView::Session(s) => View::mouse_move(s, lx, ly),
            TabView::Run(r) => View::mouse_move(r, lx, ly),
            TabView::Result(r) => View::mouse_move(r, lx, ly),
            TabView::Canvas(c) => View::mouse_move(c, lx, ly),
            TabView::Leaf(l) => Leaf::mouse_move(l, lx, ly),
        };
        reminder_moved || moved || hover_changed || chrome_moved || thumb_tone || tooltip_changed
    }
}

/// One reader of the Terminal settings for the whole process, started by the
/// first window's maintenance pass — so a terminal tab opened before anybody
/// visits Settings still draws the stored cursor. Never blocks.
fn poll_terminal_prefs() {
    static READER: std::sync::Mutex<Option<crate::terminal_prefs::Reader>> =
        std::sync::Mutex::new(None);
    let mut reader = READER.lock().unwrap_or_else(|held| held.into_inner());
    reader
        .get_or_insert_with(crate::terminal_prefs::Reader::new)
        .poll();
}
