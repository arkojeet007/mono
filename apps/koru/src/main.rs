// SPDX-License-Identifier: MPL-2.0

// A GUI app, in every build: launched from Explorer or the taskbar it must not
// drag a console window along. A debug build used to keep its own console
// subsystem, which made a Jump List launch (a second `koru.exe`) flash a
// console window -- the one thing a taskbar entry must never do. So the
// subsystem is now unconditional, and the console a terminal launch needs is
// *attached* rather than owned -- see `console.rs`. Tests keep the console
// subsystem, or `cargo test` would print into the void.
#![cfg_attr(not(test), windows_subsystem = "windows")]

//! The container: one process, one window, many tabs.
//!
//! Everything that used to be here has moved. Windows are `koru-workspace-host`, editing
//! is `koru-editor`, the chrome is `koru-shell-chrome` (with the Koru panes in
//! `panes/`), and the views are libraries under `apps/`. What is left is the one thing only something that can see
//! *all* of them is able to say: which view a given node calls for, and what a
//! window is made of.
//!
//! That is why the views are hosted in one process rather than being separate
//! binaries. A document opens instantly and shares the undo history and the
//! in-memory document with the outline that spawned it; two binaries would have
//! to converge through the peer file transport instead, with the lag and the
//! split history that implies. Each app *can* still run alone —
//! `cargo run -p koru-document` — which is what its own `main.rs` is for.
//!
//! **A window is a workspace; a tab is a thing you opened.** Clicking a document
//! pill opens a tab, and only Ctrl+N opens a second window.

mod agent;
mod ai_chat_view;
mod build_stamp;
mod canvas_view;
mod console;
mod diff;
mod feedback;
mod format;
mod leaf;
pub(crate) mod mainview;
mod manage;
mod manager_view;
pub(crate) mod panes;
mod reminder_settings;
mod reopen_claim;
mod result_document;
mod result_view;
mod run_view;
mod schedule_index;
mod schedule_words;
mod search;
mod session_tab;
mod settings_view;
mod socials_view;
mod support;
mod taskbar;
mod tasks;
mod tasks_view;
mod terminal_prefs;
mod trash;
mod website;
mod welcome;
mod whatsnew;

use koru_core::schema::WorkspaceDoc;
use koru_core::{DocumentId, NodeId};
use koru_workspace::Workspace;
use koru_workspace_host::Host;
use koru_workspace_host::PopoutClose;
use mainview::MainView;
use manager_view::{DocumentPane, ManagerView};

struct Manager {
    new_document: bool,
    /// A taskbar *Search* task was chosen on a cold launch — this process is the
    /// one that has to ask the agent for its overlay.
    search: bool,
    /// A taskbar *AI Chat* task was chosen on a cold launch. Once the Agent is
    /// connected, ask it to open or focus the singleton standalone chat.
    ai_chat: bool,
    /// A document a taskbar Recent entry named, when this process is the one
    /// that has to open it — a cold launch, with no Koru already running to
    /// hand it to.
    open: Option<NodeId>,
    /// A stable document destination. Unlike `open`, its current workspace is
    /// resolved by the Store before the view is asked to display it.
    open_document: Option<DocumentId>,
    trash_root: Option<NodeId>,
    match_text: Option<String>,
    /// `--no-search`: this window offers no search surface.
    search_enabled: bool,
    /// `--test-upgrade`: replace the agent's report with a safe local offer.
    test_upgrade: bool,
    /// This window's end of the conversation with the Koru Agent — the global
    /// shortcut, the machine-wide overlay, and the routing of a result to the
    /// window that should show it. Absent when no agent could be reached, which
    /// is a degradation rather than a failure; see [`agent`].
    agent: agent::AgentLink,
    /// Retained so each newly constructed window can wake on reminder worker
    /// completion without polling the event loop.
    wake: Option<winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>>,
    /// The support chat, one per process, alive across the window being
    /// hidden and shown; ended on quit, on a last-document quit, and by
    /// End chat. `None` until Contact support is first chosen.
    support: Option<support::Session>,
}

