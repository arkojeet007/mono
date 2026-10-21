// SPDX-License-Identifier: MPL-2.0

//! **Real `koru-agent.exe --worker delegate` children**, under a real Agent
//! on a temporary root, driving a delegated run against the checked-in
//! Codex fixture — P1.6's integration gate.
//!
//! Three shapes:
//!
//! - **The role by hand**, the way `gateway_worker_process.rs` spawns the
//!   gateway role: the strict refusals (exit 2, the role's own usage, never
//!   the Agent's help), and parent death by EOF — a valid bootstrap whose
//!   pipe is let go ends the worker cleanly even while it is still waiting
//!   for the peers it never gets.
//! - **A run to its end.** A real Agent supervises the sessions, gateway
//!   and delegate workers; `POST /v1/runs` against the fixture profile with
//!   `CODEX_FIXTURE_TURN=complete`, which ends its turn with a usage
//!   report; the run reaches *finished needing attention* (D29 — nothing
//!   registered, because the registering tools are P1.11's); the `runs.db`
//!   attempt row and the Run document agree on status and attempt; a
//!   cancel ends it; and no objective reaches the worker's log.
//! - **A killed worker is restarted within budget**, and the run it was
//!   driving reads `Interrupted` — never `Failed` — once its session is
//!   gone; **and a person resumes it**, which opens a second generation
//!   through the recorded-context fallback (§7.6) and gets a provider
//!   working on the objective again.
//!
//! Every Agent runs on its own temporary root; nothing here touches
//! `%LOCALAPPDATA%`.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use koru_agent_wire::{FRAME_VERSION, FromShell, PROTOCOL_VERSION, Request, ShellHello};
use koru_api_client::{Connection, Event};
use koru_core::NodeId;
use koru_core::node::Node;
use koru_core::schema::{Parent, WorkspaceDoc};
use koru_delegate_api::wire::{CreateRunRequest, TimelineItem};
use koru_delegate_api::{DelegatePort as _, LocalDelegate};
use koru_delegate_core::{
    ActorRef, Attention, DocumentVersionId, Objective, RunContextItem, RunId, RunStatus,
    SessionProfileKey, StepKind, TimelineRef, WorkspaceScope,
};
use koru_delegate_document::RunDocument;
use koru_delegate_sqlite::{AttemptPhase, RunsDb};

/// Text no real objective carries, so a leak is unmistakable.
const OBJECTIVE_SENTINEL: &str = "OBJECTIVE-SENTINEL write the quarterly summary";
/// What the attached page said when it was pinned, and what it says after —
/// two strings that cannot be mistaken for each other in a rendered file.
const CAPTURED_BODY: &str = "CAPTURED-BODY the paragraph as it was pinned";
const EDITED_BODY: &str = "EDITED-BODY the paragraph after the run started";
/// How long a durable write or a journal round trip is given.
const PROMPT: Duration = Duration::from_secs(30);

/// The event names of a page, in journal order.
fn names_of(rows: &[koru_events::wire::ActivityEntry]) -> Vec<&str> {
    rows.iter().map(|entry| entry.name.as_str()).collect()
}

fn position(names: &[&str], name: &str) -> Option<usize> {
    names.iter().position(|candidate| *candidate == name)
}

fn segment() -> &'static str {
    koru_channel::segment()
}

fn agent_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koru-agent"))
}

fn wait_until(what: &str, deadline: Duration, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + deadline;
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn kill_pid(pid: u32) {
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
    #[cfg(not(windows))]
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .output();
}

/// The checked-in Codex app-server fixture on a directory a host can be
/// handed on its `PATH`, laid out the way npm lays the real CLI out.
fn codex_fixture_path(root: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/codex-app-server/0.150.1/bin")
        .canonicalize()
        .expect("checked-in Codex fixture directory");
    let destination = root.join("codex-fixture-bin");
    std::fs::create_dir_all(&destination).unwrap();
    #[cfg(windows)]
    {
        std::fs::write(destination.join("codex"), "not a Windows executable").unwrap();
        std::fs::copy(source.join("codex.cmd"), destination.join("codex.cmd")).unwrap();
        std::fs::copy(
            source.join("codex-fixture.ps1"),
            destination.join("codex-fixture.ps1"),
        )
        .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let executable = destination.join("codex");
        std::fs::copy(source.join("codex"), &executable).unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
    }
    destination
}

// ------------------------------------------------------------ the refusals

struct Refusal {
    code: Option<i32>,
    stderr: String,
}

fn refusal(args: &[&str], stdin: Option<&[u8]>) -> Refusal {
    let mut child = Command::new(agent_exe())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the agent");
    {
        let mut pipe = child.stdin.take().expect("the bootstrap pipe");
        if let Some(bytes) = stdin {
            let _ = pipe.write_all(bytes);
            let _ = pipe.flush();
        }
    }
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr")
        .read_to_string(&mut stderr)
        .expect("read stderr");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Refusal {
                code: status.code(),
                stderr,
            };
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("`{args:?}` neither refused nor exited: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

const AGENT_HELP: &str = "koru-agent — the Koru Agent. It owns the device.";

#[test]
fn the_role_refuses_what_it_does_not_understand_with_exit_2() {
    let refused = refusal(&["--worker", "delegate", "--supervised"], None);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("unknown argument `--supervised`"),
        "{}",
        refused.stderr
    );
    let refused = refusal(&["--worker", "delegate", "--root", "--supervised"], None);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("--root needs a path"),
        "{}",
        refused.stderr
    );
    // The unknown-role usage now offers the third role.
    let refused = refusal(&["--worker", "frobnicate"], None);
    assert_eq!(refused.code, Some(2));
    assert!(refused.stderr.contains("delegate"), "{}", refused.stderr);
}

#[test]
fn a_missing_or_malformed_bootstrap_is_refused_before_any_agent_machinery() {
    let home = tempfile::tempdir().expect("a temporary home");
    let root = home.path().to_string_lossy().into_owned();
    let refused = refusal(&["--worker", "delegate", "--root", &root], None);
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused
            .stderr
            .contains("could not read the Agent bootstrap"),
        "{}",
        refused.stderr
    );
    assert!(
        refused
            .stderr
            .contains("usage: koru-agent --worker delegate"),
        "{}",
        refused.stderr
    );
    assert!(!refused.stderr.contains(AGENT_HELP), "{}", refused.stderr);
    assert!(!home.path().join("agent").exists());
    let refused = refusal(
        &["--worker", "delegate", "--root", &root],
        Some(b"{ not a launch bootstrap }\n"),
    );
    assert_eq!(refused.code, Some(2), "{}", refused.stderr);
    assert!(
        refused.stderr.contains("invalid Agent bootstrap"),
        "{}",
        refused.stderr
    );
}

