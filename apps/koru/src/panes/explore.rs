// SPDX-License-Identifier: MPL-2.0

//! The explore column: the workspace's library as a tree you can open things
//! from.
//!
//! Rows are a **cache rebuilt from the document**, the same contract `Body` has
//! and for the same reason: the document is the truth, and a view is a
//! projection that can be thrown away and rebuilt whenever it changes.
//!
//! It shows the document-like landmarks in the *library* realm. People remain
//! available to references and assignee pickers, but are intentionally omitted
//! from this navigation tree. The `outline/` realm is what the permanent
//! Workspace tab already shows, so listing it here as well would be the same
//! rows twice.
//!
//! ## Two constraints worth knowing
//!
//! This crate must not depend on `koru-workspace`, so the "what is this node
//! called" lookup is inlined rather than calling `koru_workspace::name_of`.
//! It is four lines, and the alternative is a dependency edge from the chrome
//! to the workspace that nothing else needs.
//!
//! And rows are **culled to the visible band** before drawing, like
//! `rowlayout::visible_range` does for the editor: a library with a thousand
//! nodes must cost a screenful of layouts per frame, not a thousand.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use koru_core::explore::{ExploreEntry, KIND_CANVAS, KIND_DOCUMENT, KIND_FILE};
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_core::{DocumentId, Kind, NodeId, ProfileId, ProfileSummary, WorkspaceEntry, WorkspaceId};
use koru_draw::anim::ease_out_cubic as ease;
use koru_draw::geometry::Rect;
use koru_draw::text::TextEngine;
use koru_draw::theme;
use koru_draw::{icons, scene};
use koru_session_protocol::{
    ProcessState, ProjectId, ProjectSummary, SessionId, SessionProfileKey, SessionSummary,
    WorkspaceScope,
};
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko;
use winit::window::CursorIcon;

use super::Outcome;
use super::fold;
use super::{DropTarget, DropZone};
use koru_shell_chrome::tabs::{draw_icon, draw_icon_filled};
use koru_shell_chrome::title_icon::{PAGE_ICON_SLOT, draw_page_emoji, split_leading_emoji};

/// How far the pointer must travel before a press on a manifest row stops being
/// a click and becomes a drag. Small enough that an intent to drag reads as
/// one, large enough that a click on a row does not shuffle the tree.
const DRAG_ARM_PX: f32 = 4.0;

/// How long a press must be held before the cursor reveals the grab hand. The
/// hand is an affordance for "you may drag", and showing it on hover would imply
/// every manifest row is draggable furniture; showing it only after a held press
/// lets an ordinary click read as a click.
const DRAG_HOLD_MS: f32 = 100.0;

/// How long the pointer must linger on a collapsed node during a drag before it
/// springs open — long enough not to fire in passing, short enough to feel like
/// a deliberate pause.
const SPRING_OPEN_SECS: f32 = 0.3;

/// What a plain click on a row does when it turns out not to be a drag.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum ClickAction {
    /// Nothing beyond selecting. A file whose bytes are already here: a stray
    /// click must not launch another application. Double-click opens it.
    Select,
    /// Documents activate on a plain click.
    Open,
    /// A file listed here but held on another device. The click is the request
    /// for its bytes — which is the whole point of showing it as a placeholder.
    Download,
}

/// Whether this device holds a file's bytes.
///
/// A file's *reference* replicates with the workspace manifest; its payload does
/// not follow until somebody asks for it. So a device can list a file it has
/// never downloaded, and the row has to say which it is — otherwise clicking one
/// is a coin toss between instant and a wait on a peer that may be asleep.
///
/// Only files have this. A document's content arrives with its reference.
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub enum FileState {
    /// Local bytes, or not a file at all. The ordinary row.
    #[default]
    Present,
    /// Listed here, held elsewhere. Drawn as a placeholder; a click fetches it.
    Remote,
    /// A fetch is running for this row.
    Fetching,
}

/// One line of the tree.
#[derive(Clone, PartialEq, Debug)]
pub struct Row {
    pub node: NodeId,
    pub depth: u16,
    pub label: String,
    pub kind: Kind,
    pub mime: Option<String>,
    pub icon: Option<String>,
    pub has_children: bool,
    pub expanded: bool,
    pub file_state: FileState,
    /// The note attached to this file, if it has one — a `DocumentId` in
    /// `NodeId`'s clothing, the same bridge [`node`](Self::node) is. Clicking
    /// the note glyph opens it as a tab; nothing else carries this.
    pub note: Option<NodeId>,
}

/// Which projection the left navigator is showing. Files share the workspace
/// manifest with documents, but they are deliberately a mode of the same pane
/// rather than a tab: navigation furniture must not become workspace content.
///
/// **Tasks is the third**, and the same argument decides it: a queue is an
/// index over the workspace, not a thing in it. It is the one mode whose rows
/// are not the manifest — see [`rail`] — which is why [`kind`](Self::kind)
/// answers `None` for it rather than naming a manifest kind nothing carries.
/// `Agents` projects the **session host's catalog** — the device's sessions
/// grouped by project, the same catalog Koru Terminal's tree shows — rather
/// than the workspace manifest. The pane holds no project truth of its own:
/// every gesture that changes anything (create, rename, delete a project;
/// move a session between projects) is reported as an [`Outcome`] and the
/// container sends the typed backend verb — the tree reflects the
/// `ProjectsChanged`/catalog that comes back on the wake path (Q-E4/Q-E6,
/// ADR-264).
#[derive(Copy, Clone, Default, PartialEq, Eq, Debug)]
pub enum NavigatorMode {
    #[default]
    Documents,
    Files,
    Tasks,
    /// **Every AI chat, newest first**, with who answered it (AGENTUX I5).
    /// Right of Tasks (R3). Like Artifacts its rows are not the manifest:
    /// they are the AI service's, read through the gateway by
    /// [`koru_chats_ui::ChatsSurface`] — the same component the standalone
    /// chat window hosts as its floating pane. Opening, starting and the
    /// question before a delete are reported as outcomes; the container
    /// answers them.
    Chats,
    Agents,
    /// What the workspace's delegated runs produced, and what they left in
    /// their `.artifacts/<RunId>/` folders that is not a result yet (D19,
    /// D29, D37). Like `Tasks` its rows are not the manifest: they are the
    /// Artifact Store's, read through the gateway by
    /// [`koru_artifacts_ui::ArtifactsSurface`], which owns everything in
    /// this mode below the heading — including the presses, because unlike
    /// the tasks rail it can perform what it offers.
    Artifacts,
    /// **Every canvas in the workspace, one flat list** (CANVAS P1.3). The
    /// seventh icon, after Artifacts. Manifest-backed like Documents and Files
    /// — the rows are the manifest's `canvas` entries — but never a tree: a
    /// canvas holds no pages, and nothing here is reordered or nested. See
    /// [`canvases`] for the section's rules.
    Canvases,
    /// **Social posts, channel queues and the publishing schedule** (ADR-452).
    /// The eighth icon, after Canvases. Its rows are the Socials store's, read
    /// by [`koru_socials_ui`]; opening one is reported for the container.
    Socials,
}

impl NavigatorMode {
    /// The manifest kind this mode filters by — for the two manifest-backed
    /// modes. `None` for Tasks, whose rows are a queue over the workspace, and
    /// `None` for Agents, whose rows come from the session catalog; neither is
    /// in the manifest.
    fn kind(self) -> Option<&'static str> {
        match self {
            Self::Documents => Some(KIND_DOCUMENT),
            Self::Files => Some(KIND_FILE),
            Self::Canvases => Some(KIND_CANVAS),
            Self::Tasks | Self::Chats | Self::Agents | Self::Artifacts | Self::Socials => None,
        }
    }

    /// The stable token a session stores. Never the enum's discriminant and
    /// never a bool: `navigator_files=1` was a flag for a two-state control,
    /// and a third mode is exactly what a flag cannot carry.
    pub fn token(self) -> &'static str {
        match self {
            Self::Documents => "documents",
            Self::Files => "files",
            Self::Tasks => "tasks",
            Self::Chats => "chats",
            Self::Agents => "agents",
            Self::Artifacts => "artifacts",
            Self::Canvases => "canvases",
            Self::Socials => "socials",
        }
    }

    /// The word the icon's tooltip says — the registry's label for the same
    /// chord, so the tooltip and the shortcut sheet agree.
    pub fn label(self) -> &'static str {
        match self {
            Self::Documents => "Documents",
            Self::Files => "Files",
            Self::Tasks => "Tasks",
            // Keyed, for Artifacts' reason below: a new label is not a new
            // English literal.
            Self::Chats => koru_i18n::t(crate::format::lang(), koru_i18n::Key::ChatsTitle),
            Self::Agents => "Agents",
            // The one label of the five that goes through `koru-i18n`: the
            // four above it are literals this plan did not open, and a
            // fifth English literal beside a keyed one would be the wrong
            // half to add. See `docs/contributing/techdebt/README.md`.
            Self::Artifacts => koru_i18n::t(crate::format::lang(), koru_i18n::Key::ArtifactsTitle),
            Self::Canvases => koru_i18n::t(crate::format::lang(), koru_i18n::Key::CanvasesTitle),
            Self::Socials => {
                koru_i18n::socials::t(crate::format::lang(), koru_i18n::socials::Key::Title)
            }
        }
    }

    /// The chord that shows this view, by its registry id.
    pub fn shortcut(self) -> koru_keys::ShortcutId {
        match self {
            Self::Documents => koru_keys::ShortcutId::ShowDocuments,
            Self::Files => koru_keys::ShortcutId::ShowFiles,
            Self::Tasks => koru_keys::ShortcutId::ShowTasks,
            Self::Chats => koru_keys::ShortcutId::ShowChats,
            Self::Agents => koru_keys::ShortcutId::ShowAgents,
            Self::Artifacts => koru_keys::ShortcutId::ShowArtifacts,
            Self::Canvases => koru_keys::ShortcutId::ShowCanvases,
            Self::Socials => koru_keys::ShortcutId::ShowSocials,
        }
    }

    /// Every mode, in the order the icons sit and the digits count. The rects,
    /// the hit test and [`ExploreView::modes_right`] all read this, so adding a
    /// mode is one entry here rather than four widths to keep in step.
    pub const ALL: [Self; 8] = [
        Self::Documents,
        Self::Files,
        Self::Tasks,
        Self::Chats,
        Self::Agents,
        Self::Artifacts,
        Self::Canvases,
        Self::Socials,
    ];

    /// Whether this build shows the mode (`koru_features`,
    /// guide/contributing/FEATURE-FLAGS.md): Tasks with tasks, Chats with Koru AI,
    /// Agents and Artifacts with agent views, Canvases with canvas, Socials with
    /// socials. Documents and Files always.
    pub fn shown_in_product(self) -> bool {
        match self {
            Self::Documents | Self::Files => true,
            Self::Tasks => koru_features::tasks(),
            Self::Chats => koru_features::ai(),
            Self::Agents | Self::Artifacts => koru_features::agent_views(),
            Self::Canvases => koru_features::canvas(),
            Self::Socials => koru_features::socials(),
        }
    }

    /// [`ALL`](Self::ALL) minus the modes this build hides, in the same
    /// order. What the icons, their rects, the hit test and the
    /// accessibility tree are built from. A hidden mode leaves no gap: the
    /// icons close up, while its `Alt+digit` simply stops resolving
    /// (`mainview::keys::Chord::shown_in_product`).
    pub fn visible() -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|mode| mode.shown_in_product())
            .collect()
    }

    /// A stored token, or `None` for one this build does not know — which
    /// reopens on Documents rather than guessing.
    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "documents" => Some(Self::Documents),
            "files" => Some(Self::Files),
            "tasks" => Some(Self::Tasks),
            "chats" => Some(Self::Chats),
            "agents" => Some(Self::Agents),
            "artifacts" => Some(Self::Artifacts),
            "canvases" => Some(Self::Canvases),
            "socials" => Some(Self::Socials),
            _ => None,
        }
    }
}

/// Which page of the switcher is showing, or the library's action menu.
///
/// **One switcher, two pages.** Profiles and Workspaces used to be two
/// popovers, one under each half of the header. They are now the two pages of
/// one: choosing a Profile slides its workspaces in from the right, and a back
/// row slides out again. The header's halves still open their own page.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PopoverKind {
    /// One Profile's workspaces — the open one unless a Profile was chosen.
    Workspaces,
    /// Every profile on this machine.
    Profiles,
    /// The compact action menu at the right end of the library heading.
    LibraryActions,
}

/// The pane's open popover.
///
/// **A floating panel, not a section that pushes the library down.** It was the
/// latter first, and the list jumping 180px every time you glanced at the
/// workspace name is what decided it — a menu should cover what is under it and
/// give it back, which is what every other menu in the app already does.
///
/// `None` is the common case and costs nothing, the same shape `MainView::menu`
/// has.
///
/// ## It outlives being shut
///
/// A popover that vanished on the frame it was dismissed would have nothing to
/// fade, so `closing` keeps it in `Some` until its fade reaches zero. Everything
/// that takes input therefore asks [`ExploreView::live`] rather than
/// `popover.is_some()` — a menu on its way out must not answer for a click meant
/// for what is behind it, which is precisely the bug an "is it there" test would
/// miss.
struct Popover {
    kind: PopoverKind,
    /// Which row the pointer or the keyboard is on.
    hover: Option<usize>,
    /// Offset of the scrollable item band between the fixed heading and footer.
    scroll: f32,
    /// 0 gone, 1 fully open. Eased, so the chevron and the panel move together
    /// off one number.
    progress: f32,
    /// When the current fade began and the progress it began from. `None` at
    /// rest — and that is what [`ExploreView::popover_animating`] reports, so
    /// the shell's fast wake-up ends on its own (ADR-26).
    fade: Option<(Instant, f32)>,
    /// On its way out. Dropped entirely when `progress` reaches 0.
    closing: bool,
    /// Whose workspaces the Workspaces page lists. `None` is the open Profile.
    browse: Option<ProfileId>,
    /// Where the slide between the pages is: 0 Profiles, 1 Workspaces.
    page: f32,
    /// When the current slide began and the page it began from. `None` at rest,
    /// for the same reason `fade` is.
    slide: Option<(Instant, f32)>,
}

impl Popover {
    /// Whether `kind` is the page a header half (or its shortcut) would open.
    /// The Workspaces half means the **open** Profile's; `browse` outlives a
    /// Back so the page can slide out, which must not stop the Profiles page
    /// from counting as showing.
    fn showing(&self, kind: PopoverKind) -> bool {
        self.kind == kind && (kind != PopoverKind::Workspaces || self.browse.is_none())
    }
}

/// The page value the switcher slides to for `kind`.
fn page_of(kind: PopoverKind) -> f32 {
    if kind == PopoverKind::Workspaces {
        1.0
    } else {
        0.0
    }
}

/// One row of an open popover, in the order it is listed.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PopoverRow {
    /// The Workspaces page's first row: the Profile it lists, and the way back
    /// to the Profiles page.
    Back,
    WorkspacesHeading,
    /// An index into the browsed Profile's workspace list.
    Workspace(usize),
    NewWorkspace,
    /// "Import content…" — the import formats, as a menu.
    ImportContent,
    /// "Manage workspaces…" — the workspace manager tab.
    ManageWorkspaces,

    ProfilesHeading,
    /// An index into the profile list — **every** profile the machine knows.
    /// Choosing one opens its workspaces; it does not switch to it.
    Profile(usize),
    /// The hairline between a page's *New* row and its commands.
    Divider,
    /// "New profile" — the Profile manager's create form.
    NewProfile,
    /// Pick a folder and make a profile in it.
    // Test-driven only since the ADR-262 move; kept as the documented seam.
    #[cfg_attr(not(test), allow(dead_code))]
    AddAccount,
    /// Pick a folder that already holds one. What makes "losing the list costs
    /// nothing" reachable without a command line.
    // Test-driven only since the ADR-262 move; kept as the documented seam.
    #[cfg_attr(not(test), allow(dead_code))]
    OpenProfile,
    /// Open the singleton Profile and account-management tab.
    ManageProfiles,
    /// "Link a device…" — global device synchronization, not workspace sharing.
    LinkDevice,
    /// A mode-dependent action from the library heading's `…` menu.
    LibraryAction(actions::Action),
}

impl PopoverRow {
    /// Whether the keyboard may land on it. The headings and the divider are
    /// furniture, and the arrows step over them the way `next_enabled` steps
    /// over a dimmed menu item.
    fn selectable(self) -> bool {
        !matches!(
            self,
            Self::WorkspacesHeading | Self::ProfilesHeading | Self::Divider
        )
    }
}

/// One caret mid-turn.
///
/// `progress` is 0 pointing right (shut) to 1 pointing down (open), which is a
/// quarter turn of one glyph rather than two glyphs swapped — the same trick the
/// header's chevron uses, and the reason either can be partway.
struct Turn {
    node: NodeId,
    progress: f32,
    /// When this turn began and the progress it began from. Elapsed time against
    /// a duration, never a per-frame step, or a dropped frame strands it.
    started: Instant,
    from: f32,
    /// Where it is going: 1 opening, 0 shutting.
    to: f32,
}

/// A manifest row the pointer has hold of.
///
/// Seeded on a press over a document or file row and **latent** until the
/// pointer crosses [`DRAG_ARM_PX`]: under that, the release is a click (it
/// opens the node, or toggles the selection under Ctrl). Past it, the drag
/// [`arms`](Self::armed): the carried rows lift and follow the pointer while
/// their neighbours slide, the way a tab reorder does.
///
/// `index` is the grab row's current slot and follows it as the cache reorders,
/// so hit-testing and the drop-zone math read a cache that already reflects the
/// last swap. The carried set is a snapshot of the selection at arm time (plus
/// the grab), so a Ctrl-built multi-select moves together.
struct RowDrag {
    /// The row under the pointer that began the drag.
    grab: NodeId,
    /// Where the grab row currently sits in `rows`. Follows the reorder.
    index: usize,
    /// The press's offset into the row, so the block travels from the point it
    /// was grabbed rather than snapping its top edge under the cursor.
    grab_dy: f32,
    /// The press's `y`, for the arm threshold and to anchor the block.
    origin_y: f32,
    /// When the press began, for the held-press cursor reveal.
    pressed_at: Instant,
    /// Where the pointer is now, in window space. Also drives auto-scroll.
    y: f32,
    /// The column rect at the last move, for edge-band auto-scroll.
    area: Rect,
    /// Whether the threshold has been crossed. False until it is.
    armed: bool,
    /// Whether the press is selection-only (Ctrl or Shift). Only the no-arm
    /// release reads it; a plain click still opens its row.
    select_only: bool,
    /// What a plain click on this row means, decided at press time.
    click: ClickAction,
    /// The nodes being carried, snapshotted at arm time. Empty until then, so
    /// a latent drag costs nothing.
    carried: HashSet<NodeId>,
    /// The carried roots in document order. Kept separately because a
    /// multi-drag projects all of them into the grab row's single visual slot.
    carried_order: Vec<NodeId>,
    /// Entries that actually travel, including descendants inside each
    /// carried root. Distinct from `carried_order.len()`: one selected root can
    /// visibly carry several documents beneath it.
    item_count: usize,
    /// The last drop zone applied, so a relocate only happens when it changes.
    last: Option<DropTarget>,
    /// A collapsed node the pointer is resting on as a Nest target, and when
    /// the rest began — held past [`SPRING_OPEN_SECS`] it springs open.
    spring: Option<(NodeId, Instant)>,
}

/// Which part of the pane's header, if any.
///
/// The header is two targets, not one: the avatar answers for *who*, and the
/// name and chevron for *where*. Being an enum from the start is what made
/// adding the second a variant rather than a rewrite of every caller.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum HeaderPart {
    /// The avatar — opens the profiles.
    Avatar,
    /// The name and the chevron — open the workspaces.
    Workspace,
}

/// Every rect in a workspace row of the switcher, derived from the row alone.
///
/// See [`ExploreView::workspace_row_parts`], which is the only thing that
/// computes these.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct WorkspaceRowParts {
    /// The rounded tile carrying the workspace's colour and emoji.
    pub chip: Rect,
    /// What is left for the name, before the *Pinned* lane.
    pub name: Rect,
    /// Where a pinned workspace's pin mark sits. A mark, not a control: pin
    /// and unpin live in the workspace manager.
    pub pin: Rect,
}

/// Every rect in the pane's header, derived from the pane's rect alone.
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct HeaderRects {
    /// The whole band. Drawing uses it; hit-testing splits it in two.
    pub row: Rect,
    /// The avatar chip or picture, square, centred in `row`.
    pub avatar: Rect,
    /// What a press on the avatar answers for — the circle plus the padding
    /// around it, because a 22px target is a small thing to have to hit.
    pub avatar_hit: Rect,
    /// What is left for the workspace name, inside [`name_hit`].
    ///
    /// [`name_hit`]: HeaderRects::name_hit
    pub name: Rect,
    /// The workspace half's own box — everything after the gap. Its own control,
    /// which is why it has its own rect rather than being "the row minus the
    /// avatar".
    pub name_hit: Rect,
    /// The chevron at the right end, which is what says the row is clickable.
    pub chevron: Rect,
    /// The hairline under the band.
    pub divider: Rect,
}

/// A row pinned to the floor of the column, below the library.
///
/// **A list, not a special case.** Trash is the first of these and there will be
/// more — the column's standing places, as against the library's contents. They
/// are reserved out of the rows' band, so the library scrolls *above* them, and
/// adding one grows the stack upward without a second draw path.
///
/// They are ordinary nav rows: no separator, no panel, the same hover fill and
/// the same icon lane as a document row. A band with a rule over it would read
/// as a different kind of thing, which is the opposite of what they are.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FooterRow {
    Trash,
}

/// Stable-within-the-window keys for schedule presentation rows. A fired run
/// also appears in the Runs section, so neither its RunId nor the ScheduleId
/// alone can be used as a unique tree row identity.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum ScheduleNode {
    Schedule(koru_delegate_core::ScheduleId),
    Rule(koru_delegate_core::ScheduleId),
    Next(koru_delegate_core::ScheduleId),
    Owner(koru_delegate_core::ScheduleId),
    State(koru_delegate_core::ScheduleId),
    Run(koru_delegate_core::ScheduleId, koru_delegate_core::RunId),
    Catalog,
}

/// Every rect inside a library row. See [`ExploreView::row_parts`].
#[derive(Copy, Clone, PartialEq, Debug)]
pub struct RowParts {
    /// Zero-sized for a leaf, so clicking where a caret would be does not
    /// expand nothing.
    pub caret: Rect,
    pub icon: Rect,
    /// What is left for the label, between the icon and the note lane (or the
    /// `…` when the row has no note).
    pub label: Rect,
    /// The note glyph at the right of a file row that has one. **Zero-sized
    /// when the row has no note**, so hit-testing it answers nothing.
    pub note: Rect,
    /// The `…` that opens this row's menu. Drawn on hover; **its width is
    /// reserved always.**
    pub dots: Rect,
}

#[derive(Default)]
pub struct ExploreView {
    mode: NavigatorMode,
    mode_hover: Option<NavigatorMode>,
    /// The tasks mode's content. Composed rather than a second view, because
    /// the header, the mode row and the column's own fill are the same in all
    /// three modes — a sibling would have to be handed all of them.
    rail: rail::TasksRail,
    /// Whether the artifacts column's reader has been started. The
    /// container starts it the first time the column is shown, because it is
    /// the only thing that knows this window's `--root` — and a column
    /// nobody has opened dials nothing.
    artifacts_started: bool,
    /// The artifacts mode's content, composed for the rail's reason.
    ///
    /// **It performs what it offers.** The rail reports a request because it
    /// cannot open a tab; this one registers a file and asks the gateway for
    /// a provenance walk through its own client, so its presses answer
    /// `true`/`false` and the container routes nothing.
    artifacts: koru_artifacts_ui::ArtifactsSurface,
    /// Whether the Chats view's reader has been started — the first time it
    /// is shown, by the container, which knows this window's `--root`.
    chats_started: bool,
    /// The Chats mode's content, composed for the rail's reason. Unlike the
    /// artifacts column it reports what it cannot do itself — open a tab,
    /// start a chat, ask before a delete — as [`Outcome`]s.
    chats: koru_chats_ui::ChatsSurface,
    socials: Option<koru_socials_ui::SharedSurface>,
    /// The manifest is already the projection supplied by the Store. Keeping
    /// this small cache lets the mode buttons swap lists without another IPC
    /// round trip or a document walk.
    manifest_entries: Vec<ExploreEntry>,
    /// Files whose bytes this device does not hold, and the ones being fetched
    /// right now.
    ///
    /// A side table rather than a field on [`ExploreEntry`]: that type mirrors
    /// the Store's `DocumentEntry` field for field, and locality is a fact about
    /// *this device* rather than about the workspace, so it has no business in a
    /// replicated shape. Answering it means reading each file document's blob
    /// hash and comparing it against the local blob list, which is a worker's
    /// job — so it arrives here already decided, and never on the draw path.
    file_states: HashMap<NodeId, FileState>,
    // ---------------------------------------------------------- agents mode
    /// The session host's project registry, pushed by the container whenever
    /// the per-window backend delivers `BackendEvent::Projects`. Held here for
    /// the same reason `manifest_entries` is: the mode buttons swap lists
    /// without another round trip, and the catalog already arrived on the wake
    /// path — nothing here does IO.
    agent_projects: Vec<ProjectSummary>,
    /// The device's sessions, pushed on every catalog event the same way.
    agent_sessions: Vec<SessionSummary>,
    /// Bridged row key → the session it names. `Row::node` carries the
    /// session's UUID bytes (the same bridge the manifest rows use for
    /// `DocumentId`), and this map is how a press converts back without
    /// guessing from depth.
    agent_session_ids: HashMap<NodeId, SessionId>,
    /// **The workspace's delegated runs**, listed under their own heading
    /// beneath the projects. A run is not a session — it outlives every
    /// session it used, and it exists on devices that never ran one — so it
    /// is its own group rather than a row among the sessions (P1.16).
    agent_runs: Vec<koru_runs_ui::RunRow>,
    agent_run_ids: HashMap<NodeId, koru_delegate_core::RunId>,
    /// **The run rows that are still working**, so the draw asks a set rather
    /// than searching the listing per row. Rebuilt with the rows, and empty
    /// on every projection but Agents — which is what keeps the turning ring
    /// off a navigator that is not showing runs.
    agent_runs_working: HashSet<NodeId>,
    /// **The run rows that are stopped and waiting for a person** — an
    /// interrupted run, which offers Resume and is doing nothing until
    /// somebody presses it. A second set rather than a flag on the first,
    /// because these rows must never contribute to
    /// [`ExploreView::runs_turning`]: a pause mark does not move, and a
    /// surface that asked for frames to redraw a still glyph would be a
    /// navigator that never goes idle.
    agent_runs_paused: HashSet<NodeId>,
    /// When the working rings started turning. **One anchor for the whole
    /// list**, so six runs turn together instead of each starting from
    /// wherever it happened to appear; `None` the moment nothing is working,
    /// which is what makes [`ExploreView::animating`] fall back to `false`.
    agent_runs_spin_started: Option<Instant>,
    /// Scheduled work is a third typed catalog in the Agents projection. Its
    /// rows arrive already localized from `ScheduleIndex`; this pane owns only
    /// tree geometry and interaction roles.
    agent_schedules: Vec<crate::schedule_index::ScheduleRow>,
    agent_schedule_state: crate::schedule_index::CatalogState,
    /// False until the index has supplied its first state. Keeps a cold
    /// Explore fixture (and a window before its first maintenance pass) from
    /// pretending it has already queried schedules.
    agent_schedules_known: bool,
    agent_schedule_ids: HashMap<NodeId, koru_delegate_core::ScheduleId>,
    /// Schedule rows whose durable lifecycle is paused. Kept separate from
    /// localized state words so icon choice never parses display text.
    agent_schedules_paused: HashSet<NodeId>,
    agent_schedule_run_ids: HashMap<NodeId, koru_delegate_core::RunId>,
    agent_schedule_metadata: HashSet<NodeId>,
    agent_schedule_metadata_ids: HashMap<NodeId, koru_delegate_core::ScheduleId>,
    /// Presentation identities for rows whose domain identity is not unique in
    /// this tree (one Run can appear under Runs and under a Schedule).
    agent_schedule_nodes: HashMap<ScheduleNode, NodeId>,
    /// Folded Agents groups. A set of its own rather than `collapsed`,
    /// because that one is pruned against the manifest or the document and
    /// captured into the session settings — a project id lives in neither, so
    /// sharing the set would forget every Agents fold on the next Documents
    /// rebuild.
    agent_collapsed: HashSet<NodeId>,
    rows: Vec<Row>,
    scroll: f32,
    /// Offset from the thumb top while the library scrollbar is grabbed.
    scroll_drag: Option<f32>,
    /// The pointer is in the scrollbar's wider hit lane.
    scrollbar_hover: bool,
    hover: Option<usize>,
    /// Which pinned footer row the pointer is on, if any.
    footer_hover: Option<FooterRow>,
    /// Which pinned row the active tab is showing, if any — the footer's
    /// counterpart to [`selected`](Self::selected).
    footer_active: Option<FooterRow>,
    /// The empty-state "Create document" button, as laid out by the last draw.
    /// `None` when the column is not empty, or before the first frame.
    empty_cta: Option<Rect>,
    cta_hover: bool,
    /// The empty-state "Subscribe to Koru Cloud" button under it, when the
    /// sentence offers Cloud. Laid out by the last draw, like `empty_cta`.
    empty_subscribe: Option<Rect>,
    subscribe_hover: bool,
    /// How many items are in the Trash, as of the last [`rebuild`](Self::rebuild).
    /// **Never asked per frame** — see where it is set.
    trash_count: usize,
    /// Which row's `…` menu is open, so the button stays visible while it is.
    menu_row: Option<NodeId>,
    /// Nodes whose children are hidden. Collapsed is the *exception* so that a
    /// newly-seen node appears rather than having to be found.
    collapsed: HashSet<NodeId>,
    /// Every row's parent, **including the ones a fold is hiding**.
    ///
    /// The rows cannot answer this: a document three folds deep contributes no
    /// row at all, which is precisely the case [`reveal`](Self::reveal) exists
    /// for. Recorded from the projection, before anything is filtered, in both
    /// rebuild paths.
    parents: HashMap<NodeId, NodeId>,
    /// A row to bring into view once the rows next hold it.
    ///
    /// Two steps, not one: [`reveal`](Self::reveal) can expand the folds
    /// straight away, but the row it wants has no index until the rebuild it
    /// just made necessary has run, and no `y` until a caller offers the
    /// column's rect.
    pending_reveal: Option<NodeId>,
    /// A Collapse-all or Expand-all press waiting for the next
    /// [`rebuild`](Self::rebuild), which is the first place after the click with
    /// a `WorkspaceDoc` in hand to walk.
    pending_fold: Option<fold::Fold>,
    /// Whether the library heading's `…` actions button is under the pointer.
    actions_hover: bool,
    search_hover: bool,
    /// The row the active tab is showing, so the tree says where you are.
    selected: Option<NodeId>,
    /// The row the **keyboard** is on, which is not the same thing.
    ///
    /// `selected` follows the active tab — it says *what you have open*.
    /// This says *what you are looking at with the arrows*, and the whole point
    /// of the two being separate is that moving through the tree does not open
    /// anything. Enter is what makes the two agree.
    ///
    /// `Some` doubles as **the tree holds the keyboard**: while it does, the
    /// arrows and Enter are the tree's rather than the document's. Clicking into
    /// the document, `Escape`, or opening a row all give that back — a list that
    /// keeps the arrows after you have left it is a list that has stolen them.
    ///
    /// A `NodeId` rather than a row index, because the rows are a cache rebuilt
    /// on every change: an index would point at a different node the moment
    /// anything above it expanded.
    focus: Option<NodeId>,

    // ---------------------------------------------------- header and popovers
    /// **Every** profile on the machine, not only the open one.
    profiles: Vec<ProfileSummary>,
    current_profile: Option<ProfileId>,
    receiving_profiles: HashSet<ProfileId>,
    workspaces: Vec<WorkspaceEntry>,
    /// Every Profile's workspaces from the Store catalog, so the switcher can
    /// list a Profile that is not open. The open one reads `workspaces`.
    catalog: HashMap<ProfileId, Vec<WorkspaceEntry>>,
    current: Option<WorkspaceId>,
    /// Which popover is open, and which of its rows is highlighted.
    popover: Option<Popover>,
    /// Carets part-way through their quarter turn, by node.
    ///
    /// **Only the ones actually moving.** A settled caret is not in here at all
    /// — its angle is implied by `expanded` — so a tree of a thousand nodes
    /// carries state for the one or two you have just clicked, and the list
    /// empties itself as they arrive. That is also what keeps
    /// [`animating`](Self::animating) honest: nothing to advance means nothing
    /// to redraw for.
    turns: Vec<Turn>,
    /// Which half of the header the pointer is on, if either.
    header_hover: Option<HeaderPart>,

    // ------------------------------------------------------------- drag-to-move
    /// Documents chosen with Ctrl+click or Shift+click. The same set is drawn,
    /// carried by a drag, and handed to a menu opened inside it.
    selection: HashSet<NodeId>,
    /// The fixed end of the next Shift+click range. A node id rather than an
    /// index because rebuilding the row cache can move every index.
    selection_anchor: Option<NodeId>,
    /// A press on a document row, latent until it moves past `DRAG_ARM_PX`.
    drag: Option<RowDrag>,
    /// The vertical slide rows make into their new slots during a drag. Empty
    /// at rest, so an idle pane carries nothing for it.
    slides: RowSlides,
    /// When [`advance_drag`](Self::advance_drag) last ran, so auto-scroll scales
    /// by elapsed time rather than per frame. `None` until a drag arms.
    drag_tick: Option<Instant>,
    /// Set by the container while the drag is over a pane: there the drop opens
    /// the document — the pane's slot marker or split preview is the answer —
    /// so the lifted row must not follow the pointer saying "move". The tree's
    /// ghost is only honest over the tree.
    carried_hidden: bool,
}

impl ExploreView {
    pub fn set_account_state(&mut self, state: &koru_agent_wire::AccountState) -> bool {
        let receiving = state
            .enrollments
            .iter()
            .filter_map(|row| ProfileId::parse(&row.profile_id))
            .collect();
        if self.receiving_profiles == receiving {
            return false;
        }
        self.receiving_profiles = receiving;
        true
    }
    pub fn new() -> Self {
        Self::default()
    }