impl Host for Manager {
    fn open_support(&mut self, _ws: &WorkspaceDoc) -> Option<ManagerView> {
        let origin = support::url::origin();
        let fresh = self
            .support
            .as_ref()
            .is_none_or(|s| s.borrow().state == support::session::State::Closed);
        if fresh {
            self.support = Some(support::SupportSession::new(origin, format::lang()));
        }
        let session = self.support.clone()?;
        Some(ManagerView::Container(MainView::support_window(
            self.agent.update_feed(),
            session,
        )))
    }

    fn open_fab(&mut self, _ws: &WorkspaceDoc) -> Option<ManagerView> {
        let session = self.support.clone()?;
        if session.borrow().state == support::session::State::Closed {
            return None;
        }
        Some(ManagerView::Container(MainView::fab_window(
            self.agent.update_feed(),
            session,
        )))
    }

    fn exiting(&mut self) {
        if let Some(session) = &self.support {
            session.borrow_mut().end();
        }
    }

    fn last_window_closing(&mut self) {
        if let Some(session) = &self.support {
            session.borrow_mut().end();
        }
    }

    type V = ManagerView;

    fn oauth_credential_context(&self) -> Option<koru_oauth_credentials::CredentialContext> {
        self.agent.credential_context()
    }

    fn account_state(&self) -> koru_agent_wire::AccountState {
        self.agent.account_state()
    }

    fn take_account_dirty(&self) -> bool {
        self.agent.take_account_dirty()
    }

    fn take_account_result(&self) -> Option<(u64, koru_agent_wire::AccountOperationResult)> {
        self.agent.take_account_result()
    }

    fn take_scoped_account_result(
        &self,
    ) -> Option<(u64, u64, koru_agent_wire::AccountOperationResult)> {
        self.agent.take_scoped_account_result()
    }

    fn account_command(&self, command: koru_agent_wire::AccountCommand) {
        self.agent.account(command);
    }
    fn account_command_owned(&self, owner_scope: u64, command: koru_agent_wire::AccountCommand) {
        self.agent.account_owned(Some(owner_scope), command);
    }
    fn account_command_tracked_owned(
        &self,
        submission: koru_workspace_host::account_ui::CommandSubmissionId,
        owner_scope: u64,
        command: koru_agent_wire::AccountCommand,
    ) {
        self.agent
            .account_tracked_owned(submission, Some(owner_scope), command);
    }
    fn account_command_tracked(
        &self,
        submission: koru_workspace_host::account_ui::CommandSubmissionId,
        command: koru_agent_wire::AccountCommand,
    ) {
        self.agent.account_tracked(submission, command);
    }
    fn take_account_command_failure(
        &self,
    ) -> Option<koru_workspace_host::account_ui::CommandFailure> {
        self.agent.take_account_command_failure()
    }
    fn take_account_command_admission(
        &self,
    ) -> Option<koru_workspace_host::account_ui::CommandAdmission> {
        self.agent.take_account_command_admission()
    }
    fn account_command_deadline(&self) -> Option<std::time::Instant> {
        self.agent.account_command_deadline()
    }
    fn remove_local_profile(&self, profile: koru_core::ProfileId) {
        self.agent.remove_local_profile(profile);
    }

    fn take_local_profile_removal(
        &self,
    ) -> Option<(
        String,
        Result<koru_agent_wire::LocalProfileRemovalOutcome, String>,
    )> {
        self.agent.take_local_profile_removal()
    }

    fn bind_local_profile_account(
        &self,
        profile: koru_core::ProfileId,
        expected_identity_id: Option<String>,
        new_identity_id: String,
    ) {
        self.agent
            .bind_local_profile_account(profile, expected_identity_id, new_identity_id);
    }

    fn take_local_profile_binding(
        &self,
    ) -> Option<(
        String,
        Result<koru_agent_wire::LocalProfileAccountBindingOutcome, String>,
    )> {
        self.agent.take_local_profile_binding()
    }

    fn install_wake_proxy(
        &mut self,
        proxy: winit::event_loop::EventLoopProxy<koru_workspace_host::app::Wake>,
    ) {
        self.agent.install_wake_proxy(proxy.clone());
        crate::taskbar::install_wake_proxy(proxy.clone());
        self.wake = Some(proxy);
    }