/// **Parent death by EOF.** A valid bootstrap with no peers to reach: the
/// worker waits for the session host, the supervisor lets the pipe go, and
/// the worker stops cleanly — exit 0, nothing written under the root but
/// its own log.
#[test]
fn a_parent_death_by_eof_stops_the_worker_cleanly_while_it_waits_for_its_peers() {
    let home = tempfile::tempdir().expect("a temporary home");
    let root = home.path().join("eof");
    let store = koru_channel::store_root_under(&root);
    std::fs::create_dir_all(&store).unwrap();
    let bootstrap = koru_delegate_host::LaunchBootstrap::new(
        uuid::Uuid::new_v4().to_string(),
        segment().into(),
        root.clone(),
        store.clone(),
        true,
        uuid::Uuid::new_v4().to_string(),
        uuid::Uuid::new_v4().to_string(),
    );
    let mut line = serde_json::to_vec(&bootstrap).unwrap();
    line.push(b'\n');
    let mut child = Command::new(agent_exe())
        .args(["--worker", "delegate", "--root"])
        .arg(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the delegate worker");
    let mut pipe = child.stdin.take().expect("the bootstrap pipe");
    pipe.write_all(&line).unwrap();
    pipe.flush().unwrap();
    // Let it read the line and start waiting for the session host.
    std::thread::sleep(Duration::from_secs(2));
    assert!(
        child.try_wait().unwrap().is_none(),
        "it left before its parent did"
    );
    drop(pipe);
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "the worker did not stop on EOF");
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stderr = String::new();
    let _ = child.stderr.take().unwrap().read_to_string(&mut stderr);
    assert!(
        status.success(),
        "a parent death is a clean stop: {status} {stderr}"
    );
    assert!(!root.join("agent").exists(), "no Agent machinery ran");
}

// ------------------------------------------------------------- the fixture

/// An Agent on a root of its own, its three workers under it, stopped when
/// the test ends.
struct Agent {
    child: std::process::Child,
    root: tempfile::TempDir,
    // Last, so the slot is given back only once the stack has stopped and
    // its root is gone.
    _slot: StackSlot,
}

/// How many whole stacks this binary runs at once: half the machine.
///
/// Each one is an Agent, a store, a gateway, an AI worker, a delegate worker,
/// a session host and a PowerShell fixture, and they all start at the same
/// moment. Four of them on CI's four vCPUs starved each other forty-fold —
/// a fixture that answers `initialize` in 0.2 s here had not read its first
/// line after 7.6 s there — and the provider's 10 s request limit failed
/// four runs as `SessionCreation`. A workstation keeps its parallelism.
struct StackSlot;

static STACKS: (std::sync::Mutex<usize>, std::sync::Condvar) =
    (std::sync::Mutex::new(0), std::sync::Condvar::new());

impl StackSlot {
    fn acquire() -> Self {
        let limit = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(1));
        let (running, freed) = &STACKS;
        let mut running = running.lock().unwrap_or_else(|held| held.into_inner());
        while *running >= limit {
            running = freed.wait(running).unwrap_or_else(|held| held.into_inner());
        }
        *running += 1;
        Self
    }
}

impl Drop for StackSlot {
    fn drop(&mut self) {
        let (running, freed) = &STACKS;
        *running.lock().unwrap_or_else(|held| held.into_inner()) -= 1;
        freed.notify_one();
    }
}

impl Agent {
    fn start(complete_turns: bool) -> Self {
        Self::with_turn(if complete_turns {
            Some("complete")
        } else {
            None
        })
    }

    /// The fixture that registers a result through the run's own MCP
    /// endpoint before it ends its turn — a delegated agent doing the one
    /// thing the whole milestone is for.
    fn registering() -> Self {
        Self::with_turn(Some("register"))
    }

    fn with_turn(turn: Option<&str>) -> Self {
        Self::with_root(turn, tempfile::tempdir().expect("a temp root"))
    }

    fn with_root(turn: Option<&str>, root: tempfile::TempDir) -> Self {
        Self::with_runtime_path(turn, root, None)
    }