    // Unconstructed since the ADR-262 move made dead code visible; a later wave of
    // the app-shell migration deletes or revives it.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn navigator_mode(&self) -> NavigatorMode {
        self.mode
    }

    pub fn show_documents(&mut self) {
        self.set_navigator_mode(NavigatorMode::Documents);
    }

    pub fn show_files(&mut self) {
        self.set_navigator_mode(NavigatorMode::Files);
    }

    pub fn show_tasks(&mut self) {
        self.set_navigator_mode(NavigatorMode::Tasks);
    }

    pub fn show_artifacts(&mut self) {
        self.set_navigator_mode(NavigatorMode::Artifacts);
    }

    pub fn show_chats(&mut self) {
        self.set_navigator_mode(NavigatorMode::Chats);
    }

    pub fn show_socials(&mut self) {
        self.set_navigator_mode(NavigatorMode::Socials);
    }

    pub fn showing_socials(&self) -> bool {
        self.mode == NavigatorMode::Socials
    }

    pub fn socials(&self) -> Option<koru_socials_ui::SharedSurface> {
        self.socials.clone()
    }

    /// One authoring surface is shared by the navigator and its main tab.
    pub fn ensure_socials(
        &mut self,
        root: Option<std::path::PathBuf>,
    ) -> koru_socials_ui::SharedSurface {
        self.socials
            .get_or_insert_with(|| {
                let mut surface = koru_socials_ui::Surface::new(root);
                surface.set_browser_opener(koru_window::open_uri);
                std::rc::Rc::new(std::cell::RefCell::new(surface))
            })
            .clone()
    }

    fn socials_outcome(outcome: koru_socials_ui::NavigationOutcome) -> Outcome {
        match outcome {
            koru_socials_ui::NavigationOutcome::Ignored => Outcome::Ignored,
            koru_socials_ui::NavigationOutcome::Changed => Outcome::Consumed,
            koru_socials_ui::NavigationOutcome::Open => Outcome::OpenSocials,
        }
    }

    /// Give the Chats view its reader, once — and ask for the list, because
    /// the view boots when it is first shown.
    pub fn start_chats(&mut self, root: Option<std::path::PathBuf>) {
        if self.chats_started {
            return;
        }
        self.chats_started = true;
        // Started in place, not replaced: a `Ctrl+Shift+E` that arrived before
        // the reader did is remembered on this surface.
        self.chats.start(root);
        self.chats.refresh();
    }

    /// The Chats view as it stands. Test-only, for the artifacts column's
    /// reason.
    #[cfg(test)]
    pub fn chats(&self) -> &koru_chats_ui::ChatsSurface {
        &self.chats
    }

    pub fn chats_mut(&mut self) -> &mut koru_chats_ui::ChatsSurface {
        &mut self.chats
    }

    /// Whether the Chats view is what is showing.
    pub fn showing_chats(&self) -> bool {
        self.mode == NavigatorMode::Chats
    }

    /// Whether the Chats view owes an answer — the one reason the container
    /// keeps a wake clock for it.
    pub fn chats_awaiting(&self) -> bool {
        self.chats.awaiting()
    }

    /// **Show the Chats view on one chat, with the keyboard on it** — what
    /// `Ctrl+Shift+E` in an AI chat tab means. The mode switch and the
    /// surface's reveal in one call, so `rows_top` stays this pane's
    /// arithmetic.
    ///
    /// `false` when the list does not hold the chat yet; the surface lands on
    /// it when the list that does arrives.
    pub fn reveal_chat(&mut self, chat: koru_chats_ui::ChatId, area: Rect) -> bool {
        self.show_chats();
        let top = self.rows_top(area);
        self.chats.reveal(chat, area, top)
    }

    /// What the Chats surface asked for on the press or key just handled,
    /// as the outcome the container answers.
    fn chats_outcome(&mut self) -> Outcome {
        if let Some(chat) = self.chats.take_wants_open() {
            return Outcome::OpenAiChat(chat);
        }
        if self.chats.take_wants_new() {
            return Outcome::NewAiChat;
        }
        if let Some(chat) = self.chats.take_wants_delete() {
            return Outcome::DeleteAiChat(chat);
        }
        Outcome::Consumed
    }

    /// Give the artifacts column its reader, once.
    pub fn start_artifacts(&mut self, root: Option<std::path::PathBuf>) {
        if self.artifacts_started {
            return;
        }
        self.artifacts_started = true;
        self.artifacts = koru_artifacts_ui::ArtifactsSurface::new(root);
    }

    /// The artifacts column as it stands. **Test-only, and marked so**: the
    /// container drives it through `artifacts_mut`, and a public reader with
    /// no caller outside the suite is dead code that reads as API — the rail
    /// carries the same gate for the same reason.
    #[cfg(test)]
    pub fn artifacts(&self) -> &koru_artifacts_ui::ArtifactsSurface {
        &self.artifacts
    }

    pub fn artifacts_mut(&mut self) -> &mut koru_artifacts_ui::ArtifactsSurface {
        &mut self.artifacts
    }

    /// Whether the artifacts column is what is showing.
    pub fn showing_artifacts(&self) -> bool {
        self.mode == NavigatorMode::Artifacts
    }

    /// **Show the artifacts column on one version, with the keyboard on it.**
    ///
    /// The seam `Ctrl+Shift+E` on a result tab reaches: the mode switch and
    /// the surface's own reveal in one call, so `rows_top` — where this
    /// column's rows begin — stays this pane's arithmetic rather than the
    /// container's.
    ///
    /// `false` when the catalog holds no such version yet; the column is
    /// still showing, and the caller decides what to do about the keyboard.
    pub fn reveal_artifact(
        &mut self,
        version: koru_artifacts_ui::ArtifactVersionId,
        area: Rect,
    ) -> bool {
        self.show_artifacts();
        let top = self.rows_top(area);
        self.artifacts.reveal_version(version, area, top)
    }

    /// The tasks rail, for the container to push counts and lists into and to
    /// read the selection back out of.
    pub fn rail(&self) -> &rail::TasksRail {
        &self.rail
    }

    pub fn rail_mut(&mut self) -> &mut rail::TasksRail {
        &mut self.rail
    }

    /// Whether the tasks rail is what the column is showing.
    pub fn showing_tasks(&self) -> bool {
        self.mode == NavigatorMode::Tasks
    }

    /// Where the rail's rows begin — the same floor the library's rows use, so
    /// the two modes start on the same line under the mode row.
    pub fn rail_top(&self, area: Rect) -> f32 {
        self.rows_top(area)
    }

    /// Show the Agents catalog. `Ctrl+Shift+E` on a run tab is its caller in
    /// the container: revealing a run means this column, on that run's row.
    pub fn show_agents(&mut self) {
        self.set_navigator_mode(NavigatorMode::Agents);
    }

    /// Replace the session half of the Agents catalog. Returns whether the
    /// visible rows changed, so a caller can skip a repaint.
    ///
    /// Stored whatever the mode, so switching to Agents shows the catalog the
    /// window already holds; rebuilt only when Agents is the mode showing.
    pub fn set_agent_sessions(&mut self, sessions: Vec<SessionSummary>) -> bool {
        if self.agent_sessions == sessions {
            return false;
        }
        self.agent_sessions = sessions;
        if self.mode != NavigatorMode::Agents {
            return false;
        }
        self.rebuild_agent_rows();
        true
    }

    /// The project half, same contract as [`set_agent_sessions`].
    ///
    /// [`set_agent_sessions`]: Self::set_agent_sessions
    pub fn set_agent_projects(&mut self, projects: Vec<ProjectSummary>) -> bool {
        if self.agent_projects == projects {
            return false;
        }
        self.agent_projects = projects;
        if self.mode != NavigatorMode::Agents {
            return false;
        }
        self.rebuild_agent_rows();
        true
    }

    /// The project registry this navigator was last handed.
    ///
    /// Read by the delegation modal's working-scope picker, which offers the
    /// same projects the Agents navigator groups sessions under — one list, so
    /// a folder you can launch a session in and a folder you can delegate into
    /// cannot disagree.
    pub fn agent_projects(&self) -> &[ProjectSummary] {
        &self.agent_projects
    }

    /// Every session this window has heard of, running and not. What a run
    /// view's pass reads `handover` off, so the surface is told where a
    /// takeover stands by the session's own catalog rather than by guessing —
    /// and what the `+` menu filters into live shells and agents.
    pub fn agent_sessions(&self) -> &[SessionSummary] {
        &self.agent_sessions
    }

    /// The catalog's summary for one session, if this window has heard of it —
    /// what the container labels a tab with when the navigator opens one.
    pub fn agent_session(&self, session: SessionId) -> Option<&SessionSummary> {
        self.agent_sessions
            .iter()
            .find(|summary| summary.id == session)
    }

    fn set_navigator_mode(&mut self, mode: NavigatorMode) {
        // **A hidden mode is never shown**, whoever asks — a restored token, a
        // reveal, an accessibility action. Documents is the floor, the same
        // answer an unknown token gets.
        let mode = if mode.shown_in_product() {
            mode
        } else {
            NavigatorMode::Documents
        };
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        self.scroll = 0.0;
        self.focus = None;
        self.selection.clear();
        // The anchor is the fixed end of a selection that has just gone; a
        // Shift+click in the new mode extends from the row it lands on.
        self.selection_anchor = None;
        // **Coming back to the artifacts column asks again.** Nothing here
        // polls on a timer, so what is on screen is as old as the last time
        // it was looked at — and *coming back to a result you can read* is
        // the whole point of the column. Free before the reader exists: the
        // first showing asks through `set_workspace` instead.
        if mode == NavigatorMode::Artifacts {
            self.artifacts.refresh();
        }
        // **The Chats view boots when shown**, for the same reason; while it
        // is showing, the container refreshes it whenever a chat surface
        // says the list changed. Free before its reader exists.
        if mode == NavigatorMode::Chats {
            self.chats.refresh();
        }
        self.chats.clear_focus();
        if let Some(surface) = &self.socials {
            surface.borrow_mut().navigation_blur();
        }
        // A half-typed list name does not follow you out of the mode it was
        // being typed in. The container drops its field on the same press.
        self.rail.end_new_list();
        self.rail.clear_queue_focus();
        self.rail.clear_hover();
        self.rebuild_manifest_rows();
    }

    /// The fold set the mode's rows read. Agents keeps its own — see the
    /// field note on [`agent_collapsed`](Self::agent_collapsed) — so the
    /// caret and the arrows fold whichever tree is showing without the
    /// manifest prune forgetting the other's state.
    fn fold_set(&mut self) -> &mut HashSet<NodeId> {
        match self.mode {
            NavigatorMode::Agents => &mut self.agent_collapsed,
            // Tasks folds nothing of its own — the rail owns its rows — so it
            // shares the manifest's set rather than inventing a third.
            NavigatorMode::Documents
            | NavigatorMode::Files
            | NavigatorMode::Tasks
            // Chats and Artifacts fold nothing of the manifest either — each
            // surface owns its own rows, keyed by ids this set has no word
            // for. Canvases are a flat list: no row there has a fold.
            | NavigatorMode::Chats
            | NavigatorMode::Artifacts
            | NavigatorMode::Canvases
            | NavigatorMode::Socials => &mut self.collapsed,
        }
    }

    /// [`fold_set`](Self::fold_set), read-only.
    fn fold_set_ref(&self) -> &HashSet<NodeId> {
        match self.mode {
            NavigatorMode::Agents => &self.agent_collapsed,
            NavigatorMode::Documents
            | NavigatorMode::Files
            | NavigatorMode::Tasks
            | NavigatorMode::Chats
            | NavigatorMode::Artifacts
            | NavigatorMode::Canvases
            | NavigatorMode::Socials => &self.collapsed,
        }
    }

    /// Which nodes are collapsed, so a session can put them back.
    ///
    /// **Only the ones still in the tree** — but the filtering happens in
    /// [`rebuild`](Self::rebuild), against the *document*, not here against the
    /// rows.
    ///
    /// It used to be here: collapsing a node and then deleting it would
    /// otherwise leave its id in the set for the life of the process and then in
    /// the settings file forever, and the rows looked like the live answer. They
    /// are not. A node collapsed *behind* another collapsed node contributes no
    /// row at all, so Collapse all followed by a session capture threw away
    /// everything below the first fold. Liveness is a question about the
    /// document, and that is where it is now asked.
    pub fn collapsed(&self) -> Vec<NodeId> {
        self.collapsed.iter().copied().collect()
    }

    /// Put a remembered set back. Anything naming a node that has gone is
    /// dropped by [`collapsed`] on the way out, so nothing has to be trusted on
    /// the way in.
    ///
    /// [`collapsed`]: Self::collapsed
    pub fn set_collapsed(&mut self, ids: impl IntoIterator<Item = NodeId>) {
        self.collapsed = ids.into_iter().collect();
    }

    /// Learn the parent of every document, ahead of any rebuild.
    ///
    /// **A reveal happens before the rows are walked, not after.** Opening a
    /// search result unfolds the branch above it and *then* marks the pane
    /// stale, so `reveal` runs against whatever `parents` held at the time — and
    /// after a workspace switch that is the previous workspace's chains, in
    /// which the document being revealed does not appear at all. The walk found
    /// no parent, unfolded nothing, and the result opened behind a shut caret.
    ///
    /// Called from wherever the listing is pushed, which is one line earlier
    /// than the rebuild and always current.
    pub fn note_parents(&mut self, entries: &[ExploreEntry]) {
        self.parents.clear();
        for entry in entries {
            if let Some(parent) = entry.parent {
                self.parents.insert(
                    NodeId::from_uuid(entry.id.as_uuid()),
                    NodeId::from_uuid(parent.as_uuid()),
                );
            }
        }
    }

    /// Open every fold above `node`, and ask for its row to be scrolled to.
    ///
    /// **Opening a search result is the case this exists for.** A hit names a
    /// document anywhere in the library, and the tree it lives in is very often
    /// shut — so the tab opened, the row highlighted, and the highlight was
    /// three collapsed carets away from anything on screen.
    ///
    /// Returns whether a fold actually changed, because that is what makes the
    /// rows wrong: the caller marks the pane stale and rebuilds. The scroll is
    /// asked for either way and is applied by
    /// [`apply_reveal`](Self::apply_reveal), which is the first place after the
    /// rebuild that knows both the row's index and the column's rect.
    pub fn reveal(&mut self, node: NodeId) -> bool {
        self.pending_reveal = Some(node);
        let mut changed = false;
        let mut at = node;
        // Bounded by the map rather than trusted to terminate: the projection is
        // a tree, so the walk ends at a root — but a parent chain read from a
        // manifest is data, and a cycle in it would hang the pane rather than
        // draw it wrong.
        for _ in 0..self.parents.len() {
            let Some(parent) = self.parents.get(&at).copied() else {
                break;
            };
            changed |= self.collapsed.remove(&parent);
            at = parent;
        }
        changed
    }

    /// Scroll the row [`reveal`](Self::reveal) asked for into the band, if the
    /// rows now hold it. Returns whether the scroll moved.
    ///
    /// **The request survives a miss.** The rows may not have been rebuilt yet,
    /// the column may be shut, or the window may still be swapping to the
    /// workspace the document is in — all three are "not yet" rather than "not
    /// here", and a `NodeId` names one document, so a request that lands late
    /// lands on the right row.
    pub fn apply_reveal(&mut self, area: Rect) -> bool {
        let Some(node) = self.pending_reveal else {
            return false;
        };
        let Some(i) = self.rows.iter().position(|r| r.node == node) else {
            return false;
        };
        self.pending_reveal = None;
        let before = self.scroll;
        self.scroll_into_view(area, i);
        self.scroll != before
    }

    // ------------------------------------------------------ keyboard the tree

    /// Which node the keyboard is on, if the tree has it.
    pub fn focused(&self) -> Option<NodeId> {
        self.focus
    }

    /// Whether the tree holds the keyboard — and therefore whether the arrows
    /// and Enter belong to it rather than to the document.
    pub fn has_focus(&self) -> bool {
        // The artifacts column holds the keyboard on its own terms — its
        // rows are not `self.rows` — and *the column has the keyboard* is
        // one question however the mode answers it.
        if self.showing_tasks() {
            self.rail.has_queue_focus()
        } else {
            self.focus.is_some()
                || (self.mode == NavigatorMode::Artifacts && self.artifacts.has_keyboard())
                || (self.mode == NavigatorMode::Chats && self.chats.has_keyboard())
                || (self.showing_socials()
                    && self
                        .socials
                        .as_ref()
                        .is_some_and(|s| s.borrow().navigation_has_keyboard()))
        }
    }

    /// Give the keyboard back. Returns whether the tree had it, so a caller can
    /// decide whether the press that did it was worth a repaint.
    pub fn clear_focus(&mut self) -> bool {
        let artifacts = self.mode == NavigatorMode::Artifacts
            && self.artifacts.key(
                Rect::new(0.0, 0.0, 1.0, 1.0),
                0.0,
                koru_artifacts_ui::Press::Escape,
            );
        let chats = self.chats.clear_focus();
        let socials = self.socials.as_ref().is_some_and(|s| {
            let mut s = s.borrow_mut();
            let focused = s.navigation_has_keyboard();
            s.navigation_blur();
            focused
        });
        let rail_had_focus = self.rail.clear_queue_focus();
        let tree_had_focus = self.focus.take().is_some();
        tree_had_focus || artifacts || chats || socials || rail_had_focus
    }

    /// Take the keyboard, landing on the row that is already open if one is —
    /// so `Ctrl+E` starts you **on the document you are in** rather than at the
    /// top of a list you would have to walk back down. Scrolled to, because a
    /// focus ring below the fold is a focus you cannot see.
    pub fn focus_tree(&mut self, area: Rect) -> bool {
        if self.showing_socials() {
            return self.socials.as_ref().is_some_and(|surface| {
                surface.borrow_mut().navigation_focus();
                true
            });
        }
        if self.mode == NavigatorMode::Artifacts {
            let top = self.rows_top(area);
            return self.artifacts.focus_first(area, top);
        }
        // **Focus enters on open**: on the chat in front of you when the
        // list holds it, else the newest.
        if self.mode == NavigatorMode::Chats {
            let top = self.rows_top(area);
            return self.chats.focus_first(area, top) || self.chats.has_keyboard();
        }
        if self.showing_tasks() {
            let top = self.rows_top(area);
            self.focus = None;
            return self.rail.focus_queues(area, top);
        }
        if self.rows.is_empty() {
            return false;
        }
        let landing = self
            .selected
            .and_then(|id| self.rows.iter().position(|r| r.node == id))
            .unwrap_or(0);
        self.focus = Some(self.rows[landing].node);
        self.scroll_into_view(area, landing);
        true
    }

    /// Which row the keyboard is on, as an index into the visible rows.
    fn focus_index(&self) -> Option<usize> {
        let id = self.focus?;
        self.rows.iter().position(|r| r.node == id)
    }

    /// The row that contains row `i` — the nearest one above it at a shallower
    /// depth.
    ///
    /// Read off the rows rather than the document, because "the parent" here
    /// means *the row you would fold this one into*, which is a question about
    /// what is on screen. A node whose parent contributed no row of its own
    /// (`push` recurses through everything that is not a document) has no parent
    /// here either, which is the right answer: there is nothing to fold into.
    fn parent_of(&self, i: usize) -> Option<usize> {
        let depth = self.rows.get(i)?.depth;
        self.rows[..i].iter().rposition(|r| r.depth < depth)
    }

    /// The keys the tree answers to while it holds the keyboard.
    ///
    /// `None` for anything else, which **falls through** to the document — so
    /// typing still reaches what you are writing in even while the tree has the
    /// arrows. Only navigation is claimed, and only while there is a row to
    /// navigate from.
    ///
    /// **Nothing here opens anything.** Moving through a tree is looking, not
    /// choosing; `Enter` is the one key that hands over, and it is also the one
    /// that gives the keyboard back.
    pub fn nav_key(&mut self, area: Rect, key: &winit::keyboard::Key) -> Option<Outcome> {
        self.nav_key_with_modifiers(area, key, winit::keyboard::ModifiersState::empty())
    }

    pub fn nav_key_with_modifiers(
        &mut self,
        area: Rect,
        key: &winit::keyboard::Key,
        mods: winit::keyboard::ModifiersState,
    ) -> Option<Outcome> {
        use winit::keyboard::{Key, NamedKey};
        if self.showing_socials() {
            let surface = self.socials.as_ref()?;
            let outcome = surface.borrow_mut().navigation_key(key, mods);
            return match Self::socials_outcome(outcome) {
                Outcome::Ignored => None,
                outcome => Some(outcome),
            };
        }
        // The artifacts column claims the same keys and no others, and
        // answers them itself.
        if self.mode == NavigatorMode::Artifacts {
            let press = koru_artifacts_ui::Press::of(key)?;
            let top = self.rows_top(area);
            return self
                .artifacts
                .key(area, top, press)
                .then_some(Outcome::Consumed);
        }
        // The Chats view claims its contract's keys — the arrows, Home/End,
        // Enter, Delete, Escape — and no others.
        if self.mode == NavigatorMode::Chats {
            let press = koru_chats_ui::Press::of(key)?;
            let top = self.rows_top(area);
            if !self.chats.key(area, top, press) {
                return None;
            }
            return Some(self.chats_outcome());
        }
        if self.showing_tasks() {
            let top = self.rows_top(area);
            return self.rail.queue_key(area, top, key);
        }
        let i = self.focus_index()?;
        match key {
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                let next = if matches!(key, Key::Named(NamedKey::ArrowDown)) {
                    (i + 1).min(self.rows.len().saturating_sub(1))
                } else {
                    i.saturating_sub(1)
                };
                self.focus = Some(self.rows[next].node);
                self.scroll_into_view(area, next);
                Some(Outcome::Consumed)
            }
            // Open this one, or — if it is already open — step into it. The
            // second half is what makes Right a way *down* as well as a way
            // open, so one key walks a whole branch.
            Key::Named(NamedKey::ArrowRight) => {
                let row = self.rows[i].clone();
                if row.has_children && self.fold_set_ref().contains(&row.node) {
                    self.fold_set().remove(&row.node);
                    self.start_turn(row.node, true);
                } else if let Some(child) = self.rows.get(i + 1).filter(|c| c.depth > row.depth) {
                    self.focus = Some(child.node);
                    self.scroll_into_view(area, i + 1);
                }
                Some(Outcome::Consumed)
            }
            // Shut this one, or — if it is already shut — fold the branch it is
            // in and land on the row that contains it. Pressed repeatedly that
            // walks you out to the top, folding as it goes, which is what
            // "collapse to the nearest parent" means.
            Key::Named(NamedKey::ArrowLeft) => {
                let row = self.rows[i].clone();
                if row.has_children && !self.fold_set_ref().contains(&row.node) {
                    self.fold_set().insert(row.node);
                    self.start_turn(row.node, false);
                } else if let Some(p) = self.parent_of(i) {
                    let parent = self.rows[p].clone();
                    self.fold_set().insert(parent.node);
                    self.start_turn(parent.node, false);
                    self.focus = Some(parent.node);
                    self.scroll_into_view(area, p);
                }
                Some(Outcome::Consumed)
            }
            // **The only key that opens anything**, and it gives the keyboard
            // back at the same time: you asked for the document, so the document
            // should have it.
            Key::Named(NamedKey::Enter) => {
                let node = self.rows[i].node;
                self.focus = None;
                Some(self.open_row_outcome(node))
            }
            Key::Named(NamedKey::Escape) => {
                self.focus = None;
                Some(Outcome::Consumed)
            }
            _ => None,
        }
    }

    /// Scroll so row `i` is inside the band, moving as little as possible.
    fn scroll_into_view(&mut self, area: Rect, i: usize) {
        let top = self.rows_top(area);
        let band = (self.rows_bottom(area) - top).max(0.0);
        let y = i as f32 * theme::NAV_ROW_H;
        let above = y - self.scroll;
        if above < 0.0 {
            self.scroll += above;
        } else if above + theme::NAV_ROW_H > band {
            self.scroll += above + theme::NAV_ROW_H - band;
        }
        self.clamp_scroll(area);
    }

    /// Which pinned row the active tab is showing, if any.
    ///
    /// The footer's counterpart to [`set_selected`](Self::set_selected): the
    /// Trash tab makes the Trash row active, and it wears the same card an open
    /// document does.
    pub fn set_footer_active(&mut self, row: Option<FooterRow>) -> bool {
        let changed = self.footer_active != row;
        self.footer_active = row;
        changed
    }

    pub fn set_selected(&mut self, node: Option<NodeId>) -> bool {
        let changed = self.selected != node;
        self.selected = node;
        changed
    }

    /// Pick exactly one actionable row independently of the active tab. This
    /// is the persistent half of following a schedule pill: `selected` is
    /// re-derived from the active tab every refresh, while this set records a
    /// person's explicit row selection until their next row gesture.
    pub(crate) fn select_only(&mut self, node: NodeId) -> bool {
        let changed = self.selection.len() != 1 || !self.selection.contains(&node);
        self.selection.clear();
        self.selection.insert(node);
        self.selection_anchor = Some(node);
        changed
    }

    /// The document row the open tab is highlighting, if any.
    pub fn selected(&self) -> Option<NodeId> {
        self.selected
    }

    /// **The one file a person has selected**, if that is what the selection
    /// is (D259 §4).
    ///
    /// `None` for anything else, and deliberately so for a *multi*-selection:
    /// the context pane explains one thing, and "the context of these four
    /// files" is a different question nobody has asked for. A document row is
    /// not a file either — it has its own Cloud context, which is the one the
    /// pane already showed.
    ///
    /// O(rows) over a list already in memory, and asked only while the Cloud
    /// section is on screen.
    pub fn selected_file(&self) -> Option<&Row> {
        let mut chosen = self
            .rows
            .iter()
            .filter(|row| self.selection.contains(&row.node));
        let row = chosen.next()?;
        if chosen.next().is_some() {
            return None;
        }
        (row.kind == Kind::File).then_some(row)
    }

    /// Every row in the multi-selection, in tree order. Empty when nothing is
    /// selected. Used by Move to… (Ctrl+M) so a multi-select moves as one set.
    pub fn selection_nodes(&self) -> Vec<NodeId> {
        self.rows
            .iter()
            .map(|r| r.node)
            .filter(|id| self.selection.contains(id))
            .collect()
    }

    /// Rebuild the rows from the document.
    ///
    /// Cheap enough to call on every `adopt`: it is a walk of the library and a
    /// string per node, with no text layout — the labels are measured only for
    /// the handful of rows actually drawn.
    pub fn rebuild(&mut self, ws: &WorkspaceDoc) {
        if self.mode == NavigatorMode::Agents {
            // Agents rows come from the session catalog, not the document —
            // but the pinned Trash row is on screen in every mode, and this
            // is its counting place on the document-backed path.
            self.trash_count = ws.trash_len();
            self.rebuild_agent_rows();
            return;
        }
        let span = koru_core::timing::Span::start("explore rebuild");
        self.rows.clear();
        // Tasks shows the rail rather than the library, and a walk whose rows
        // nothing draws is the one cost this column is least allowed to pay —
        // `rebuild` runs on every adopt. Same guard, same reason, as
        // `rebuild_manifest_rows`.
        if self.mode.kind().is_none() {
            self.slides.clear();
            return;
        }
        // A rebuild renumbers every row, at which point an offset recorded
        // against an old index would be applied to a different row — the same
        // reason the tab slide clears on any structural change.
        self.slides.clear();
        // A focus naming a node that has gone would keep the keyboard while
        // answering for nothing — the arrows would appear to be swallowed.
        let keep = self.focus.take();
        // **The structure comes from the model now.** This used to walk the
        // tree itself — deciding what counted as a document, recursing through
        // what did not, and working out each row's depth. All three are facts
        // about the workspace rather than about the pane, and `explore()` is
        // where they live; what is left here is presentation: labels, icons,
        // folds and the numbering.
        //
        // `explore_bridged` rather than `explore` because a row still has to
        // carry a `NodeId` for opening, dragging and renaming. That is the one
        // line that changes when a document becomes its own document.
        let Ok(mut entries) = ws.explore_bridged() else {
            return;
        };
        // **A canvas is a manifest kind**, and a workspace with no manifest has
        // none: its tree projection is pages. Listing those under Canvases
        // would be the one place a page claimed to be something else.
        if self.mode == NavigatorMode::Canvases {
            entries.retain(|(entry, _)| entry.kind == KIND_CANVAS);
        }
        let top: Vec<NodeId> = entries
            .iter()
            .filter(|(e, _)| e.parent.is_none())
            .map(|(_, node)| *node)
            .collect();
        // Fold or unfold everything, if that was asked for since the last
        // rebuild. It waits until here because collapsing *all* of it means
        // walking the document, and the click that asked has no `ws`.
        match self.pending_fold.take() {
            Some(fold::Fold::Expand) => self.collapsed.clear(),
            Some(fold::Fold::Collapse) => {
                self.collapsed.clear();
                for id in top.iter().copied() {
                    self.collapse_every(ws, id);
                }
            }
            None => {}
        }
        // Drop ids naming nodes that have left the document. This is the only
        // place that can be asked: a node collapsed behind another collapsed
        // node has no row, so filtering against the rows would forget it. See
        // [`collapsed`](Self::collapsed).
        self.collapsed.retain(|id| ws.kind_of(*id).is_some());
        self.push_entries(ws, &entries);
        self.focus = keep.filter(|id| self.rows.iter().any(|r| r.node == *id));
        // Drop selection entries naming nodes that have left the tree, the way
        // `collapsed` is filtered — a slow leak of ids naming nothing.
        self.selection
            .retain(|id| self.rows.iter().any(|r| r.node == *id));
        self.selection_anchor = self
            .selection_anchor
            .filter(|id| self.rows.iter().any(|r| r.node == *id));
        // **Counted here, never per frame.** `rebuild` already walks the library
        // once per structural change and this is one key lookup on the end of
        // it. Drawing reads the field and asks the document nothing (AGENTS.md:
        // "nothing walks the document on a timer").
        //
        // It goes stale while the sidebar is shut, because `rebuild` does not
        // run then — which costs nothing, since the row is not on screen and
        // opening the column rebuilds before the first frame. Do not "fix" that
        // into a per-frame walk.
        self.trash_count = ws.trash_len();
        let n = self.rows.len();
        span.stop_with(|| format!("{n} rows"));
    }

    /// Collapse `id` and everything under it that could ever show a chevron.
    ///
    /// Walks the *document*, not the rows: Collapse all has to reach the folds
    /// hidden behind other folds, or reopening one top-level document would
    /// reveal a subtree still spread out underneath it.
    fn collapse_every(&mut self, ws: &WorkspaceDoc, id: NodeId) {
        let children = ws.children(Parent::Node(id)).unwrap_or_default();
        if ws.kind_of(id) == Some(Kind::Document)
            && children.iter().any(|c| has_document_under(ws, *c))
        {
            self.collapsed.insert(id);
        }
        for child in children {
            self.collapse_every(ws, child);
        }
    }

    /// Emit `id`'s row if it is a document, then its document descendants.
    /// Returns whether anything was emitted at or under it.
    ///
    /// A node that is *not* a document is recursed **through** rather than
    /// skipped: a document nested inside a canvas still deserves a row,
    /// flattened to its nearest document ancestor's depth rather than vanishing
    /// with its parent.
    /// Turn the model's projection into rows.
    ///
    /// The entries arrive pre-order with a parent each, so **depth is derived
    /// in one pass**: a parent always precedes its children, so its depth is
    /// already known by the time they are reached. That replaces a recursive
    /// walk that had to be told what a document was.
    ///
    /// Two things stay here because they are the pane's, not the model's:
    ///
    /// - **Folds.** A collapsed document's descendants are skipped, but whether
    ///   it *has* any still decides its caret — so `has_children` is computed
    ///   from the projection before anything is filtered, rather than by asking
    ///   the document a second question.
    /// - **Labels and icons.** A leading emoji in the title is the page icon
    ///   and is stripped so it is not drawn twice; the explicit F2 icon is the
    ///   fallback. That needs the node, which is why the entries are bridged.
    /// Build rows from the **workspace's** entries, with no document loaded.
    ///
    /// The store lists a workspace from its manifest, and the documents it names
    /// are not open — that is the point of a manifest. So there is no node to
    /// read a label or an icon from, and both come from the entry: the title,
    /// and a leading emoji in it.
    ///
    /// **`Row::node` carries the `DocumentId`'s bytes**, not a `NodeId` from any
    /// document. Both are UUID newtypes, so the conversion is total and
    /// reversible, and it means selection, folds, drag and hit-testing keep
    /// working unchanged on rows that name a document rather than a node. The
    /// caller converts back before asking the shell to open one. It is a bridge
    /// and it goes when `Row` carries a `DocumentId` outright.
    pub fn rebuild_from_entries(&mut self, entries: &[ExploreEntry]) {
        self.manifest_entries = entries.to_vec();
        self.rebuild_manifest_rows();
    }

    fn rebuild_manifest_rows(&mut self) {
        // Agents mode reaches here through the same two rebuild entry points
        // the manifest modes use (`set_navigator_mode`, `rebuild_from_entries`)
        // and diverts to its own projection — so the container's one
        // `explore_stale` path refreshes whichever mode is showing.
        if self.mode == NavigatorMode::Agents {
            return self.rebuild_agent_rows();
        }
        // Tasks is an index over the workspace rather than a projection of the
        // manifest, so it contributes no rows at all and the rail draws in
        // their place. Building them anyway would cost a walk per rebuild for
        // a list nothing shows.
        let Some(kind) = self.mode.kind() else {
            self.rows.clear();
            self.slides.clear();
            return;
        };
        // A flat list, not a tree: its own few lines, in `canvases`.
        if self.mode == NavigatorMode::Canvases {
            return self.rebuild_canvas_rows();
        }
        let span = koru_core::timing::Span::start("explore rebuild (manifest)");
        let mut entries: Vec<ExploreEntry> = self
            .manifest_entries
            .iter()
            .filter(|entry| entry.kind == kind)
            .cloned()
            .collect();
        if self.mode == NavigatorMode::Files {
            // Files are appended as they arrive, so tree order is oldest first.
            // The Files projection is a recency list, not a tree people arrange:
            // reverse so the latest addition is on top.
            entries.reverse();
        }
        let entries = entries.as_slice();
        self.rows.clear();
        self.slides.clear();
        let keep = self.focus.take();

        let mut depth_of: HashMap<DocumentId, u16> = HashMap::new();
        let mut has_children: HashSet<DocumentId> = HashSet::new();
        // Also here, and not only where the listing is pushed: this path is
        // driven directly by tests and by the store-backed rebuild, and a
        // parent map that depended on a caller having remembered would be one
        // more thing to get wrong.
        self.note_parents(entries);
        // **Depth is counted from the nearest ancestor this mode lists.** A
        // page whose manifest parent is filtered out of the section — a page
        // nested under a canvas, which Documents does not list — used to be
        // drawn one level in under nothing (`unwrap_or(0) + 1`). Walking up
        // the whole manifest to the first listed ancestor puts it under the
        // row it actually sits beneath, or at the top when there is none.
        let parent_of: HashMap<DocumentId, DocumentId> = self
            .manifest_entries
            .iter()
            .filter_map(|entry| entry.parent.map(|parent| (entry.id, parent)))
            .collect();
        for entry in entries {
            let mut above = entry.parent;
            // Bounded by the map, for `reveal`'s reason: a parent chain read
            // from a manifest is data, and a cycle must not hang the pane.
            for _ in 0..=parent_of.len() {
                match above {
                    Some(p) if !depth_of.contains_key(&p) => above = parent_of.get(&p).copied(),
                    _ => break,
                }
            }
            let depth = match above.filter(|p| depth_of.contains_key(p)) {
                Some(p) => {
                    has_children.insert(p);
                    depth_of.get(&p).copied().unwrap_or(0) + 1
                }
                None => 0,
            };
            depth_of.insert(entry.id, depth);
        }

        // **Collapse all and expand all, on this path too.** They were applied
        // only in `rebuild`, which walks the *document* — so store-backed, where
        // the rows come from the manifest, both buttons set a flag nothing ever
        // read and pressing them did nothing at all.
        //
        // The same rule as the tree's: only a row with something under it earns
        // a fold, or collapsing everything leaves a caret on a document with
        // nothing beneath it. `has_children` is the answer, already counted.
        match self.pending_fold.take() {
            Some(fold::Fold::Expand) => self.collapsed.clear(),
            Some(fold::Fold::Collapse) => {
                self.collapsed = has_children
                    .iter()
                    .map(|id| NodeId::from_uuid(id.as_uuid()))
                    .collect();
            }
            None => {}
        }
        // Ids naming entries that have left the workspace are dropped here,
        // against the complete manifest rather than this mode's projection.
        // Files and documents share the set because their ids share one
        // namespace; pruning against only `entries` made Files discard every
        // remembered document fold, so returning to Documents expanded the
        // whole tree.
        //
        // An empty manifest is also what a window holds between restoring the
        // set from `Settings` and receiving the first entries. Do not read it
        // as proof that nothing exists or every remembered fold is lost before
        // it can be shown.
        if !self.manifest_entries.is_empty() {
            let live: HashSet<NodeId> = self
                .manifest_entries
                .iter()
                .map(|e| NodeId::from_uuid(e.id.as_uuid()))
                .collect();
            self.collapsed.retain(|id| live.contains(id));
        }

        let mut skip_below: Option<u16> = None;
        for entry in entries {
            let depth = depth_of.get(&entry.id).copied().unwrap_or(0);
            let node = NodeId::from_uuid(entry.id.as_uuid());
            if let Some(above) = skip_below {
                if depth > above {
                    continue;
                }
                skip_below = None;
            }
            let expanded = !self.collapsed.contains(&node);
            if !expanded {
                skip_below = Some(depth);
            }
            let raw = if entry.title.is_empty() {
                "Untitled".to_string()
            } else {
                entry.title.clone()
            };
            let (leading, rest) = split_leading_emoji(&raw);
            let icon = leading.map(str::to_owned);
            let label = if leading.is_some() {
                rest.to_owned()
            } else {
                raw
            };
            self.rows.push(Row {
                node,
                depth,
                label,
                // Off the mode's manifest kind rather than a second match on
                // the mode, which would need an arm for Agents — a mode this
                // path never builds for. **A file is the exception, not the
                // rule**: every other listed kind opens as a document, and the
                // old `== KIND_DOCUMENT` test made any new one — a canvas — a
                // file that a click would only select.
                kind: if kind == KIND_FILE {
                    Kind::File
                } else {
                    Kind::Document
                },
                mime: entry.mime.clone(),
                icon,
                has_children: has_children.contains(&entry.id),
                expanded,
                file_state: self.file_state_of(node),
                note: entry.note.map(|note| NodeId::from_uuid(note.as_uuid())),
            });
        }

        self.focus = keep.filter(|id| self.rows.iter().any(|r| r.node == *id));
        self.selection
            .retain(|id| self.rows.iter().any(|r| r.node == *id));
        self.selection_anchor = self
            .selection_anchor
            .filter(|id| self.rows.iter().any(|r| r.node == *id));
        let n = self.rows.len();
        span.stop_with(|| format!("{n} rows"));
    }

    /// The virtual Unassigned bucket's row key. The nil UUID cannot collide
    /// with a minted project or session id — both are v4 — and giving the
    /// bucket a minted id would make it look like a project something could
    /// address. It is a presentation identity, exactly as Koru Terminal's
    /// `GroupId::Unassigned` is: never renamable, never a destination.
    fn agent_unassigned_node() -> NodeId {
        NodeId::from_uuid(uuid::Uuid::nil())
    }

    /// The *Projects* heading's row key — Koru Terminal's `Projects` section
    /// row with its `+`, lifted into this tree's own row list so hit-testing,
    /// culling and the keyboard need no second geometry. A fixed non-v4 UUID:
    /// it can collide with no minted project or session id, and like the
    /// bucket it is a presentation identity — never a fold, never a
    /// destination, never a thing to open.
    fn agent_projects_header_node() -> NodeId {
        NodeId::from_uuid(uuid::Uuid::from_u128(1))
    }

    /// Whether this row names a session (as against a project group).
    fn is_agent_session_row(&self, node: NodeId) -> bool {
        self.agent_session_ids.contains_key(&node)
    }

    /// The *Runs* heading. A presentation identity like the *Projects* one:
    /// never a fold, never a destination, never a thing to open.
    fn agent_runs_header_node() -> NodeId {
        NodeId::from_uuid(uuid::Uuid::from_u128(2))
    }

    /// The *Schedules* heading. Like Projects/Runs it is section furniture,
    /// never a schedule row or an action target.
    fn agent_schedules_header_node() -> NodeId {
        NodeId::from_uuid(uuid::Uuid::from_u128(3))
    }

    fn schedule_node(&mut self, key: ScheduleNode) -> NodeId {
        *self.agent_schedule_nodes.entry(key).or_default()
    }

    /// Replace the run half of the Agents catalog. Returns whether the rows
    /// showing changed, so an unchanged listing costs no rebuild.
    pub fn set_agent_runs(&mut self, runs: Vec<koru_runs_ui::RunRow>) -> bool {
        if self.agent_runs == runs {
            return false;
        }
        self.agent_runs = runs;
        if self.mode != NavigatorMode::Agents {
            return false;
        }
        self.rebuild_agent_rows();
        true
    }

    /// Replace the schedule half of the Agents catalog. Read-only rows are
    /// already localized and joined to the run index by `ScheduleIndex`.
    pub(crate) fn set_agent_schedules(
        &mut self,
        schedules: Vec<crate::schedule_index::ScheduleRow>,
        state: crate::schedule_index::CatalogState,
    ) -> bool {
        if self.agent_schedules_known
            && self.agent_schedules == schedules
            && self.agent_schedule_state == state
        {
            return false;
        }
        self.agent_schedules = schedules;
        self.agent_schedule_state = state;
        self.agent_schedules_known = true;
        if self.mode != NavigatorMode::Agents {
            return false;
        }
        self.rebuild_agent_rows();
        true
    }

    #[cfg(test)]
    pub(crate) fn agent_schedule_run(&self, node: NodeId) -> Option<koru_delegate_core::RunId> {
        self.agent_schedule_run_ids.get(&node).copied()
    }

    #[cfg(test)]
    pub(crate) fn agent_schedule_metadata_row(&self, node: NodeId) -> bool {
        self.agent_schedule_metadata.contains(&node)
    }

    /// Resolve only schedule parents and their metadata. A fired Run child and
    /// the Schedules heading are not aliases for schedule management.
    pub(crate) fn agent_schedule_at(&self, node: NodeId) -> Option<koru_delegate_core::ScheduleId> {
        self.agent_schedule_ids
            .get(&node)
            .copied()
            .or_else(|| self.agent_schedule_metadata_ids.get(&node).copied())
    }

    pub(crate) fn agent_schedule_node(
        &self,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Option<NodeId> {
        self.agent_schedule_nodes
            .get(&ScheduleNode::Schedule(schedule))
            .copied()
    }

    pub(crate) fn agent_schedule_label_rect(
        &self,
        area: Rect,
        schedule: koru_delegate_core::ScheduleId,
    ) -> Option<Rect> {
        let node = self
            .agent_schedule_nodes
            .get(&ScheduleNode::Schedule(schedule))?;
        self.row_label_rect(area, *node)
    }

    /// Which run a row names, if it names one. What the suite asks so a
    /// press can be checked against the row it landed on rather than
    /// against the outcome the press happened to produce.
    #[cfg(test)]
    pub fn agent_run(&self, node: NodeId) -> Option<koru_delegate_core::RunId> {
        self.agent_run_ids.get(&node).copied()
    }

    /// The runs this navigator is listing, as the container last pushed
    /// them. Read when a menu or a drag has to say what a run *is* —
    /// whether it is still working, and whether it produced anything — and
    /// the container's own copy is the index behind this one.
    pub fn agent_runs(&self) -> &[koru_runs_ui::RunRow] {
        &self.agent_runs
    }

    /// The same question for a session row — asked by the suite so a nested
    /// row can be checked against the session it names rather than against
    /// its label, which two sessions of the same profile share.
    #[cfg(test)]
    pub fn agent_session_id(&self, node: NodeId) -> Option<SessionId> {
        self.agent_session_ids.get(&node).copied()
    }

    /// **Whether a run in this listing is driving `session`.**
    ///
    /// What the container asks before opening a session tab: a session a run
    /// holds the controller of opens read-only (ADR-361). Asked of the tree
    /// rather than of the index behind it, because the tree's listing is the
    /// one the person just clicked in.
    pub fn session_belongs_to_a_run(&self, session: SessionId) -> bool {
        self.agent_runs
            .iter()
            .any(|run| run.session == Some(session))
    }

    /// The product mark a session row wears — the **same** projection the tab
    /// draws (`koru_draw::brand::for_profile` over the session's frozen wire
    /// key), so the row and its tab cannot show different faces (Q-E7 P6).
    /// `None` for a custom profile or a built-in this build predates; the row
    /// then keeps the generic terminal glyph.
    fn agent_session_mark(&self, node: NodeId) -> Option<koru_draw::brand::ProfileMark> {
        let session = self.agent_session_ids.get(&node)?;
        let summary = self
            .agent_sessions
            .iter()
            .find(|summary| summary.id == *session)?;
        match &summary.profile {
            SessionProfileKey::BuiltIn(key) => koru_draw::brand::for_profile(key.as_str()),
            SessionProfileKey::Custom(_) => None,
        }
    }

    /// The device's projects the *Move to project ▸* flyout offers, in the
    /// tree's own collation and under the same workspace-scope visibility the
    /// rows use — the menu affordance for the verb P5's drag already sends.
    /// Read by the container when it opens a session's right-click menu.
    pub fn agent_menu_projects(&self) -> Vec<(ProjectId, String)> {
        let current = (
            self.current_profile.map(|p| p.as_uuid()),
            self.current.map(|w| w.as_uuid()),
        );
        let mut visible: Vec<&ProjectSummary> = self
            .agent_projects
            .iter()
            .filter(|project| match project.workspace_scope {
                None => true,
                Some(scope) => {
                    current
                        == (
                            Some(scope.profile.as_uuid()),
                            Some(scope.workspace.as_uuid()),
                        )
                }
            })
            .collect();
        visible.sort_by_key(|project| (project.name.to_lowercase(), project.id));
        visible
            .into_iter()
            .map(|project| (project.id, project.name.clone()))
            .collect()
    }

    /// The project a session is filed under, for dimming its current row in the
    /// *Move to project ▸* flyout.
    pub fn agent_session_project_of(&self, session: SessionId) -> Option<ProjectId> {
        self.agent_session_project(session)
    }

    /// The project a group row names, when it names one — `None` for the
    /// bucket and the heading, which is what every caller needing a real
    /// mutation target checks (the same shape as Koru Terminal's
    /// `GroupId::project`).
    pub fn agent_project_at(&self, node: NodeId) -> Option<ProjectId> {
        self.agent_projects
            .iter()
            .find(|project| NodeId::from_uuid(project.id.as_uuid()) == node)
            .map(|project| project.id)
    }

    /// The registry's current name for a project, for seeding the rename
    /// prompt with what is already true.
    pub fn agent_project_name(&self, id: ProjectId) -> Option<&str> {
        self.agent_projects
            .iter()
            .find(|project| project.id == id)
            .map(|project| project.name.as_str())
    }

    /// The project's recorded folder, if it has one. What the launch path
    /// starts sessions in, and what S11's refusal checks against.
    pub fn agent_project_root(&self, id: ProjectId) -> Option<&str> {
        self.agent_projects
            .iter()
            .find(|project| project.id == id)?
            .root_directory
            .as_deref()
    }

    /// This window's local answer to "has this project a live session" — what
    /// dims *Delete project* with its reason, exactly as Koru Terminal's
    /// `project_rows` does. The host still refuses on the wire; this only
    /// keeps the menu honest before the press.
    pub fn agent_project_has_running(&self, id: ProjectId) -> bool {
        self.agent_sessions.iter().any(|session| {
            session.project_id == Some(id) && session.process == ProcessState::Running
        })
    }

    /// The catalog project a session is filed under, for the drop path's
    /// no-op check (dropping a session where it already is moves nothing).
    fn agent_session_project(&self, session: SessionId) -> Option<ProjectId> {
        self.agent_sessions
            .iter()
            .find(|summary| summary.id == session)
            .and_then(|summary| summary.project_id)
    }

    /// The scope a project created from this window carries: the window's own
    /// `(profile, workspace)` — Koru Terminal's `creation_scope`, read off the
    /// context the header already shows. Through the string form because that
    /// is the typed ids' only constructor besides `mint`, and minting here
    /// would invent an identity rather than copy the catalog's. `None` (no
    /// context yet, or a non-v4 id) creates the project unscoped, which is
    /// visible everywhere rather than orphaned nowhere.
    pub fn agent_creation_scope(&self) -> Option<WorkspaceScope> {
        let profile = self
            .current_profile?
            .as_uuid()
            .hyphenated()
            .to_string()
            .parse()
            .ok()?;
        let workspace = self
            .current?
            .as_uuid()
            .hyphenated()
            .to_string()
            .parse()
            .ok()?;
        Some(WorkspaceScope { profile, workspace })
    }

    /// What opening the row `node` means in the mode showing. The manifest
    /// modes open the node; an Agents session row asks for its tab, and an
    /// Agents group row is a place, not a thing to open.
    fn open_row_outcome(&self, node: NodeId) -> Outcome {
        if self.mode != NavigatorMode::Agents {
            return Outcome::OpenNode(node);
        }
        if let Some(run) = self.agent_schedule_run_ids.get(&node) {
            return Outcome::OpenRun(*run);
        }
        if self.agent_schedule_ids.contains_key(&node)
            || self.agent_schedule_metadata.contains(&node)
            || node == Self::agent_schedules_header_node()
        {
            return Outcome::Consumed;
        }
        if let Some(run) = self.agent_run_ids.get(&node) {
            return Outcome::OpenRun(*run);
        }
        match self.agent_session_ids.get(&node) {
            Some(session) => Outcome::OpenSession(*session),
            None => Outcome::Consumed,
        }
    }

    /// A press on an Agents **group** row, after the caret and the session
    /// rows have declined it: the row folds in place — the whole row answers,
    /// not only the caret, because a plain press on a group has nothing else
    /// it could mean (its verbs are the right-click menu's).
    fn agents_row_pressed(&mut self, node: NodeId) -> Outcome {
        if let Some(run) = self.agent_schedule_run_ids.get(&node).copied() {
            return Outcome::OpenRun(run);
        }
        if self.agent_schedule_metadata.contains(&node)
            || node == Self::agent_schedules_header_node()
        {
            return Outcome::Consumed;
        }
        if let Some(session) = self.agent_session_ids.get(&node).copied() {
            return Outcome::OpenSession(session);
        }
        if let Some(run) = self.agent_run_ids.get(&node).copied() {
            return Outcome::OpenRun(run);
        }
        let has_children = self
            .rows
            .iter()
            .find(|row| row.node == node)
            .is_some_and(|row| row.has_children);
        if has_children {
            let expanded = self.agent_collapsed.remove(&node);
            if !expanded {
                self.agent_collapsed.insert(node);
            }
            self.start_turn(node, expanded);
        }
        Outcome::Consumed
    }

    /// Build rows from the **session catalog**: this workspace's projects,
    /// each with its sessions beneath it, then the virtual Unassigned bucket.
    ///
    /// The visibility rule is a lean reading of the deleted Terminal app's
    /// `placement()` (ADR-264): a project scoped to
    /// exactly this window's `(profile, workspace)` is shown, a project with
    /// no scope is shown (it belongs to no workspace, so no workspace's
    /// filter excludes it — and this pane has no bottom group to file it
    /// under yet), and a project scoped elsewhere is hidden **with its
    /// sessions** — hiding the group and stranding its rows in the bucket
    /// would relabel placed work as unplaced. The orphan/repair area stays
    /// Koru Terminal's: it needs the including-deleted workspace catalog,
    /// which this pane does not hold, and inventing a repair surface without
    /// it would be guessing about somebody's workspaces.
    ///
    /// A session whose project id names nothing the registry has — or that
    /// nobody placed — is *unplaced*, which is the bucket.
    ///
    /// Ordering is the catalog surface's own collation (ADR-264): projects by
    /// case-folded name then id, sessions by creation time then id, so equal
    /// names still order deterministically instead of shuffling.
    fn rebuild_agent_rows(&mut self) {
        let span = koru_core::timing::Span::start("explore rebuild (agents)");
        self.rows.clear();
        self.slides.clear();
        let keep = self.focus.take();
        self.agent_session_ids.clear();
        self.agent_run_ids.clear();
        self.agent_runs_working.clear();
        self.agent_runs_paused.clear();
        self.agent_schedule_ids.clear();
        self.agent_schedules_paused.clear();
        self.agent_schedule_run_ids.clear();
        self.agent_schedule_metadata.clear();
        self.agent_schedule_metadata_ids.clear();

        // Allocate schedule presentation identities before collapse-all reads
        // them. The map keeps the same row stable across list invalidations.
        let schedules = self.agent_schedules.clone();
        for schedule in &schedules {
            self.schedule_node(ScheduleNode::Schedule(schedule.schedule));
        }

        let current = (
            self.current_profile.map(|p| p.as_uuid()),
            self.current.map(|w| w.as_uuid()),
        );
        let mut visible: Vec<&ProjectSummary> = self
            .agent_projects
            .iter()
            .filter(|project| match project.workspace_scope {
                None => true,
                Some(scope) => {
                    current
                        == (
                            Some(scope.profile.as_uuid()),
                            Some(scope.workspace.as_uuid()),
                        )
                }
            })
            .collect();
        visible.sort_by_key(|project| (project.name.to_lowercase(), project.id));
        let known: HashSet<ProjectId> = self.agent_projects.iter().map(|p| p.id).collect();

        // **The sessions a run is driving belong to that run**, not to the
        // bucket. A delegated run works in a temporary folder and so has no
        // project, which put its session under *Unassigned* with nothing
        // saying what it was for — three rows reading *Codex (Embedded)*
        // beside three runs, and no line between them. A session named by a
        // run in this window's listing is drawn under that run below, and
        // is skipped here so it appears once.
        //
        // A session whose run this window does not know still goes in the
        // bucket: the listing is one device's answer about one workspace,
        // and a session it cannot account for is not a session to hide.
        let claimed: HashMap<SessionId, usize> = self
            .agent_runs
            .iter()
            .enumerate()
            .filter_map(|(index, run)| run.session.map(|session| (session, index)))
            .collect();

        // Sessions, grouped: placed under a project the registry knows, or in
        // the bucket. A hidden project's sessions stay in the map and are
        // never emitted — scoped-away work hides whole, it does not migrate.
        let mut grouped: HashMap<Option<ProjectId>, Vec<&SessionSummary>> = HashMap::new();
        for session in &self.agent_sessions {
            if claimed.contains_key(&session.id) {
                continue;
            }
            let group = match session.project_id {
                Some(id) if known.contains(&id) => Some(id),
                _ => None,
            };
            grouped.entry(group).or_default().push(session);
        }
        for sessions in grouped.values_mut() {
            sessions.sort_by_key(|session| (session.created_at_ms, session.id));
        }

        // Collapse all / expand all, over the groups this projection has.
        match self.pending_fold.take() {
            Some(fold::Fold::Expand) => self.agent_collapsed.clear(),
            Some(fold::Fold::Collapse) => {
                self.agent_collapsed = visible
                    .iter()
                    .filter(|project| {
                        grouped
                            .get(&Some(project.id))
                            .is_some_and(|sessions| !sessions.is_empty())
                    })
                    .map(|project| NodeId::from_uuid(project.id.as_uuid()))
                    .chain(
                        grouped
                            .get(&None)
                            .is_some_and(|sessions| !sessions.is_empty())
                            .then(Self::agent_unassigned_node),
                    )
                    // A run with its session under it folds like a group,
                    // because that is what it now is.
                    .chain(
                        self.agent_runs
                            .iter()
                            .filter(|run| run.session.is_some())
                            .map(|run| NodeId::from_uuid(run.run.as_uuid())),
                    )
                    .chain(schedules.iter().filter_map(|schedule| {
                        self.agent_schedule_nodes
                            .get(&ScheduleNode::Schedule(schedule.schedule))
                            .copied()
                    }))
                    .collect();
            }
            None => {}
        }

        let mut groups: Vec<(NodeId, String, Vec<&SessionSummary>)> = visible
            .iter()
            .map(|project| {
                (
                    NodeId::from_uuid(project.id.as_uuid()),
                    project.name.clone(),
                    grouped.remove(&Some(project.id)).unwrap_or_default(),
                )
            })
            .collect();
        // The bucket sits last: it is where things have not been put yet, and
        // first would read as the default place rather than the leftover one.
        // Drawn when it holds something, **or when the tree would otherwise be
        // blank** — Koru Terminal's rule: an empty group beside real content
        // is noise, and a blank pane with nothing under its heading is worse.
        let bucket = grouped.remove(&None).unwrap_or_default();
        // …and **only** when the tree would otherwise be blank: with a run
        // under its own heading below, an empty *Unassigned* is the noise
        // the rule exists to avoid rather than the emptiness it guards
        // against. This is the maintainers' own case — three delegated runs, three
        // sessions, no projects — where every session now hangs under its
        // run and the bucket has nothing left in it.
        if !bucket.is_empty()
            || (groups.is_empty() && self.agent_runs.is_empty() && self.agent_schedules.is_empty())
        {
            groups.push((Self::agent_unassigned_node(), "Unassigned".into(), bucket));
        }

        // The *Projects* heading, first — the pane's counterpart to
        // Koru Terminal's section row, carrying the `+` that creates a project
        // in its reserved trailing lane. An ordinary row of the cache so the
        // hit test, the culling and the scroll all handle it for free.
        self.rows.push(Row {
            node: Self::agent_projects_header_node(),
            depth: 0,
            label: "Projects".to_owned(),
            kind: Kind::Task,
            mime: None,
            icon: None,
            has_children: false,
            expanded: true,
            file_state: FileState::Present,
            note: None,
        });

        for (node, label, sessions) in groups {
            let expanded = !self.agent_collapsed.contains(&node);
            self.rows.push(Row {
                node,
                depth: 0,
                label,
                // Any kind that is neither `Document` nor `File` keeps the
                // drag-to-move machinery off these rows; the glyph is chosen
                // by mode in `draw_row_body`, never read off this field.
                kind: Kind::Task,
                mime: None,
                icon: None,
                has_children: !sessions.is_empty(),
                expanded,
                file_state: FileState::Present,
                note: None,
            });
            if !expanded {
                continue;
            }
            for session in sessions {
                let row = NodeId::from_uuid(session.id.as_uuid());
                self.agent_session_ids.insert(row, session.id);
                self.rows.push(Row {
                    node: row,
                    depth: 1,
                    label: session.display_name.clone(),
                    kind: Kind::Task,
                    mime: None,
                    icon: None,
                    has_children: false,
                    expanded: true,
                    file_state: FileState::Present,
                    note: None,
                });
            }
        }

        // **Scheduled work, as its own section beside Projects and Runs.** A
        // schedule row is its authored name; opening the fold reveals the
        // localized rule, fixed-height metadata, and actual fired runs.
        if self.agent_schedules_known {
            self.rows.push(Row {
                node: Self::agent_schedules_header_node(),
                depth: 0,
                label: koru_i18n::t(
                    crate::format::lang(),
                    koru_i18n::Key::ScheduleNavigatorTitle,
                )
                .to_owned(),
                kind: Kind::Task,
                mime: None,
                icon: None,
                has_children: false,
                expanded: true,
                file_state: FileState::Present,
                note: None,
            });
            let catalog_label = match self.agent_schedule_state {
                crate::schedule_index::CatalogState::Loading => {
                    Some(koru_i18n::Key::ScheduleNavigatorLoading)
                }
                crate::schedule_index::CatalogState::Incomplete => {
                    Some(koru_i18n::Key::ScheduleNavigatorIncomplete)
                }
                crate::schedule_index::CatalogState::Error => {
                    Some(koru_i18n::Key::ScheduleNavigatorError)
                }
                crate::schedule_index::CatalogState::Ready if schedules.is_empty() => {
                    Some(koru_i18n::Key::ScheduleNavigatorEmpty)
                }
                crate::schedule_index::CatalogState::Ready => None,
            };
            if let Some(key) = catalog_label {
                let node = self.schedule_node(ScheduleNode::Catalog);
                self.agent_schedule_metadata.insert(node);
                self.rows.push(Row {
                    node,
                    depth: 1,
                    label: koru_i18n::t(crate::format::lang(), key).to_owned(),
                    kind: Kind::Task,
                    mime: None,
                    icon: None,
                    has_children: false,
                    expanded: true,
                    file_state: FileState::Present,
                    note: None,
                });
            }
            let mut live_schedule_nodes = HashSet::new();
            if catalog_label.is_some() {
                live_schedule_nodes.insert(ScheduleNode::Catalog);
            }
            for schedule in schedules {
                let key = ScheduleNode::Schedule(schedule.schedule);
                live_schedule_nodes.insert(key);
                let node = self.schedule_node(key);
                self.agent_schedule_ids.insert(node, schedule.schedule);
                if schedule.paused {
                    self.agent_schedules_paused.insert(node);
                }
                let expanded = !self.agent_collapsed.contains(&node);
                self.rows.push(Row {
                    node,
                    depth: 1,
                    label: schedule.name,
                    kind: Kind::Task,
                    mime: None,
                    icon: None,
                    has_children: true,
                    expanded,
                    file_state: FileState::Present,
                    note: None,
                });
                if !expanded {
                    continue;
                }
                for (key, label) in [
                    (ScheduleNode::Rule(schedule.schedule), schedule.rule),
                    (ScheduleNode::Next(schedule.schedule), schedule.next),
                    (ScheduleNode::Owner(schedule.schedule), schedule.owner),
                    (ScheduleNode::State(schedule.schedule), schedule.state),
                ] {
                    live_schedule_nodes.insert(key);
                    let child = self.schedule_node(key);
                    self.agent_schedule_metadata.insert(child);
                    self.agent_schedule_metadata_ids
                        .insert(child, schedule.schedule);
                    self.rows.push(Row {
                        node: child,
                        depth: 2,
                        label,
                        kind: Kind::Task,
                        mime: None,
                        icon: None,
                        has_children: false,
                        expanded: true,
                        file_state: FileState::Present,
                        note: None,
                    });
                }
                for run in schedule.runs {
                    let key = ScheduleNode::Run(schedule.schedule, run.run);
                    live_schedule_nodes.insert(key);
                    let child = self.schedule_node(key);
                    self.agent_schedule_run_ids.insert(child, run.run);
                    self.rows.push(Row {
                        node: child,
                        depth: 2,
                        label: run.label,
                        kind: Kind::Task,
                        mime: None,
                        icon: None,
                        has_children: false,
                        expanded: true,
                        file_state: FileState::Present,
                        note: None,
                    });
                }
            }
            self.agent_schedule_nodes
                .retain(|key, _| live_schedule_nodes.contains(key));
        } else {
            self.agent_schedule_nodes.clear();
        }

        // **The runs, under their own heading.** Below the sessions because
        // a session is what is happening now and a run is what was asked
        // for; a person looking for the thing they started yesterday scrolls
        // to it rather than hunting for it among live processes.
        if !self.agent_runs.is_empty() {
            self.rows.push(Row {
                node: Self::agent_runs_header_node(),
                depth: 0,
                label: "Runs".to_owned(),
                kind: Kind::Task,
                mime: None,
                icon: None,
                has_children: false,
                expanded: true,
                file_state: FileState::Present,
                note: None,
            });
            for run in &self.agent_runs {
                let node = NodeId::from_uuid(run.run.as_uuid());
                self.agent_run_ids.insert(node, run.run);
                // **A run that is still working turns; one that is waiting
                // for a person is paused; one that has stopped keeps the
                // folder.** The answer is the listing's own
                // (`koru_runs_ui::motion`), whose *unfinished* half is what
                // the run index polls by — so nothing turns on a list that is
                // not being refreshed, and an interrupted run is still
                // watched without pretending work is happening in it.
                match run.motion {
                    koru_runs_ui::Motion::Working => {
                        self.agent_runs_working.insert(node);
                    }
                    koru_runs_ui::Motion::Paused => {
                        self.agent_runs_paused.insert(node);
                    }
                    koru_runs_ui::Motion::Still => {}
                }
                // The session this run is being driven in, when this device
                // is driving one and the catalog still has it. A run whose
                // session ended has no child and folds to nothing.
                let session = run
                    .session
                    .and_then(|id| self.agent_sessions.iter().find(|summary| summary.id == id));
                let expanded = !self.agent_collapsed.contains(&node);
                self.rows.push(Row {
                    node,
                    depth: 1,
                    // **The run's name, not its objective.** A sentence is
                    // not a name you can point at in a list; the objective
                    // is the Overview's own section, one press away.
                    label: run.name.clone(),
                    kind: Kind::Task,
                    mime: None,
                    icon: None,
                    has_children: session.is_some(),
                    expanded,
                    file_state: FileState::Present,
                    // A row's `note` is a document a file carries, not a
                    // sentence — where a run got to is the run view's
                    // Overview, which is one press away.
                    note: None,
                });
                if let Some(session) = session.filter(|_| expanded) {
                    let row = NodeId::from_uuid(session.id.as_uuid());
                    self.agent_session_ids.insert(row, session.id);
                    self.rows.push(Row {
                        node: row,
                        depth: 2,
                        label: session.display_name.clone(),
                        kind: Kind::Task,
                        mime: None,
                        icon: None,
                        has_children: false,
                        expanded: true,
                        file_state: FileState::Present,
                        note: None,
                    });
                }
            }
        }

        // Drop fold entries naming groups the catalog no longer has — but not
        // while the catalog is empty, which is also what a window holds
        // between opening and the backend's first answer.
        if !self.agent_projects.is_empty() || !self.agent_sessions.is_empty() {
            let live: HashSet<NodeId> = self
                .agent_projects
                .iter()
                .map(|project| NodeId::from_uuid(project.id.as_uuid()))
                .chain(std::iter::once(Self::agent_unassigned_node()))
                // A run is a group too now, and a run that folded shut must
                // still be shut on the next rebuild.
                .chain(
                    self.agent_runs
                        .iter()
                        .map(|run| NodeId::from_uuid(run.run.as_uuid())),
                )
                .chain(self.agent_schedule_ids.keys().copied())
                .collect();
            self.agent_collapsed.retain(|id| live.contains(id));
        }

        // **The clock the rings turn on, started once and stopped for good.**
        // It starts when the first working run appears and is kept across
        // rebuilds while any is still working, so a listing that arrives every
        // three seconds does not reset the turn to noon each time. The moment
        // none is working it is dropped, which is what returns `animating` to
        // `false` and stops the shell waking this window (ADR-26).
        self.agent_runs_spin_started = if self.agent_runs_working.is_empty() {
            None
        } else {
            self.agent_runs_spin_started
                .or_else(|| Some(Instant::now()))
        };

        self.focus = keep.filter(|id| self.rows.iter().any(|r| r.node == *id));
        self.selection
            .retain(|id| self.rows.iter().any(|r| r.node == *id));
        self.selection_anchor = self
            .selection_anchor
            .filter(|id| self.rows.iter().any(|r| r.node == *id));
        let n = self.rows.len();
        span.stop_with(|| format!("{n} rows"));
    }

    fn push_entries(&mut self, ws: &WorkspaceDoc, entries: &[(ExploreEntry, NodeId)]) {
        let mut depth_of: HashMap<DocumentId, u16> = HashMap::new();
        let mut has_children: HashSet<DocumentId> = HashSet::new();
        // A row is keyed by its `NodeId` here rather than by the entry's id, so
        // the parent chain has to be spelled that way too. Pre-order is what
        // makes one pass enough: a parent's node is recorded before any child
        // asks for it.
        let mut node_of: HashMap<DocumentId, NodeId> = HashMap::new();
        self.parents.clear();
        for (entry, node) in entries {
            node_of.insert(entry.id, *node);
            let depth = match entry.parent {
                Some(p) => {
                    has_children.insert(p);
                    if let Some(parent) = node_of.get(&p).copied() {
                        self.parents.insert(*node, parent);
                    }
                    depth_of.get(&p).copied().unwrap_or(0) + 1
                }
                None => 0,
            };
            depth_of.insert(entry.id, depth);
        }

        // Skip the subtree under a collapsed row. Pre-order means everything
        // below a document sits between it and the next entry at its depth or
        // above, so one watermark is enough — the same shape `Body::rebuild`
        // uses to stop at a child document.
        let mut skip_below: Option<u16> = None;
        for (entry, node) in entries {
            let depth = depth_of.get(&entry.id).copied().unwrap_or(0);
            if let Some(above) = skip_below {
                if depth > above {
                    continue;
                }
                skip_below = None;
            }
            let Ok(read) = ws.read(*node) else { continue };
            let expanded = !self.collapsed.contains(node);
            if !expanded {
                skip_below = Some(depth);
            }
            // **Both counters are zero, and were before this pass too.** They
            // number unnamed images and code blocks ("Image 3"), from when
            // those earned rows of their own; only documents do now, and the
            // old walk had already returned before it could increment either.
            // So `label_for` is reached with a document every time and takes
            // its text branch. Passed explicitly rather than deleted, because
            // the function is still tested on the other kinds and will be
            // wanted again when images or canvases earn rows — see ROADMAP.
            let raw = label_for(&read, 0, 0);
            // A leading emoji in the title is the page icon in the tree (and is
            // stripped from the label so it is not drawn twice). It wins over
            // the explicit F2 page icon; that is only the fallback when the
            // title has no leading emoji.
            let (leading, rest) = split_leading_emoji(&raw);
            let icon = leading
                .map(str::to_owned)
                .or_else(|| read.meta.icon.clone());
            let label = if leading.is_some() {
                rest.to_owned()
            } else {
                raw
            };
            self.rows.push(Row {
                node: *node,
                depth,
                label,
                kind: read.kind,
                mime: match &read.content {
                    koru_core::Content::Blob { mime, .. } => Some(mime.clone()),
                    _ => None,
                },
                icon,
                has_children: has_children.contains(&entry.id),
                expanded,
                file_state: self.file_state_of(*node),
                note: entry.note.map(|note| NodeId::from_uuid(note.as_uuid())),
            });
        }
    }

    /// What the last locality sweep said about this row.
    ///
    /// Unknown means present: a file whose state has not been decided yet draws
    /// as an ordinary row rather than flickering through a placeholder on every
    /// rebuild. Being told a file is here when it is not costs one honest error
    /// on click; the reverse would make the whole list twitch.
    fn file_state_of(&self, node: NodeId) -> FileState {
        self.file_states.get(&node).copied().unwrap_or_default()
    }

    /// Replace what this device knows about which files it holds.
    ///
    /// Computed off the draw path — one blob-hash read per file document plus
    /// the local blob list — and pushed in whenever the manifest changes or a
    /// fetch finishes. Returns whether anything moved, so a caller can skip a
    /// rebuild it does not need.
    pub fn set_file_states(&mut self, states: HashMap<NodeId, FileState>) -> bool {
        if self.file_states == states {
            return false;
        }
        self.file_states = states;
        for row in &mut self.rows {
            row.file_state = self.file_states.get(&row.node).copied().unwrap_or_default();
        }
        true
    }

    /// Mark one row as fetching, or back again, without a whole sweep.
    ///
    /// The click that starts a download has to change the row it just hit, and
    /// waiting for the next locality sweep to say so would leave the press with
    /// no visible answer.
    pub fn set_file_state(&mut self, node: NodeId, state: FileState) {
        self.file_states.insert(node, state);
        if let Some(row) = self.rows.iter_mut().find(|row| row.node == node) {
            row.file_state = state;
        }
    }

    /// Whether this row is listed here but held elsewhere.
    // Test-driven only since the ADR-262 move; kept as the documented seam.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_remote_file(&self, node: NodeId) -> bool {
        self.file_state_of(node) == FileState::Remote
    }

    // -------------------------------------------------- the header and popovers

    /// Tell the pane who and where it is. Cheap and idempotent; returns whether
    /// anything changed, so a caller can skip a repaint.
    ///
    /// `profiles` is **every** profile the machine knows about, not just this
    /// one — that omission is what made the profile switcher unable to switch
    /// profiles: it drew one row, for the profile you were already in.
    pub fn set_context(
        &mut self,
        profiles: Vec<ProfileSummary>,
        current_profile: ProfileId,
        workspaces: Vec<WorkspaceEntry>,
        current: impl Into<Option<WorkspaceId>>,
    ) -> bool {
        let current = current.into();
        let changed = self.profiles != profiles
            || self.current_profile != Some(current_profile)
            || self.workspaces != workspaces
            || self.current != current;
        self.profiles = profiles;
        self.current_profile = Some(current_profile);
        self.workspaces = workspaces;
        self.current = current;
        changed
    }

    /// Every Profile's workspaces, so the switcher can list one that is not
    /// open. Returns whether anything changed.
    pub fn set_profile_workspaces(
        &mut self,
        workspaces: HashMap<ProfileId, Vec<WorkspaceEntry>>,
    ) -> bool {
        if self.catalog == workspaces {
            return false;
        }
        self.catalog = workspaces;
        true
    }

    /// A workspace of any Profile from that catalog, with the Profile that
    /// owns it — so a verb can name a workspace whose Profile is not open.
    pub fn catalog_workspace(&self, id: WorkspaceId) -> Option<(ProfileId, &WorkspaceEntry)> {
        self.catalog.iter().find_map(|(owner, rows)| {
            rows.iter()
                .find(|workspace| workspace.id == id)
                .map(|workspace| (*owner, workspace))
        })
    }

    /// One Profile's workspaces from that catalog; empty when it has none.
    pub fn catalog_workspaces_of(&self, owner: ProfileId) -> &[WorkspaceEntry] {
        self.catalog.get(&owner).map_or(&[], Vec::as_slice)
    }

    /// The profile this pane is in, if the shell has said yet.
    pub fn profile(&self) -> Option<&ProfileSummary> {
        let id = self.current_profile?;
        self.profiles.iter().find(|p| p.id == id)
    }

    /// The blob hash of the avatar to draw, if there is one. The container looks
    /// it up and hands the brush back to [`draw`] — this crate has no decoder.
    ///
    /// [`draw`]: Self::draw
    pub fn avatar_hash(&self) -> Option<&str> {
        self.profile()?.avatar.as_deref()
    }

    /// The popover that is **taking input**, which is not the same as the one
    /// being drawn: one that is fading out is still on screen and must already
    /// be deaf.
    fn live(&self) -> Option<&Popover> {
        self.popover.as_ref().filter(|p| !p.closing)
    }

    pub fn popover_open(&self) -> bool {
        self.live().is_some()
    }

    /// Which row of the open popover is highlighted, if any.
    ///
    /// Public because a row's **remove** box only draws while its row is hot, so
    /// "is that row hovered" is the same question as "is the ✕ reachable" — and
    /// that is worth being able to assert from the container, where the bug
    /// was: the popover is wider than the column it hangs off, so a pointer
    /// moving towards the ✕ left the pane's region and the hover was cleared by
    /// the very move that approached it.
    // Test-driven only since the ADR-262 move; kept as the documented seam.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn popover_hover(&self) -> Option<usize> {
        self.live().and_then(|p| p.hover)
    }

    /// Whether there is anything to *draw* — including one on its way out.
    /// Only the container's draw order asks this.
    pub fn popover_visible(&self) -> bool {
        self.popover.is_some()
    }

    /// True only while a fade or a caret's turn is in flight. Must return to
    /// `false` on its own, or the shell spins at frame rate forever (ADR-26).
    pub fn popover_animating(&self) -> bool {
        self.popover
            .as_ref()
            .is_some_and(|p| p.fade.is_some() || p.slide.is_some())
            || !self.turns.is_empty()
    }

    /// How far round `node`'s caret is: 0 pointing right, 1 pointing down.
    ///
    /// A node with no turn in flight is wherever `expanded` says it is, which is
    /// why a settled caret costs no state.
    fn turn_of(&self, node: NodeId, expanded: bool) -> f32 {
        match self.turns.iter().find(|t| t.node == node) {
            Some(t) => t.progress,
            None if expanded => 1.0,
            None => 0.0,
        }
    }

    /// Start `node`'s caret turning towards `expanded`.
    ///
    /// Re-aimed rather than restarted if one is already in flight, so clicking
    /// twice quickly turns back from where it had got to instead of jumping to
    /// the far end first.
    fn start_turn(&mut self, node: NodeId, expanded: bool) {
        let now = Instant::now();
        let to = if expanded { 1.0 } else { 0.0 };
        match self.turns.iter_mut().find(|t| t.node == node) {
            Some(t) => {
                t.from = t.progress;
                t.started = now;
                t.to = to;
            }
            None => self.turns.push(Turn {
                node,
                progress: 1.0 - to,
                started: now,
                from: 1.0 - to,
                to,
            }),
        }
    }

    /// Advance every caret mid-turn, dropping the ones that have arrived.
    fn advance_turns(&mut self, now: Instant) -> bool {
        if self.turns.is_empty() {
            return false;
        }
        let secs = (theme::POPOVER_FADE_MS / 1000.0).max(f32::EPSILON);
        for t in &mut self.turns {
            let x = (now.duration_since(t.started).as_secs_f32() / secs).clamp(0.0, 1.0);
            t.progress = t.from + (t.to - t.from) * ease(x);
        }
        // Arrived means "at its end and out of time", so a caret cannot be
        // dropped early by a `progress` that merely passed through its target.
        self.turns
            .retain(|t| now.duration_since(t.started).as_secs_f32() < secs);
        true
    }

    /// How far open `kind` is, 0 to 1 — and 0 for a kind that is not the one
    /// showing. The chevron reads this, so it turns in step with the panel
    /// rather than keeping a clock of its own.
    pub fn popover_progress(&self, kind: PopoverKind) -> f32 {
        match self.popover.as_ref() {
            Some(p) if p.kind == kind => p.progress,
            _ => 0.0,
        }
    }

    /// Advance the fade and the slide between pages. Returns whether anything
    /// moved.
    ///
    /// A **deadline against elapsed time**, not a per-frame step, for the reason
    /// [`panel`] gives: a dropped frame strands a decrement, and a stranded
    /// animation is a window that never stops redrawing.
    ///
    /// [`panel`]: ../../../crates/koru-app-shell/src/panel.rs
    pub fn advance_popover(&mut self, now: Instant) -> bool {
        let mut moved = self.advance_turns(now);
        let Some(p) = self.popover.as_mut() else {
            return moved;
        };
        let secs = (theme::POPOVER_FADE_MS / 1000.0).max(f32::EPSILON);
        if let Some((started, from)) = p.slide {
            let t = (now.duration_since(started).as_secs_f32() / secs).clamp(0.0, 1.0);
            let to = page_of(p.kind);
            p.page = from + (to - from) * ease(t);
            if t >= 1.0 {
                p.page = to;
                p.slide = None;
            }
            moved = true;
        }
        let Some((started, from)) = p.fade else {
            return moved;
        };
        let t = (now.duration_since(started).as_secs_f32() / secs).clamp(0.0, 1.0);
        let to = if p.closing { 0.0 } else { 1.0 };
        p.progress = from + (to - from) * ease(t);
        if t >= 1.0 {
            p.progress = to;
            p.fade = None;
            if p.closing {
                self.popover = None;
            }
        }
        true
    }

    /// Shut whichever popover is open. Returns whether one was, so the caller
    /// can decide whether the press that did it was worth a repaint.
    ///
    /// It stays in `Some` until the fade finishes, but stops taking input on
    /// this call — so the press that dismissed it cannot also choose a row.
    pub fn close_popover(&mut self) -> bool {
        let Some(p) = self.popover.as_mut() else {
            return false;
        };
        if p.closing {
            return false;
        }
        p.closing = true;
        p.hover = None;
        p.fade = Some((Instant::now(), p.progress));
        true
    }

    /// Show `kind`, whatever was showing before. For the Workspaces page that
    /// is the **open** Profile's workspaces.
    ///
    /// **Not [`toggle_popover`](Self::toggle_popover)**, which a second press of
    /// the same chord would close: this is what a keyboard shortcut asks for,
    /// and a shortcut that opens the switcher has to leave it open so the arrows
    /// can walk it. Escape and Enter are how it shuts, which is what
    /// [`popover_key`](Self::popover_key) already answers.
    ///
    /// Answers whether anything changed, so a caller repaints only when it must.
    pub fn open_popover(&mut self, kind: PopoverKind) -> bool {
        if self.live().is_some_and(|p| p.showing(kind)) {
            return false;
        }
        self.toggle_popover(kind);
        true
    }

    /// Open `kind`, or shut it if it is already the page showing.
    ///
    /// Pressing the *other* half of the header slides the switcher to the
    /// other page rather than closing — the header is one control with two
    /// halves, and the switcher is one panel with two pages.
    fn toggle_popover(&mut self, kind: PopoverKind) {
        let now = Instant::now();
        let switcher = |k: PopoverKind| k != PopoverKind::LibraryActions;
        match self.popover.as_mut() {
            Some(p) if !p.closing && p.showing(kind) => {
                self.close_popover();
            }
            Some(p) if !p.closing && switcher(p.kind) && switcher(kind) => {
                p.kind = kind;
                p.browse = None;
                p.hover = None;
                p.scroll = 0.0;
                p.slide = Some((now, p.page));
            }
            Some(p) => {
                p.kind = kind;
                p.browse = None;
                p.hover = None;
                p.scroll = 0.0;
                p.closing = false;
                p.fade = Some((now, p.progress));
                p.page = page_of(kind);
                p.slide = None;
            }
            None => {
                self.popover = Some(Popover {
                    kind,
                    hover: None,
                    scroll: 0.0,
                    progress: 0.0,
                    fade: Some((now, 0.0)),
                    closing: false,
                    browse: None,
                    page: page_of(kind),
                    slide: None,
                })
            }
        }
    }

    /// Slide to `profile`'s workspaces. The switcher stays open: choosing a
    /// Profile is a step towards a workspace, not a switch.
    fn drill(&mut self, profile: ProfileId) {
        let browse = (self.current_profile != Some(profile)).then_some(profile);
        if let Some(p) = self.popover.as_mut().filter(|p| !p.closing) {
            p.kind = PopoverKind::Workspaces;
            p.browse = browse;
            p.hover = None;
            p.scroll = 0.0;
            p.slide = Some((Instant::now(), p.page));
        }
    }

    /// Slide back to the Profiles page, with the Profile just shown under the
    /// keyboard, so Enter goes straight back in.
    fn back(&mut self) {
        let browsed = self.browsed_profile();
        let hover = self.rows_for(PopoverKind::Profiles).iter().position(|row| {
            matches!(row, PopoverRow::Profile(i) if self.profiles.get(*i).map(|p| p.id) == browsed)
        });
        if let Some(p) = self.popover.as_mut().filter(|p| !p.closing) {
            p.kind = PopoverKind::Profiles;
            p.hover = hover;
            p.scroll = 0.0;
            p.slide = Some((Instant::now(), p.page));
        }
    }

    /// The Profile the Workspaces page lists.
    fn browsed_profile(&self) -> Option<ProfileId> {
        self.popover
            .as_ref()
            .and_then(|p| p.browse)
            .or(self.current_profile)
    }

    /// Another Profile's id while the Workspaces page lists it; `None` while it
    /// lists the open one.
    fn browsing_other(&self) -> Option<ProfileId> {
        self.popover
            .as_ref()
            .and_then(|p| p.browse)
            .filter(|id| self.current_profile != Some(*id))
    }

    /// The workspaces the Workspaces page lists: the open Profile's live view,
    /// or another Profile's rows from the Store catalog.
    fn browsed_workspaces(&self) -> &[WorkspaceEntry] {
        match self.browsing_other() {
            Some(id) => self.catalog.get(&id).map_or(&[], Vec::as_slice),
            None => &self.workspaces,
        }
    }

    /// The rows of the open popover, in order. Empty when none is.
    fn popover_rows(&self) -> Vec<PopoverRow> {
        match self.popover.as_ref() {
            Some(p) => self.rows_for(p.kind),
            None => Vec::new(),
        }
    }

    /// The rows of one page, whether or not it is the one showing — mid-slide
    /// both are drawn.
    fn rows_for(&self, kind: PopoverKind) -> Vec<PopoverRow> {
        match kind {
            PopoverKind::Workspaces => {
                let list = self.browsed_workspaces();
                let open_here = self.browsing_other().is_none();
                let mut out = vec![PopoverRow::Back, PopoverRow::WorkspacesHeading];
                // **Pinned first, archived not at all — except the one you are
                // in — and deleted never.** A workspace can be archived while
                // this window is showing it, so its current row remains useful.
                // Delete is different: the shell has already moved the window
                // elsewhere, and the manager's Deleted section is the one
                // deliberate route back to it.
                //
                // The index is into the browsed list and is what the row
                // carries, so filtering here cannot renumber anything: the rows
                // are built *from* indices rather than being renumbered after.
                let mut shown: Vec<usize> = (0..list.len())
                    .filter(|&i| {
                        let w = &list[i];
                        w.deleted_at.is_none()
                            && (!w.archived || (open_here && self.current == Some(w.id)))
                    })
                    .collect();
                shown.sort_by_key(|&i| !list[i].pinned);
                out.extend(shown.into_iter().map(PopoverRow::Workspace));
                out.extend([
                    PopoverRow::NewWorkspace,
                    PopoverRow::Divider,
                    PopoverRow::ImportContent,
                    PopoverRow::ManageWorkspaces,
                ]);
                out
            }
            PopoverKind::Profiles => {
                let mut out = vec![PopoverRow::ProfilesHeading];
                out.extend((0..self.profiles.len()).map(PopoverRow::Profile));
                out.extend([
                    PopoverRow::NewProfile,
                    PopoverRow::Divider,
                    PopoverRow::ManageProfiles,
                    PopoverRow::LinkDevice,
                ]);
                out
            }
            PopoverKind::LibraryActions => actions::available(self.mode)
                .iter()
                .copied()
                .map(PopoverRow::LibraryAction)
                .collect(),
        }
    }

    fn popover_row_h(row: PopoverRow) -> f32 {
        match row {
            PopoverRow::WorkspacesHeading | PopoverRow::ProfilesHeading => SECTION_H,
            PopoverRow::Divider => POPOVER_DIVIDER_H,
            PopoverRow::Profile(_) => koru_ui::profile_picker::ROW_H,
            _ => POPOVER_ROW_H,
        }
    }

    /// Whether `row` is one of a page's fixed top rows.
    fn page_head(row: PopoverRow) -> bool {
        matches!(
            row,
            PopoverRow::Back | PopoverRow::WorkspacesHeading | PopoverRow::ProfilesHeading
        )
    }

    /// Whether `row` begins a page's fixed footer.
    fn page_foot(row: PopoverRow) -> bool {
        matches!(row, PopoverRow::NewWorkspace | PopoverRow::NewProfile)
    }

    /// A page's height with every row showing, padding included.
    fn page_natural_h(&self, kind: PopoverKind) -> f32 {
        self.rows_for(kind)
            .into_iter()
            .map(Self::popover_row_h)
            .sum::<f32>()
            + POPOVER_PAD * 2.0
    }

    /// The floating panel, or `None` when nothing is open.
    ///
    /// **The one function that owns the popover's geometry**, alongside
    /// [`popover_rects`] which stacks inside it. Anchored under the header and
    /// clamped to the window, the same shape the tab menu has. Mid-slide its
    /// height moves between the two pages' heights, off the same number the
    /// pages slide by.
    ///
    /// [`popover_rects`]: Self::popover_rects
    pub fn popover_panel(&self, area: Rect, viewport: (f32, f32)) -> Option<Rect> {
        let p = self.popover.as_ref()?;
        if area.w <= 0.0 {
            return None;
        }
        let available_h = (viewport.1 - POPOVER_EDGE * 2.0).max(0.0);
        let h = if p.kind == PopoverKind::LibraryActions {
            self.page_natural_h(p.kind).min(available_h)
        } else {
            let cap = |kind| {
                self.page_natural_h(kind)
                    .min(SWITCHER_MAX_H)
                    .min(available_h)
            };
            let profiles = cap(PopoverKind::Profiles);
            let workspaces = cap(PopoverKind::Workspaces);
            profiles + (workspaces - profiles) * p.page
        };
        let w = POPOVER_W.max(area.w - theme::NAV_PAD * 2.0);
        let header = Self::header_rects(area);
        let x = match p.kind {
            PopoverKind::Profiles | PopoverKind::Workspaces => header.row.x,
            PopoverKind::LibraryActions => {
                let button = self.actions_button(area);
                (button.x + button.w - w).max(POPOVER_EDGE)
            }
        };
        let x = x.min((viewport.0 - w - POPOVER_EDGE).max(POPOVER_EDGE));
        let anchor = match p.kind {
            PopoverKind::LibraryActions => self.actions_button(area),
            PopoverKind::Profiles | PopoverKind::Workspaces => header.row,
        };
        let y =
            (anchor.y + anchor.h + POPOVER_GAP).min((viewport.1 - h - POPOVER_EDGE).max(anchor.y));
        Some(Rect::new(x.max(0.0), y.max(0.0), w, h))
    }

    /// Every row of the open popover, in the panel. Drawing, hit-testing and the
    /// keyboard all come here.
    pub fn popover_rects(&self, area: Rect, viewport: (f32, f32)) -> Vec<(PopoverRow, Rect)> {
        let Some(panel) = self.popover_panel(area, viewport) else {
            return Vec::new();
        };
        let Some(p) = self.popover.as_ref() else {
            return Vec::new();
        };
        self.page_rects(p.kind, panel, p.scroll)
    }

    /// One page's rows in `panel`: the fixed top rows, the scrolling list, and
    /// the fixed footer pinned to the panel's floor.
    fn page_rects(&self, kind: PopoverKind, panel: Rect, scroll: f32) -> Vec<(PopoverRow, Rect)> {
        let rows = self.rows_for(kind);
        if kind == PopoverKind::LibraryActions {
            let mut y = panel.y + POPOVER_PAD;
            return rows
                .into_iter()
                .map(|row| {
                    let h = Self::popover_row_h(row);
                    let r = Rect::new(panel.x, y, panel.w, h);
                    y += h;
                    (row, r)
                })
                .collect();
        }
        let head = rows.iter().take_while(|row| Self::page_head(**row)).count();
        let foot = rows
            .iter()
            .position(|row| Self::page_foot(*row))
            .unwrap_or(rows.len());
        let mut out = Vec::with_capacity(rows.len());
        let mut y = panel.y + POPOVER_PAD;
        for row in rows[..head].iter().copied() {
            let h = Self::popover_row_h(row);
            out.push((row, Rect::new(panel.x, y, panel.w, h)));
            y += h;
        }
        let mut list_y = y - scroll;
        for row in rows[head..foot].iter().copied() {
            let h = Self::popover_row_h(row);
            out.push((row, Rect::new(panel.x, list_y, panel.w, h)));
            list_y += h;
        }
        let foot_h: f32 = rows[foot..].iter().copied().map(Self::popover_row_h).sum();
        let mut foot_y = panel.y + panel.h - POPOVER_PAD - foot_h;
        for row in rows[foot..].iter().copied() {
            let h = Self::popover_row_h(row);
            out.push((row, Rect::new(panel.x, foot_y, panel.w, h)));
            foot_y += h;
        }
        out
    }

    /// The band between a page's fixed top rows and its fixed footer, where
    /// the list scrolls.
    fn page_band(&self, kind: PopoverKind, panel: Rect) -> Option<Rect> {
        if kind == PopoverKind::LibraryActions {
            return None;
        }
        let rows = self.rows_for(kind);
        let head_h: f32 = rows
            .iter()
            .copied()
            .take_while(|row| Self::page_head(*row))
            .map(Self::popover_row_h)
            .sum();
        let foot_h: f32 = rows
            .iter()
            .position(|row| Self::page_foot(*row))
            .map_or(0.0, |at| {
                rows[at..].iter().copied().map(Self::popover_row_h).sum()
            });
        let top = panel.y + POPOVER_PAD + head_h;
        let bottom = panel.y + panel.h - POPOVER_PAD - foot_h;
        Some(Rect::new(panel.x, top, panel.w, (bottom - top).max(0.0)))
    }

    fn popover_list_band(&self, area: Rect, viewport: (f32, f32)) -> Option<Rect> {
        let kind = self.popover.as_ref()?.kind;
        let panel = self.popover_panel(area, viewport)?;
        self.page_band(kind, panel)
    }

    fn page_list_content_h(&self, kind: PopoverKind) -> f32 {
        self.rows_for(kind)
            .into_iter()
            .filter(|row| matches!(row, PopoverRow::Workspace(_) | PopoverRow::Profile(_)))
            .map(Self::popover_row_h)
            .sum()
    }

    fn popover_list_content_h(&self) -> f32 {
        self.popover
            .as_ref()
            .map_or(0.0, |p| self.page_list_content_h(p.kind))
    }

    fn popover_scroll_max(&self, area: Rect, viewport: (f32, f32)) -> f32 {
        let visible = self.popover_list_band(area, viewport).map_or(0.0, |r| r.h);
        (self.popover_list_content_h() - visible).max(0.0)
    }

    fn clamp_popover_scroll(&mut self, area: Rect, viewport: (f32, f32)) {
        let max = self.popover_scroll_max(area, viewport);
        if let Some(popover) = self.popover.as_mut() {
            popover.scroll = popover.scroll.clamp(0.0, max);
        }
    }

    /// Scroll only the item band; the heading and footer never move.
    pub fn scroll_popover(&mut self, area: Rect, viewport: (f32, f32), dy: f32) -> bool {
        if !matches!(
            self.live().map(|p| p.kind),
            Some(PopoverKind::Workspaces | PopoverKind::Profiles)
        ) {
            return false;
        }
        let before = self.popover.as_ref().map_or(0.0, |p| p.scroll);
        if let Some(popover) = self.popover.as_mut() {
            popover.scroll -= dy;
        }
        self.clamp_popover_scroll(area, viewport);
        self.popover.as_ref().is_some_and(|p| p.scroll != before)
    }

    /// Keep a row reached with the arrow keys inside the scrolling identity
    /// band. Pointer scrolling and keyboard navigation share the same offset;
    /// only the latter needs this reveal step.
    pub fn reveal_popover_hover(&mut self, area: Rect, viewport: (f32, f32)) -> bool {
        let Some(index) = self.live().and_then(|popover| popover.hover) else {
            return false;
        };
        let Some(band) = self.popover_list_band(area, viewport) else {
            return false;
        };
        let Some((row, rect)) = self.popover_rects(area, viewport).get(index).copied() else {
            return false;
        };
        if !matches!(row, PopoverRow::Workspace(_) | PopoverRow::Profile(_)) {
            return false;
        }
        let delta = if rect.y < band.y {
            rect.y - band.y
        } else if rect.y + rect.h > band.y + band.h {
            rect.y + rect.h - (band.y + band.h)
        } else {
            0.0
        };
        if delta == 0.0 {
            return false;
        }
        if let Some(popover) = self.popover.as_mut() {
            popover.scroll += delta;
        }
        self.clamp_popover_scroll(area, viewport);
        true
    }

    /// Where each piece of a workspace row goes.
    ///
    /// One function, because drawing and hit-testing both come here. Zero-sized
    /// rects for a row that is not a workspace, so a caller never has to ask
    /// twice.
    pub fn workspace_row_parts(&self, row: PopoverRow, r: Rect) -> WorkspaceRowParts {
        let empty = Rect::new(r.x, r.y, 0.0, 0.0);
        if !matches!(row, PopoverRow::Workspace(_)) {
            return WorkspaceRowParts {
                chip: empty,
                name: empty,
                pin: empty,
            };
        }
        let chip = Rect::new(
            r.x + theme::NAV_PAD,
            r.y + (r.h - WORKSPACE_CHIP) * 0.5,
            WORKSPACE_CHIP,
            WORKSPACE_CHIP,
        );
        // **There is no tick.** The workspace you are in is the one in bold,
        // which is a stronger signal than a mark at the far end of the row.
        let pin = Rect::new(
            (r.x + r.w - WORKSPACE_DOTS - 8.0).max(r.x),
            r.y,
            WORKSPACE_DOTS,
            r.h,
        );
        let name_x = chip.x + chip.w + 8.0;
        WorkspaceRowParts {
            chip,
            name: Rect::new(name_x, r.y, (pin.x - name_x - 4.0).max(1.0), r.h),
            pin,
        }
    }

    /// Where a workspace's row is, so the container can draw a field over it.
    ///
    /// The rename field lives in the container, not here — this pane stays below
    /// `koru-editor`, and `TextField` is the editor's. So the pane
    /// hands out the rectangle and the container puts an editable box in it,
    /// which keeps the crate boundary and reuses the one text field the app has.
    pub fn popover_workspace_rect(
        &self,
        area: Rect,
        viewport: (f32, f32),
        id: WorkspaceId,
    ) -> Option<Rect> {
        let list = self.browsed_workspaces();
        self.popover_rects(area, viewport)
            .into_iter()
            .find_map(|(row, r)| match row {
                PopoverRow::Workspace(i) if list.get(i).is_some_and(|w| w.id == id) => {
                    Some(self.workspace_row_parts(row, r).name)
                }
                _ => None,
            })
    }

    pub fn popover_profile_rect(
        &self,
        area: Rect,
        viewport: (f32, f32),
        id: ProfileId,
    ) -> Option<Rect> {
        let band = self.popover_list_band(area, viewport)?;
        self.popover_rects(area, viewport)
            .into_iter()
            .find_map(|(row, r)| match row {
                PopoverRow::Profile(i)
                    if self.profiles.get(i).is_some_and(|profile| profile.id == id)
                        && r.y >= band.y
                        && r.y + r.h <= band.y + band.h =>
                {
                    Some(koru_ui::profile_picker::row_parts(r).name)
                }
                _ => None,
            })
    }

    /// Keep the workspace that owns an open context menu visibly hot.
    pub fn highlight_popover_workspace(&mut self, id: WorkspaceId) -> bool {
        let list = self.browsed_workspaces();
        let Some(index) = self.popover_rows().iter().position(|row| {
            matches!(row, PopoverRow::Workspace(i) if list.get(*i).is_some_and(|w| w.id == id))
        }) else {
            return false;
        };
        let Some(popover) = self.popover.as_mut().filter(|p| !p.closing) else {
            return false;
        };
        let changed = popover.hover != Some(index);
        popover.hover = Some(index);
        changed
    }

    fn popover_at(
        &self,
        area: Rect,
        viewport: (f32, f32),
        x: f32,
        y: f32,
    ) -> Option<(usize, PopoverRow)> {
        // Deaf the moment it is dismissed, however long it is still on screen.
        self.live()?;
        let band = self.popover_list_band(area, viewport);
        self.popover_rects(area, viewport)
            .into_iter()
            .enumerate()
            .find(|(_, (row, r))| {
                let in_band = !matches!(row, PopoverRow::Workspace(_) | PopoverRow::Profile(_))
                    || band.is_some_and(|band| band.contains(x, y));
                in_band && r.contains(x, y)
            })
            .map(|(i, (row, _))| (i, row))
    }

    /// Whether choosing `row` keeps the switcher open. Profiles and the back
    /// row move between its pages; everything else is a destination.
    fn stays_open(row: PopoverRow) -> bool {
        matches!(row, PopoverRow::Profile(_) | PopoverRow::Back)
    }

    /// What the container should do about a popover row being chosen.
    fn choose(&mut self, row: PopoverRow) -> Outcome {
        match row {
            PopoverRow::Workspace(i) => {
                let Some(id) = self.browsed_workspaces().get(i).map(|w| w.id) else {
                    return Outcome::Consumed;
                };
                match self.browsing_other() {
                    Some(profile) => Outcome::SwitchToWorkspaceIn {
                        profile,
                        workspace: id,
                    },
                    None => Outcome::SwitchWorkspace(id),
                }
            }
            PopoverRow::NewWorkspace => match self.browsing_other() {
                Some(profile) => Outcome::NewWorkspaceIn(profile),
                None => Outcome::NewWorkspace,
            },
            PopoverRow::ImportContent => Outcome::ImportContent,
            PopoverRow::ManageWorkspaces => Outcome::ManageWorkspaces(self.browsed_profile()),
            // **A Profile opens its workspaces; it does not switch.** You
            // switch Profile by choosing one of its workspaces.
            PopoverRow::Profile(i) => {
                if let Some(id) = self.profiles.get(i).map(|p| p.id) {
                    self.drill(id);
                }
                Outcome::Consumed
            }
            PopoverRow::Back => {
                self.back();
                Outcome::Consumed
            }
            PopoverRow::NewProfile => Outcome::NewProfile,
            PopoverRow::AddAccount => Outcome::AddProfile,
            PopoverRow::OpenProfile => Outcome::OpenProfile,
            PopoverRow::ManageProfiles => Outcome::ManageProfiles,
            PopoverRow::LinkDevice => Outcome::LinkDevice,
            PopoverRow::LibraryAction(action) => self.choose_library_action(action),
            PopoverRow::WorkspacesHeading | PopoverRow::ProfilesHeading | PopoverRow::Divider => {
                Outcome::Consumed
            }
        }
    }

    fn choose_library_action(&mut self, action: actions::Action) -> Outcome {
        match action {
            actions::Action::Import => match self.mode {
                NavigatorMode::Documents => Outcome::ImportContent,
                NavigatorMode::Files => Outcome::ImportFiles,
                // Never offered by either. Tasks lists no actions at all, and
                // `actions::available` lists no Import for Agents, whose
                // catalog nothing here may write.
                NavigatorMode::Tasks
                | NavigatorMode::Chats
                | NavigatorMode::Agents
                | NavigatorMode::Artifacts
                | NavigatorMode::Canvases
                | NavigatorMode::Socials => Outcome::Consumed,
            },
            actions::Action::CollapseAll => {
                self.pending_fold = Some(fold::Fold::Collapse);
                Outcome::Consumed
            }
            actions::Action::ExpandAll => {
                self.pending_fold = Some(fold::Fold::Expand);
                Outcome::Consumed
            }
        }
    }

    /// The keys an open popover answers to.
    ///
    /// `None` for anything else, and that falls **through** to the container's
    /// chords rather than being swallowed: a popover hanging off a column is not
    /// a modal over the document, so Ctrl+E must still shut the pane under it.
    ///
    /// Left goes back from a Profile's workspaces; Right and Enter on a Profile
    /// go in, landing on its first row so the arrows carry on from there.
    pub fn popover_key(&mut self, key: &winit::keyboard::Key) -> Option<Outcome> {
        use winit::keyboard::{Key, NamedKey};
        let kind = self.live()?.kind;
        let rows = self.popover_rows();
        let hovered = self
            .live()
            .and_then(|p| p.hover)
            .and_then(|i| rows.get(i).copied());
        match key {
            Key::Named(NamedKey::Escape) => {
                self.close_popover();
                Some(Outcome::Consumed)
            }
            Key::Named(NamedKey::ArrowDown) | Key::Named(NamedKey::ArrowUp) => {
                let forward = matches!(key, Key::Named(NamedKey::ArrowDown));
                let from = self.live().and_then(|p| p.hover);
                if let Some(p) = self.popover.as_mut() {
                    p.hover = next_selectable(&rows, from, forward);
                }
                Some(Outcome::Consumed)
            }
            Key::Named(NamedKey::ArrowLeft) if kind == PopoverKind::Workspaces => {
                self.back();
                Some(Outcome::Consumed)
            }
            Key::Named(NamedKey::ArrowRight) | Key::Named(NamedKey::Enter)
                if matches!(hovered, Some(PopoverRow::Profile(_))) =>
            {
                let outcome = self.choose(hovered.expect("matched above"));
                self.hover_first_workspace();
                Some(outcome)
            }
            Key::Named(NamedKey::Enter) if hovered == Some(PopoverRow::Back) => {
                Some(self.choose(PopoverRow::Back))
            }
            Key::Named(NamedKey::Enter) => {
                self.close_popover();
                Some(match hovered {
                    Some(row) => self.choose(row),
                    None => Outcome::Consumed,
                })
            }
            _ => None,
        }
    }

    /// Put the keyboard on the first row after the back row and heading.
    fn hover_first_workspace(&mut self) {
        let rows = self.popover_rows();
        let first = rows
            .iter()
            .position(|row| !Self::page_head(*row) && row.selectable());
        if let Some(p) = self.popover.as_mut() {
            p.hover = first;
        }
    }

    /// The one function that owns the header's geometry. Drawing and
    /// hit-testing both come here, so the chevron can never be clickable
    /// somewhere other than where it is drawn. Font-free, like every other rect
    /// in this crate.
    pub fn header_rects(area: Rect) -> HeaderRects {
        // **The band is a tab pill in another column, and shares its insets.**
        // It had its own, three pixels taller and half a pixel lower, which is
        // visible the moment a tab sits beside it across the divide. Taking
        // `theme::TAB_INSET_*` is what stops the two drifting apart again.
        let inset_top = theme::TAB_INSET_TOP;
        let inset_bottom = theme::TAB_INSET_BOTTOM;
        // Mid-slide the pane is narrower than its own padding, and a row inset
        // by 8 into a 1px column would start outside the pane it lives in — and
        // `Rect::contains` being inclusive, would then answer for a click on the
        // view beside it.
        let row = Rect::new(
            (area.x + theme::NAV_PAD).min(area.x + area.w),
            area.y + inset_top,
            (area.w - theme::NAV_PAD * 2.0).max(0.0),
            (WORKSPACE_ROW_H - inset_top - inset_bottom).max(0.0),
        );
        // **Two controls with a gap between them**, not one row split down the
        // middle. The avatar opens the profiles and the name opens the
        // workspaces, and a hover fill that ran from one straight into the
        // other would say they were the same button.
        let avatar_hit = Rect::new(row.x, row.y, row.h.min(row.w), row.h);
        let avatar = Rect::new(
            avatar_hit.x + (avatar_hit.w - AVATAR).max(0.0) * 0.5,
            avatar_hit.y + (avatar_hit.h - AVATAR).max(0.0) * 0.5,
            AVATAR.min(avatar_hit.w),
            AVATAR.min(avatar_hit.h),
        );
        let name_x = (avatar_hit.x + avatar_hit.w + AVATAR_GAP).min(row.x + row.w);
        let name_hit = Rect::new(name_x, row.y, (row.x + row.w - name_x).max(0.0), row.h);
        let chevron = Rect::new(
            (name_hit.x + name_hit.w - CHEVRON_W - CHEVRON_PAD).max(name_hit.x),
            row.y,
            CHEVRON_W.min(name_hit.w),
            row.h,
        );
        let name = Rect::new(
            (name_hit.x + NAME_PAD).min(name_hit.x + name_hit.w),
            row.y,
            (chevron.x - name_hit.x - NAME_PAD - 4.0).max(0.0),
            row.h,
        );
        HeaderRects {
            row,
            avatar,
            avatar_hit,
            name,
            name_hit,
            chevron,
            // **Inside the band**, exactly as the caption draws its own rule at
            // `cap.y + cap.h - 1`. One pixel lower and the pane reads as the
            // taller of the two.
            divider: Rect::new(area.x, area.y + WORKSPACE_ROW_H - 1.0, area.w, 1.0),
        }
    }

    /// Which part of the header a point is on.
    ///
    /// `None` in a collapsed column, the same guard `row_rects` uses and for the
    /// same reason: `contains` is inclusive on every edge, so a zero-width
    /// header would answer for the view beside it.
    pub fn header_at(area: Rect, x: f32, y: f32) -> Option<HeaderPart> {
        if area.w <= 0.0 {
            return None;
        }
        let h = Self::header_rects(area);
        if !h.row.contains(x, y) {
            return None;
        }
        // The avatar first: it sits inside the band, so the band would
        // otherwise claim it, exactly as a tab's close box would be swallowed
        // by the tab it is drawn in.
        if h.avatar_hit.contains(x, y) {
            return Some(HeaderPart::Avatar);
        }
        // The gap between the two belongs to neither — pressing it should do
        // nothing rather than open whichever control happens to be adjacent.
        h.name_hit.contains(x, y).then_some(HeaderPart::Workspace)
    }

    /// Where the first library row sits.
    ///
    /// Everything above it — the workspace row, the divider, the section gap,
    /// the "Library" heading and the second section gap — is chrome the rows
    /// begin below. `row_rects`, `scroll_by` and `draw` all come here, so the
    /// band they agree on cannot drift when the header grows again.
    ///
    /// **Constant.** It briefly depended on an open switcher's height, back when
    /// the switcher pushed the library down; a popover floats over the rows
    /// instead, so opening one moves nothing. The trailing `NAV_PAD` is the gap
    /// between the tab row and the nodes content — the same gap the header and
    /// the tab row have, owned by the shell's stack and visible in the
    /// inspector as an `ExploreSpacer`.
    fn rows_top(&self, area: Rect) -> f32 {
        area.y + WORKSPACE_ROW_H + theme::NAV_PAD + HEADER_H + theme::NAV_PAD
    }

    /// The "Library" heading band, which is also where the fold buttons live.
    ///
    /// Positioned from the shell's stack directly rather than back from
    /// [`rows_top`](Self::rows_top): the gap between this band and the rows is
    /// a section gap of the shell, not part of either band.
    ///
    /// Zero-width in a collapsed column, so heading actions decline it — the same
    /// guard `row_rects` and `header_at` use, and for the same reason.
    fn heading_band(&self, area: Rect) -> Rect {
        if area.w <= 0.0 {
            return Rect::new(area.x, area.y, 0.0, 0.0);
        }
        Rect::new(
            area.x,
            area.y + WORKSPACE_ROW_H + theme::NAV_PAD,
            area.w,
            HEADER_H,
        )
    }

    /// Where the mode selectors end, which is the floor every heading action
    /// yields to.
    ///
    /// **One function, because there are three modes now.** Four call sites
    /// each wrote `NAV_PAD + HEADER_H * 2.0 + 2.0` from the same premise, and
    /// adding a third icon to a two-icon sum in three places is how a `…`
    /// starts overlapping a button.
    fn modes_right(&self, area: Rect) -> f32 {
        let modes = self.navigator_mode_rects(area);
        modes
            .last()
            .map(|(_, r)| r.x + r.w + 2.0)
            .unwrap_or(area.x + theme::NAV_PAD)
    }

    /// The library heading's compact actions button. It yields instead of
    /// overlapping the mode selectors while a narrow pane is animating.
    fn actions_button(&self, area: Rect) -> Rect {
        let band = self.heading_band(area);
        // A mode with no actions has no button — a zero-width rect, the same
        // idiom a collapsed column and a hidden context toggle both use, so the
        // search icon takes the room back with no second code path.
        if actions::available(self.mode).is_empty() {
            return Rect::new(band.x + band.w, band.y, 0.0, band.h);
        }
        actions::button(band, self.modes_right(area))
    }

    fn actions_at(&self, area: Rect, x: f32, y: f32) -> bool {
        let button = self.actions_button(area);
        button.w > 0.0 && button.contains(x, y)
    }

    pub fn search_button(&self, area: Rect) -> Rect {
        let band = self.heading_band(area);
        let menu = self.actions_button(area);
        let modes_right = self.modes_right(area);
        let w = menu.h;
        if band.w <= 0.0 || menu.x - modes_right < w {
            return Rect::new(menu.x, menu.y, 0.0, menu.h);
        }
        Rect::new(menu.x - w, menu.y, w, menu.h)
    }

    fn search_at(&self, area: Rect, x: f32, y: f32) -> bool {
        let button = self.search_button(area);
        button.w > 0.0 && button.contains(x, y)
    }

    /// The mode selectors, left to right. **One arithmetic for all four**, so
    /// a fifth is a row in this array rather than a fourth `+ size + 2.0`.
    pub fn navigator_mode_rects(&self, area: Rect) -> Vec<(NavigatorMode, Rect)> {
        let band = self.heading_band(area);
        // **Sized from the modes this build shows** (decided 2026-10-10).
        // The flags are latched per
        // process, so within one build the set — and with it every icon's
        // size and place — never changes, and Files is always second. A
        // build that shows few modes gets full `HEADER_H` icons; they shrink
        // only when more are shown than fit.
        let visible = NavigatorMode::visible();
        let count = visible.len().max(2) as f32;
        // The two trailing lanes, search and `…`, each `HEADER_H` wide, plus
        // the 2 px `modes_right` leaves after the last icon.
        let lanes = 2. * HEADER_H + 2.;
        // Gaps shrink first (2 px down to 1), then the icons, so that the
        // shown icons and both lanes share the default column. Below the width
        // where an icon would drop under `MIN_MODE_ICON`, the lanes give way
        // instead (their buttons decline the room) and the icons take the
        // whole band.
        let gap = ((area.w - theme::NAV_PAD - count * HEADER_H - lanes) / (count - 1.))
            .floor()
            .clamp(1., 2.);
        let with_lanes = ((area.w - theme::NAV_PAD - lanes - (count - 1.) * gap) / count).floor();
        let size = if with_lanes >= MIN_MODE_ICON {
            HEADER_H.min(with_lanes)
        } else {
            HEADER_H.min(((area.w - theme::NAV_PAD * 2. - (count - 1.) * gap) / count).max(0.))
        };
        // **One order**, the one the chords count in: `Alt+1` is the first
        // icon because both read `NavigatorMode::ALL`. Only the modes this
        // build shows get an icon ([`NavigatorMode::visible`]), packed left.
        visible
            .into_iter()
            .enumerate()
            .map(|(i, mode)| {
                (
                    mode,
                    Rect::new(
                        area.x + theme::NAV_PAD + i as f32 * (size + gap),
                        band.y,
                        size,
                        band.h,
                    ),
                )
            })
            .collect()
    }

    fn navigator_mode_at(&self, area: Rect, x: f32, y: f32) -> Option<NavigatorMode> {
        self.navigator_mode_rects(area)
            .into_iter()
            .find_map(|(mode, rect)| rect.contains(x, y).then_some(mode))
    }

    /// The glyph the mode switcher draws for each mode. Its own function so
    /// drawing and the test agree on one answer: the Agents mark is a robot
    /// (Q-E7 P7), the per-row session icons are the products' own marks (P6).
    fn navigator_mode_icon(mode: NavigatorMode) -> char {
        match mode {
            NavigatorMode::Documents => icons::DOCUMENT,
            NavigatorMode::Files => icons::FILE,
            NavigatorMode::Tasks => icons::LIST_CHECKS,
            NavigatorMode::Agents => icons::ROBOT,
            // Named by the surface, so the mode row and the column cannot
            // disagree about which picture means *artifacts*.
            NavigatorMode::Artifacts => koru_artifacts_ui::MODE_ICON,
            // Named by the surface, for the same reason.
            NavigatorMode::Chats => koru_chats_ui::MODE_ICON,
            NavigatorMode::Canvases => icons::CANVAS,
            NavigatorMode::Socials => icons::MEGAPHONE,
        }
    }

    fn draw_navigator_modes(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
    ) {
        let p = theme::p();
        for (mode, rect) in self.navigator_mode_rects(area) {
            if mode == self.mode || self.mode_hover == Some(mode) {
                scene::fill(
                    scene,
                    root,
                    if mode == self.mode {
                        p.panel_bg
                    } else {
                        p.hover_tint
                    },
                    &scene::rrect(rect.x, rect.y + 2.0, rect.w, rect.h - 4.0, 5.0),
                );
            }
            let icon = Self::navigator_mode_icon(mode);
            draw_icon(
                scene,
                root,
                engine,
                icon,
                rect,
                if mode == self.mode {
                    p.text
                } else {
                    p.text_dim
                },
            );
        }
    }

    /// Where row `i` sits — scrolled or not, on screen or not.
    ///
    /// **The arithmetic on its own.** [`row_rects`](Self::row_rects) is this
    /// plus the culling; the accessibility projection is this *without* it,
    /// because a row scrolled out of view is still a row a screen reader has to
    /// be able to find and reveal. Two callers, one sum, so a row can never be
    /// published somewhere other than where it is drawn.
    pub fn row_band(&self, area: Rect, i: usize) -> Rect {
        let y = self.rows_top(area) + i as f32 * theme::NAV_ROW_H - self.scroll;
        Rect::new(area.x, y, area.w, theme::NAV_ROW_H)
    }

    /// The one function that owns the row geometry. Drawing and hit-testing
    /// both come here, so a row can never be clickable somewhere other than
    /// where it is drawn.
    ///
    /// Rows outside the band below the header are not returned at all, which is
    /// both the culling and the hit test's bounds check.
    pub fn row_rects(&self, area: Rect) -> Vec<(usize, Rect)> {
        // A collapsed column has no rows to offer. `Rect::contains` is
        // inclusive on every edge, so a zero-width row at x=0 would otherwise
        // answer for clicks meant for the view beside it.
        if area.w <= 0.0 {
            return Vec::new();
        }
        let top = self.rows_top(area);
        let first = ((self.scroll / theme::NAV_ROW_H).floor().max(0.0)) as usize;
        let visible = (area.h / theme::NAV_ROW_H).ceil() as usize + 2;
        (first..(first + visible).min(self.rows.len()))
            .map(|i| (i, self.row_band(area, i)))
            // Culled against the rows' **own** band, not the pane's. Scrolled
            // far enough, a row's `y` goes above `top` — and against `area.y` it
            // would survive the filter and be drawn over the header. The floor
            // is `rows_bottom`, not the pane's, so the library scrolls above the
            // pinned Trash row rather than under it.
            .filter(|(_, r)| r.y + r.h > top && r.y < self.rows_bottom(area))
            .collect()
    }

    /// The caret box inside a row, or a zero-sized rect for a leaf. Separate so
    /// clicking the caret expands without also opening the node.
    fn caret_rect(&self, row: &Row, r: Rect) -> Rect {
        if !row.has_children {
            return Rect::new(r.x, r.y, 0.0, 0.0);
        }
        Rect::new(
            r.x + theme::NAV_PAD + row.depth as f32 * theme::NAV_INDENT,
            r.y,
            theme::NAV_INDENT,
            r.h,
        )
    }

    /// Every rect inside a library row, derived from the row and its band alone.
    ///
    /// One function per row rather than two, so the label's width and the `…`'s
    /// position cannot be computed from different premises.
    pub fn row_parts(&self, row: &Row, r: Rect) -> RowParts {
        // The caret lane is reserved across the board in Documents, where any
        // row can grow one as data lands; elsewhere only a row that actually
        // has one pays for it. See `icon_lane`.
        let icon_x = icon_lane(
            r.x,
            row.depth,
            self.mode == NavigatorMode::Documents || row.has_children,
        );
        // `PAGE_ICON_SLOT`, not a number of its own: the slot has to hold a
        // colour emoji as well as an outline glyph, and the tab strip reserves
        // the same width for the same reason.
        let text_x = icon_x + PAGE_ICON_SLOT + 4.0;
        let dots = Rect::new(
            (r.x + r.w - theme::NAV_PAD - ROW_DOTS).max(r.x),
            r.y + (r.h - ROW_DOTS) * 0.5,
            ROW_DOTS,
            ROW_DOTS,
        );
        // The note lane sits immediately left of the `…` lane, and exists only
        // on a row that carries a note. Zero-sized otherwise — `Rect::contains`
        // is inclusive on every edge, so a zero-width rect at the row's edge
        // would answer for clicks on the `…`'s doorstep.
        let note = if row.note.is_some() {
            let x = (dots.x - ROW_NOTE - 4.0).max(text_x);
            Rect::new(x, r.y + (r.h - ROW_NOTE) * 0.5, ROW_NOTE, ROW_NOTE)
        } else {
            Rect::new(dots.x, dots.y, 0.0, 0.0)
        };
        // The label stops at whichever adornment the row actually has.
        let right = if row.note.is_some() { note.x } else { dots.x };
        RowParts {
            caret: self.caret_rect(row, r),
            icon: Rect::new(icon_x, r.y, PAGE_ICON_SLOT, r.h),
            // **Bounded by the trailing lanes whether or not they are
            // showing.** If the `…` took its space only on hover, the label
            // would reflow as the pointer moved down the list, which reads as
            // the list twitching rather than as a button arriving. Same rule,
            // same reason, as `workspace_row_parts`.
            label: Rect::new(text_x, r.y, (right - 6.0 - text_x).max(1.0), r.h),
            note,
            dots,
        }
    }

    /// The Trash row, pinned to the floor of the column.
    ///
    /// **Reserved out of the rows' band**, so the library scrolls *above* it
    /// rather than under it. That is the whole of "pinned": there is no second
    /// draw path and no second scroll origin, only one fewer row of band.
    ///
    /// Zero-sized in a collapsed column, the guard `row_rects` and `header_at`
    /// both use — `Rect::contains` is inclusive on every edge, so a zero-width
    /// row at `x = 0` would answer for a click meant for the view beside it.
    /// Which rows are pinned to the floor, top to bottom.
    ///
    /// One place to add the next one. Everything else — the band the library
    /// gets, the hit test, the hover, the drawing — follows from this.
    fn footer_rows(&self) -> &'static [FooterRow] {
        if self.showing_socials() {
            &[]
        } else {
            &[FooterRow::Trash]
        }
    }

    /// Where each pinned row sits.
    ///
    /// Bottom-anchored, so adding a row grows the stack upward and the library's
    /// band shrinks by exactly that much. Empty in a collapsed column — the
    /// guard `row_rects` and `header_at` both use, because `Rect::contains` is
    /// inclusive on every edge and a zero-width row at `x = 0` would answer for
    /// a click meant for the view beside it.
    pub fn footer_rects(&self, area: Rect) -> Vec<(FooterRow, Rect)> {
        if area.w <= 0.0 {
            return Vec::new();
        }
        let rows = self.footer_rows();
        let mut out = Vec::with_capacity(rows.len());
        let mut y = area.y + area.h - FOOTER_PAD;
        for row in rows.iter().rev() {
            y -= theme::NAV_ROW_H;
            out.push((*row, Rect::new(area.x, y, area.w, theme::NAV_ROW_H)));
        }
        out.reverse();
        out
    }

    /// Whether an armed drag is currently over the Trash row.
    ///
    /// **`y` only.** A row-drag follows the pointer vertically and is allowed to
    /// wander outside the column horizontally — the same reason `drop_target_for`
    /// ignores `x` — so asking for a hit inside the rect would make the gesture
    /// fail whenever the pointer drifted a few pixels left or right.
    fn pointer_over_trash(&self, area: Rect, y: f32) -> bool {
        self.footer_rects(area)
            .into_iter()
            .any(|(row, r)| row == FooterRow::Trash && y >= r.y && y < r.y + r.h)
    }

    /// Whether the Trash row should be drawn as a live drop target.
    pub fn trash_is_drop_target(&self, area: Rect) -> bool {
        self.drag
            .as_ref()
            .is_some_and(|d| d.armed && self.pointer_over_trash(area, d.y))
    }

    /// Which pinned row `(x, y)` is on, if any.
    pub fn footer_at(&self, area: Rect, x: f32, y: f32) -> Option<FooterRow> {
        self.footer_rects(area)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(row, _)| row)
    }

    /// Where the Trash row is. Kept as its own name because it is the one the
    /// container and the tests ask about by name.
    // Test-driven only since the ADR-262 move; kept as the documented seam.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn trash_rect(&self, area: Rect) -> Rect {
        self.footer_rects(area)
            .into_iter()
            .find(|(row, _)| *row == FooterRow::Trash)
            .map(|(_, r)| r)
            .unwrap_or_else(|| Rect::new(area.x, area.y, 0.0, 0.0))
    }

    /// Where the library rows end. Four places used to say `area.y + area.h`;
    /// the pinned rows come out of the band once, here.
    fn rows_bottom(&self, area: Rect) -> f32 {
        self.footer_rects(area)
            .first()
            .map(|(_, r)| r.y)
            .unwrap_or(area.y + area.h)
    }

    /// What a pinned row shows: its glyph and its label.
    ///
    /// **No count.** A number here is one more thing to read on every glance at
    /// the column, and it answers a question almost nobody is asking — the bin
    /// is a place you go when you want it, and it says how full it is when you
    /// get there. It also means the row is **always live**: a row that refused
    /// to open with nothing to explain why is worse than one that opens on
    /// "The Trash is empty."
    fn footer_parts(&self, row: FooterRow) -> (char, &'static str) {
        match row {
            FooterRow::Trash => (icons::TRASH, "Trash"),
        }
    }

    /// How many items are in the Trash, as of the last rebuild.
    /// How many documents are in the workspace's trash.
    ///
    /// Pushed rather than counted here, because the bin is the *manifest's* and
    /// this crate holds none. `rebuild` still sets it from `ws.trash_len()` for
    /// a workspace with no manifest, which is what the editor's own suite
    /// drives; a push overrides that the moment one arrives.
    pub fn set_trash_count(&mut self, n: usize) -> bool {
        let changed = self.trash_count != n;
        self.trash_count = n;
        changed
    }

    pub fn trash_count(&self) -> usize {
        self.trash_count
    }

    /// The node row `i` shows, if `i` is a row.
    // Test-driven only since the ADR-262 move; kept as the documented seam.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn row_node(&self, i: usize) -> Option<NodeId> {
        self.rows.get(i).map(|r| r.node)
    }

    /// The node of the row at `(x, y)`, if there is one there.
    ///
    /// What `F2` asks: the container has a point and wants to know which node
    /// the person is pointing at, without learning what a row index means.
    pub fn row_node_at(&self, area: Rect, x: f32, y: f32) -> Option<NodeId> {
        self.row_at(area, x, y).map(|i| self.rows[i].node)
    }

    /// The label column of the row naming `node`, so a rename field can sit
    /// over the name rather than over the caret or the `…`.
    pub fn row_label_rect(&self, area: Rect, node: NodeId) -> Option<Rect> {
        self.row_rects(area).into_iter().find_map(|(i, r)| {
            (self.rows[i].node == node).then(|| self.row_parts(&self.rows[i], r).label)
        })
    }

    fn row_at(&self, area: Rect, x: f32, y: f32) -> Option<usize> {
        self.row_rects(area)
            .into_iter()
            .find(|(_, r)| r.contains(x, y))
            .map(|(i, _)| i)
    }

    /// Window coordinates, not panel-local: this is chrome, and the container
    /// hands it the rect it lives in.
    ///
    /// `ctrl` is the only route to opening a workspace in a second window, the
    /// same modifier that means "somewhere else" everywhere it already appears.
    ///
    /// `viewport` is the window, which the popover needs and the rows do not —
    /// a floating panel is clamped to the window rather than to the column.
    #[cfg(test)]
    pub fn mouse_down(
        &mut self,
        area: Rect,
        viewport: (f32, f32),
        x: f32,
        y: f32,
        ctrl: bool,
    ) -> Outcome {
        self.mouse_down_with_modifiers(area, viewport, x, y, ctrl, false)
    }

    /// Pointer press with the complete selection modifiers supplied by the
    /// container. Shift arrives through `View::set_mods`, while Ctrl is part of
    /// the shell's established pointer vocabulary.
    pub fn mouse_down_with_modifiers(
        &mut self,
        area: Rect,
        viewport: (f32, f32),
        x: f32,
        y: f32,
        ctrl: bool,
        shift: bool,
    ) -> Outcome {
        // The popover first: it floats *over* the pane, so a row underneath it
        // would otherwise answer for a press meant for a menu item — the same
        // ordering a tab's close box needs against the tab it sits in.
        if self.popover_open() {
            if let Some((_, row)) = self.popover_at(area, viewport, x, y) {
                if !Self::stays_open(row) {
                    self.close_popover();
                }
                return self.choose(row);
            }
            // The header's halves move the switcher between its pages, or shut
            // the page already showing. Anywhere else shuts it; falling through
            // to the header below would immediately reopen it.
            match Self::header_at(area, x, y) {
                Some(HeaderPart::Avatar) => self.toggle_popover(PopoverKind::Profiles),
                Some(HeaderPart::Workspace) => self.toggle_popover(PopoverKind::Workspaces),
                None => {
                    self.close_popover();
                }
            }
            return Outcome::Consumed;
        }
        // Match the document scrollbar: the wider invisible gutter claims the
        // press before any row beneath it. The thumb drags from the exact grab
        // point; the track above and below pages by one visible band.
        if let Some(g) = self.scrollbar_geom(area)
            && g.in_gutter(x, y)
        {
            if g.on_thumb(x, y) {
                self.scroll_drag = Some(y - g.thumb_top);
            } else {
                let band = (g.track_bottom - g.track_top).max(0.0);
                self.scroll += if y < g.thumb_top { -band } else { band };
                self.clamp_scroll(area);
            }
            return Outcome::Consumed;
        }

        // Each half of the header opens its own list, in place, telling the
        // container nothing. That is the caret precedent: this view already
        // mutates `collapsed` without asking, because where its own furniture is
        // open is its own business.
        match Self::header_at(area, x, y) {
            Some(HeaderPart::Avatar) => {
                self.toggle_popover(PopoverKind::Profiles);
                return Outcome::Consumed;
            }
            Some(HeaderPart::Workspace) => {
                self.toggle_popover(PopoverKind::Workspaces);
                return Outcome::Consumed;
            }
            None => {}
        }

        if let Some(mode) = self.navigator_mode_at(area, x, y) {
            self.set_navigator_mode(mode);
            return if mode == NavigatorMode::Socials {
                Outcome::OpenSocials
            } else {
                Outcome::Consumed
            };
        }

        if self.actions_at(area, x, y) {
            self.toggle_popover(PopoverKind::LibraryActions);
            return Outcome::Consumed;
        }
        if self.search_at(area, x, y) {
            return Outcome::Search;
        }

        // The rail owns everything below the heading in tasks mode — including
        // the background, which is still ours: a press on it must not fall
        // through to the view beside the column.
        if self.mode == NavigatorMode::Tasks {
            let top = self.rows_top(area);
            return match self.rail.mouse_down(area, top, x, y, ctrl) {
                Some(request) => Outcome::Tasks(request),
                None => Outcome::Consumed,
            };
        }
        // The artifacts surface owns everything below the heading in its
        // mode, on the rail's terms and for the rail's reason — and reports
        // no request, because it performs what it offers itself.
        if self.mode == NavigatorMode::Artifacts {
            let top = self.rows_top(area);
            self.artifacts.mouse_down(area, top, x, y);
            return Outcome::Consumed;
        }
        // The Chats view owns everything below the heading in its mode, and
        // reports what it cannot do itself.
        if self.mode == NavigatorMode::Chats {
            let top = self.rows_top(area);
            self.chats.mouse_down(area, top, x, y);
            return self.chats_outcome();
        }

        if self.showing_socials() {
            return self.socials.as_ref().map_or(Outcome::Consumed, |surface| {
                Self::socials_outcome(surface.borrow_mut().navigation_mouse_down(x, y))
            });
        }

        // The empty-state button, which stands where the rows would be.
        if self.empty_cta.is_some_and(|cta| cta.contains(x, y)) {
            return self.empty_state().2;
        }
        if koru_features::cloud_sync() && self.empty_subscribe.is_some_and(|cta| cta.contains(x, y))
        {
            return Outcome::SubscribeCloud;
        }
        // The pinned rows, before `row_at` because they sit below the library's
        // band and are not part of it. A collapsed column offers none, so the
        // zero-width guard is inside `footer_rects` rather than repeated here.
        if let Some(row) = self.footer_at(area, x, y) {
            // **Always live**, empty or not. Without a count beside it there is
            // nothing to explain a row that refuses to open, and the Trash says
            // it is empty perfectly well once you are in it.
            return match row {
                FooterRow::Trash => Outcome::OpenTrash,
            };
        }

        let Some(i) = self.row_at(area, x, y) else {
            // Still ours — a press on the column's background must not fall
            // through to whatever is behind it.
            return Outcome::Consumed;
        };
        // Pointing at a row is also how the tree takes the keyboard, so the
        // arrows carry on from what you just clicked rather than from wherever
        // they were last. The Agents heading is furniture: pressing its `+`
        // must not park the focus ring on a label.
        if !(self.mode == NavigatorMode::Agents
            && matches!(
                self.rows[i].node,
                node if node == Self::agent_projects_header_node()
                    || node == Self::agent_schedules_header_node()
            ))
        {
            self.focus = Some(self.rows[i].node);
        }
        let rects = self.row_rects(area);
        let Some((_, r)) = rects.iter().find(|(j, _)| *j == i) else {
            return Outcome::Consumed;
        };
        let row = self.rows[i].clone();
        let parts = self.row_parts(&row, *r);
        // **The `…` before anything else in the row**, and — more to the point —
        // before the drag is seeded below. The lane is reserved on every row
        // whether the button shows or not, so a press inside it can only ever
        // mean the menu; not seeding a drag there is what keeps the ordering
        // from being load-bearing.
        // No `…` in Agents mode: its rows raise their menus by right-click,
        // Koru Terminal's shape — the one thing in the trailing lane there is
        // the heading's `+`, handled in the Agents branch below.
        if self.mode != NavigatorMode::Agents && parts.dots.contains(x, y) {
            let nodes = self.select_for_menu(row.node);
            self.menu_row = Some(row.node);
            return Outcome::NodeMenu {
                nodes,
                // The button's bottom-right, so the menu hangs back under it
                // rather than out past the column's edge.
                at: (parts.dots.x + parts.dots.w, parts.dots.y + parts.dots.h),
                from_button: true,
            };
        }
        // The note glyph, with the same standing as the `…`: it answers for
        // its lane before the row can seed a drag, because pressing it means
        // *open the note*, never *move the file*. Its own outcome rather than
        // `OpenNode`, which in this projection means *Save as* for the row's
        // own id — and this id is the note's.
        if let Some(note) = row.note
            && parts.note.contains(x, y)
        {
            return Outcome::OpenNote(note);
        }
        if parts.caret.contains(x, y) {
            // Toggled against the fold set rather than `row.expanded`, which
            // is a *cached* copy: the rows are only refreshed when the caller
            // rebuilds, so trusting them here would make two clicks in a row
            // collapse twice instead of collapsing and expanding.
            let expanded = self.fold_set().remove(&row.node);
            if !expanded {
                self.fold_set().insert(row.node);
            }
            self.start_turn(row.node, expanded);
            return Outcome::Consumed;
        }
        // Agents rows: the heading's `+` creates, a session row seeds a latent
        // drag (release opens its tab, crossing the threshold carries it
        // between projects), and a project row folds. Reported before the
        // manifest drag seed below so the document machinery never arms over
        // the catalog.
        if self.mode == NavigatorMode::Agents {
            if row.node == Self::agent_projects_header_node() {
                if parts.dots.contains(x, y) {
                    return Outcome::NewAgentProject;
                }
                return Outcome::Consumed;
            }
            // **A run row seeds one too.** Its release opens the run, exactly
            // as a plain press always did; what crossing the threshold buys
            // is the one thing a run can be dragged *to* — the pinned Trash
            // row, which is the same gesture the document tree already has.
            //
            // **And it builds the selection on the way**, through the same
            // `press_selection` a document row uses — so Ctrl and Shift
            // compose with the latent drag here exactly as they do there
            // rather than undoing it: the release is still what opens the
            // run, and `select_only` is what tells it not to.
            let is_run = self.agent_run_ids.contains_key(&row.node);
            let is_schedule = self.agent_schedule_ids.contains_key(&row.node);
            let is_session = self.is_agent_session_row(row.node);
            if is_schedule {
                if self
                    .selection
                    .iter()
                    .any(|node| !self.agent_schedule_ids.contains_key(node))
                {
                    self.selection.clear();
                    self.selection_anchor = None;
                }
                if !ctrl && !shift {
                    self.selection.clear();
                }
                self.press_selection(i, ctrl, shift);
                if ctrl || shift {
                    return Outcome::Consumed;
                }
                return self.agents_row_pressed(row.node);
            }
            if is_run || is_session {
                if is_run {
                    if self
                        .selection
                        .iter()
                        .any(|node| !self.agent_run_ids.contains_key(node))
                    {
                        self.selection.clear();
                        self.selection_anchor = None;
                    }
                    self.press_selection(i, ctrl, shift);
                } else {
                    // A session is not a run, so it joins no selection — and
                    // a press on one drops the runs' selection rather than
                    // letting `arm_drag` carry them off behind a session.
                    self.selection.clear();
                    self.selection_anchor = None;
                }
                self.drag = Some(RowDrag {
                    grab: row.node,
                    index: i,
                    grab_dy: y - r.y,
                    origin_y: y,
                    pressed_at: Instant::now(),
                    y,
                    area,
                    armed: false,
                    select_only: is_run && (ctrl || shift),
                    // Unused on the Agents release path — `mouse_up` answers
                    // through `open_row_outcome`, which is the session's tab
                    // or the run's.
                    click: ClickAction::Select,
                    carried: HashSet::new(),
                    carried_order: Vec::new(),
                    item_count: 1,
                    last: None,
                    spring: None,
                });
                return Outcome::Consumed;
            }
            // A group row is a place. Pressing one puts the runs' selection
            // down, the way pressing a document row that is not in the
            // selection does.
            self.selection.clear();
            self.selection_anchor = None;
            return self.agents_row_pressed(row.node);
        }
        // Manifest rows may be dragged. Seed a **latent** drag — it stays a
        // click until the pointer crosses `DRAG_ARM_PX`. Documents activate on
        // release; files stay selected because opening or saving a file is an
        // explicit action. Non-manifest landmarks open immediately.
        if matches!(row.kind, Kind::Document | Kind::File) {
            self.press_selection(i, ctrl, shift);
            self.drag = Some(RowDrag {
                grab: row.node,
                index: i,
                grab_dy: y - r.y,
                origin_y: y,
                pressed_at: Instant::now(),
                y,
                area,
                armed: false,
                select_only: ctrl || shift,
                click: match (row.kind, row.file_state) {
                    (Kind::Document, _) => ClickAction::Open,
                    (Kind::File, FileState::Remote) => ClickAction::Download,
                    _ => ClickAction::Select,
                },
                carried: HashSet::new(),
                carried_order: Vec::new(),
                item_count: 1,
                last: None,
                spring: None,
            });
            return Outcome::Consumed;
        }
        Outcome::OpenNode(row.node)
    }

    /// **The press half of the multi-selection**, for every projection that
    /// has one.
    ///
    /// Plain, Ctrl and Shift mean the same three things in the library and in
    /// the Agents tree, so they are decided once here rather than twice: the
    /// only thing that differs between the two is *which rows may join a
    /// selection at all*, and that is [`joins_selection`](Self::joins_selection).
    fn press_selection(&mut self, i: usize, ctrl: bool, shift: bool) {
        let node = self.rows[i].node;
        if shift {
            // Rows are a pre-order projection of the tree, so the inclusive
            // slice includes every visible child between the two ends.
            let anchor = self
                .selection_anchor
                .and_then(|id| self.rows.iter().position(|r| r.node == id))
                .unwrap_or(i);
            let (first, last) = if anchor <= i {
                (anchor, i)
            } else {
                (i, anchor)
            };
            // Include the lower endpoint's visible subtree too. Its
            // children follow it in pre-order and stop at the next row at
            // the same or a shallower depth.
            let last_depth = self.rows[last].depth;
            let mut subtree_end = last;
            while self
                .rows
                .get(subtree_end + 1)
                .is_some_and(|r| r.depth > last_depth)
            {
                subtree_end += 1;
            }
            let range: Vec<NodeId> = self.rows[first..=subtree_end]
                .iter()
                .map(|r| r.node)
                .filter(|node| self.joins_selection(*node))
                .collect();
            self.selection.clear();
            self.selection.extend(range);
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some(node);
            }
        } else if ctrl {
            // Ctrl builds a selection rather than clearing it; the toggle
            // is what a no-arm release leaves behind.
            if !self.selection.remove(&node) {
                self.selection.insert(node);
            }
            self.selection_anchor = Some(node);
        } else {
            // Keep an existing multi-selection alive while the press is
            // latent, so dragging any selected row carries the whole set.
            // A release that never arms collapses it back to this row.
            if !self.selection.contains(&node) {
                self.selection.clear();
                self.selection.insert(node);
            }
            self.selection_anchor = Some(node);
        }
    }

    /// **May this row join the multi-selection?**
    ///
    /// The library says yes to everything in its band — a Shift range there
    /// spans a subtree, and the odd landmark caught between two documents has
    /// always travelled with them. The Agents tree says yes only to **runs**:
    /// its other rows are a live process and a registry record, each with its
    /// own verbs and neither of them a thing the set-shaped ones (delete, and
    /// the drag onto the bin) could act on. Without this the one selection
    /// model would hand a *Delete 4 runs* menu a project and a session.
    fn joins_selection(&self, node: NodeId) -> bool {
        if self.mode != NavigatorMode::Agents {
            return true;
        }
        self.agent_run_ids.contains_key(&node) || self.agent_schedule_ids.contains_key(&node)
    }

    /// The runs of the multi-selection, in the order the rows are shown.
    ///
    /// The row order rather than the listing's, because *in the order the
    /// rows are shown* is what a Shift range means and what a receipt counts.
    fn selected_runs(&self) -> Vec<koru_delegate_core::RunId> {
        self.rows
            .iter()
            .filter(|row| self.selection.contains(&row.node))
            .filter_map(|row| self.agent_run_ids.get(&row.node).copied())
            .collect()
    }

    fn selected_schedules(&self) -> Vec<koru_delegate_core::ScheduleId> {
        self.rows
            .iter()
            .filter(|row| self.selection.contains(&row.node))
            .filter_map(|row| self.agent_schedule_ids.get(&row.node).copied())
            .collect()
    }

    /// Count what a latent drag will actually move, including documents nested
    /// below each selected root.
    ///
    /// Mouse geometry deliberately has no document parameter, so the container
    /// calls this immediately after a row press while it already holds the
    /// workspace. Selected descendants are excluded as roots before summing,
    /// otherwise a Shift range would count the same subtree twice.
    /// Say how many documents this drag is carrying.
    ///
    /// **Pushed when there is a manifest**, because the count lives there and
    /// this crate holds none. [`refresh_drag_item_count`] below counts through
    /// the document *tree*, which knows nothing about manifest documents — so
    /// store-backed it answered for a shape that is not there, and the drop
    /// label over-counted in one direction and under-counted in the other.
    ///
    /// [`refresh_drag_item_count`]: Self::refresh_drag_item_count
    pub fn set_drag_item_count(&mut self, count: usize) {
        if let Some(d) = self.drag.as_mut() {
            d.item_count = count.max(1);
        }
    }

    /// The selected roots this drag is carrying, for a caller that can count
    /// what travels with them.
    pub fn dragged_roots(&self) -> Vec<NodeId> {
        let Some(grab) = self.drag.as_ref().map(|d| d.grab) else {
            return Vec::new();
        };
        let mut roots: Vec<NodeId> = self.selection.iter().copied().collect();
        if !roots.contains(&grab) {
            roots.push(grab);
        }
        roots
    }

    pub fn refresh_drag_item_count(&mut self, ws: &WorkspaceDoc) {
        let Some(grab) = self.drag.as_ref().map(|d| d.grab) else {
            return;
        };
        let mut selected = self.selection.clone();
        selected.insert(grab);
        let roots: Vec<_> = selected
            .iter()
            .copied()
            .filter(|node| {
                !ws.ancestors(*node)
                    .map(|ancestors| ancestors.iter().any(|id| selected.contains(id)))
                    .unwrap_or(false)
            })
            .collect();
        let count = roots
            .into_iter()
            .map(|node| 1 + ws.nested_documents(node))
            .sum::<usize>()
            .max(1);
        if let Some(d) = self.drag.as_mut() {
            d.item_count = count;
        }
    }

    /// What a right-click in the column means.
    ///
    /// A row wants its node menu. Empty space in the rows band offers importing;
    /// chrome outside that band is swallowed rather than falling through to the
    /// view underneath, exactly as the popover's right-click is.
    /// Opening a menu is the container's job; this updates the pane's selection
    /// and reports the nodes the menu is about.
    pub fn context_menu(&mut self, area: Rect, x: f32, y: f32) -> Outcome {
        // The rail owns everything below the heading in tasks mode, on this
        // gesture for the reason it owns `mouse_down`: `self.rows` is the
        // library's, and asking it where a right-click landed in tasks mode
        // would answer with a document nothing on screen is showing.
        if self.mode == NavigatorMode::Tasks {
            let top = self.rows_top(area);
            return match self.rail.context_menu(area, top, x, y) {
                Some(request) => Outcome::Tasks(request),
                None => Outcome::Consumed,
            };
        }
        // **No row menu here, deliberately.** Everything a result offers is
        // on the row; a right-click that opened a menu of one disabled item
        // would be a promise the column cannot keep. The press is still
        // ours, so it does not fall through to the view beside the column.
        if matches!(self.mode, NavigatorMode::Artifacts | NavigatorMode::Socials) {
            return Outcome::Consumed;
        }
        // A chat row's menu, whose one verb is *Delete chat*. Reported, not
        // built — the boundary every other row menu here keeps.
        if self.mode == NavigatorMode::Chats {
            let top = self.rows_top(area);
            return match self.chats.context_menu(area, top, x, y) {
                Some(chat) => Outcome::AiChatMenu { chat, at: (x, y) },
                None => Outcome::Consumed,
            };
        }

        // The Agents projection raises Koru Terminal's tree menus: a project
        // row offers New session ▸, Rename… and Delete project; the bucket and
        // empty tree space offer only New session — the bucket is never
        // renamed, deleted or chosen (ADR-264). A session row offers its own
        // menu — Open in new tab, Move to new window, Move to project ▸,
        // Terminate (Q-E7 P8) — reported for the container to build.
        if self.mode == NavigatorMode::Agents {
            return match self.row_at(area, x, y) {
                Some(i) => {
                    let node = self.rows[i].node;
                    // A session row raises Koru Terminal's session menu: Open
                    // in new tab, Move to new window, Move to project ▸,
                    // Terminate (Q-E7 P8). The pane reports which session; the
                    // container builds the menu, the same boundary the project
                    // menu keeps.
                    if let Some(session) = self.agent_session_ids.get(&node).copied() {
                        // A session joins no run selection, so a menu about
                        // one puts that selection down first — the library's
                        // rule that a menu's subject is what is chosen.
                        self.selection.clear();
                        self.selection_anchor = None;
                        return Outcome::AgentSessionMenu {
                            session,
                            at: (x, y),
                        };
                    }
                    // A run row's one verb is Delete. Reported, not built:
                    // whether these runs may be deleted and what should happen
                    // to what they produced are both the container's.
                    //
                    // **The selection when the click landed inside it**, on
                    // `select_for_menu`'s own rule — a right-click on a run
                    // that is not selected makes it the sole selection first,
                    // so the menu's subject is always what the column is
                    // showing as chosen.
                    if self.agent_run_ids.contains_key(&node) {
                        self.select_for_menu(node);
                        return Outcome::AgentRunMenu {
                            runs: self.selected_runs(),
                            at: (x, y),
                        };
                    }
                    if self.agent_schedule_ids.contains_key(&node) {
                        self.select_for_menu(node);
                        return Outcome::AgentScheduleMenu {
                            schedules: self.selected_schedules(),
                            at: (x, y),
                        };
                    }
                    if let Some(schedule) = self.agent_schedule_metadata_ids.get(&node).copied() {
                        self.selection.clear();
                        self.selection_anchor = None;
                        return Outcome::AgentScheduleMenu {
                            schedules: vec![schedule],
                            at: (x, y),
                        };
                    }
                    // A fired-run child and the heading never resolve to a
                    // schedule mutation.
                    if self.agent_schedule_run_ids.contains_key(&node)
                        || node == Self::agent_schedules_header_node()
                    {
                        self.selection.clear();
                        self.selection_anchor = None;
                        return Outcome::Consumed;
                    }
                    // Neither a session nor a run: the menu is about a place,
                    // and the runs' selection is not its subject.
                    self.selection.clear();
                    self.selection_anchor = None;
                    if let Some(project) = self.agent_project_at(node) {
                        return Outcome::AgentTreeMenu {
                            project: Some(project),
                            at: (x, y),
                        };
                    }
                    // The bucket or the heading: launch into Unassigned.
                    Outcome::AgentTreeMenu {
                        project: None,
                        at: (x, y),
                    }
                }
                None => {
                    let top = self.rows_top(area);
                    let bottom = self.rows_bottom(area);
                    let in_rows_band =
                        Rect::new(area.x, top, area.w, (bottom - top).max(0.0)).contains(x, y);
                    let on_scrollbar = self.scrollbar_geom(area).is_some_and(|g| g.in_gutter(x, y));
                    if in_rows_band && !on_scrollbar {
                        Outcome::AgentTreeMenu {
                            project: None,
                            at: (x, y),
                        }
                    } else {
                        Outcome::Consumed
                    }
                }
            };
        }
        match self.row_at(area, x, y) {
            Some(i) => {
                let nodes = self.select_for_menu(self.rows[i].node);
                Outcome::NodeMenu {
                    nodes,
                    at: (x, y),
                    from_button: false,
                }
            }
            // The library menu is *import into this workspace*, and nothing
            // imports as a canvas: empty space in the section offers nothing.
            None if self.mode == NavigatorMode::Canvases => Outcome::Consumed,
            None => {
                let top = self.rows_top(area);
                let bottom = self.rows_bottom(area);
                let in_rows_band =
                    Rect::new(area.x, top, area.w, (bottom - top).max(0.0)).contains(x, y);
                let on_scrollbar = self.scrollbar_geom(area).is_some_and(|g| g.in_gutter(x, y));
                if in_rows_band && !on_scrollbar {
                    Outcome::LibraryMenu { at: (x, y) }
                } else {
                    Outcome::Consumed
                }
            }
        }
    }

    /// Use the complete selection only when the menu was requested from inside
    /// it. A menu on an unselected row first makes that row the sole selection,
    /// matching the action's visible subject.
    fn select_for_menu(&mut self, node: NodeId) -> Vec<NodeId> {
        if !self.selection.contains(&node) {
            self.selection.clear();
            self.selection.insert(node);
            self.selection_anchor = Some(node);
        }
        self.rows
            .iter()
            .map(|r| r.node)
            .filter(|id| self.selection.contains(id))
            .collect()
    }

    /// Forget which row's menu is open, so its `…` stops being drawn.
    // Unconstructed since the ADR-262 move made dead code visible; a later wave of
    // the app-shell migration deletes or revives it.
    #[allow(dead_code)]
    pub fn close_row_menu(&mut self) -> bool {
        self.menu_row.take().is_some()
    }

    /// A double-click on an image row asks the container to open its rename
    /// field. A double-click on a file whose bytes are here opens it in the
    /// associated app. Document rows already navigated on the first click.
    pub fn mouse_double_click(&self, area: Rect, viewport: (f32, f32), x: f32, y: f32) -> Outcome {
        // The header and the popover acted on the first click. Without this,
        // the second click of a double-click would re-toggle the popover and it
        // would appear to ignore every other press.
        if Self::header_at(area, x, y).is_some() || self.popover_at(area, viewport, x, y).is_some()
        {
            return Outcome::Consumed;
        }
        let Some(i) = self.row_at(area, x, y) else {
            return Outcome::Consumed;
        };
        let row = &self.rows[i];
        // The note glyph answered on the first click — opening the note — so
        // its lane is not the row's: a double-click that lands there must not
        // also open the file in its OS app on the way down.
        let parts = self
            .row_rects(area)
            .into_iter()
            .find(|(r, _)| *r == i)
            .map(|(_, rect)| self.row_parts(row, rect));
        if row.note.is_some() && parts.is_some_and(|p| p.note.contains(x, y)) {
            return Outcome::Consumed;
        }
        match (row.kind, row.file_state) {
            (Kind::Image, _) => Outcome::RenameImage(row.node),
            (Kind::File, FileState::Remote) => Outcome::DownloadFile(row.node),
            (Kind::File, FileState::Present) => Outcome::OpenFile(row.node),
            _ => Outcome::Consumed,
        }
    }

    pub fn mouse_move(&mut self, area: Rect, viewport: (f32, f32), x: f32, y: f32) -> bool {
        // A grabbed thumb follows the pointer outside its narrow lane, exactly
        // like the document scrollbar and the row drag below.
        if let Some(grab) = self.scroll_drag {
            if let Some(g) = self.scrollbar_geom(area) {
                self.scroll = g.scroll_for_pointer(y, grab);
                self.clamp_scroll(area);
            }
            return true;
        }

        // A drag in flight follows the pointer wherever it goes — including out
        // of the column, the way a leaf row-drag does — so it is settled before
        // any hover tracking that would otherwise move a highlight under it.
        if let Some(d) = self.drag.as_mut() {
            d.y = y;
            d.area = area;
            if !d.armed {
                if (y - d.origin_y).abs() < DRAG_ARM_PX {
                    // Still latent: track the hover the way a click would, so
                    // the row under the pointer reads as active.
                    let hover = self.row_at(area, x, y);
                    let moved = hover != self.hover;
                    self.hover = hover;
                    return moved;
                }
                self.arm_drag();
            }
            self.update_drop(Instant::now());
            return true;
        }

        let header = Self::header_at(area, x, y);
        let popover = self.popover_at(area, viewport, x, y).map(|(i, _)| i);
        let actions = popover.is_none() && self.actions_at(area, x, y);
        let search = popover.is_none() && self.search_at(area, x, y);
        let mode = if popover.is_none() {
            self.navigator_mode_at(area, x, y)
        } else {
            None
        };
        let on_footer = if popover.is_some() {
            None
        } else {
            self.footer_at(area, x, y)
        };
        let on_cta = popover.is_none() && self.empty_cta.is_some_and(|c| c.contains(x, y));
        let on_subscribe =
            popover.is_none() && self.empty_subscribe.is_some_and(|c| c.contains(x, y));
        let scrollbar =
            popover.is_none() && self.scrollbar_geom(area).is_some_and(|g| g.in_gutter(x, y));
        let hover = if header.is_some()
            || popover.is_some()
            || actions
            || search
            || mode.is_some()
            || on_footer.is_some()
            || scrollbar
        {
            None
        } else {
            self.row_at(area, x, y)
        };

        // The rail's own rows, tracked on the same pass and under the same
        // exclusions — the heading, the mode buttons and an open popover all
        // outrank it.
        let artifacts_moved = if self.mode == NavigatorMode::Artifacts {
            let top = self.rows_top(area);
            if header.is_some() || popover.is_some() || actions || search || mode.is_some() {
                self.artifacts.clear_hover()
            } else {
                self.artifacts.mouse_move(area, top, x, y)
            }
        } else {
            false
        };
        let chats_moved = if self.mode == NavigatorMode::Chats {
            let top = self.rows_top(area);
            if header.is_some() || popover.is_some() || actions || search || mode.is_some() {
                self.chats.clear_hover()
            } else {
                self.chats.mouse_move(area, top, x, y)
            }
        } else {
            false
        };
        let rail_moved = if self.mode == NavigatorMode::Tasks {
            let top = self.rows_top(area);
            // A live drag follows the pointer wherever it went, including over
            // the header — the same rule the library's own drag keeps.
            if !self.rail.dragging()
                && (header.is_some() || popover.is_some() || actions || search || mode.is_some())
            {
                self.rail.clear_hover()
            } else {
                self.rail.mouse_move(area, top, x, y)
            }
        } else {
            false
        };
        let socials_moved = self.showing_socials()
            && self.socials.as_ref().is_some_and(|surface| {
                let blocked =
                    header.is_some() || popover.is_some() || actions || search || mode.is_some();
                surface.borrow_mut().navigation_mouse_move(
                    if blocked { -1. } else { x },
                    if blocked { -1. } else { y },
                )
            });
        let mut moved = socials_moved
            || rail_moved
            || artifacts_moved
            || chats_moved
            || hover != self.hover
            || header != self.header_hover
            || actions != self.actions_hover
            || search != self.search_hover
            || mode != self.mode_hover
            || on_footer != self.footer_hover
            || on_cta != self.cta_hover
            || on_subscribe != self.subscribe_hover
            || scrollbar != self.scrollbar_hover;
        self.hover = hover;
        self.header_hover = header;
        self.actions_hover = actions;
        self.search_hover = search;
        self.mode_hover = mode;
        self.footer_hover = on_footer;
        self.cta_hover = on_cta;
        self.subscribe_hover = on_subscribe;
        self.scrollbar_hover = scrollbar;
        // A closing one keeps whatever it had: repainting a highlight onto a
        // panel already on its way out is the pointer chasing a ghost.
        if let Some(p) = self.popover.as_mut().filter(|p| !p.closing) {
            moved |= p.hover != popover;
            p.hover = popover;
        }
        moved
    }

    // --------------------------------------------------------- drag-to-move
    //
    // The contract mirrors the tab strip's, lifted into a tree. The `rows` cache
    // is the live display truth and reorders the instant a swap is decided; what
    // animates is only how far from its slot each row is *drawn* ([`RowSlides`]).
    // The carried rows themselves are drawn where the pointer puts them and get
    // exactly one easing, on release. Chrome never mutates the document: a drag
    // ends by reporting [`Outcome::MoveNodes`], and the container calls
    // `move_node`.

    /// A drag is armed — the pointer crossed the threshold and rows are moving.
    pub fn drag_armed(&self) -> bool {
        self.drag.as_ref().is_some_and(|d| d.armed)
    }

    /// A press is in flight, latent or armed.
    pub fn drag_active(&self) -> bool {
        self.drag.is_some()
    }

    /// Any pointer capture that must keep receiving moves outside the pane —
    /// the library's row drag, a scrollbar, or a list row on the tasks rail.
    pub fn pointer_active(&self) -> bool {
        self.drag.is_some() || self.scroll_drag.is_some() || self.rail.dragging()
    }

    /// Abort either kind of pointer capture. Returns whether anything changed.
    pub fn cancel_pointer(&mut self) -> bool {
        let had = self.pointer_active();
        self.scroll_drag = None;
        let _ = self.cancel_drag();
        let _ = self.rail.cancel_drag();
        had
    }

    /// Abort a press or armed drag without committing. The document was never
    /// mutated — only the row cache was — so the caller rebuilds from the
    /// document to restore positions. Returns whether a drag was cleared.
    pub fn cancel_drag(&mut self) -> bool {
        let Some(_) = self.drag.take() else {
            return false;
        };
        self.drag_tick = None;
        self.slides.clear();
        true
    }

    /// Show or hide the lifted carried rows mid-drag. The container hides them
    /// while the pointer is over a pane — there the drop opens the document and
    /// the pane's own preview (slot marker, split wash) is the answer, so a
    /// ghost row following the pointer would promise a tree move that will not
    /// happen. Answers whether it changed, for the redraw question.
    pub fn set_carried_hidden(&mut self, hidden: bool) -> bool {
        std::mem::replace(&mut self.carried_hidden, hidden) != hidden
    }

    /// Whether the lifted carried rows are currently hidden — test-facing.
    #[cfg(test)]
    pub fn carried_hidden(&self) -> bool {
        self.carried_hidden
    }

    /// Drop any in-flight slide. Used when a move could not be committed and the
    /// cache has to snap back to what the document says.
    pub fn clear_animation(&mut self) {
        self.slides.clear();
    }

    /// Whether a row slide is in flight — the release settle or a neighbour's
    /// relocation. Distinct from [`animating`](Self::animating), which also
    /// covers popover fades and an armed drag.
    pub fn slides_animating(&self) -> bool {
        self.slides.animating()
    }

    /// True while a slide, a popover fade, or an armed drag is in flight. The
    /// armed-drag term keeps frames coming for auto-scroll even when the slides
    /// are at rest; it ends with the release, so this returns to `false` on its
    /// own (ADR-26). The held-press term keeps frames coming for the ~100 ms it
    /// takes the grab cursor to reveal, then drops out the moment it has.
    /// ...and while a run in the *Runs* group is still working, which is the
    /// one term here that is not a gesture: it is the list itself moving.
    pub fn animating(&self) -> bool {
        self.popover_animating()
            || self.slides.animating()
            || self.drag_armed()
            || self.hold_reveal_pending()
            || self.rail.animating()
            || self.runs_turning()
    }

    /// **Whether a run in the navigator still has a ring to turn.**
    ///
    /// `agent_runs_working` is filled only by the Agents rebuild and cleared
    /// at the top of it, so a navigator showing documents, files or tasks
    /// answers `false` and does no timer work — and so does one whose runs
    /// have all finished, failed or been cancelled. That is standing
    /// requirement 3's "idle surfaces do no timer work", and it is the same
    /// answer `koru_runs_ui`'s worker stops polling on.
    fn runs_turning(&self) -> bool {
        self.mode == NavigatorMode::Agents && !self.agent_runs_working.is_empty()
    }

    /// How far round every working run's ring is at `now`, in radians.
    ///
    /// One anchor for the list rather than one per row: six rings turning out
    /// of step read as six unrelated things happening, and the truth is one.
    /// The loop is the app's progress loop — `PULSE_LOOP_MS`, the same 1.2 s
    /// the pulsing dots breathe on — over `koru_draw::anim::loop_phase`,
    /// which is this tree's one piece of repeating-animation arithmetic.
    fn runs_spin_angle(&self, now: Instant) -> f64 {
        let Some(started) = self.agent_runs_spin_started else {
            return 0.0;
        };
        f64::from(koru_draw::anim::loop_phase(
            started,
            now,
            theme::PULSE_LOOP_MS,
        )) * std::f64::consts::TAU
    }

    /// A latent press not yet held past `DRAG_HOLD_MS` — the window in which the
    /// cursor still has to flip to the grab hand, so the shell keeps frames
    /// coming until it does.
    fn hold_reveal_pending(&self) -> bool {
        match self.drag.as_ref() {
            Some(d) if !d.armed => {
                Instant::now().duration_since(d.pressed_at).as_secs_f32() < DRAG_HOLD_MS / 1000.0
            }
            _ => false,
        }
    }

    /// Advance the slide and, while a drag is armed, drive edge-band auto-scroll
    /// and the spring-open of a collapsed node held under the pointer. Returns
    /// whether anything moved. Called from the container's `advance` pass so
    /// every motion in the window runs off one `Instant`.
    pub fn advance_drag(&mut self, ws: &WorkspaceDoc, now: Instant) -> bool {
        let mut moved = self.slides.advance(now) | self.rail.advance(now);
        if !self.drag_armed() {
            self.drag_tick = Some(now);
            return moved;
        }
        let dt = self
            .drag_tick
            .map(|prev| now.duration_since(prev).as_secs_f32())
            .unwrap_or(0.0);
        self.drag_tick = Some(now);
        let (y, area) = {
            let d = self.drag.as_ref().expect("armed");
            (d.y, d.area)
        };
        if area.h <= 0.0 {
            return moved;
        }
        let top = self.rows_top(area);
        // Library floor, not the pane: the edge band is the last ~2 library rows,
        // not the Trash footer. `scroll_by` is `scroll -= dy`, so positive dy
        // reveals content above (same sign the wheel uses).
        let bottom = self.rows_bottom(area);
        let band = theme::AUTO_SCROLL_BAND;
        let speed = theme::AUTO_SCROLL_MAX_SPEED;
        let mut dy = 0.0_f32;
        // Only inside the rows' own region: a grab held in the header above, or
        // over the pinned Trash footer below, must not keep the list crawling —
        // the band is the last few library rows at each end, nothing past them.
        if y >= top && y - top < band {
            let strength = ((band - (y - top)) / band).clamp(0.0, 1.0);
            dy += strength * speed * dt;
        } else if y < bottom && bottom - y < band {
            let strength = ((band - (bottom - y)) / band).clamp(0.0, 1.0);
            dy -= strength * speed * dt;
        }
        if dy != 0.0 {
            self.scroll_by(area, dy);
            // The list moved under the pointer, so the drop zone may have too.
            self.update_drop(now);
            moved = true;
        }
        // Spring-open: a collapsed Nest target held past the threshold opens in
        // place, so the drag can descend into it.
        let spring = match self.drag.as_ref().and_then(|d| d.spring) {
            Some((node, since))
                if now.duration_since(since).as_secs_f32() >= SPRING_OPEN_SECS
                    && self.collapsed.contains(&node) =>
            {
                Some(node)
            }
            _ => None,
        };
        if let Some(node) = spring {
            self.collapsed.remove(&node);
            self.rebuild_during_drag(ws, now);
            if let Some(d) = self.drag.as_mut() {
                d.spring = None;
            }
            moved = true;
        }
        moved
    }

    /// Rebuild the cache mid-drag (for a spring-open) without losing the carried
    /// block's dragged position.
    ///
    /// [`rebuild`] puts the carried block back at its document position, since
    /// the move has not been committed yet; this re-folds the carried roots and
    /// re-applies the last drop zone so the block lands where the drag had it,
    /// while the newly-expanded target's children appear in between.
    ///
    /// [`rebuild`]: Self::rebuild
    fn rebuild_during_drag(&mut self, ws: &WorkspaceDoc, now: Instant) {
        let (carried, last) = match self.drag.as_ref() {
            Some(d) => (d.carried.clone(), d.last),
            None => return,
        };
        self.rebuild(ws);
        self.collapse_carried_subtrees(&carried);
        let representative = self.drag.as_ref().map(|d| d.grab);
        if let Some(representative) = representative {
            self.collapse_carried_selection(&carried, representative);
        }
        if let Some(DropTarget::Row { node, zone }) = last
            && let Some(ti) = self.rows.iter().position(|r| r.node == node)
        {
            self.relocate_carried(ti, zone, &carried, now);
        }
    }

    /// Cross the arm threshold: snapshot the carried set, lift the rows.
    fn arm_drag(&mut self) {
        let (carried, grabbed) = if let Some(d) = self.drag.as_ref() {
            // The carried set is the press-built selection plus the grab, so a
            // Ctrl-built multi-select moves together and a plain drag carries
            // exactly the one node.
            let mut c = self.selection.clone();
            c.insert(d.grab);
            (c, d.grab)
        } else {
            return;
        };
        // A grabbed node that was open folds up for the move — the carried
        // block is the roots, not the whole subtree — and opens again once the
        // rebuilt cache lands after the drop. `collapsed` is left untouched:
        // this is a visual fold for the drag alone, and a node that was already
        // shut has nothing to hide. See [`collapse_carried_subtrees`].
        self.collapse_carried_subtrees(&carried);
        let carried_order = self.carried_roots_in_order(&carried);
        let representative = carried_order
            .iter()
            .copied()
            .find(|node| *node == grabbed)
            .or_else(|| carried_order.first().copied());
        let Some(representative) = representative else {
            return;
        };
        self.collapse_carried_selection(&carried, representative);
        if let Some(d) = self.drag.as_mut() {
            d.grab = representative;
            d.carried = carried;
            d.carried_order = carried_order;
            d.item_count = d.item_count.max(d.carried_order.len());
            // Refresh the slot: the fold above may have removed rows above the
            // grab, shifting its index.
            d.index = self
                .rows
                .iter()
                .position(|r| r.node == d.grab)
                .unwrap_or(d.index);
            d.armed = true;
        }
        // A lifted row owns the hover; tracking it underneath would light a
        // second row behind the drag.
        self.hover = None;
        self.drag_tick = None;
        self.update_drop(Instant::now());
    }

    /// Strip the non-carried children of each carried root from the row cache,
    /// so the carried block is compact during the drag.
    ///
    /// A node that was **expanded** when grabbed has its subtree hidden for the
    /// move; the rebuilt cache after the drop shows it expanded again, because
    /// [`collapsed`] was never changed. A node that was already **collapsed**
    /// has no children in the cache to strip, and stays collapsed after the
    /// move — exactly the two cases the gesture calls for.
    ///
    /// [`collapsed`]: ExploreView::collapsed
    fn collapse_carried_subtrees(&mut self, carried: &HashSet<NodeId>) {
        let mut kept = Vec::with_capacity(self.rows.len());
        // The depth of the outermost carried root whose subtree we are currently
        // inside, or `None` when we are not. Sibling and nested carried roots
        // are handled by the depth comparison: a new root at a shallower-or-equal
        // depth closes the previous fold and may open its own.
        let mut fold_depth: Option<u16> = None;
        for r in &self.rows {
            let d = r.depth;
            if let Some(fd) = fold_depth {
                if d <= fd {
                    fold_depth = None;
                }
            }
            if carried.contains(&r.node) && fold_depth.is_none() {
                // A carried root opens a fold at its depth, unless one is already
                // open above it (in which case it is a nested carried node and
                // rides along inside the outer fold).
                fold_depth = Some(d);
                kept.push(r.clone());
            } else if fold_depth.is_none() {
                kept.push(r.clone());
            }
            // else: a non-carried child of a carried root — dropped for the drag.
        }
        self.rows = kept;
    }

    /// Project a multi-selection into one row for the duration of a drag.
    ///
    /// The complete ordered set remains on [`RowDrag`]; this only changes the
    /// disposable row cache. Keeping every selected root here made the lifted
    /// selection look like a stack of separate cards and consume several drop
    /// slots even though the gesture is one move.
    fn collapse_carried_selection(&mut self, carried: &HashSet<NodeId>, representative: NodeId) {
        self.rows
            .retain(|row| !carried.contains(&row.node) || row.node == representative);
    }

    /// Recompute the drop zone and, if it changed, relocate the carried block
    /// in the cache so neighbours begin sliding to their new slots.
    fn update_drop(&mut self, now: Instant) {
        let (carried, y, grab_dy, area) = match self.drag.as_ref() {
            Some(d) if d.armed => (d.carried.clone(), d.y, d.grab_dy, d.area),
            _ => return,
        };
        // An Agents drag asks a different question: not "which slot", but
        // "which project group". The cache is never reordered — order within a
        // project is the catalog's collation, not the pointer's — so the
        // carried row follows the pointer while the target group is only
        // highlighted, and the drop reports a move verb rather than a reorder.
        if self.mode == NavigatorMode::Agents {
            let target = self.agents_drop_group(y, grab_dy, area);
            if let Some(d) = self.drag.as_mut() {
                d.last = target.map(|node| DropTarget::Row {
                    node,
                    zone: DropZone::Nest,
                });
                d.spring = None;
            }
            return;
        }
        if self.mode == NavigatorMode::Canvases {
            return self.hold_canvas_drop();
        }
        let Some(target) = self.drop_target_for(&carried, y, grab_dy, area) else {
            // No valid landing: clear any in-flight slide target and any
            // spring-open candidate so a later valid one starts fresh.
            if let Some(d) = self.drag.as_mut() {
                d.last = None;
                d.spring = None;
            }
            return;
        };
        let same = self.drag.as_ref().and_then(|d| d.last) == Some(target);
        if same {
            return;
        }
        if let DropTarget::Row { node, zone } = target
            && let Some(ti) = self.rows.iter().position(|r| r.node == node)
        {
            self.relocate_carried(ti, zone, &carried, now);
        }
        if let Some(d) = self.drag.as_mut() {
            d.last = Some(target);
            // A Nest target that is collapsed is a spring-open candidate: rest
            // on it past `SPRING_OPEN_SECS` and it opens in place. Anything else
            // cancels the wait.
            d.spring = match target {
                DropTarget::Row {
                    node,
                    zone: DropZone::Nest,
                } if self.collapsed.contains(&node) => Some((node, now)),
                _ => None,
            };
        }
    }

    /// The nearest non-carried row to the grab row's centre, and the zone the
    /// grab sits in relative to it. `None` when every row is carried.
    ///
    /// Computed against non-carried rows only: the carried block follows the
    /// pointer, so the pointer is always over one of its own rows, and the
    /// meaningful question is which *other* row the block has reached.
    fn drop_target_for(
        &self,
        carried: &HashSet<NodeId>,
        y: f32,
        grab_dy: f32,
        area: Rect,
    ) -> Option<DropTarget> {
        if self.rows.is_empty() || area.w <= 0.0 {
            return None;
        }
        let top = self.rows_top(area);
        let centre = y - grab_dy + theme::NAV_ROW_H * 0.5;
        // Dropping past the end of the list appends at the top level (the
        // library root), however deep the last visible row is — so a tree whose
        // last node is open still lets a drag land after it at depth 0 rather
        // than nesting inside the open branch.
        let last_bottom = top + self.rows.len() as f32 * theme::NAV_ROW_H - self.scroll;
        if centre > last_bottom {
            for r in self.rows.iter().rev() {
                if r.depth == 0 && !carried.contains(&r.node) {
                    return Some(DropTarget::Row {
                        node: r.node,
                        zone: DropZone::After,
                    });
                }
            }
        }
        // The non-carried row whose slot centre is nearest the grab's centre.
        // The carried block follows the pointer, so the pointer is always over
        // one of its own rows; the meaningful question is which *other* row the
        // block has reached.
        let mut best: Option<(usize, f32)> = None;
        for (i, r) in self.rows.iter().enumerate() {
            if carried.contains(&r.node) {
                continue;
            }
            let slot_c = top + i as f32 * theme::NAV_ROW_H - self.scroll + theme::NAV_ROW_H * 0.5;
            let dist = (centre - slot_c).abs();
            match best {
                Some((_, d)) if dist >= d => {}
                _ => best = Some((i, dist)),
            }
        }
        let Some((i, _)) = best else {
            return (!carried.is_empty()).then_some(DropTarget::Library);
        };
        let slot_y = top + i as f32 * theme::NAV_ROW_H - self.scroll;
        let rel = ((centre - slot_y) / theme::NAV_ROW_H).clamp(0.0, 1.0);
        let zone = if rel < 0.25 {
            DropZone::Before
        } else if rel > 0.75 {
            DropZone::After
        } else if self.rows[i].kind == Kind::Document {
            DropZone::Nest
        } else if rel < 0.5 {
            // A non-document row cannot take a child, so the middle band falls
            // back to the nearer edge rather than refusing the drop.
            DropZone::Before
        } else {
            DropZone::After
        };
        Some(DropTarget::Row {
            node: self.rows[i].node,
            zone,
        })
    }

    /// The project group an Agents drag is resting on: the group row itself,
    /// or the group that owns the session row nearest the carried row's
    /// centre. `None` over the heading or nothing — a landing that means no
    /// move.
    ///
    /// The same nearest-slot arithmetic `drop_target_for` uses, resolved one
    /// level up: a session dropped on another session joins that session's
    /// project, because where the eye put it is inside that group.
    fn agents_drop_group(&self, y: f32, grab_dy: f32, area: Rect) -> Option<NodeId> {
        if self.rows.is_empty() || area.w <= 0.0 {
            return None;
        }
        let top = self.rows_top(area);
        let centre = y - grab_dy + theme::NAV_ROW_H * 0.5;
        let grab = self.drag.as_ref().map(|d| d.grab);
        let mut best: Option<(usize, f32)> = None;
        for (i, r) in self.rows.iter().enumerate() {
            if Some(r.node) == grab {
                continue;
            }
            let slot_c = top + i as f32 * theme::NAV_ROW_H - self.scroll + theme::NAV_ROW_H * 0.5;
            let dist = (centre - slot_c).abs();
            match best {
                Some((_, d)) if dist >= d => {}
                _ => best = Some((i, dist)),
            }
        }
        let (i, _) = best?;
        let target_node = self.rows[i].node;
        if self.agent_schedule_ids.contains_key(&target_node)
            || self.agent_schedule_metadata.contains(&target_node)
            || self.agent_schedule_run_ids.contains_key(&target_node)
            || target_node == Self::agent_schedules_header_node()
        {
            return None;
        }
        // Resolve to the owning group: a session row belongs to the nearest
        // group row above it; a group row is its own answer.
        let owning = if self.is_agent_session_row(self.rows[i].node) {
            self.rows[..i]
                .iter()
                .rposition(|r| r.depth == 0)
                .map(|j| self.rows[j].node)?
        } else {
            self.rows[i].node
        };
        (owning != Self::agent_projects_header_node()).then_some(owning)
    }

    /// The group row an armed Agents drag would drop into, for the draw pass's
    /// highlight. `None` at rest, in every other mode, and over no landing.
    fn agents_drop_target_row(&self) -> Option<NodeId> {
        if self.mode != NavigatorMode::Agents {
            return None;
        }
        match self.drag.as_ref().filter(|d| d.armed)?.last {
            Some(DropTarget::Row { node, .. }) => Some(node),
            _ => None,
        }
    }

    /// Release: a click if it never armed, otherwise commit the carried set to
    /// its landing and hand the move to the container.
    pub fn mouse_up(&mut self) -> Outcome {
        // The rail's own drag first: it is the only pointer capture the tasks
        // mode has, and the library's never starts while that mode is showing.
        if self.rail.dragging() {
            return match self.rail.mouse_up() {
                Some(request) => Outcome::Tasks(request),
                None => Outcome::Consumed,
            };
        }
        if self.scroll_drag.take().is_some() {
            return Outcome::Consumed;
        }
        let d = match self.drag.take() {
            Some(d) => d,
            None => return Outcome::Ignored,
        };
        // The Agents drag resolves to a move verb, never a reorder — and its
        // unarmed release is the click that opens the session's tab. Answered
        // before the Trash arm below: a session is not a document, and a
        // release over the pinned bin must not report a trash of nothing.
        if self.mode == NavigatorMode::Agents {
            self.slides.clear();
            self.drag_tick = None;
            if !d.armed {
                // Under the threshold, on the library's own terms: Ctrl and
                // Shift leave the selection change the press made and open
                // nothing, while a plain click collapses the selection back
                // to this row and opens it.
                if d.select_only {
                    return Outcome::Consumed;
                }
                if self.agent_run_ids.contains_key(&d.grab) {
                    self.selection.clear();
                    self.selection.insert(d.grab);
                    self.selection_anchor = Some(d.grab);
                }
                return self.open_row_outcome(d.grab);
            }
            // **Dropped on the Trash row**, asked before the tree's own
            // landing for the reason the document arm gives: the pinned rows
            // sit below the band the drop targets are computed in. A run is
            // the one thing in this tree that can be binned — a session is a
            // process, and a project is a registry record with its own
            // Delete in the menu.
            //
            // **The whole selection goes**, not the row that was grabbed:
            // `arm_drag` lifted the set, the ghost counted the set, and a
            // drop that binned one of four would be the gesture doing less
            // than it showed.
            if self.pointer_over_trash(d.area, d.y) {
                let runs: Vec<koru_delegate_core::RunId> = d
                    .carried_order
                    .iter()
                    .filter_map(|node| self.agent_run_ids.get(node).copied())
                    .collect();
                if runs.is_empty() {
                    return Outcome::Consumed;
                }
                self.selection.clear();
                self.selection_anchor = None;
                return Outcome::TrashRuns(runs);
            }
            let Some(session) = self.agent_session_ids.get(&d.grab).copied() else {
                return Outcome::Consumed;
            };
            let Some(DropTarget::Row { node, .. }) = d.last else {
                return Outcome::Consumed;
            };
            let project = if node == Self::agent_unassigned_node() {
                None
            } else {
                match self.agent_project_at(node) {
                    Some(id) => Some(id),
                    // The heading, or a group the catalog dropped mid-drag:
                    // nothing to move into.
                    None => return Outcome::Consumed,
                }
            };
            if self.agent_session_project(session) == project {
                // Where it already is — the drag's reading of the menu's
                // dimmed "already there" row: a no-op, not a verb.
                return Outcome::Consumed;
            }
            return Outcome::MoveSessionToProject { session, project };
        }
        if !d.armed {
            // Under the threshold: a plain click opens, while Ctrl and Shift
            // leave the selection change they made on press.
            return if d.select_only {
                Outcome::Consumed
            } else {
                self.selection.clear();
                self.selection.insert(d.grab);
                self.selection_anchor = Some(d.grab);
                match d.click {
                    ClickAction::Open => Outcome::OpenNode(d.grab),
                    ClickAction::Download => Outcome::DownloadFile(d.grab),
                    ClickAction::Select => Outcome::Consumed,
                }
            };
        }
        // **Dropped on the Trash row.** Asked before the tree's own landing,
        // because the pinned rows sit below the library's band and
        // `drop_target_for` only knows about rows in it — it would answer "after
        // the last one" for a pointer down here and quietly reorder instead of
        // binning.
        if self.pointer_over_trash(d.area, d.y) {
            self.slides.clear();
            self.drag_tick = None;
            let nodes = d.carried_order.clone();
            self.selection.clear();
            self.selection_anchor = None;
            return Outcome::TrashNodes(nodes);
        }
        // Canvases are never reordered or nested: a release over the list is
        // no move, and the carried row eases back to its own slot.
        let landing = if self.mode == NavigatorMode::Canvases {
            None
        } else {
            self.drop_target_for(&d.carried, d.y, d.grab_dy, d.area)
        };
        // Nesting into a **collapsed** target expands it, so the dropped child
        // is visible rather than vanishing under a shut caret. The cache's shape
        // changes when it does (hidden children appear), so a settle would ease
        // toward the wrong slot — clear the slide and let the rebuild show the
        // final layout instead.
        let expanded_nest = matches!(
            landing,
            Some(DropTarget::Row {
                node: t,
                zone: DropZone::Nest,
            }) if self.collapsed.remove(&t)
        );
        if !expanded_nest {
            // Ease the carried rows from the pointer into their slots, so the
            // gesture ends the way it ran rather than snapping.
            self.settle_carried(&d, Instant::now());
        } else {
            self.slides.clear();
        }
        self.drag_tick = None;
        match landing {
            Some(target) => Outcome::MoveNodes {
                nodes: d.carried_order,
                target,
            },
            None => Outcome::Consumed,
        }
    }

    /// The carried **root** nodes — the selected documents, not the descendants
    /// that ride along with them — in the order they currently sit in the cache.
    /// `move_node` relocates each one's whole subtree, so the descendants are
    /// the container's concern, not the list carried here.
    fn carried_roots_in_order(&self, carried: &HashSet<NodeId>) -> Vec<NodeId> {
        self.rows
            .iter()
            .map(|r| r.node)
            .filter(|n| carried.contains(n))
            .collect()
    }

    fn carried_item_count(&self) -> usize {
        self.drag.as_ref().map(|d| d.item_count).unwrap_or(0)
    }

    /// Move the carried block to `target`/`zone` in the cache and start its
    /// neighbours sliding. See the module-level notes on the block model.
    fn relocate_carried(
        &mut self,
        target_i: usize,
        zone: DropZone,
        carried: &HashSet<NodeId>,
        now: Instant,
    ) {
        let top = match self.drag.as_ref() {
            Some(d) if d.area.w > 0.0 => self.rows_top(d.area),
            _ => return,
        };
        let scroll = self.scroll;
        // Drawn Y per node before the reorder — old slot plus whatever slide is
        // still owed, so a swap mid-slide carries on from where the row is.
        let mut drawn_y = std::collections::HashMap::with_capacity(self.rows.len());
        for (i, r) in self.rows.iter().enumerate() {
            let slot = top + i as f32 * theme::NAV_ROW_H - scroll;
            drawn_y.insert(r.node, slot + self.slides.offset(i));
        }

        let target_node = self.rows[target_i].node;

        // Split into kept rows and the carried block (carried roots and their
        // landmark descendants), remembering each carried row's root so its
        // depth can be rebased onto the new base.
        let mut kept: Vec<Row> = Vec::new();
        let mut block: Vec<Row> = Vec::new();
        let mut block_root_depth: Vec<u16> = Vec::new();
        let mut i = 0;
        while i < self.rows.len() {
            let r = self.rows[i].clone();
            if carried.contains(&r.node) {
                let root_depth = r.depth;
                block.push(r);
                block_root_depth.push(root_depth);
                i += 1;
                // Its subtree in the cache follows at a greater depth.
                while i < self.rows.len() && self.rows[i].depth > root_depth {
                    block.push(self.rows[i].clone());
                    block_root_depth.push(root_depth);
                    i += 1;
                }
            } else {
                kept.push(r);
                i += 1;
            }
        }
        if block.is_empty() {
            return;
        }

        // Where in `kept` the block goes, and the base depth for its roots.
        let (seam, base_depth) = {
            let kt = kept.iter().position(|r| r.node == target_node);
            let Some(kt) = kt else {
                return;
            };
            // The target's non-carried subtree, counted in `kept` rows after it.
            let kd = kept[kt].depth;
            let mut subtree = 0;
            for r in &kept[kt + 1..] {
                if r.depth > kd {
                    subtree += 1;
                } else {
                    break;
                }
            }
            match zone {
                DropZone::Before => (kt, kd),
                DropZone::After => (kt + 1 + subtree, kd),
                DropZone::Nest => (kt + 1 + subtree, kd + 1),
            }
        };
        // Rebase the block's depths onto `base_depth`, preserving the relative
        // shape of each carried subtree.
        for (r, &root) in block.iter_mut().zip(&block_root_depth) {
            r.depth = base_depth + r.depth.saturating_sub(root);
        }

        // Assemble the new cache and the per-row slide offsets.
        let mut new_rows = Vec::with_capacity(self.rows.len());
        let mut from = vec![0.0_f32; self.rows.len()];
        let place = |rows: &mut Vec<Row>, from: &mut [f32], src: &[Row]| {
            for r in src {
                let j = rows.len();
                rows.push(r.clone());
                let slot = top + j as f32 * theme::NAV_ROW_H - scroll;
                from[j] = drawn_y.get(&r.node).copied().unwrap_or(slot) - slot;
            }
        };
        place(&mut new_rows, &mut from, &kept[..seam]);
        let block_start = new_rows.len();
        place(&mut new_rows, &mut from, &block);
        place(&mut new_rows, &mut from, &kept[seam..]);

        // The carried rows are drawn at the pointer, not their slot, so their
        // slide entry is unused until the release settles it. Zero it so a
        // mid-drag relocation never eases a row the eye is already tracking.
        for j in block_start..new_rows.len() {
            from[j] = 0.0;
        }
        self.rows = new_rows;
        self.slides.relocate(from, now);
        if let Some(d) = self.drag.as_mut() {
            d.index = self
                .rows
                .iter()
                .position(|r| r.node == d.grab)
                .unwrap_or(d.index);
        }
    }

    /// On release, ease the carried rows from the pointer into their slots.
    /// Neighbours keep whatever motion they still owe, carried-on from where
    /// they are.
    fn settle_carried(&mut self, d: &RowDrag, now: Instant) {
        if self.rows.is_empty() {
            return;
        }
        let top = self.rows_top(d.area);
        let scroll = self.scroll;
        // After its last relocate the carried block is contiguous: every
        // carried root sits beside the next, each with its subtree. Find the
        // spans, then the block's overall extent.
        let mut ranges: Vec<(usize, usize)> = Vec::new();
        let mut i = 0;
        while i < self.rows.len() {
            if d.carried.contains(&self.rows[i].node) {
                let root_depth = self.rows[i].depth;
                let start = i;
                i += 1;
                while i < self.rows.len() && self.rows[i].depth > root_depth {
                    i += 1;
                }
                ranges.push((start, i));
            } else {
                i += 1;
            }
        }
        if ranges.is_empty() {
            return;
        }
        let first_start = ranges[0].0;
        let last_end = ranges.last().map(|&(_, e)| e).unwrap_or(first_start);
        // The grab row's top follows the pointer; the block stacks above and
        // below it the way it is laid out in the cache.
        let grab_abs = self
            .rows
            .iter()
            .position(|r| r.node == d.grab)
            .unwrap_or(first_start);
        let grab_offset = grab_abs.saturating_sub(first_start) as f32;
        let block_top = d.y - d.grab_dy - grab_offset * theme::NAV_ROW_H;

        let mut from = vec![0.0_f32; self.rows.len()];
        for j in 0..self.rows.len() {
            // Neighbours carry on from where they are; carried rows are
            // overwritten below.
            from[j] = self.slides.offset(j);
        }
        for j in first_start..last_end {
            let slot = top + j as f32 * theme::NAV_ROW_H - scroll;
            let drawn = block_top + (j - first_start) as f32 * theme::NAV_ROW_H;
            from[j] = drawn - slot;
        }
        self.slides.relocate(from, now);
    }

    pub fn clear_hover(&mut self) -> bool {
        let had = self.rail.clear_hover()
            || self.hover.is_some()
            || self.header_hover.is_some()
            || self.actions_hover
            || self.search_hover
            || self.footer_hover.is_some()
            || self.cta_hover
            || self.subscribe_hover
            || self.scrollbar_hover;
        self.hover = None;
        self.header_hover = None;
        self.actions_hover = false;
        self.search_hover = false;
        self.footer_hover = None;
        self.cta_hover = false;
        self.subscribe_hover = false;
        self.scrollbar_hover = false;
        if let Some(p) = self.popover.as_mut() {
            p.hover = None;
        }
        had
    }

    pub fn scroll_by(&mut self, area: Rect, dy: f32) {
        if self.showing_socials() {
            if let Some(surface) = &self.socials {
                surface.borrow_mut().navigation_scroll(
                    &winit::event::MouseScrollDelta::PixelDelta(winit::dpi::PhysicalPosition::new(
                        0., dy as f64,
                    )),
                );
            }
            return;
        }
        // The rail keeps its own offset: a workspace with forty lists scrolls,
        // and the library's scroll is a different position over a different
        // list — sharing one number would make switching modes jump.
        if self.mode == NavigatorMode::Tasks {
            let top = self.rows_top(area);
            self.rail.scroll_by(area, top, dy);
            return;
        }
        if self.mode == NavigatorMode::Artifacts {
            let top = self.rows_top(area);
            self.artifacts.scroll_by(area, top, dy);
            return;
        }
        if self.mode == NavigatorMode::Chats {
            let top = self.rows_top(area);
            self.chats.scroll_by(area, top, dy);
            return;
        }
        self.scroll -= dy;
        self.clamp_scroll(area);
    }

    /// Hold the scroll inside what there is to scroll. Its own function because
    /// the wheel and the keyboard both need it and they must not disagree about
    /// where the bottom is.
    fn clamp_scroll(&mut self, area: Rect) {
        let content = self.rows.len() as f32 * theme::NAV_ROW_H;
        // The band the rows actually have, which shrinks as the header grows.
        // Same source as `row_rects`, so the two cannot disagree about where
        // the list starts.
        let band = (self.rows_bottom(area) - self.rows_top(area)).max(0.0);
        let max = (content - band).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Geometry over only the rows band: header and pinned footer are
    /// navigation chrome, not scrollable content.
    fn scrollbar_geom(&self, area: Rect) -> Option<koru_ui::scrollbar::ScrollbarGeom> {
        if self.showing_socials() {
            return None;
        }
        let top = self.rows_top(area);
        let bottom = self.rows_bottom(area);
        let band = Rect::new(area.x, top, area.w, (bottom - top).max(0.0));
        let content = self.rows.len() as f32 * theme::NAV_ROW_H;
        koru_ui::scrollbar::ScrollbarGeom::for_content(band, content, self.scroll)
    }

    pub fn cursor_icon(&self) -> CursorIcon {
        if self.scroll_drag.is_some() || self.scrollbar_hover {
            return CursorIcon::NsResize;
        }
        // The grab hand is a *press-and-hold* affordance, not a hover one. A
        // document row shows the ordinary cursor until the pointer has been
        // held on it past `DRAG_HOLD_MS`, and only then becomes a grab — so an
        // ordinary click reads as a click rather than as draggable furniture.
        if let Some(d) = self.drag.as_ref() {
            if d.armed {
                return CursorIcon::Grabbing;
            }
            if self.held_past_threshold() {
                return CursorIcon::Grab;
            }
            // Latent but under the hold: the ordinary cursor.
            return koru_ui::cursor::IDLE;
        }
        // A document row shows the ordinary cursor either way — the grab hand
        // is a press-and-hold affordance, not a hover one — so this arm and
        // the one below now agree. It is kept separate because *why* they
        // agree differs, and the grab is a real cursor change one hold away.
        let over_document = self
            .hover
            .and_then(|i| self.rows.get(i))
            .is_some_and(|r| r.kind == Kind::Document);
        if over_document {
            return koru_ui::cursor::IDLE;
        }
        // Koru does not use the hand cursor (`docs/features/design/DESIGN.md`), so a pressable
        // row and an inert one look the same to the pointer. The hit test
        // still has to be right — the row's own hover background is what says
        // it can be pressed.
        if self.hover.is_some()
            || self.header_hover.is_some()
            || self.actions_hover
            || self.search_hover
            || self.cta_hover
            || self.subscribe_hover
            || self.footer_hover.is_some()
            || self.rail.pressable()
            || self.popover.as_ref().is_some_and(|p| p.hover.is_some())
        {
            koru_ui::cursor::PRESS
        } else {
            koru_ui::cursor::IDLE
        }
    }

    /// Whether a latent press has been held long enough to reveal the grab hand.
    fn held_past_threshold(&self) -> bool {
        match self.drag.as_ref() {
            Some(d) if !d.armed => {
                Instant::now().duration_since(d.pressed_at).as_secs_f32() >= DRAG_HOLD_MS / 1000.0
            }
            _ => false,
        }
    }

    /// Whether the empty state's sentence offers Koru Cloud, and so carries
    /// the Subscribe button under its own: a profile with no workspace yet
    /// that is waiting on another device to bring them.
    fn empty_state_offers_cloud(&self) -> bool {
        // No Koru Cloud pointer in a build that hides it (koru-features).
        koru_features::cloud_sync()
            && !matches!(
                self.mode,
                NavigatorMode::Tasks
                    | NavigatorMode::Chats
                    | NavigatorMode::Agents
                    | NavigatorMode::Artifacts
            )
            && self.current.is_none()
            && self
                .current_profile
                .is_some_and(|id| self.receiving_profiles.contains(&id))
    }

    /// Copy and action for a library with no rows. A local Welcome document is
    /// deliberately not a workspace: until Store has one, every content action
    /// must lead through workspace creation rather than promise a destination
    /// that does not exist.
    fn empty_state(&self) -> (&'static str, &'static str, Outcome) {
        if matches!(
            self.mode,
            NavigatorMode::Tasks
                | NavigatorMode::Chats
                | NavigatorMode::Agents
                | NavigatorMode::Socials
                | NavigatorMode::Artifacts
        ) {
            return (
                "No sessions yet. Start one from the + menu.",
                "",
                Outcome::Consumed,
            );
        }
        if self.current.is_none() {
            return (
                if self
                    .current_profile
                    .is_some_and(|id| self.receiving_profiles.contains(&id))
                {
                    if koru_features::cloud_sync() {
                        "Your workspaces will appear when a device that has them comes online. You can also create a workspace, or subscribe to Koru Cloud to keep your files securely in the cloud."
                    } else {
                        "Your workspaces will appear when a device that has them comes online. You can also create a workspace."
                    }
                } else {
                    "There isn't a workspace here yet. Create one to start adding content."
                },
                "Create workspace",
                Outcome::NewWorkspace,
            );
        }
        match self.mode {
            NavigatorMode::Documents => (
                if self
                    .current_profile
                    .is_some_and(|id| self.receiving_profiles.contains(&id))
                {
                    "Synced documents will appear here as they arrive. You can also create a document."
                } else {
                    "Nothing here yet. Documents you make appear in this column."
                },
                "Create document",
                Outcome::NewDocument,
            ),
            NavigatorMode::Files => (
                "No files yet. Drop files into this window or add them here.",
                "Add files",
                Outcome::ImportFiles,
            ),
            NavigatorMode::Canvases => canvases::empty_state(),
            NavigatorMode::Tasks
            | NavigatorMode::Chats
            | NavigatorMode::Agents
            | NavigatorMode::Socials
            | NavigatorMode::Artifacts => ("", "", Outcome::Consumed),
        }
    }

    /// `avatar` is the profile's picture, already decoded by the container —
    /// this crate has no image decoder and is not getting one. `None` falls back
    /// to the initials chip.
    pub fn draw(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        avatar: Option<&peniko::ImageBrush>,
    ) {
        let p = theme::p();
        scene::fill(
            scene,
            root,
            p.status_bg,
            &scene::rect(area.x, area.y, area.w, area.h),
        );
        // Hairline down the inside edge, separating the column from the view.
        scene::fill(
            scene,
            root,
            p.panel_border,
            &scene::rect(area.x + area.w - 1.0, area.y, 1.0, area.h),
        );

        // The explore shell is one section structure for every tab: the
        // profile/workspace row, the tab row, and the nodes band. Only the
        // nodes band's contents change between tabs; the pinned footer rows
        // answer to the shell, not to the nodes band they sit below.
        {
            let _section = koru_draw::inspect::scope(
                "ExploreHeader",
                "container",
                root,
                scene::rect(area.x, area.y, area.w.max(0.0), WORKSPACE_ROW_H.max(0.0)),
                |_| {},
            );
            self.draw_header(scene, root, engine, area, avatar);
        }
        // The gaps between the shell's sections are the stack's geometry, not
        // a margin any one section owns — named, so the spacing between the
        // rows is itself inspectable instead of an anonymous constant in
        // `rows_top`.
        if area.w > 0.0 {
            let _gap = koru_draw::inspect::scope(
                "ExploreSpacer",
                "part",
                root,
                scene::rect(area.x, area.y + WORKSPACE_ROW_H, area.w, theme::NAV_PAD),
                |r| {
                    r.property("height", theme::NAV_PAD, "shell section gap");
                    r.property("separates", "ExploreHeader / ExploreTabs", "shell stack");
                },
            );
        }
        {
            let band = self.heading_band(area);
            let _section = koru_draw::inspect::scope(
                "ExploreTabs",
                "container",
                root,
                scene::rect(band.x, band.y, band.w, band.h),
                |_| {},
            );
            self.draw_navigator_modes(scene, root, engine, area);
            actions::draw_search_button(
                scene,
                root,
                engine,
                self.search_button(area),
                self.search_hover,
            );
            actions::draw_button(scene, root, self.actions_button(area), self.actions_hover);
        }
        if area.w > 0.0 {
            let top = self.rows_top(area);
            let _gap = koru_draw::inspect::scope(
                "ExploreSpacer",
                "part",
                root,
                scene::rect(area.x, top - theme::NAV_PAD, area.w, theme::NAV_PAD),
                |r| {
                    r.property("height", theme::NAV_PAD, "shell section gap");
                    r.property("separates", "ExploreTabs / ExploreNodes", "shell stack");
                },
            );
        }

        let footer = self.draw_nodes(scene, root, engine, area, p);
        if footer {
            self.draw_footer_rows(scene, root, engine, area, p);
        }
    }

    /// The nodes band — the one section of the shell that changes between
    /// tabs: the library rows for Documents and Files, the rail for Tasks,
    /// the results column for Artifacts, or the mode's empty state. Returns
    /// whether the pinned footer rows show in this mode (the rail and the
    /// artifacts column have no Trash row).
    fn draw_nodes(
        &mut self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        p: &theme::Palette,
    ) -> bool {
        let top = self.rows_top(area);
        let _section = koru_draw::inspect::scope(
            "ExploreNodes",
            "container",
            root,
            scene::rect(
                area.x,
                top,
                area.w.max(0.0),
                (self.rows_bottom(area) - top).max(0.0),
            ),
            |record| {
                record.property(
                    "mode",
                    format!("{:?}", self.mode),
                    "NavigatorMode; only this section's contents change between tabs",
                );
            },
        );

        // The tasks mode's whole content. It comes before the empty-state
        // branch below on purpose: the rail has no rows in `self.rows` and
        // never will, so falling through would draw "Nothing here yet" over it.
        if self.mode == NavigatorMode::Tasks {
            self.empty_cta = None;
            self.empty_subscribe = None;
            self.rail.draw(scene, root, engine, area, top);
            return false;
        }
        // The artifacts mode's whole content, before the empty-state branch
        // for the rail's reason: its rows are never in `self.rows`, so
        // falling through would draw "Nothing here yet" over a column full
        // of results.
        if self.mode == NavigatorMode::Artifacts {
            self.empty_cta = None;
            self.empty_subscribe = None;
            self.artifacts.set_lang(crate::format::lang());
            self.artifacts.draw(scene, root, engine, area, top);
            return false;
        }
        // The Chats view's whole content, before the empty-state branch for
        // the same reason: its rows are never in `self.rows`.
        if self.mode == NavigatorMode::Chats {
            self.empty_cta = None;
            self.empty_subscribe = None;
            self.chats.set_lang(crate::format::lang());
            self.chats.draw(scene, root, engine, area, top);
            return false;
        }

        if self.showing_socials() {
            self.empty_cta = None;
            if let Some(surface) = &self.socials {
                let band = Rect::new(area.x, top, area.w, (area.y + area.h - top).max(0.));
                surface.borrow_mut().navigation_draw(scene, root, band);
            }
            return false;
        }

        if self.rows.is_empty() {
            let x = area.x + theme::NAV_PAD * 2.0;
            let (hint_text, cta_label, _) = self.empty_state();
            let hint = engine.simple_layout_wrapped(
                hint_text,
                theme::role::meta(),
                (area.w - theme::NAV_PAD * 4.0).max(1.0),
            );
            scene::draw_layout(scene, root, &hint, x, top, Some(p.text_dim));

            // The way out of the empty state, under the sentence that describes
            // it. A column that only explains why it is empty leaves you to
            // find the answer somewhere else. **No button for Agents**: its
            // sentence already names the way out, and a second launcher beside
            // the `+` menu would be a second place to keep the profile list
            // correct.
            //
            // **Measured here and remembered**, because the hint's height
            // depends on how the sentence wraps at this column's width and the
            // hit test has no engine. `WelcomeView` caches its font chips for
            // the same reason.
            if matches!(self.mode, NavigatorMode::Tasks | NavigatorMode::Agents) {
                self.empty_cta = None;
                self.empty_subscribe = None;
                return true;
            }
            let label = engine.simple_layout(cta_label, theme::role::row());
            let cta = Rect::new(
                x,
                top + hint.height() + CTA_GAP,
                (label.width() + CTA_PAD_X * 2.0).min((area.w - theme::NAV_PAD * 4.0).max(1.0)),
                CTA_H,
            );
            self.empty_cta = Some(cta);
            // **Cloud, where the sentence offers it**: a profile waiting on
            // another device's workspaces can also keep them in Koru Cloud.
            // Filled, because it is the one step this column cannot take for
            // you; opens the same Pricing page the account card does.
            self.empty_subscribe = self.empty_state_offers_cloud().then(|| {
                let label = koru_i18n::t(crate::format::lang(), koru_i18n::Key::CloudSubscribe);
                let rect = Rect::new(
                    x,
                    cta.y + cta.h + SUBSCRIBE_GAP,
                    koru_ui::button::button_width(engine, label)
                        .min((area.w - theme::NAV_PAD * 4.0).max(1.0)),
                    CTA_H,
                );
                koru_ui::button::draw_button(
                    scene,
                    root,
                    engine,
                    rect,
                    label,
                    koru_ui::button::ButtonRole::Primary,
                    self.subscribe_hover,
                    false,
                );
                rect
            });
            let shape = scene::rrect(cta.x, cta.y, cta.w, cta.h, 6.0);
            scene::fill(scene, root, p.panel_bg, &shape);
            scene::stroke(scene, root, p.panel_border, theme::BORDER_W, &shape);
            if self.cta_hover {
                scene::fill(scene, root, p.hover_tint, &shape);
            }
            scene::draw_layout_first_line(
                scene,
                root,
                &label,
                cta.x + (cta.w - label.width()) * 0.5,
                cta.y + (cta.h - scene::first_line_height(&label)) * 0.5,
                Some(p.text),
            );
            return true;
        }
        self.empty_cta = None;
        self.empty_subscribe = None;

        // The rows get their own clip. A row straddling `top` should be *cut* at
        // the header rather than dropped by the cull, or the list would appear
        // to jump a whole row as it scrolled under it. The floor is
        // `rows_bottom`, not the pane: without that cut, a straddling row paints
        // into the pinned Trash band (which has no idle fill to cover it).
        let bottom = self.rows_bottom(area);
        scene.push_clip_layer(
            vello::peniko::Fill::NonZero,
            root,
            &scene::rect(area.x, top, area.w, (bottom - top).max(0.0)),
        );
        let armed = self.drag_armed();
        for (i, r) in self.row_rects(area) {
            let Some(row) = self.rows.get(i).cloned() else {
                continue;
            };
            // The carried rows are lifted and drawn separately; a second copy in
            // their slots would show the drag and the drop at once.
            if armed
                && self
                    .drag
                    .as_ref()
                    .is_some_and(|d| d.carried.contains(&row.node))
            {
                continue;
            }
            // How far this row is being drawn off its slot by the slide.
            let oy = self.slides.offset(i);
            let r = Rect::new(r.x, r.y + oy, r.w, r.h);
            let treatment = self.row_treatment(&row, i);
            let _row_scope = koru_draw::inspect::scope(
                "ExploreRow",
                "container",
                root,
                scene::rect(r.x, r.y, r.w, r.h),
                |record| {
                    record.property("treatment", format!("{treatment:?}"), "row state");
                },
            );
            let chip = row_chip(r);
            match treatment {
                // An Agents drag resting on this group: the accent target
                // treatment the Trash row uses, because it means the same
                // thing — let go and it goes in.
                RowTreatment::DropTarget => {
                    scene::fill(scene, root, p.accent.multiply_alpha(0.14), &chip);
                    scene::stroke(scene, root, p.accent, theme::BORDER_W, &chip);
                }
                // The heading is furniture: no hover fill, no card — its `+`
                // is the only live part, drawn by `draw_row_body`.
                RowTreatment::Heading | RowTreatment::Idle => {}
                RowTreatment::Card => draw_row_card(scene, root, r, p),
                RowTreatment::Hover => scene::fill(scene, root, p.hover_tint, &chip),
            }
            // **An outline, not a fill.** Where the keyboard is and what is
            // open are two different things, and while you are walking the tree
            // both are usually on screen at once — a second filled row would
            // read as two documents open. A ring says "here, not yet chosen",
            // which is exactly what moving through a tree means.
            //
            // **In the border colour, not `caret`.** It used to be near-black,
            // and since a press sets the focus before the release opens the
            // document, every click flashed a black rectangle for as long as the
            // button was down. The same grey the open row's card uses says the
            // same thing without the bang: outlined is *here*, outlined and
            // filled is *open*.
            if self.focus == Some(row.node) && treatment != RowTreatment::Card {
                scene::stroke(
                    scene,
                    root,
                    p.panel_border,
                    theme::BORDER_W,
                    &row_chip(Rect::new(r.x + 0.5, r.y + 0.5, r.w - 1.0, r.h - 1.0)),
                );
            }
            // Full-strength on a card, dim off one. A picked row drawn as a
            // white sheet with dim text on it would read as half-disabled,
            // which is the opposite of what choosing it meant.
            self.draw_row_body(
                scene,
                root,
                engine,
                &row,
                r,
                p,
                treatment == RowTreatment::Card,
                self.hover == Some(i),
            );
        }

        // The carried block is lifted out of the flow and follows the pointer.
        // It is drawn last, over its neighbours, the way a carried tab is.
        if armed {
            self.draw_carried(scene, root, engine, area, top, p);
        }
        scene.pop_layer();

        if let Some(g) = self.scrollbar_geom(area) {
            koru_ui::scrollbar::draw(
                scene,
                root,
                g,
                self.scroll_drag.is_some() || self.scrollbar_hover,
            );
        }
        true
    }

    /// The pinned Trash row at the foot of the column.
    ///
    /// Dimmed and inert when the bin is empty, and then it shows no count at
    /// all: a `0` beside a row is a number somebody has to read before deciding
    /// it means nothing.
    /// The rows pinned to the floor of the column.
    ///
    /// **Ordinary nav rows, not a panel.** No separator and no band: they use
    /// the same hover fill and the same icon lane as a document row, because
    /// they are the same kind of thing — somewhere in this column you can go.
    fn draw_footer_rows(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        p: &theme::Palette,
    ) {
        let band_top = self.rows_bottom(area);
        let _footer = koru_draw::inspect::scope(
            "ExploreFooter",
            "container",
            root,
            scene::rect(
                area.x,
                band_top,
                area.w.max(0.0),
                (area.y + area.h - band_top).max(0.0),
            ),
            |_| {},
        );
        let dropping = self.trash_is_drop_target(area);
        for (row, r) in self.footer_rects(area) {
            let _row_scope = koru_draw::inspect::scope(
                "ExploreRow",
                "container",
                root,
                scene::rect(r.x, r.y, r.w, r.h),
                |record| {
                    record.property("row", format!("{row:?}"), "FooterRow");
                    record.property("active", self.footer_active == Some(row), "view state");
                },
            );
            let (glyph, label) = self.footer_parts(row);
            let label = if dropping && row == FooterRow::Trash {
                trash_drop_label(self.carried_item_count())
            } else {
                label.to_owned()
            };
            // A drag resting here reads as a target rather than as a hover:
            // the accent says "let go and it goes in", which a grey tint does
            // not, and the two states are never both true.
            if dropping && row == FooterRow::Trash {
                let chip = row_chip(r);
                scene::fill(scene, root, p.accent.multiply_alpha(0.14), &chip);
                scene::stroke(scene, root, p.accent, theme::BORDER_W, &chip);
            } else if self.footer_active == Some(row) {
                // The same card the open document wears, because it means the
                // same thing: this is what you are looking at.
                draw_row_card(scene, root, r, p);
            } else if self.footer_hover == Some(row) {
                // The library rows' own fill, inset the same 4px, so a pinned
                // row highlights exactly like the ones above it.
                scene::fill(scene, root, p.hover_tint, &row_chip(r));
            }
            let fg = if dropping && row == FooterRow::Trash {
                p.accent
            } else {
                p.text
            };
            // The mode's own lane: a pinned row reserves the caret lane exactly
            // when the rows above it do (Documents), so the footer cannot drift
            // a lane right of the list it is pinned under.
            let icon_x = icon_lane(area.x, 0, self.mode == NavigatorMode::Documents);
            draw_icon(
                scene,
                root,
                engine,
                glyph,
                Rect::new(icon_x, r.y, 18.0, r.h),
                fg,
            );
            let text = engine.simple_layout(&label, theme::role::row());
            scene::draw_layout_first_line(
                scene,
                root,
                &text,
                icon_x + 22.0,
                r.y + (r.h - scene::first_line_height(&text)) * 0.5,
                Some(fg),
            );
        }
    }

    /// What row `i` is wearing. See [`RowTreatment`].
    ///
    /// **Order is the whole content of this function.** A drop target outranks
    /// everything, because it is about the gesture in flight rather than about
    /// the row; the Agents heading is not a row you can be on at all; a chosen
    /// row outranks a hovered one, because the pointer merely being somewhere
    /// must never overwrite what you picked.
    pub(crate) fn row_treatment(&self, row: &Row, i: usize) -> RowTreatment {
        if self.agents_drop_target_row() == Some(row.node) {
            return RowTreatment::DropTarget;
        }
        if self.mode == NavigatorMode::Agents && row.node == Self::agent_projects_header_node() {
            return RowTreatment::Heading;
        }
        // Open **or** picked. One treatment, because a multi-selection and the
        // document you have open are the same claim on this column; splitting
        // them put a chosen row in the hover's clothes, and then `draw_carried`
        // promoted the very same set to cards the instant a drag armed.
        if self.selected == Some(row.node) || self.selection.contains(&row.node) {
            return RowTreatment::Card;
        }
        if self.hover == Some(i) {
            return RowTreatment::Hover;
        }
        RowTreatment::Idle
    }

    /// A row's caret, icon and label — everything after the fills and the focus
    /// ring. Shared by the main pass and the lifted carried block so the two
    /// cannot drift apart in what a row looks like.
    fn draw_row_body(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        row: &Row,
        r: Rect,
        p: &theme::Palette,
        emphasised: bool,
        hovered: bool,
    ) {
        let parts = self.row_parts(row, r);
        // The Agents *Projects* heading: a dim section label with the create
        // `+` in the reserved trailing lane — Koru Terminal's heading band,
        // drawn as a row of this tree. Nothing else of a row applies to it.
        if self.mode == NavigatorMode::Agents && row.node == Self::agent_projects_header_node() {
            let label = engine.simple_layout(&row.label, theme::role::hint());
            scene::draw_layout_first_line(
                scene,
                root,
                &label,
                r.x + theme::NAV_PAD + 4.0,
                r.y + (r.h - scene::first_line_height(&label)) * 0.5,
                Some(p.row_hint),
            );
            draw_icon(scene, root, engine, icons::PLUS, parts.dots, p.text_dim);
            return;
        }
        if row.has_children {
            let caret = parts.caret;
            let (cx, cy) = caret.centre();
            let turn = self.turn_of(row.node, row.expanded);
            let spun = root
                * Affine::translate((cx as f64, cy as f64))
                * Affine::rotate(turn as f64 * std::f64::consts::FRAC_PI_2)
                * Affine::translate((-(cx as f64), -(cy as f64)));
            draw_caret(scene, spun, engine, caret, p.text_dim);
        }
        let icon_x = parts.icon.x;
        let text_x = parts.label.x;
        // Bounded by the `…`'s reserved lane, so the label is the same width
        // hovered and idle and cannot shuffle as the pointer goes past.
        let fitted = engine.elided(&row.label, theme::role::row(), parts.label.w);
        // **One signal, not two.** The open document used to be semibold as well
        // as full-strength, which earned its keep while "open" was a slightly
        // darker stripe. It is a card now — filled, outlined, lifted off the
        // column — and a weight change on top of that is a second answer to a
        // question already answered, and one that makes the row jump as the
        // selection moves.
        let label = engine.simple_layout(&fitted, theme::role::row());
        let label_metrics = label.lines().next().map(|line| *line.metrics());
        let line_h = label_metrics.map_or(0.0, |metrics| metrics.line_height);
        let label_y = r.y + (r.h - line_h) * 0.5;
        if self.mode == NavigatorMode::Agents {
            // A session wears its product's mark — the **same** projection the
            // tab draws (Q-E7 P6) — falling back to the terminal glyph only
            // when the profile resolves to no mark. A project group keeps the
            // folder glyph. `row.kind` is only the drag-machinery guard on
            // this projection — see `rebuild_agent_rows`.
            let slot = Rect::new(icon_x, r.y, PAGE_ICON_SLOT, r.h);
            if self.is_agent_session_row(row.node) {
                match self.agent_session_mark(row.node) {
                    Some(koru_draw::brand::ProfileMark::Glyph(glyph)) => {
                        draw_icon(scene, root, engine, glyph, slot, p.text_dim);
                    }
                    // A vector logo the icon font cannot carry: a CAPTION_ICON
                    // square centred in the slot, the size the tab strip and
                    // the + menu draw it at, so every surface's marks line up.
                    Some(koru_draw::brand::ProfileMark::Brand(brand)) => {
                        let side = theme::CAPTION_ICON;
                        scene::draw_brand_icon(
                            scene,
                            root,
                            Rect::new(icon_x + (PAGE_ICON_SLOT - side) * 0.5, r.y, side, r.h),
                            p.text_dim,
                            brand,
                        );
                    }
                    None => draw_icon(scene, root, engine, icons::TERMINAL, slot, p.text_dim),
                }
            } else if self.agent_schedule_ids.contains_key(&row.node) {
                draw_icon(
                    scene,
                    root,
                    engine,
                    if self.agent_schedules_paused.contains(&row.node) {
                        icons::PAUSE
                    } else {
                        icons::CLOCK_COUNTDOWN
                    },
                    slot,
                    p.text_dim,
                );
            } else if self.agent_schedule_metadata.contains(&row.node) {
                // Metadata is text under the schedule, not another object. An
                // empty icon lane preserves the tree's fixed pitch without
                // suggesting that it can be opened.
            } else if self.agent_schedule_run_ids.contains_key(&row.node) {
                draw_icon(scene, root, engine, icons::FOLDER, slot, p.text_dim);
            } else if self.agent_runs_working.contains(&row.node) {
                // **A working run shows movement, not a folder.** An open ring
                // turning on the app's progress loop: a folder says "a place
                // things are", which is exactly what a run in flight is not.
                // It stops the moment the run does — the row goes back to the
                // folder, and with the last one the window stops asking for
                // frames.
                let (cx, cy) = slot.centre();
                let spun = root
                    * Affine::translate((cx as f64, cy as f64))
                    * Affine::rotate(self.runs_spin_angle(Instant::now()))
                    * Affine::translate((-(cx as f64), -(cy as f64)));
                draw_icon(scene, spun, engine, icons::CIRCLE_NOTCH, slot, p.text_dim);
            } else if self.agent_runs_paused.contains(&row.node) {
                // **A run that stopped and is waiting for a person does not
                // turn.** An interrupted run's process is gone after a
                // restart and it sits still until Resume is pressed; a ring
                // going round beside it said work was happening when none
                // was. A **pause** mark rather than a stop mark, because
                // Resume is exactly what the row offers and a stop sign
                // beside it would say two different things (ADR-376).
                //
                // Drawn from `root`, not from a spun affine: it is a still
                // glyph, and nothing here asks for another frame.
                draw_icon(scene, root, engine, icons::PAUSE, slot, p.text_dim);
            } else {
                draw_icon(scene, root, engine, icons::FOLDER, slot, p.text_dim);
            }
        } else if row.kind == Kind::File {
            // A file held elsewhere shows the download glyph in place of its
            // type glyph, because that is what pressing it does. Keeping the
            // type icon and dimming it would say "disabled" — the opposite of
            // the truth, which is that this row is the one that wants a click.
            let (glyph, icon_colour) = match row.file_state {
                FileState::Remote => (icons::IMPORT, p.placeholder),
                FileState::Fetching => (icons::IMPORT, p.accent),
                FileState::Present => (
                    koru_ui::file_icon::FileIcon::new(row.mime.as_deref(), Some(&row.label))
                        .glyph(),
                    p.text_dim,
                ),
            };
            draw_icon(
                scene,
                root,
                engine,
                glyph,
                Rect::new(icon_x, r.y, PAGE_ICON_SLOT, r.h),
                icon_colour,
            );
        } else if let Some(icon) = &row.icon {
            let label_bl = label_metrics.map(|m| label_y + m.baseline);
            draw_page_emoji(scene, root, engine, icon, icon_x, r, label_bl);
        } else {
            // Every row under Canvases is a canvas; its row wears the glyph the
            // section's icon and the canvas's tab wear.
            let glyph = if self.mode == NavigatorMode::Canvases {
                icons::CANVAS
            } else {
                icon_for(row.kind)
            };
            draw_icon(
                scene,
                root,
                engine,
                glyph,
                Rect::new(icon_x, r.y, PAGE_ICON_SLOT, r.h),
                p.text_dim,
            );
        }
        let label_colour = match row.file_state {
            // Placeholder, not disabled: the name is real and the bytes are not
            // here yet. `placeholder` is the token that means exactly that, and
            // this is its first use.
            FileState::Remote => p.placeholder,
            FileState::Fetching => p.accent,
            FileState::Present if emphasised => p.text,
            FileState::Present => p.text_dim,
        };
        scene::draw_layout_first_line(scene, root, &label, text_x, label_y, Some(label_colour));
        // The note marker is **always** drawn on a row that has one, not
        // hover-only like the `…`: it says something true about the file, and
        // it is the way into the note. Dim like the type glyph — it is an
        // adornment, not the row's subject.
        if row.note.is_some() {
            draw_icon(scene, root, engine, icons::NOTE, parts.note, p.text_dim);
        }
        // On hover, like the editor's grip and `+`: a mark sitting permanently
        // beside every name reads as part of the name. It stays while its own
        // menu is open, or the button would vanish under the menu it opened.
        // Never in Agents mode: its menus open by right-click (Koru Terminal's
        // shape), and the trailing lane there belongs to the heading's `+`.
        if self.mode != NavigatorMode::Agents && (hovered || self.menu_row == Some(row.node)) {
            scene::dots_three(scene, root, parts.dots, p.text_dim);
        }
    }

    /// The lifted carried selection: one representative row drawn at the
    /// pointer, over a fill that reads as "the thing being dragged".
    fn draw_carried(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        top: f32,
        p: &theme::Palette,
    ) {
        let Some(d) = self.drag.as_ref() else {
            return;
        };
        if d.carried.is_empty() {
            return;
        }
        // Over a pane the drop opens the document — the pane's slot marker or
        // split preview is speaking, and a lifted row promising a tree move
        // would be lying the whole way.
        if self.carried_hidden {
            return;
        }
        // A multi-selection has already collapsed to the representative row in
        // the cache. The generic range arithmetic remains useful for the plain
        // one-root case and for its release settle.
        let mut first_start = None;
        let mut grab_abs = 0;
        let mut end = 0;
        for (i, r) in self.rows.iter().enumerate() {
            if d.carried.contains(&r.node) {
                first_start.get_or_insert(i);
                end = i + 1;
                if r.node == d.grab {
                    grab_abs = i;
                }
            }
        }
        let Some(first_start) = first_start else {
            return;
        };
        // Extend `end` over any descendants trailing the final carried root.
        let root_depth = self.rows[first_start].depth;
        while end < self.rows.len() && self.rows[end].depth > root_depth {
            end += 1;
        }
        let grab_offset = grab_abs.saturating_sub(first_start) as f32;
        let block_top = d.y - d.grab_dy - grab_offset * theme::NAV_ROW_H;
        // Keep the block for the rows band even if the pointer has left it, so
        // a drag past the top or bottom does not paint over the header.
        let _ = top;
        for j in first_start..end {
            let mut row = self.rows[j].clone();
            if d.item_count > 1 {
                row.label = multi_drag_label(&row.label, d.item_count);
            }
            let ry = block_top + (j - first_start) as f32 * theme::NAV_ROW_H;
            let r = Rect::new(area.x, ry, area.w, theme::NAV_ROW_H);
            // Lifted, so it wears the same card the open row does: the sheet
            // colour, outlined. That is what "off the column" looks like here,
            // and a carried row is off the column by definition. Context tells
            // the two apart — this one is following the pointer.
            draw_row_card(scene, root, r, p);
            self.draw_row_body(scene, root, engine, &row, r, p, true, false);
        }
    }

    fn draw_header(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        avatar: Option<&peniko::ImageBrush>,
    ) {
        let p = theme::p();
        let h = Self::header_rects(area);
        if h.row.w <= 0.0 {
            return;
        }
        // Each half lights on its own, because each opens its own list. A
        // popover mid-fade still counts as open here, so the half it belongs to
        // stays lit for exactly as long as it is on screen.
        let open = self.popover.as_ref().map(|x| x.kind);
        let avatar_hot =
            self.header_hover == Some(HeaderPart::Avatar) || open == Some(PopoverKind::Profiles);
        let name_hot = self.header_hover == Some(HeaderPart::Workspace)
            || open == Some(PopoverKind::Workspaces);
        if avatar_hot {
            let a = h.avatar_hit;
            scene::fill(
                scene,
                root,
                p.hover_tint,
                &scene::rrect(a.x, a.y, a.w, a.h, 6.0),
            );
        }
        if name_hot {
            let n = h.name_hit;
            scene::fill(
                scene,
                root,
                p.hover_tint,
                &scene::rrect(n.x, n.y, n.w, n.h, 6.0),
            );
        }
        self.draw_avatar(scene, root, engine, h.avatar, avatar, theme::role::hint());

        // **The same tile the switcher's rows wear**, in front of the name. The
        // header says *where you are*, and it should say it the way the list you
        // chose from says it — a name alone made the two read as different
        // things. One component, one size argument.
        let entry = self
            .current
            .and_then(|id| self.workspaces.iter().find(|w| w.id == id));
        let mut name = h.name;

        if let Some(w) = entry {
            let chip = Rect::new(
                name.x,
                name.y + (name.h - HEADER_CHIP) * 0.5,
                HEADER_CHIP,
                HEADER_CHIP,
            );
            koru_draw::tile::Tile {
                icon: w.icon.as_deref(),
                color: w.color.as_deref(),
                name: &w.name,
            }
            .draw(scene, root, engine, chip);
            let shift = HEADER_CHIP + 7.0;
            name = Rect::new(name.x + shift, name.y, (name.w - shift).max(1.0), name.h);
        }
        let label = entry.map(|w| w.name.as_str()).unwrap_or("Workspace");
        draw_middle(
            scene,
            root,
            engine,
            &label_fit(label),
            name,
            theme::role::row(),
            p.text,
        );

        // **The chevron turns over**, so the header says open or shut rather
        // than leaving it to be inferred. `ph-caret-up` is not in `icons.rs` and
        // those codepoints are lifted from the pinned stylesheet rather than
        // guessed — so the open state is `CARET_DOWN` mirrored about its own
        // centre, the same `Affine` trick the logo's turn uses.
        //
        // **It turns rather than snapping**, off the popover's own progress
        // rather than a clock of its own — so it cannot drift out of step with
        // the panel, and it adds nothing to `animating`. Scaling y from +1 to −1
        // passes through 0 at the halfway point, which is the glyph seen
        // edge-on: the same silhouette a real flip has, and why this needs no
        // second icon.
        let (cx, cy) = h.chevron.centre();
        let turn = self.popover_progress(PopoverKind::Workspaces);
        let chevron_root = if turn > 0.0 {
            root * Affine::translate((cx as f64, cy as f64))
                * Affine::scale_non_uniform(1.0, (1.0 - 2.0 * turn) as f64)
                * Affine::translate((-(cx as f64), -(cy as f64)))
        } else {
            root
        };
        draw_icon(
            scene,
            chevron_root,
            engine,
            icons::CARET_DOWN,
            h.chevron,
            if name_hot { p.text } else { p.text_dim },
        );
        scene::fill(
            scene,
            root,
            p.panel_border,
            &scene::rect(h.divider.x, h.divider.y, h.divider.w, h.divider.h),
        );
    }

    /// The floating panel and its rows.
    ///
    /// Drawn by its **own call**, after both columns, rather than from inside
    /// `draw`: a popover hangs over the active view, so where it lands in the
    /// frame is a z-order decision the container should be making out loud.
    pub fn draw_popover(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        area: Rect,
        viewport: (f32, f32),
        avatar: Option<&peniko::ImageBrush>,
    ) {
        let Some(panel) = self.popover_panel(area, viewport) else {
            return;
        };
        // **It appears and goes rather than blinking.** Two things at once, both
        // off the one eased number: it fades, and it settles the last few pixels
        // down from the button that opened it — which is what says the panel
        // came *from* there rather than arriving from nowhere.
        //
        // Shadowing `root` is deliberate: everything below draws under the same
        // transform the group is clipped by, so nothing can be left behind by
        // the motion.
        let t = self.popover.as_ref().map_or(0.0, |x| x.progress);
        if t <= 0.0 {
            return;
        }
        let root = root * Affine::translate((0.0, -((1.0 - t) * theme::POPOVER_RISE) as f64));
        // **Only while it is actually moving.** A fully-open popover is the
        // common case by a long way — it sits there while you read it — and an
        // opacity group is a texture allocation per frame. At `t == 1` the group
        // would composite the panel unchanged, so it is skipped and the menu at
        // rest costs exactly what it did before there was an animation.
        let fading = t < 1.0;
        if fading {
            // Generous enough for the shadow's rings, which reach further than
            // the panel: clipping them would give the fade a square edge.
            const HALO: f32 = 24.0;
            scene::fade_layer(
                scene,
                root,
                t,
                &scene::rect(
                    panel.x - HALO,
                    panel.y - HALO,
                    panel.w + HALO * 2.0,
                    panel.h + HALO * 2.0,
                ),
            );
        }
        // The frame every floating thing wears. The shadow is a soft falloff
        // rather than one offset rect — a slab with a hard edge reads as a
        // second panel behind the first rather than as depth.
        scene::floating_panel(
            scene,
            root,
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            &scene::Panel::popover(POPOVER_RADIUS),
        );

        let Some(state) = self.popover.as_ref() else {
            return;
        };
        if state.slide.is_some() {
            // **Both pages, side by side, sliding together.** The Profiles page
            // leaves to the left as a Profile's workspaces arrive from the
            // right, and the reverse on the way back. Clipped to the panel so
            // neither page is seen outside it.
            scene.push_clip_layer(
                peniko::Fill::NonZero,
                root,
                &scene::rrect(panel.x, panel.y, panel.w, panel.h, POPOVER_RADIUS),
            );
            for kind in [PopoverKind::Profiles, PopoverKind::Workspaces] {
                let dx = (page_of(kind) - state.page) * panel.w;
                let current = kind == state.kind;
                let scroll = if current { state.scroll } else { 0.0 };
                let hover = if current { state.hover } else { None };
                self.draw_page(
                    scene,
                    root * Affine::translate((dx as f64, 0.0)),
                    engine,
                    kind,
                    panel,
                    scroll,
                    hover,
                    avatar,
                );
            }
            scene.pop_layer();
        } else {
            self.draw_page(
                scene,
                root,
                engine,
                state.kind,
                panel,
                state.scroll,
                state.hover,
                avatar,
            );
        }
        if fading {
            scene.pop_layer();
        }
    }

    /// One page of a popover: its rows in `panel`, the list clipped to its
    /// band, and the band's scrollbar. `hover` is an index into this page's
    /// rows, and only the page taking input has one.
    #[allow(clippy::too_many_arguments)]
    fn draw_page(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        kind: PopoverKind,
        panel: Rect,
        scroll: f32,
        hover: Option<usize>,
        avatar: Option<&peniko::ImageBrush>,
    ) {
        let p = theme::p();
        let list_band = self.page_band(kind, panel);
        let content_h = self.page_list_content_h(kind);
        let scroll_max = list_band.map_or(0.0, |band| (content_h - band.h).max(0.0));
        if let Some(band) = list_band
            && scroll_max > 0.0
        {
            // A list that scrolls is separated from the fixed footer under it,
            // so rows slipping out of view read as going *under* the commands.
            scene::fill(
                scene,
                root,
                p.panel_border,
                &scene::rect(band.x + 8.0, band.y + band.h, (band.w - 16.0).max(0.0), 1.0),
            );
        }
        let browsed = self.browsed_workspaces();
        let open_here = self.browsing_other().is_none();
        for (i, (row, r)) in self.page_rects(kind, panel, scroll).into_iter().enumerate() {
            let list_row = matches!(row, PopoverRow::Workspace(_) | PopoverRow::Profile(_));
            if list_row {
                let Some(band) = list_band else { continue };
                if r.y + r.h <= band.y || r.y >= band.y + band.h {
                    continue;
                }
                if matches!(row, PopoverRow::Workspace(i) if browsed.get(i).is_none())
                    || matches!(row, PopoverRow::Profile(i) if self.profiles.get(i).is_none())
                {
                    continue;
                }
                scene.push_clip_layer(
                    peniko::Fill::NonZero,
                    root,
                    &scene::rect(band.x, band.y, band.w, band.h),
                );
            }
            let hot = hover == Some(i);
            if hot && row.selectable() {
                scene::fill(
                    scene,
                    root,
                    p.hover_tint,
                    &scene::rrect(r.x + 4.0, r.y, (r.w - 8.0).max(0.0), r.h, 5.0),
                );
            }
            let icon_x = r.x + theme::NAV_PAD;
            let text_x = icon_x + 22.0;
            let text = |x: f32| Rect::new(x, r.y, (r.x + r.w - x - 8.0).max(1.0), r.h);
            match row {
                PopoverRow::WorkspacesHeading | PopoverRow::ProfilesHeading => {
                    let workspaces = matches!(row, PopoverRow::WorkspacesHeading);
                    let label = if workspaces { "WORKSPACES" } else { "PROFILES" };
                    let x = r.x + theme::NAV_PAD * 2.0;
                    // **The heading teaches the chord that opens it.** A
                    // shortcut nobody can find is one nobody uses, and this
                    // panel has no other row to hang it off. Rendered from the
                    // registry, so it says `⌥⌘W` on a Mac without a second
                    // literal to keep in step.
                    let mut label_right = r.x + r.w - 8.0;
                    let shortcut = if workspaces {
                        koru_keys::ShortcutId::SwitchWorkspace
                    } else {
                        koru_keys::ShortcutId::SwitchProfile
                    };
                    let chord = koru_keys::show_hint(shortcut, koru_keys::Section::Heading);
                    if let Some(chord) = chord {
                        let laid = engine.simple_layout(&chord, theme::role::hint());
                        let chord_x = r.x + r.w - laid.width() - theme::NAV_PAD * 2.0;
                        scene::draw_layout_first_line(
                            scene,
                            root,
                            &laid,
                            chord_x,
                            r.y + (r.h - scene::first_line_height(&laid)) * 0.5,
                            Some(p.row_hint),
                        );
                        // The heading stops before it, so a longer word in
                        // another language wraps or elides rather than running
                        // underneath the chord.
                        label_right = chord_x - 8.0;
                    }
                    draw_middle(
                        scene,
                        root,
                        engine,
                        label,
                        Rect::new(x, r.y, (label_right - x).max(1.0), r.h),
                        theme::role::hint(),
                        p.row_hint,
                    );
                }
                PopoverRow::Back => {
                    // The way back, and the name of the Profile whose
                    // workspaces these are — one row, so the page says whose
                    // list it is without a second heading.
                    draw_icon(
                        scene,
                        root,
                        engine,
                        icons::CARET_LEFT,
                        Rect::new(icon_x, r.y, 16.0, r.h),
                        p.text_dim,
                    );
                    let profile = self
                        .browsed_profile()
                        .and_then(|id| self.profiles.iter().find(|p| p.id == id));
                    let name = profile.map_or("Profile", |p| p.name.as_str());
                    let chip = Rect::new(
                        text_x,
                        r.y + (r.h - WORKSPACE_CHIP) * 0.5,
                        WORKSPACE_CHIP,
                        WORKSPACE_CHIP,
                    );
                    koru_draw::tile::Tile {
                        icon: profile.and_then(|p| p.icon.as_deref()),
                        color: profile.and_then(|p| p.color.as_deref()),
                        name,
                    }
                    .draw(scene, root, engine, chip);
                    draw_middle(
                        scene,
                        root,
                        engine,
                        &label_fit(name),
                        text(chip.x + chip.w + 8.0),
                        theme::role::row().with_weight(theme::FontWeight::SemiBold),
                        p.text,
                    );
                }
                PopoverRow::Workspace(i) => {
                    let w = &browsed[i];
                    let current = open_here && self.current == Some(w.id);
                    let parts = self.workspace_row_parts(row, r);
                    koru_draw::tile::Tile {
                        icon: w.icon.as_deref(),
                        color: w.color.as_deref(),
                        name: &w.name,
                    }
                    .draw(scene, root, engine, parts.chip);
                    // Bold **is** the "you are here" mark. It reads at a glance
                    // in the column where you are already looking, rather than
                    // at the far end of the row where a tick sat.
                    let colour = if current { p.text } else { p.text_dim };
                    let style = if current {
                        theme::role::row().with_weight(theme::FontWeight::SemiBold)
                    } else {
                        theme::role::row()
                    };
                    let fitted = engine.elided(&label_fit(&w.name), style, parts.name.w);
                    let label = engine.simple_layout(&fitted, style);
                    scene::draw_layout_first_line(
                        scene,
                        root,
                        &label,
                        parts.name.x,
                        scene::centre_y_in(&label, parts.name),
                        Some(colour),
                    );
                    // A mark, not a control: pinning lives in the manager.
                    if w.pinned {
                        draw_icon_filled(
                            scene,
                            root,
                            engine,
                            icons::PUSH_PIN,
                            parts.pin,
                            p.row_hint,
                        );
                    }
                }
                PopoverRow::LibraryAction(action) => {
                    draw_icon(
                        scene,
                        root,
                        engine,
                        actions::glyph(action),
                        Rect::new(icon_x, r.y, 16.0, r.h),
                        p.text_dim,
                    );
                    draw_middle(
                        scene,
                        root,
                        engine,
                        actions::label(action, self.mode),
                        text(text_x),
                        theme::role::row(),
                        p.text_dim,
                    );
                }
                PopoverRow::NewWorkspace | PopoverRow::NewProfile => {
                    draw_icon(
                        scene,
                        root,
                        engine,
                        icons::PLUS,
                        Rect::new(icon_x, r.y, 16.0, r.h),
                        p.text_dim,
                    );
                    let label = if row == PopoverRow::NewWorkspace {
                        "New workspace"
                    } else {
                        "New profile"
                    };
                    draw_middle(
                        scene,
                        root,
                        engine,
                        label,
                        text(text_x),
                        theme::role::row(),
                        p.text_dim,
                    );
                    // **No chord printed here.** `Ctrl+Shift+N` was once
                    // printed and is the document's Narrow view mode. See
                    // `mainview::keys::resolve`.
                }
                PopoverRow::Divider => {
                    scene::fill(
                        scene,
                        root,
                        p.panel_border,
                        &scene::rect(
                            r.x + theme::NAV_PAD,
                            r.y + (r.h - 1.0) * 0.5,
                            (r.w - theme::NAV_PAD * 2.0).max(0.0),
                            1.0,
                        ),
                    );
                }
                PopoverRow::Profile(i) => {
                    let profile = &self.profiles[i];
                    let current = self.current_profile == Some(profile.id);
                    // Only the profile you are *in* has a decoded picture to
                    // hand — the others are rows in an index, and their blobs
                    // live in folders this process has not opened. The initials
                    // chip is the honest fallback rather than a blank circle.
                    let brush = if current { avatar } else { None };
                    let detail = if profile.email.is_empty() {
                        folder_hint(&profile.folder)
                    } else {
                        profile.email.clone()
                    };
                    koru_ui::profile_picker::draw_row(
                        scene,
                        root,
                        engine,
                        r,
                        koru_ui::profile_picker::ProfileRow {
                            name: &profile.name,
                            detail: &detail,
                            palette_key: &profile.email,
                            selected: current,
                            removable: false,
                            menu: false,
                            avatar: brush,
                            icon: profile.icon.as_deref(),
                            color: profile.color.as_deref(),
                        },
                        hot,
                    );
                    // The row goes somewhere — its workspaces — and says so.
                    draw_icon(
                        scene,
                        root,
                        engine,
                        icons::CARET_RIGHT,
                        koru_ui::profile_picker::row_parts(r).trailing,
                        p.row_hint,
                    );
                }
                PopoverRow::ImportContent => {
                    draw_icon(
                        scene,
                        root,
                        engine,
                        icons::IMPORT,
                        Rect::new(icon_x, r.y, 16.0, r.h),
                        p.text_dim,
                    );
                    draw_middle(
                        scene,
                        root,
                        engine,
                        "Import content\u{2026}",
                        text(text_x),
                        theme::role::row(),
                        p.text_dim,
                    );
                    // A flyout of formats opens from it, so it carries the
                    // submenu caret every menu row with a flyout carries.
                    draw_icon(
                        scene,
                        root,
                        engine,
                        icons::CARET_RIGHT,
                        Rect::new(r.x + r.w - WORKSPACE_DOTS - 8.0, r.y, WORKSPACE_DOTS, r.h),
                        p.row_hint,
                    );
                }
                PopoverRow::AddAccount
                | PopoverRow::OpenProfile
                | PopoverRow::ManageProfiles
                | PopoverRow::ManageWorkspaces
                | PopoverRow::LinkDevice => {
                    let (glyph, label) = match row {
                        // `ph-plus-circle` would suit better; it is not in
                        // `icons.rs`, and adding a codepoint means reading it
                        // out of the pinned stylesheet rather than inferring it.
                        PopoverRow::AddAccount => (icons::PLUS, "Add account"),
                        PopoverRow::OpenProfile => (icons::FOLDER, "Open a profile folder"),
                        PopoverRow::ManageProfiles => (icons::LINK, "Manage profiles\u{2026}"),
                        PopoverRow::ManageWorkspaces => (icons::LINK, "Manage workspaces\u{2026}"),
                        _ => (icons::LINK, "Link a device\u{2026}"),
                    };
                    draw_icon(
                        scene,
                        root,
                        engine,
                        glyph,
                        Rect::new(icon_x, r.y, 16.0, r.h),
                        p.text_dim,
                    );
                    draw_middle(
                        scene,
                        root,
                        engine,
                        label,
                        text(text_x),
                        theme::role::row(),
                        p.text_dim,
                    );
                }
            }
            if list_row {
                scene.pop_layer();
            }
        }
        if let Some(band) = list_band
            && scroll_max > 0.0
        {
            let thumb_h = (band.h * band.h / content_h).max(24.0).min(band.h);
            let thumb_y = band.y + (band.h - thumb_h) * (scroll / scroll_max);
            scene::fill(
                scene,
                root,
                p.row_hint,
                &scene::rrect(band.x + band.w - 4.0, thumb_y, 2.0, thumb_h, 1.0),
            );
        }
    }

    /// The header's avatar: this profile's picture, or its initials chip.
    fn draw_avatar(
        &self,
        scene: &mut Scene,
        root: Affine,
        engine: &mut TextEngine,
        at: Rect,
        avatar: Option<&peniko::ImageBrush>,
        style: theme::TextStyle,
    ) {
        static NOBODY: std::sync::OnceLock<ProfileSummary> = std::sync::OnceLock::new();
        let profile = self
            .profile()
            .unwrap_or_else(|| NOBODY.get_or_init(ProfileSummary::default));
        if profile.icon.is_some() || (avatar.is_none() && profile.color.is_some()) {
            koru_draw::tile::Tile {
                icon: profile.icon.as_deref(),
                color: profile.color.as_deref(),
                name: &profile.name,
            }
            .draw(scene, root, engine, at);
            return;
        }
        koru_ui::profile_picker::draw_avatar(
            scene,
            root,
            engine,
            at,
            avatar,
            &profile.name,
            &profile.email,
            style,
        );
    }
}