    fn voice_ensure(&self) -> Option<Box<dyn Fn() + Send + Sync>> {
        if !self.agent.connected() {
            return None;
        }
        let agent = self.agent.clone();
        Some(Box::new(move || agent.ensure_voice()))
    }

    /// **Updating is the agent's**, and what is left for the loop is to notice.
    /// A report arrives on the link's reader thread; a window that only found
    /// out on the next keystroke would sit on a finished download.
    fn take_update_redraw(&self) -> bool {
        self.agent.update_feed().take_dirty()
    }

    /// A window refused [`koru_workspace_host::AgentAsk::CloseForUpdate`] — somebody
    /// answered a save prompt with *cancel*.
    ///
    /// **Said as itself, not as *Later*.** It was `Later` here, which means
    /// *not this build* and defers it until a newer one appears — so cancelling
    /// one save prompt silently withdrew the update, and on a channel that
    /// ships a few builds a day the offer simply never came back for that one.
    /// A person keeping unsaved work has said nothing about the build.
    fn update_close_refused(&self) {
        let accepted = self
            .agent
            .update_feed()
            .choose(koru_agent_wire::UpdateChoice::CloseRefused);
        // `accepted=false` is the expensive case: the agent never hears the
        // refusal and waits out its full 120-second no-progress deadline.
        tracing::info!(target: "koru::update", accepted, "close refused, telling the agent");
    }

    /// **This window no longer owns the global shortcut**, and answering `None`
    /// is how that is said to the shell.
    ///
    /// `RegisterHotKey` is machine-wide, so two Koru windows meant the second
    /// silently lost Ctrl+Shift+K — which is why this used to be gated on
    /// winning the taskbar activation pipe, an election borrowed from another
    /// feature. The agent is the owner by construction: binding its endpoint is
    /// the exclusion, and a second agent stands down before it ever registers.
    /// A hit chosen in its overlay arrives as [`Host::take_agent_asks`] instead.
    fn take_global_search_request(&self) -> Option<koru_workspace_host::HotkeyActivation> {
        None
    }

    /// The one thing worth saying about the agent, once: that there is not one.
    fn take_global_search_notice(&self) -> Option<String> {
        self.agent.take_trouble()
    }

    fn take_agent_notice(&self) -> Option<koru_workspace_host::ErrorReport> {
        self.agent.take_notice()
    }

    /// A Jump List click asks this window to come forward. The shell focuses
    /// its own window; the taskbar module only signals that one is owed.
    fn take_activation_focus(&self) -> bool {
        crate::taskbar::take_foreground()
    }

    /// A Jump List *Search* click. The overlay is the agent's, so the host
    /// performs the whole ask itself — grant the agent the foreground and open
    /// its overlay. Polled in `about_to_wait`, so it fires on the listener's
    /// wake rather than waiting for a redraw.
    fn take_unpinned_documents(&self) -> Vec<koru_core::DocumentId> {
        crate::taskbar::take_unpinned()
    }

    fn take_search_activation(&self) -> bool {
        if crate::taskbar::take_search() {
            tracing::info!(target: "koru::taskbar", "search activation drained");
            self.agent.grant_foreground();
            self.agent.show_overlay();
            true
        } else {
            false
        }
    }

    fn take_ai_chat_activation(&self) -> bool {
        if crate::taskbar::take_ai_chat() {
            tracing::info!(target: "koru::taskbar", "AI Chat activation drained");
            self.agent.grant_foreground();
            self.agent.open_ai_chat();
            true
        } else {
            false
        }
    }

    fn open_uri(&self, url: &str) {
        let url = url.to_owned();
        let _ = std::thread::Builder::new()
            .name("koru-open-uri".into())
            .spawn(move || {
                let _ = koru_window::open_uri(&url);
            });
    }

    fn open_chat_link(&self, chat: String, block: String) {
        self.agent.open_chat_link(chat, block);
    }

    fn open_external_url(&self, url: String) -> Result<(), String> {
        self.agent.open_external_url(&url)
    }