    fn with_runtime_path(
        turn: Option<&str>,
        root: tempfile::TempDir,
        live_path: Option<std::ffi::OsString>,
    ) -> Self {
        let live = live_path.is_some();
        // **Where a temporary run does its work, chosen under this root**
        // (ADR-365). The default is the machine's own temporary directory,
        // which a rooted test must not scatter itself through — and writing
        // the setting is also how this test proves the worker reads it.
        // `--root` is explicit here, so the settings live under the root and
        // the ordinary channel's are untouched.
        let runs_base = root.path().join("delegate-runs");
        let settings = root.path().join("settings");
        std::fs::create_dir_all(&settings).expect("a settings directory");
        std::fs::write(
            settings.join("desktop-v1.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "delegate_temporary_root": runs_base.to_string_lossy(),
            }))
            .expect("a settings file"),
        )
        .expect("write the settings file");
        let path = live_path.unwrap_or_else(|| {
            let provider_path = codex_fixture_path(root.path());
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            std::env::join_paths(
                std::iter::once(provider_path).chain(std::env::split_paths(&inherited)),
            )
            .unwrap()
        });
        let mut command = Command::new(agent_exe());
        command
            .arg("--root")
            .arg(root.path())
            .arg("--console")
            .stdin(Stdio::null())
            .env("PATH", path)
            .env("KORU_STORE_NO_DIALOG", "1")
            .env("KORU_IROH_ISOLATED", "1")
            .env("KORU_DELEGATE_DEBUG", "1")
            .env(
                "CODEX_FIXTURE_TRACE",
                root.path().join("codex-fixture.trace"),
            );
        let fixture_clock = root.path().join("delegate-process-test-clock");
        if fixture_clock.is_file() {
            command
                .env("DELEGATE_PROCESS_TEST_CLOCK", fixture_clock)
                .env(
                    "DELEGATE_PROCESS_TEST_WAKE_AUTHORITY",
                    "delegate-process-test",
                );
        }
        if live {
            // Live provider output remains private, including on test failure.
            command
                .current_dir(root.path())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .env_remove("CODEX_FIXTURE_TRACE")
                .env_remove("KORU_DELEGATE_DEBUG");
        }
        match turn {
            Some(mode) => command.env("CODEX_FIXTURE_TURN", mode),
            None => command.env_remove("CODEX_FIXTURE_TURN"),
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000);
        }
        let _slot = StackSlot::acquire();
        let child = command.spawn().expect("the agent starts");
        let agent = Self { child, root, _slot };
        let endpoint = agent.endpoint();
        wait_until("the agent to accept", Duration::from_secs(30), || {
            koru_ipc::connect(&endpoint).is_ok()
        });
        agent
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    /// The base this stack was told to make a temporary run's folder inside.
    fn delegate_runs_base(&self) -> PathBuf {
        self.root().join("delegate-runs")
    }

    fn store_root(&self) -> PathBuf {
        koru_channel::store_root_under(self.root())
    }

    fn endpoint(&self) -> koru_ipc::Endpoint {
        koru_ipc::Endpoint::for_service_root(koru_ipc::AGENT, segment(), self.root())
            .expect("a rooted endpoint")
    }

    fn delegate(&self) -> LocalDelegate {
        LocalDelegate::new(
            koru_delegate_host::endpoint(segment(), Some(self.root())).unwrap(),
            "koru-delegate-process-test",
        )
    }

    /// The gateway this Agent supervises, once it publishes.
    fn gateway(&self) -> koru_api_client::Client {
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match koru_api_client::Client::connect_at_with_scopes(segment(), self.root(), &[]) {
                Ok(client) => return client,
                Err(error) => {
                    assert!(
                        Instant::now() < deadline,
                        "the gateway did not publish: {error}"
                    );
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    fn runs_db(&self) -> RunsDb {
        RunsDb::open(&koru_delegate_sqlite::runs_db_path(&self.store_root())).unwrap()
    }

    /// Every line the delegate worker logged, for the privacy assertion.
    fn delegate_log(&self) -> String {
        self.log("koru-delegate.log")
    }

    fn log(&self, prefix: &str) -> String {
        let logs = self.store_root().join("logs");
        let mut text = String::new();
        if let Ok(entries) = std::fs::read_dir(&logs) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(prefix) {
                    text.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
                }
            }
        }
        text
    }

    /// Run `body`, and if it panics, say what the three workers logged
    /// before the panic goes on — the roots are gone once the test ends.
    fn diagnosed(&self, body: impl FnOnce() + std::panic::UnwindSafe) {
        if let Err(panic) = std::panic::catch_unwind(body) {
            for prefix in [
                "koru-delegate.log",
                "koru-api-gateway.log",
                "koru-session-host.log",
            ] {
                let text = self.log(prefix);
                let tail: Vec<&str> = text.lines().rev().take(80).collect();
                eprintln!("---- {prefix} (last {} lines) ----", tail.len());
                for line in tail.into_iter().rev() {
                    eprintln!("{line}");
                }
            }
            // What the provider itself saw, with the fixture's clock lines:
            // a start that times out says here whether PowerShell was slow
            // to run or the request was slow to arrive.
            let trace = std::fs::read_to_string(self.root().join("codex-fixture.trace"))
                .unwrap_or_else(|_| "(the fixture was never invoked)".into());
            eprintln!("---- codex-fixture.trace ----");
            eprintln!("{trace}");
            std::panic::resume_unwind(panic);
        }
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        if let Ok(stream) = koru_ipc::connect_timed(&self.endpoint(), Duration::from_secs(2)) {
            let mut write = &stream;
            let _ = koru_ipc::frame::write_frame(
                &mut write,
                FRAME_VERSION,
                &ShellHello {
                    product: "koru-delegate-process-test".into(),
                    version: "0".into(),
                    protocol_version: PROTOCOL_VERSION,
                },
            );
            let mut read = &stream;
            let _ = koru_ipc::frame::read_raw(&mut read);
            let _ = koru_ipc::frame::write_frame(
                &mut write,
                FRAME_VERSION,
                &FromShell::Request {
                    id: 1,
                    body: Request::Shutdown,
                },
            );
            let _ = write.flush();
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A workspace to register the run in, and the person asking.
fn workspace(
    client: &koru_api_client::Client,
) -> (
    koru_delegate_core::ProfileId,
    koru_delegate_core::WorkspaceId,
) {
    let profile = client
        .store()
        .create_profile("Ada", None, None, Some("en"))
        .expect("a profile");
    let workspace = client
        .store()
        .create_workspace(profile.id, "Notes")
        .expect("a workspace");
    (profile.id, workspace.id)
}

fn request(
    profile: koru_delegate_core::ProfileId,
    workspace: koru_delegate_core::WorkspaceId,
    context: Vec<RunContextItem>,
) -> CreateRunRequest {
    CreateRunRequest {
        run: RunId::mint(),
        workspace,
        requested_by: ActorRef::Person { profile },
        objective: Objective::new(OBJECTIVE_SENTINEL).unwrap(),
        agent: SessionProfileKey::BuiltIn("codex".parse().unwrap()),
        requested_model: None,
        scope: WorkspaceScope::Temporary,
        context,
        grants: std::collections::BTreeSet::from([workspace]),
    }
}

/// A page with a title and one paragraph, imported the way a document
/// reaches the store, and the version a person attaching it would have
/// pinned (§7.2 step 2 happens on the delegating surface).
fn a_pinned_page(
    client: &koru_api_client::Client,
    workspace: koru_delegate_core::WorkspaceId,
    body: &str,
) -> (koru_delegate_core::DocumentId, DocumentVersionId, NodeId) {
    let mut page = WorkspaceDoc::create(koru_core::WorkspaceId::new(), "Notes").unwrap();
    let root = page
        .insert(Parent::Root, Node::document("Attached").unwrap())
        .unwrap();
    let line = page
        .insert(Parent::Node(root), Node::paragraph(body).unwrap())
        .unwrap();
    let document = client
        .store()
        .import_document(
            workspace,
            "Attached",
            "page",
            page.export_snapshot().unwrap(),
        )
        .expect("the page is imported");
    let entry = client
        .store()
        .capture_history(
            workspace,
            document,
            &koru_store_client::http::CaptureHistory {
                reason: "manual".into(),
                frontiers: None,
                label: None,
                message: None,
                boundary: None,
            },
        )
        .expect("the version is captured");
    (document, entry.version, line)
}

/// Rewrite the page's paragraph over the sync socket — an edit by a person
/// at a window, made durable before this returns.
fn edit_page(
    client: &koru_api_client::Client,
    workspace: koru_delegate_core::WorkspaceId,
    document: koru_delegate_core::DocumentId,
    line: NodeId,
    body: &str,
) {
    let snapshot = client.store().snapshot(workspace, document).unwrap();
    let mut page = WorkspaceDoc::from_snapshot(&snapshot).unwrap();
    let sent = page.version();
    page.set_text(line, body).unwrap();
    let room = format!("document:{}", document.to_storage());
    let mut connection = Connection::open(&client.store().discovery()).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    connection.join(&room, &sent).unwrap();
    let batch = connection
        .send_update(&room, page.export_updates_since(&sent).unwrap())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(Some(Event::Acked { ack, .. })) = connection.next_event()
            && ack.batch == batch
        {
            assert!(ack.durable, "{:?}", ack.status);
            return;
        }
        assert!(Instant::now() < deadline, "the edit was never acknowledged");
    }
}

/// One run's `delegate.*` rows, in journal order.
///
/// A Run document is a document, so the Store's own `document.created`
/// lands on this subject too, in a chain of its own. The run's chain is
/// the `delegate.*` rows, and those are what §7.5 is about.
fn activity(
    client: &koru_api_client::Client,
    document: koru_delegate_core::DocumentId,
) -> Vec<koru_events::wire::ActivityEntry> {
    client
        .events()
        .activity(
            koru_events::SubjectKind::Document,
            &document.to_storage(),
            None,
            200,
        )
        .expect("the activity read")
        .entries
        .into_iter()
        .filter(|entry| entry.name.as_str().starts_with("delegate."))
        .collect()
}

/// Wait for the delegate worker to answer `/v1/runs`: it starts after the
/// gateway and waits for it.
fn wait_for_runs(client: &koru_api_client::Client, workspace: koru_delegate_core::WorkspaceId) {
    wait_until(
        "the delegate worker to serve",
        Duration::from_secs(150),
        || client.runs().list(workspace).is_ok(),
    );
}

// --------------------------------------------------------- a run to its end

#[test]
fn a_run_is_driven_to_its_end_and_the_ledger_and_the_document_agree() {
    let agent = Agent::start(true);
    let agent_ref = &agent;
    agent.diagnosed(std::panic::AssertUnwindSafe(|| run_to_its_end(agent_ref)));
}

fn run_to_its_end(agent: &Agent) {
    let client = agent.gateway();
    let (profile, workspace) = workspace(&client);
    wait_for_runs(&client, workspace);
    assert!(
        client.runs().available(),
        "the delegate capability is published"
    );

    // UC2: a document is attached, already pinned to the version the
    // person was looking at.
    let (attached, pinned, line) = a_pinned_page(&client, workspace, CAPTURED_BODY);
    let request = request(
        profile,
        workspace,
        vec![RunContextItem::Document {
            workspace,
            document: attached,
            version: pinned,
        }],
    );
    let run = request.run;
    let created = client.runs().create(&request).expect("202");
    assert_eq!(created.run, run);
    // The source moves on the moment the request is durable. What the run
    // was given is a version, and a version cannot move.
    edit_page(&client, workspace, attached, line, EDITED_BODY);
    // Durable before the answer: the ledger names the document.
    let intent = agent.runs_db().intent(run).unwrap().expect("an intent");
    assert_eq!(intent.run_document, Some(created.document));
    assert_eq!(intent.state, koru_delegate_sqlite::IntentState::Satisfied);

    // The fixture completes its turn with a usage report; nothing was
    // registered, so the run is finished needing attention (D29).
    let mut view = None;
    wait_until("the turn to complete", Duration::from_secs(90), || {
        let latest = client.runs().run(run, None, 0, 500).expect("the run");
        let done = matches!(
            &latest.header.status,
            RunStatus::Running { attention, .. } if attention.contains(Attention::FinishedNeedingAttention)
        );
        assert!(
            !matches!(latest.header.status, RunStatus::Failed { .. }),
            "{:?}",
            latest.header.status
        );
        view = Some(latest);
        done
    });
    let view = view.unwrap();
    let steps: Vec<StepKind> = view
        .timeline
        .items
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Step(entry) => Some(entry.kind),
            _ => None,
        })
        .collect();
    assert_eq!(steps[0], StepKind::RunCreated, "{steps:?}");
    assert_eq!(steps[1], StepKind::Started, "{steps:?}");
    assert!(
        steps
            .iter()
            .any(|kind| matches!(kind, StepKind::TurnStarted { .. })),
        "{steps:?}"
    );
    assert!(
        steps.iter().any(|kind| matches!(
            kind,
            StepKind::TurnCompleted {
                artifacts_registered: 0,
                ..
            }
        )),
        "{steps:?}"
    );
    let usage: Vec<_> = view
        .timeline
        .items
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Usage(fact) => Some(fact),
            _ => None,
        })
        .collect();
    assert_eq!(view.header.context.len(), 1, "the pin rode the header");

    // §7.2 step 3: the pinned document is on disk under the run's own
    // working folder, rendered from the **captured** version — the edit
    // above is in the store and not in the file.
    // **Under the base this stack chose**, not under the channel root
    // (ADR-365). Asserted against what the run itself reported as well as
    // against the arithmetic, so a change to either is caught by the other.
    let run_workspace = agent
        .delegate_runs_base()
        .join(koru_settings::RUNS_FOLDER)
        .join(run.to_storage())
        .join("workspace");
    // The Agent canonicalizes its rooted launch path before it hands the
    // setting to the child. On Windows that preserves the target while adding
    // the verbatim `\\?\` spelling, so compare with the same authority path
    // rather than with `TempDir`'s lexical spelling.
    let reported_run_workspace = run_workspace
        .canonicalize()
        .expect("the materialized run workspace has a canonical path");
    assert_eq!(
        view.execution.run_workspace.as_deref(),
        Some(reported_run_workspace.to_string_lossy().as_ref()),
        "the run works in the folder the setting named"
    );
    let context = koru_delegate_context::layout::run_dir(&run_workspace, run);
    let rendered = std::fs::read_to_string(context.join(
        koru_delegate_context::layout::document_file(attached, pinned),
    ))
    .expect("the pinned document was materialized");
    assert!(rendered.contains(CAPTURED_BODY), "{rendered}");
    assert!(
        !rendered.contains(EDITED_BODY),
        "the run was handed the live document, not the version it was given: {rendered}"
    );
    assert!(
        context
            .join(koru_delegate_context::layout::MANIFEST_FILE)
            .exists(),
        "the manifest names what was delivered"
    );
    // The store did move on, so the two really are different bytes.
    let live = WorkspaceDoc::from_snapshot(&client.store().snapshot(workspace, attached).unwrap())
        .unwrap();
    assert_eq!(
        live.read(line).unwrap().content.as_text(),
        Some(EDITED_BODY)
    );

    // §7.5 on the bus: the run's rows are addressed at the Run document,
    // in order, and every one of them shares the chain the `created` row
    // opened.
    let mut rows = Vec::new();
    wait_until("the run's events to reach the journal", PROMPT, || {
        rows = activity(&client, created.document);
        names_of(&rows).contains(&"delegate.turn.completed")
    });
    let names = names_of(&rows);
    assert_eq!(names.first(), Some(&"delegate.run.created"), "{names:?}");
    for pair in [
        ("delegate.run.created", "delegate.run.started"),
        ("delegate.run.started", "delegate.turn.started"),
        ("delegate.turn.started", "delegate.turn.completed"),
    ] {
        let (earlier, later) = pair;
        assert!(
            position(&names, earlier) < position(&names, later),
            "{earlier} did not precede {later}: {names:?}"
        );
    }
    assert!(names.contains(&"delegate.turn.usage"), "{names:?}");
    let chain: std::collections::BTreeSet<_> =
        rows.iter().map(|entry| entry.correlation_id).collect();
    assert_eq!(chain.len(), 1, "the rows left the chain: {names:?}");

    assert_eq!(usage.len(), 1, "the fixture reported once");
    assert_eq!(usage[0].input, Some(120));
    assert_eq!(usage[0].output, Some(40));
    assert_eq!(view.header.economics.input.complete(), Some(120));
    assert_eq!(view.header.economics.orchestrator_turns, 1);
    assert_eq!(
        view.header.executing_device(),
        Some(view.header.executing_device().unwrap())
    );

    // The run row and the Run document agree on status and attempt.
    let row = agent
        .runs_db()
        .current_attempt(run)
        .unwrap()
        .expect("an attempt");
    assert_eq!(row.phase, AttemptPhase::Running);
    assert_eq!(row.generation, koru_delegate_core::AttemptGeneration::FIRST);
    assert!(!row.retired);
    assert!(row.provider_session.and_then(|key| key.session).is_some());
    let snapshot = client
        .store()
        .snapshot(workspace, created.document)
        .unwrap();
    let document = RunDocument::from_snapshot(&snapshot).unwrap();
    let header = document.header().unwrap();
    assert_eq!(header.status, view.header.status);
    assert_eq!(header.run, run);
    let started = document
        .timeline_all()
        .into_iter()
        .find_map(|item| match item {
            koru_delegate_document::TimelineItem::Step(entry)
                if entry.kind == StepKind::Started =>
            {
                Some(entry)
            }
            _ => None,
        })
        .expect("the Started step");
    assert!(started.refs.iter().any(|reference| matches!(
        reference,
        TimelineRef::Attempt { attempt } if *attempt == row.attempt
    )));
    // The stream replays the history from the start — first straight from
    // the worker over the local transport, then through the gateway.
    let mut direct = agent
        .delegate()
        .stream(
            koru_delegate_api::RequestContext {
                principal: koru_delegate_api::DelegatePrincipal {
                    label: "test".into(),
                    scopes: vec![koru_delegate_api::DelegateScope::Read],
                },
                trace_id: "t".into(),
            },
            run,
            None,
            None,
        )
        .expect("the worker streams");
    let first = koru_delegate_api::RunStream::next(&mut *direct).expect("a first item");
    assert!(
        matches!(
            first,
            koru_delegate_api::wire::RunStreamItem::Entry { index: 0, .. }
        ),
        "{first:?}"
    );
    drop(direct);
    let (send, receive) = std::sync::mpsc::channel();
    let subscription = client
        .runs()
        .stream(run, None, None, move |item| {
            let _ = send.send(item);
        })
        .unwrap();
    let first = receive
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            first,
            koru_delegate_api::wire::RunStreamItem::Entry { index: 0, .. }
        ),
        "{first:?}"
    );
    subscription.stop();

    // Listed under the workspace, and cancel ends it.
    let listed = client.runs().list(workspace).unwrap();
    assert!(listed.runs.iter().any(|header| header.run == run));
    let cancelled = client.runs().cancel(run).unwrap();
    assert_eq!(cancelled.header.status, RunStatus::Cancelled);
    // The terminal row closes the same chain. `delegate.run.completed` is
    // not reachable here: nothing registered a result, so D29 leaves the
    // run finished-needing-attention until P1.11's tools exist, and the
    // cancel is what ends it.
    wait_until("the terminal row to reach the journal", PROMPT, || {
        names_of(&activity(&client, created.document)).contains(&"delegate.run.cancelled")
    });
    let rows = activity(&client, created.document);
    let names = names_of(&rows);
    assert_eq!(names.last(), Some(&"delegate.run.cancelled"), "{names:?}");
    assert_eq!(
        rows.iter()
            .map(|entry| entry.correlation_id)
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        1,
        "{names:?}"
    );
    wait_until(
        "the ledger to retire the attempt",
        Duration::from_secs(10),
        || {
            agent
                .runs_db()
                .current_attempt(run)
                .unwrap()
                .is_some_and(|row| row.phase == AttemptPhase::Ended && row.retired)
        },
    );
    let snapshot = client
        .store()
        .snapshot(workspace, created.document)
        .unwrap();
    assert_eq!(
        RunDocument::from_snapshot(&snapshot)
            .unwrap()
            .header()
            .unwrap()
            .status,
        RunStatus::Cancelled
    );

    // Privacy: the objective reached the provider and nothing else.
    let log = agent.delegate_log();
    assert!(!log.is_empty(), "the worker logged");
    assert!(
        !log.contains(OBJECTIVE_SENTINEL),
        "the objective leaked into the log"
    );
    assert!(
        !log.contains(&agent.root().to_string_lossy().to_string()),
        "a path leaked into the log"
    );
    let trace =
        std::fs::read_to_string(agent.root().join("codex-fixture.trace")).unwrap_or_default();
    assert!(
        trace.contains("args:app-server"),
        "the provider was launched: {trace}"
    );
    // `koru-agent status` names the role beside the gateway.
    let status = Command::new(agent_exe())
        .args(["--root"])
        .arg(agent.root())
        .arg("status")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&status.stdout);
    assert!(stdout.contains("delegate:  serving"), "{stdout}");
}