/// Vertically centre one line of text in `at`, truncated to its width.
fn draw_middle(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    text: &str,
    at: Rect,
    style: theme::TextStyle,
    colour: vello::peniko::Color,
) {
    let l = engine.simple_layout_wrapped(text, style, at.w.max(1.0));
    let line_h = l.lines().next().map_or(0.0, |x| x.metrics().line_height);
    scene::draw_layout_first_line(
        scene,
        root,
        &l,
        at.x,
        at.y + (at.h - line_h) * 0.5,
        Some(colour),
    );
}

/// The last two components of a path — enough to tell two folders with the same
/// name apart, which is what the second line of a profile row is for.
///
/// The *tail*, not the head: truncating a long path from the right would hide
/// exactly the part that differs, since what collides is the last component.
fn folder_hint(folder: &str) -> String {
    let parts: Vec<&str> = folder
        .split(['/', '\\'])
        .filter(|p| !p.is_empty())
        .collect();
    match parts.len() {
        0 => String::new(),
        1 => parts[0].to_string(),
        n => format!(
            "{}{}{}",
            parts[n - 2],
            std::path::MAIN_SEPARATOR,
            parts[n - 1]
        ),
    }
}

/// A name that is definitely something. An empty one reads as a broken row
/// rather than an unnamed thing.
fn label_fit(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "Untitled".to_string()
    } else {
        trimmed.to_string()
    }
}