    fn take_agent_asks(&self) -> Vec<koru_workspace_host::AgentAsk> {
        self.agent
            .drain()
            .into_iter()
            .filter_map(|ask| match ask {
                agent::FromTheAgent::Restore => Some(koru_workspace_host::AgentAsk::Restore),
                agent::FromTheAgent::OpenReminders => {
                    Some(koru_workspace_host::AgentAsk::OpenReminders)
                }
                agent::FromTheAgent::SetUpVoice => Some(koru_workspace_host::AgentAsk::SetUpVoice),
                agent::FromTheAgent::SetVoiceInput(device) => {
                    Some(koru_workspace_host::AgentAsk::SetVoiceInput(device))
                }
                agent::FromTheAgent::DictationGesture { route, gesture } => {
                    use koru_agent_wire::{DictationGesture as G, DictationKey as K};
                    use koru_workspace_host::{
                        AgentDictationGesture as G2, AgentDictationKey as K2,
                    };
                    let key = |key| match key {
                        K::D => K2::D,
                        K::M => K2::M,
                    };
                    let gesture = match gesture {
                        G::Press { key: value } => G2::Press(key(value)),
                        G::Release { key: value } => G2::Release(key(value)),
                        G::Cancel { key: value } => G2::Cancel(key(value)),
                    };
                    Some(koru_workspace_host::AgentAsk::DictationGesture { route, gesture })
                }
                agent::FromTheAgent::AgentDisconnected => {
                    Some(koru_workspace_host::AgentAsk::AgentDisconnected)
                }
                agent::FromTheAgent::CloseForUpdate => {
                    Some(koru_workspace_host::AgentAsk::CloseForUpdate)
                }
                agent::FromTheAgent::ShowWhatsNew { version } => {
                    Some(koru_workspace_host::AgentAsk::ShowWhatsNew(version))
                }
                agent::FromTheAgent::ShowFeedback => {
                    Some(koru_workspace_host::AgentAsk::ShowFeedback)
                }
                agent::FromTheAgent::ShowSession(session) => {
                    Some(koru_workspace_host::AgentAsk::ShowSession(session))
                }
                agent::FromTheAgent::NewAgentSession => {
                    Some(koru_workspace_host::AgentAsk::NewAgentSession)
                }
                agent::FromTheAgent::ShowSupport => {
                    Some(koru_workspace_host::AgentAsk::ShowSupport)
                }
                agent::FromTheAgent::ProfileLifecycleDeleted(profile) => Some(
                    koru_workspace_host::AgentAsk::ProfileLifecycleDeleted(profile),
                ),
                // A target whose ids this build cannot use is refused rather
                // than half-opened — the rule `search::pane_request` already
                // applies, reused here rather than restated.
                agent::FromTheAgent::Open(target) => {
                    if let koru_search_ui::Target::Store {
                        profile,
                        workspace,
                        document,
                        node,
                        ..
                    } = target.as_ref()
                    {
                        tracing::info!(
                            target: "koru::notification_activation",
                            profile = %profile.to_storage(),
                            workspace = %workspace.to_storage(),
                            document = document.map(|id| id.to_storage()).as_deref().unwrap_or("none"),
                            node = node.as_deref().unwrap_or("none"),
                            "Koru window loop converted Agent Open into pane navigation"
                        );
                    }
                    search::pane_request(*target)
                        .map(|request| koru_workspace_host::AgentAsk::Open(Box::new(request)))
                }
                agent::FromTheAgent::ReminderSnooze(intent) => {
                    tracing::info!(
                        target: "koru::notification_activation",
                        profile = %intent.profile.to_storage(),
                        workspace = %intent.entity.workspace().to_storage(),
                        document = %intent.entity.document().to_storage(),
                        node = %intent.entity.node(),
                        "Koru window loop converted Agent Snooze into a semantic picker request"
                    );
                    Some(koru_workspace_host::AgentAsk::ReminderSnooze {
                        intent: koru_workspace_host::ReminderSnoozeIntent {
                            profile: intent.profile,
                            reminder: intent.reminder,
                            occurrence: intent.occurrence,
                            entity: intent.entity,
                        },
                        delivery: intent.delivery,
                    })
                }
            })
            .collect()
    }

    fn accept_reminder_snooze(&self, delivery: String) {
        self.agent.accept_reminder_snooze(delivery);
    }