// ------------------------------------------- a run that registers a result

/// **The assertion the milestone is for.** The provider is handed a real
/// endpoint and a real bearer, calls `create` at it through the run's own
/// MCP server, and the run reaches `Completed` with the result in its
/// header — not D29's finished-needing-attention, which is all a stubbed
/// endpoint could ever produce.
#[test]
fn a_delegated_run_registers_a_result_through_its_endpoint_and_completes() {
    let agent = Agent::registering();
    let agent_ref = &agent;
    agent.diagnosed(std::panic::AssertUnwindSafe(|| {
        registers_a_result(agent_ref)
    }));
}

fn registers_a_result(agent: &Agent) {
    let client = agent.gateway();
    let (profile, workspace) = workspace(&client);
    wait_for_runs(&client, workspace);

    let request = request(profile, workspace, Vec::new());
    let run = request.run;
    let created = client.runs().create(&request).expect("202");

    let mut view = None;
    wait_until("the run to complete", Duration::from_secs(90), || {
        let latest = client.runs().run(run, None, 0, 500).expect("the run");
        assert!(
            !matches!(latest.header.status, RunStatus::Failed { .. }),
            "{:?}",
            latest.header.status
        );
        let done = latest.header.status == RunStatus::Completed;
        view = Some(latest);
        done
    });
    let view = view.unwrap();

    // The result is the run's, by identity and by size — the bytes the
    // fixture wrote, as the blob store weighed them.
    assert_eq!(
        view.header.artifacts.len(),
        1,
        "{:?}",
        view.header.artifacts
    );
    let result = &view.header.artifacts[0];
    assert_eq!(result.title.as_str(), "Fixture result");
    assert_eq!(result.size_bytes, "FIXTURE_RESULT".len() as u64);

    let steps: Vec<StepKind> = view
        .timeline
        .items
        .iter()
        .filter_map(|item| match item {
            TimelineItem::Step(entry) => Some(entry.kind),
            _ => None,
        })
        .collect();
    assert!(steps.contains(&StepKind::ArtifactRegistered), "{steps:?}");
    assert!(
        steps.iter().any(|kind| matches!(
            kind,
            StepKind::TurnCompleted {
                artifacts_registered: 1,
                ..
            }
        )),
        "{steps:?}"
    );
    assert!(steps.contains(&StepKind::Completed), "{steps:?}");

    // The version really is in the Artifact Store, under this run.
    let registered = client.artifacts().run_versions(run).expect("the versions");
    assert_eq!(registered.versions.len(), 1);
    assert_eq!(registered.versions[0].id, result.artifact);
    assert_eq!(
        registered.versions[0]
            .produced_by
            .as_ref()
            .map(|produced| produced.run),
        Some(run),
        "provenance comes from the authority record, never from the call"
    );

    // §7.5 on the bus: the run completes as a row of its own, in the chain
    // its creation opened. (`delegate.artifact.registered` is still not
    // published — see `events::NoRow::ArtifactRegistered`: the row names a
    // family, an ordinal and a provenance flag, and none of the three is on
    // a `ResultReference`.)
    let mut names = Vec::new();
    wait_until("the run's rows to reach the journal", PROMPT, || {
        let rows = activity(&client, created.document);
        names = names_of(&rows)
            .iter()
            .map(|name| name.to_string())
            .collect();
        names.iter().any(|name| name == "delegate.run.completed")
    });
    assert_eq!(
        names.first().map(String::as_str),
        Some("delegate.run.created"),
        "{names:?}"
    );

    // The run ended, so its endpoint is closed: the record every later call
    // reads says revoked, which is what stops a provider that outlived the
    // worker from publishing again.
    wait_until("the authority to be revoked", PROMPT, || {
        client
            .artifacts()
            .run_authority(run)
            .is_ok_and(|view| view.state == koru_artifact_wire::authority::AuthorityState::Revoked)
    });

    // The provider reached the endpoint with the bearer it was given, and
    // the bearer was never on its command line.
    let trace =
        std::fs::read_to_string(agent.root().join("codex-fixture.trace")).unwrap_or_default();
    assert!(trace.contains("register:ok"), "{trace}");
    assert!(trace.contains("bearer_env:set"), "{trace}");
    for line in trace.lines().filter(|line| line.starts_with("args:")) {
        assert!(
            !line.contains("koru_run_"),
            "the run's bearer was rendered on argv: {line}"
        );
    }

    // Privacy holds through the new path: no objective, no path, no token.
    let log = agent.delegate_log();
    assert!(!log.contains(OBJECTIVE_SENTINEL), "the objective leaked");
    assert!(
        !log.contains("koru_run_"),
        "a run bearer leaked into the log"
    );
    assert!(
        !log.contains(&agent.root().to_string_lossy().to_string()),
        "a path leaked into the log"
    );
}