/// A tree row's disclosure caret, centred in `at` and drawn under `root` —
/// which is where its quarter turn comes from.
///
/// Its own function rather than [`draw_icon`] because that draws at
/// `CAPTION_ICON`, the size of a *command*: a close box, a window button, the
/// `+`. A disclosure caret is not a command, it is punctuation on the row, and
/// at command size it competed with the file icon beside it for attention.
/// [`CARET_SIZE`] is half of it.
fn draw_caret(
    scene: &mut Scene,
    root: Affine,
    engine: &mut TextEngine,
    at: Rect,
    colour: peniko::Color,
) {
    let layout = engine.icon_layout(icons::CARET_RIGHT, CARET_SIZE);
    let (w, h) = (layout.width(), layout.height());
    scene::draw_layout(
        scene,
        root,
        &layout,
        at.x + (at.w - w) * 0.5,
        at.y + (at.h - h) * 0.5,
        Some(colour),
    );
}

/// Step to the next row the keyboard may land on, wrapping, skipping furniture.
/// The same shape `MainView::next_enabled` has for a menu with dimmed items.
fn next_selectable(rows: &[PopoverRow], from: Option<usize>, forward: bool) -> Option<usize> {
    if rows.is_empty() {
        return None;
    }
    let n = rows.len();
    let mut i = match from {
        Some(i) => i,
        None if forward => n - 1,
        None => 0,
    };
    for _ in 0..n {
        i = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        if rows[i].selectable() {
            return Some(i);
        }
    }
    None
}