    fn agent_showing(
        &self,
        profile: koru_core::ProfileId,
        workspace: koru_core::WorkspaceId,
        profile_name: &str,
        workspace_name: &str,
        pinned: &[(String, koru_core::DocumentId, koru_core::WorkspaceId, bool)],
    ) {
        let pins: Vec<koru_agent_wire::SearchPin> = pinned
            .iter()
            .map(
                |(title, document, workspace, show_title)| koru_agent_wire::SearchPin {
                    title: title.clone(),
                    document: document.to_storage(),
                    workspace: workspace.to_storage(),
                    show_title: *show_title,
                },
            )
            .collect();
        self.agent
            .hello(profile, workspace, profile_name, workspace_name, &pins);
    }

    fn agent_available(&self) -> bool {
        self.agent.connected()
    }

    fn agent_has_notification_area(&self) -> bool {
        self.agent.has_notification_area()
    }

    fn agent_active(&self) {
        self.agent.active();
    }

    fn agent_dictation_state(&self, route: u64, state: koru_workspace_host::AgentDictationState) {
        use koru_agent_wire::{DictationMode as M, DictationState as S};
        use koru_workspace_host::AgentDictationState as S2;
        let state = match state {
            S2::Refused => S::Refused,
            S2::Listening(koru_dictation::Mode::PushToTalk) => S::Listening {
                mode: M::PushToTalk,
            },
            S2::Listening(koru_dictation::Mode::Toggle) => S::Listening { mode: M::Toggle },
            S2::Finalising => S::Finalising,
            S2::Idle => S::Idle,
        };
        self.agent.dictation_state(route, state);
    }

    // **`workspace_search_stat` is deliberately not implemented**, and neither
    // are `workspace_opened`/`workspace_closed`.
    //
    // The store has no per-workspace search status to report: it indexes every
    // workspace it holds and publishes counters for the whole index. `Host`'s
    // default answers `None`, and `WorkspaceStat` requires a view to render
    // unknown as a dash rather than as zero — so the workspace manager shows
    // three dashes rather than three numbers that stopped being true.
    //
    // The other two told the app's own index which workspaces a window held
    // live, so its disk scanner would leave them alone. There is no scanner.

    fn initial(&mut self, workspace: &mut Workspace) -> ManagerView {
        koru_editor::prepare(workspace);
        let mut view = MainView::new_with_agent(
            if self.test_upgrade {
                agent::UpdateFeed::test_upgrade()
            } else {
                self.agent.update_feed()
            },
            self.agent.clone(),
            self.search_enabled,
        );
        if let Some(proxy) = self.wake.clone() {
            view.install_reminder_wake_proxy(proxy);
        }
        if std::mem::take(&mut self.new_document) {
            view.prompt_new_document();
        }
        // A cold *Search* task: the overlay is the agent's, and it may not even
        // be running yet — `connect` above started one, and this hands it the
        // request. No view involved, so it needs nothing but the link.
        if std::mem::take(&mut self.search) {
            self.agent.grant_foreground();
            self.agent.show_overlay();
        }
        if std::mem::take(&mut self.ai_chat) {
            self.agent.grant_foreground();
            self.agent.open_ai_chat();
        }
        // Deferred until after `View::restore`: the shell restores the previous
        // session *after* `initial`, and opening here would be wiped by that
        // tab set. Keep the complete target together: deleted search results
        // need their Trash root as well as a possible matching descendant.
        let document = self.open_document.take();
        let node = self.open.take();
        let trash_root = self.trash_root.take();
        let match_text = self.match_text.take();
        if document.is_some() || node.is_some() || trash_root.is_some() {
            view.defer_search_target(document, node, trash_root, match_text);
        }
        ManagerView::Container(view)
    }

    fn duplicate(&mut self, _ws: &WorkspaceDoc) -> ManagerView {
        // A second window opens where the first one did: on a welcome tab.
        let mut view = MainView::new_with_agent(
            if self.test_upgrade {
                agent::UpdateFeed::test_upgrade()
            } else {
                self.agent.update_feed()
            },
            self.agent.clone(),
            self.search_enabled,
        );
        if let Some(proxy) = self.wake.clone() {
            view.install_reminder_wake_proxy(proxy);
        }
        ManagerView::Container(view)
    }