// ------------------------------------------------- the killed worker

#[test]
fn a_killed_worker_is_restarted_within_budget_and_the_run_reads_interrupted_never_failed() {
    let agent = Agent::start(false);
    let agent_ref = &agent;
    agent.diagnosed(std::panic::AssertUnwindSafe(|| killed_worker(agent_ref)));
}

fn killed_worker(agent: &Agent) {
    let client = agent.gateway();
    let (profile, workspace) = workspace(&client);
    wait_for_runs(&client, workspace);
    let request = request(profile, workspace, Vec::new());
    let run = request.run;
    client.runs().create(&request).expect("202");
    // The fixture asks for an approval and waits: the run is running with
    // the marker, the attempt is live, the session is up.
    wait_until("the approval to be asked", Duration::from_secs(90), || {
        matches!(
            client.runs().run(run, None, 0, 10).expect("the run").header.status,
            RunStatus::Running { attention, .. } if attention.contains(Attention::ApprovalRequired)
        )
    });
    let before = agent.delegate().hello().expect("the worker answers");
    assert_eq!(before.active_runs, 1);
    let row = agent.runs_db().current_attempt(run).unwrap().unwrap();
    let session = row
        .provider_session
        .and_then(|key| key.session)
        .expect("a session");

    // Kill the worker, and end its provider behind its back: the fixture
    // traces its own pid. Whether the restarted worker finds the session
    // known-ended and reconciles, or finds it live, reattaches and then
    // sees it exit, the run ends the same way.
    kill_pid(before.pid);
    let trace = std::fs::read_to_string(agent.root().join("codex-fixture.trace")).unwrap();
    let provider_pid: u32 = trace
        .lines()
        .filter_map(|line| line.strip_prefix("pid:"))
        .last()
        .expect("the fixture traced its pid")
        .trim()
        .parse()
        .expect("a pid");
    kill_pid(provider_pid);
    let _ = session;

    // The supervisor puts a fresh child back within its budget.
    wait_until(
        "the worker to be restarted",
        Duration::from_secs(90),
        || {
            agent
                .delegate()
                .hello()
                .is_ok_and(|hello| hello.launch_id != before.launch_id && hello.pid != before.pid)
        },
    );
    // And the run it was driving reads Interrupted — never Failed.
    wait_until(
        "the run to read interrupted",
        Duration::from_secs(90),
        || {
            let header = client.runs().run(run, None, 0, 10).expect("the run").header;
            assert!(
                !matches!(header.status, RunStatus::Failed { .. }),
                "{:?}",
                header.status
            );
            matches!(header.status, RunStatus::Interrupted { .. })
        },
    );
    let row = agent.runs_db().current_attempt(run).unwrap().unwrap();
    assert!(
        matches!(row.phase, AttemptPhase::Interrupted | AttemptPhase::Ended),
        "{:?}",
        row.phase
    );
    let steps: Vec<StepKind> = client
        .runs()
        .run(run, None, 0, 500)
        .unwrap()
        .timeline
        .items
        .into_iter()
        .filter_map(|item| match item {
            TimelineItem::Step(entry) => Some(entry.kind),
            _ => None,
        })
        .collect();
    assert!(
        steps
            .iter()
            .any(|kind| matches!(kind, StepKind::Interrupted { .. })),
        "{steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|kind| matches!(kind, StepKind::Failed { .. })),
        "{steps:?}"
    );
    resumed_after_the_interruption(agent, &client, run);
}