/// Room above the first row, for the column's own heading.
const HEADER_H: f32 = 24.0;
/// The smallest a navigator mode icon shrinks to before the heading's search
/// and `…` lanes give way to it (`ExploreView::navigator_mode_rects`).
const MIN_MODE_ICON: f32 = 20.0;

/// The workspace band at the top of the pane. `CAPTION_H` tall so the avatar and
/// the workspace name sit on the same optical line as the logo and the tabs
/// across the divide.
const WORKSPACE_ROW_H: f32 = theme::CAPTION_H;
/// A tree row's disclosure caret, at **half** `CAPTION_ICON`. It is
/// punctuation on the row rather than a command, and at command size it
/// competed with the file icon beside it.
const CARET_SIZE: f32 = theme::CAPTION_ICON * 0.5;

const AVATAR: f32 = 22.0;
/// The chip's corner. Boxy rather than round, so it does not read as a bubble
/// sitting inside the hover box behind it.
/// The space **between** the avatar's control and the workspace's, so the two
/// read as two things.
const AVATAR_GAP: f32 = 6.0;
/// The workspace name's inset inside its own box.
const NAME_PAD: f32 = 8.0;
const CHEVRON_W: f32 = 18.0;
const CHEVRON_PAD: f32 = 5.0;

/// Heights of a popover's rows.
const SECTION_H: f32 = 20.0;
const POPOVER_ROW_H: f32 = theme::ROW_H;
const POPOVER_DIVIDER_H: f32 = 9.0;
/// The switcher's cap. Taller than a plain menu's, because each page carries a
/// fixed head and a four-row footer around its scrolling list.
const SWITCHER_MAX_H: f32 = 420.0;