    /// The shell swapped the document under every view.
    ///
    /// Exactly what `initial` does to a document before a view is built over
    /// it, and nothing more: undo starts recording again. `koru-workspace-host` cannot
    /// call `prepare` itself — it does not depend on the editor — which is why
    /// this hook exists.
    fn adopt_workspace(&mut self, workspace: &mut Workspace) {
        koru_editor::prepare(workspace);
    }

    /// A document the container asked to **pop out** opens in its own window,
    /// sharing the live workspace. The container asks for this from a tab's
    /// context menu or by tearing the tab off the strip; Ctrl+N it is not.
    ///
    /// `popout_close` is the remembered close choice read from `Settings`, so a
    /// popped-out window that was told to remember skips its close dialog.
    fn open_document(&mut self, ws: &WorkspaceDoc, doc: NodeId) -> Option<ManagerView> {
        Some(ManagerView::Document(DocumentPane::open(
            ws,
            doc,
            PopoutClose::default(),
        )))
    }

    fn open_search(&mut self, _ws: &WorkspaceDoc) -> Option<ManagerView> {
        Some(ManagerView::Container(MainView::search_window(
            self.agent.update_feed(),
        )))
    }

    fn open_feedback(&mut self, _ws: &WorkspaceDoc) -> Option<ManagerView> {
        Some(ManagerView::Container(MainView::feedback_window(
            self.agent.update_feed(),
        )))
    }
}