/// §7.6 through the API: a person resumes the interrupted run.
///
/// The old attempt's creation key is retired at the host before the ledger
/// opens the next generation, the fresh attempt is generation two, and the
/// timeline says which seam it took — `recorded_context`, because the Codex
/// adapter declares `ResumeSupport::Unsupported` and no token exists to
/// continue the dead provider session with.
fn resumed_after_the_interruption(agent: &Agent, client: &koru_api_client::Client, run: RunId) {
    let first = agent.runs_db().current_attempt(run).unwrap().unwrap();
    let resumed = {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match client.runs().resume(run) {
                Ok(resumed) => break resumed,
                Err(failure) => {
                    assert!(
                        Instant::now() < deadline,
                        "the resume was refused: {failure}"
                    );
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }
    };
    assert!(
        matches!(
            resumed.header.status,
            RunStatus::Starting | RunStatus::Running { .. }
        ),
        "{:?}",
        resumed.header.status
    );
    wait_until(
        "the fresh attempt to be running",
        Duration::from_secs(90),
        || {
            let header = client.runs().run(run, None, 0, 10).expect("the run").header;
            assert!(
                !matches!(header.status, RunStatus::Failed { .. }),
                "{:?}",
                header.status
            );
            matches!(header.status, RunStatus::Running { .. })
        },
    );
    let second = agent.runs_db().current_attempt(run).unwrap().unwrap();
    assert_eq!(second.generation, first.generation.next());
    assert_ne!(
        second.attempt, first.attempt,
        "a fresh attempt, not continuity"
    );
    assert_ne!(
        second.provider_session.map(|key| key.idempotency),
        first.provider_session.map(|key| key.idempotency),
        "a fresh creation key; the old one is retired"
    );
    let steps: Vec<StepKind> = client
        .runs()
        .run(run, None, 0, 500)
        .unwrap()
        .timeline
        .items
        .into_iter()
        .filter_map(|item| match item {
            TimelineItem::Step(entry) => Some(entry.kind),
            _ => None,
        })
        .collect();
    assert!(
        steps.contains(&StepKind::Resumed {
            via: koru_delegate_core::ResumeSeam::RecordedContext
        }),
        "{steps:?}"
    );
    assert!(
        !steps
            .iter()
            .any(|kind| matches!(kind, StepKind::Failed { .. })),
        "{steps:?}"
    );
    // The objective still reaches the provider and nothing else.
    assert!(!agent.delegate_log().contains(OBJECTIVE_SENTINEL));
}

#[path = "delegate_worker_process/claude_live.rs"]
mod claude_live;

#[path = "delegate_worker_process/live_generic.rs"]
mod live_generic;

#[path = "delegate_worker_process/schedules.rs"]
mod schedules;