/// The rounded tile at the left of a workspace row, carrying its colour and its
/// emoji. Square.
const WORKSPACE_CHIP: f32 = 20.0;
/// The same tile in the pane's header, in front of the workspace's name. The
/// glyph inside it is derived from this — see `koru_draw::tile`.
const HEADER_CHIP: f32 = 18.0;
/// The `…` between the name and the tick.
/// Where a row's icon sits, given its depth and whether the row carries the
/// caret lane.
///
/// **The lane is paid for only where a caret can appear** — the same rule the
/// document canvas already keeps for the checkbox gutter. Documents rows all
/// carry it: any node can gain children as a peer's edit lands, and an icon
/// that jumped sideways when one did would read as a bug. The flat projections
/// (Files, the tasks rail) never draw a caret, so they do not reserve it, and
/// an Agents row reserves it only while it actually has one — the way Finder
/// shifts a folder's icon for its disclosure triangle and no other's.
///
/// Shared by the tree rows and the pinned footer rows, so the two **cannot**
/// drift out of alignment — a Trash row that sat a few pixels left of every
/// row above it reads as a different kind of list.
fn icon_lane(x: f32, depth: u16, caret: bool) -> f32 {
    x + theme::NAV_PAD
        + depth as f32 * theme::NAV_INDENT
        + if caret { theme::NAV_INDENT } else { 0.0 }
}