/// The command line, `--help`, `--version` and `--backup` are all handled by
/// `koru_workspace_host::run` — see `koru_workspace_host::cli`. They belong there rather than
/// here because they are true of every binary that opens a workspace, and all
/// three used to accept nonsense silently.
fn main() {
    // A `windows`-subsystem exe starts with no console even when launched
    // *from* one, so `--help`, `--version` and the strict CLI errors would exit
    // silently -- the exact failure the parser exists to prevent. Attaching to
    // the parent console restores them; started from Explorer there is no
    // parent and the call fails, which is precisely the no-console case the
    // subsystem was set for. Must run before anything prints. Unconditional:
    // the non-Windows body is the no-op stub, which is what keeps the cfg out
    // of every caller.
    console::attach();
    // The clock `first frame` is measured against. Here rather than in
    // `koru_workspace_host::run` because the jump list and the activation pipe below
    // happen first, and a startup measurement that began after them would be
    // reporting a launch nobody has.
    koru_core::timing::start();
    // Parse once before touching the Windows activation channel. A malformed
    // command line must still reach the shell's strict error path rather than
    // being mistaken for a taskbar activation and silently handed off.
    let run = matches!(
        koru_workspace_host::cli::parse(std::env::args()),
        Ok(koru_workspace_host::cli::Command::Run)
    );
    // A replayed update launch carries one durable claim. Claim it before any
    // activation or Store work: two post-crash replays may race, and exactly
    // one is allowed to continue far enough to open a window.
    let reopen_claim = run.then(|| string_argument("--reopen-claim")).flatten();
    if let Some(token) = reopen_claim.as_deref() {
        let Some(root) =
            koru_workspace_host::cli::root_from_args().or_else(koru_channel::channel_root)
        else {
            eprintln!("Koru could not resolve the root for its reopen claim");
            return;
        };
        match reopen_claim::hold(&root, token) {
            Ok(reopen_claim::Held::ThisProcess) => {}
            Ok(reopen_claim::Held::AnotherProcess) => return,
            Err(error) => {
                eprintln!("Koru could not claim its update reopen: {error}");
                return;
            }
        }
    }
    // The Store already has a durable log, but a healthy Store cannot explain
    // why a client disconnected from it. Koru therefore owns a sibling log for
    // its side of the boundary and keeps the guard for the whole process.
    // Initialize only for a real application run: `--help`, `--version` and
    // other one-shot commands should not create a log directory as a side
    // effect of answering a question.
    let _logging = run.then(|| {
        koru_workspace_host::cli::store_root_from_args()
            .or_else(koru_channel::store_root)
            .map(|root| {
                // `vello=off`: its bitmap-glyph path logs
                // "Unsupported `output_color_type`" at ERROR for every emoji
                // whose embedded PNG is not RGBA8 — Apple Color Emoji ships
                // palette strikes — and it fires **per frame**, at caret-blink
                // rate, which is activity rather than news. The glyph is also
                // *skipped*, which is the real bug and is written up in
                // ROADMAP §Rendering; RUST_LOG still overrides for a session
                // that wants vello's side of the story.
                let logging =
                    koru_log::init_file_and_stdout(&root, "koru.log", "warn,koru=info,vello=off");
                let _ = koru_workspace::store::link::set_diagnostic_sink(store_sync_diagnostic);
                tracing::info!(
                    target: "koru::startup",
                    version = env!("CARGO_PKG_VERSION"),
                    pid = std::process::id(),
                    executable = %std::env::current_exe()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|_| "unknown".to_owned()),
                    logs = %root.join(koru_log::LOG_DIR).display(),
                    "application started"
                );
                logging
            })
    });
    // **`--no-search` means this window runs no search surface.** It used to
    // mean "open no index and start no indexing services", which is no longer
    // the app's to decide — the Koru Store indexes, and it does so whether or
    // not anybody is looking. What is left is the half a person actually wants
    // it for: a Koru that does not offer search, and does not open a store
    // session for one.
    let no_search = run && std::env::args().any(|arg| arg == "--no-search");
    let test_upgrade = run && std::env::args().any(|arg| arg == "--test-upgrade");
    let new_document = run && std::env::args().any(|arg| arg == "--new-document");
    // `--search` is how the taskbar's *Search* task asks for the global overlay.
    let search = run && std::env::args().any(|arg| arg == "--search");
    // `--ai-chat` is how the taskbar's *AI Chat* task asks for the singleton
    // standalone chat window.
    let ai_chat = run && std::env::args().any(|arg| arg == "--ai-chat");
    // `--open <id>` is how a Recent entry names its document. Read here rather
    // than in the shell because it is the taskbar's business, and the shell's
    // parser has already accepted or rejected the whole line above.
    let open = if run { open_argument() } else { None };
    let open_document = if run {
        document_argument("--open-document")
    } else {
        None
    };
    let trash_root = if run {
        node_argument("--open-trash")
    } else {
        None
    };
    let force_new_window = run && std::env::args().any(|arg| arg == "--new-window");
    let match_text = run
        .then(|| std::env::var(koru_workspace_host::startup::SEARCH_MATCH_ENV).ok())
        .flatten();

    // A Jump List entry starts the exe again. Hand the request to the Koru
    // process that already owns the workspace instead of opening a second
    // default peer over the same files. If there is no process yet, this one
    // becomes the activation server and acts on it itself.
    //
    // **Everything on this path is logged.** A jump-list launch is a
    // windows-subsystem process started from Explorer — no console — so the
    // only trace of it is `koru.log`. A click that "does nothing" is otherwise
    // undiagnosable by construction.
    tracing::info!(
        target: "koru::taskbar",
        run,
        search,
        ai_chat,
        new_document,
        open = open.is_some(),
        force_new_window,
        "taskbar launch parsed"
    );
    let request = match (search, ai_chat, new_document, open_document, open) {
        (true, _, _, _, _) => Some(taskbar::Activation::Search),
        (_, true, _, _, _) => Some(taskbar::Activation::AiChat),
        (_, _, true, _, _) => Some(taskbar::Activation::NewDocument),
        (_, _, _, Some(id), _) => Some(taskbar::Activation::OpenDocument(id)),
        (_, _, _, _, Some(id)) => Some(taskbar::Activation::Open(id)),
        _ => None,
    };
    let span = koru_core::timing::Span::start("taskbar");
    let handed_off = run && !force_new_window && taskbar::prepare(request);
    tracing::info!(
        target: "koru::taskbar",
        handed_off,
        "taskbar prepare: {}",
        if handed_off {
            "handed off to the running process; this process exits"
        } else {
            "not handed off; this process continues (cold launch or no activation)"
        }
    );
    if handed_off {
        return;
    }
    if run {
        taskbar::install_jump_list();
        // **After `prepare`, because that is what decides.** Winning the
        // activation pipe is what makes this the one process on the machine
        // that may register a machine-wide shortcut; asking any earlier would
    }
    span.stop();
    // ------------------------------------------------------------- the agent
    //
    // **Before the shell, because the store depends on it.** `koru_workspace_host::run`
    // builds `App`, and `App::new` connects to the store on the way — a store
    // that has to be running by then. Asking the agent to start one is the
    // first thing this process does that another process can act on, and it is
    // also what makes the app's own fatal text ("The store starts on demand")
    // true rather than aspirational.
    //
    // A window with no agent still opens; see [`agent`] for why that is a
    // degradation and not a failure.
    let span = koru_core::timing::Span::start("agent");
    let agent = if run {
        // **The channel root, not the store's.** The agent's endpoint is derived
        // from the root it was launched with, and that is the outer directory —
        // it puts its own `agent/` beside the store's `store/`. Handing it the
        // store root here would dial a name no agent binds.
        let root = koru_workspace_host::cli::root_from_args();
        let link = agent::AgentLink::connect(root.as_deref(), reopen_claim.clone());
        // Public Gateway discovery uses the same channel root as Agent. The
        // Gateway alone derives the Store worker's inner `store/` directory.
        let gateway_root = koru_workspace_host::cli::root_from_args();
        link.ensure_store(gateway_root.as_deref());
        // On the startup trace rather than on stdout: "there is no agent" is a
        // reduced-capability launch, not a problem to interrupt somebody with,
        // and the one line a person needs is already carried to the window by
        // `take_trouble`.
        koru_ipc::ipc_debug!(
            "agent: {}",
            if link.connected() {
                "connected"
            } else {
                "none — no global shortcut and no cross-window routing"
            }
        );
        link
    } else {
        agent::AgentLink::absent()
    };
    span.stop();
    // **The same file the Agent's overlay uses**, so which profile you are
    // searching in is one answer per machine rather than one per process.
    // Here rather than in `search.rs` because a location is a decision a
    // binary takes: this crate's own tests build search surfaces by the dozen
    // and must write nothing at all.
    if run {
        koru_search_ui::persist_choices_in(koru_window::search_state_path());
    }
    // **Nothing here discovers an update.** The controller that used to be
    // built at this point — the feed URL, the state file, the background
    // check — belongs to `koru-agent` now, which is the process that survives a
    // restart and can drive the swap. What this window keeps is the banner, and
    // it reads that off the link above.
    koru_workspace_host::run(
        Manager {
            new_document,
            search,
            ai_chat,
            open,
            open_document,
            trash_root,
            match_text,
            search_enabled: !no_search,
            support: None,
            test_upgrade,
            agent,
            wake: None,
        },
        "Koru",
    );
}

fn store_sync_diagnostic(level: koru_workspace::store::link::DiagnosticLevel, message: &str) {
    match level {
        koru_workspace::store::link::DiagnosticLevel::Info => {
            tracing::info!(target: "koru::store_sync", "{message}")
        }
        koru_workspace::store::link::DiagnosticLevel::Warn => {
            tracing::warn!(target: "koru::store_sync", "{message}")
        }
    }
}

/// The value of `--open`, in either spelling, if it parses as a node id.
///
/// An id that does not parse is **dropped rather than guessed at**: it can only
/// come from a hand-edited shortcut, and the alternative to ignoring it is
/// opening something that was not asked for.
fn open_argument() -> Option<NodeId> {
    node_argument("--open")
}

fn node_argument(name: &str) -> Option<NodeId> {
    string_argument(name).and_then(|value| NodeId::parse(&value))
}

fn document_argument(name: &str) -> Option<DocumentId> {
    string_argument(name).and_then(|value| DocumentId::parse(&value))
}

fn string_argument(name: &str) -> Option<String> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let equals = format!("{name}=");
        let value = match arg.strip_prefix(&equals) {
            Some(v) => Some(v.to_string()),
            None if arg == name => args.next(),
            None => None,
        };
        if let Some(v) = value {
            return Some(v);
        }
    }
    None
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod perf;