const WORKSPACE_DOTS: f32 = 20.0;
/// The `…` at the end of a library row. Its width is reserved on every row
/// whether or not the button is showing — see [`ExploreView::row_parts`].
const ROW_DOTS: f32 = 20.0;
/// The note affordance at the end of a **file** row that carries a note.
/// Unlike the `…`, its lane is reserved only for a row that has one — the
/// others get the label width back.
const ROW_NOTE: f32 = ROW_DOTS;
/// The corner radius of a row's chip — the open card, the hover tint and the
/// keyboard ring, which all have to agree or they read as three shapes.
const ROW_RADIUS: f32 = 6.0;
/// Visible air between consecutive navigation chips. Hit targets keep the full
/// row height, so density improves without creating dead pointer strips.
const ROW_GAP: f32 = 2.0;

/// The chip a row's fill, card and ring are all drawn on.
///
/// **One shape for every row in the column** — a library row, a carried row and
/// a pinned footer row — so the three cannot drift into looking like three
/// different lists. Inset from the column's edges so the card reads as sitting
/// *in* the pane rather than spanning it.
fn row_chip(r: Rect) -> vello::kurbo::RoundedRect {
    scene::rrect(
        r.x + 4.0,
        r.y + ROW_GAP * 0.5,
        r.w - 8.0,
        (r.h - ROW_GAP).max(0.0),
        ROW_RADIUS,
    )
}

fn trash_drop_label(count: usize) -> String {
    format!(
        "Move {count} {} to trash",
        if count == 1 { "item" } else { "items" }
    )
}

fn multi_drag_label(label: &str, count: usize) -> String {
    format!("{count} items · {label}")
}

/// **What a row is wearing**, decided in one place and painted in another.
///
/// It used to be a five-armed `if` chain inside a hundred-line draw pass, which
/// is why a picked row could sit in the hover's clothes for a release without
/// anything noticing: there was nothing to ask. Naming the outcome makes the
/// question answerable without a font, a GPU or a window — see
/// `a_picked_row_wears_the_same_card_as_the_open_one` in this module's tests —
/// and leaves the paint a `match` with no opinions in it.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub(crate) enum RowTreatment {
    /// An armed Agents drag is resting here; let go and it goes in.
    DropTarget,
    /// The Agents *Projects* heading. Furniture: nothing behind it at all.
    Heading,
    /// The sheet colour, outlined — lifted off the column. **The open row and
    /// every picked row**, because the column is saying the same thing about
    /// all of them: this is one of the rows you are working with.
    Card,
    /// The pointer is passing over. A tint, which is all a pointer has earned.
    Hover,
    /// Nothing behind it.
    Idle,
}

/// The card a **chosen** row wears: the sheet colour, outlined.
///
/// Shared by the open document, every row of a multi-selection, the block a
/// drag carries, and the active pinned row — because they mean the same thing:
/// this is a row you are working with, lifted off the column.
fn draw_row_card(scene: &mut Scene, root: Affine, r: Rect, p: &theme::Palette) {
    let chip = row_chip(r);
    scene::fill(scene, root, p.panel_bg, &chip);
    scene::stroke(scene, root, p.panel_border, theme::BORDER_W, &chip);
}
/// Air below the last pinned row, so it floats off the floor rather than
/// sitting on it.
const FOOTER_PAD: f32 = 6.0;
/// The empty-state call to action: between the sentence and the button, the
/// button's height, and the air either side of its label.
const CTA_GAP: f32 = 14.0;
const CTA_H: f32 = 30.0;
const CTA_PAD_X: f32 = 14.0;
/// Between the empty state's two buttons, when it has two.
const SUBSCRIBE_GAP: f32 = 8.0;
// There is no emoji-size constant here any more: `koru_draw::tile` derives the
// glyph from the tile's size, so a caller changes one number. The trap it exists
// for is real — a colour emoji fills its em box far more completely than an
// outline glyph, the same reason [`koru_shell_chrome::PAGE_EMOJI_SIZE`] is smaller than it
// looks like it should be — but it is now solved in one place rather than five.

/// The floating panel: at least this wide, and never narrower than the pane it
/// hangs from.
const POPOVER_W: f32 = theme::PANEL_W;
const POPOVER_PAD: f32 = theme::PANEL_PAD;
const POPOVER_RADIUS: f32 = 8.0;
/// How far below the header the panel sits, and how close to the window's edge
/// it may be pushed.
const POPOVER_GAP: f32 = 4.0;
const POPOVER_EDGE: f32 = 4.0;

// There is deliberately no chord constant for "New workspace". The one that
// used to be printed here, `Ctrl+Shift+N`, was assumed free and is not — it is
// the document's Narrow view mode. See the note in the row's draw arm.

/// Whether anything under `id` — or `id` itself — is a document.
///
/// Only reached for a *collapsed* node, where the rows that would answer the
/// question have not been built: it shows a chevron only if opening it would
/// reveal a document. An expanded one gets the same answer for free from
/// `push`'s return value, which is why this is not the general path.
///
/// `kind_of` rather than `read`, for the reason in `push` — and it matters more
/// here. This is the *collapsed* path, so a shut document with ten thousand
/// paragraphs in it gets walked in full to answer "is there a caret?", and every
/// one of those nodes would otherwise be read whole.
fn has_document_under(ws: &WorkspaceDoc, id: NodeId) -> bool {
    let Some(kind) = ws.kind_of(id) else {
        return false;
    };
    if kind == Kind::Document {
        return true;
    }
    ws.children(Parent::Node(id))
        .unwrap_or_default()
        .into_iter()
        .any(|child| has_document_under(ws, child))
}

/// What a row is called.
///
/// A blob has no text and a code block's text is its code, not a title, so an
/// unnamed image or code block is numbered by position among its parent's
/// landmarks of the same kind ("Image 3", "Code 2"). A divider is always
/// "Divider" (it carries no text either). A ListBlock's text is empty too, so
/// it falls back to "List N" or its `meta.name`. Everything else uses its own
/// text. A user-authored `meta.name` replaces the generated fallback for images,
/// code blocks and lists.
fn label_for(node: &koru_core::node::Node, image_no: usize, code_no: usize) -> String {
    let generated = match node.kind {
        Kind::Image => format!("Image {image_no}"),
        Kind::Code => format!("Code {code_no}"),
        Kind::Divider => return "Divider".to_string(),
        Kind::Quote => {
            return node
                .content
                .as_text()
                .filter(|text| !text.is_empty())
                .unwrap_or("Quote")
                .to_string();
        }
        Kind::ListBlock => format!("List {image_no}"),
        Kind::Callout => "Callout".to_string(),
        _ => {
            return node
                .content
                .as_text()
                .filter(|t| !t.is_empty())
                .unwrap_or("Untitled")
                .to_string();
        }
    };
    node.meta
        .name
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .unwrap_or(generated)
}

/// The glyph a kind wears in the tree.
///
/// **Exhaustive on purpose, with no `_` arm**: adding a kind should be a
/// compile error here, so whoever adds it picks an icon rather than inheriting
/// a generic one nobody notices is wrong.
///
/// This is not where version tolerance lives. A kind *this build has never
/// heard of* never reaches this function — `read_node` rejects it and
/// [`ExploreView::push`] skips the node — which is the silent-invisibility
/// problem `docs/deferred/` describes, and it has to be fixed in the schema.
fn icon_for(kind: Kind) -> char {
    match kind {
        Kind::Document => icons::DOCUMENT,
        Kind::File => icons::FILE,
        Kind::Contact => icons::CONTACT,
        Kind::Task => icons::CHECK_SQUARE,
        Kind::Paragraph => icons::LIST,
        Kind::ListBlock => icons::LIST_CHECKS,
        Kind::Table => icons::TABLE,
        Kind::Code => icons::CODE,
        Kind::Image | Kind::Canvas => icons::IMAGE,
        Kind::Divider => icons::DIVIDER,
        Kind::Quote => icons::QUOTES,
        Kind::Callout => icons::LIGHTBULB,
        // The trash root never earns a row — `children(Parent::Root)` omits
        // it, so `push` never sees one. The arm exists because this match has no
        // `_`, and it names the right glyph anyway: the pinned row at the foot of
        // the column draws exactly this.
        Kind::Trash => icons::TRASH,
    }
}

/// What this column offers a screen reader, and what a request to one of its
/// controls means. A child module rather than a section of this file: the pane
/// gets a call, not another embedded subsystem (§7.10).
pub(crate) mod accessibility;
pub mod actions;
mod canvases;
pub mod rail;
mod slides;

use slides::RowSlides;

#[cfg(test)]
mod tests;
