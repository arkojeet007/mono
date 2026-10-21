// SPDX-License-Identifier: MPL-2.0

//! **The gate.** Real agents, real sockets, driven by hand over `koru-ipc`.
//!
//! What this asserts is what a shell would see, because it speaks the same
//! protocol the same way: frames over the endpoint, a `ShellHello` first, and
//! nothing privileged. Nothing here reaches inside the agent.
//!
//! ## Every agent runs on its own `--root`, and that is not tidiness
//!
//! An explicit root turns off **every machine-wide thing**: the hotkey is not
//! registered, no tray icon is created, start-on-login is refused, and
//! `device.json` and the desktop preferences are redirected under the root. A
//! suite without that would take Ctrl+Shift+K off whoever ran it, put icons in
//! their notification area, and fight over one registry value. Three of those
//! are asserted below, because *"it did not touch my machine"* is not something
//! anybody notices passing.
//!
//! The root also reaches the endpoint name — `Endpoint::for_service_root`
//! appends a digest of it — so several agents run at once without meeting, and
//! none of them is the one the developer has open.
//!
//! ## The supervised gateway is this same executable
//!
//! Since Plan 4 the Agent supervises `koru-agent.exe --worker gateway`, so
//! there is no sibling to find and no case here that can be skipped for a
//! missing one: the image under test hosts the role. What a case still needs
//! is patience — a real child opens Store, AI and events before it publishes.
//!
//! ## What it asserts on
//!
//! **What the agent gave back, never what the test sent.** Those are different
//! sets, and a suite that asserted on what it sent would either fail against a
//! correct agent or be relaxed until it asserted nothing. The store's own gate
//! carries the same rule and it is worth restating.
//!
//! **Never that a connection fails.** "No agent is running under `cargo test`"
//! passes on CI and fails on the machine of anyone with Koru open. Every case
//! arms the state it means to test.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use koru_agent_wire::{
    AgentGreeting, FRAME_VERSION, FromAgent, FromShell, PROTOCOL_VERSION, Push, Request, Response,
    ShellHello, Target,
};
use koru_session_protocol::{
    BuiltInProfileKey, CapabilitySet, GridSize, IdempotencyKey, ProfileKind, RawCommand,
    SessionCommand, SessionId, SessionMode, SessionProfileKey, ViewportGeneration,
};

/// The channel this test binary is, asked rather than written down so
/// `cargo test --release` moves the agent it spawns with it.
fn segment() -> &'static str {
    koru_channel::segment()
}

fn agent_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_koru-agent"))
}

fn codex_fixture_path(root: &Path) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/codex-app-server/0.150.1/bin")
        .canonicalize()
        .expect("checked-in Codex fixture directory");
    #[cfg(windows)]
    {
        let destination = root.join("codex-fixture-bin");
        std::fs::create_dir_all(&destination).unwrap();
        std::fs::copy(source.join("codex.cmd"), destination.join("codex.cmd")).unwrap();
        std::fs::copy(
            source.join("codex-fixture.ps1"),
            destination.join("codex-fixture.ps1"),
        )
        .unwrap();
        destination
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let destination = root.join("codex-fixture-bin");
        std::fs::create_dir_all(&destination).unwrap();
        let executable = destination.join("codex");
        std::fs::copy(source.join("codex"), &executable).unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&executable, permissions).unwrap();
        destination
    }
}

fn session_client(
    endpoint: &koru_ipc::Endpoint,
    product: &str,
) -> koru_session_client::SessionClient {
    use koru_session_protocol::{
        CapabilityName, CapabilitySet, MAX_PROTOCOL_VERSION, MIN_PROTOCOL_VERSION, ProtocolOffer,
        ProtocolRange, capability,
    };
    use std::str::FromStr;

    let capabilities = [
        capability::CONTROLLER_LEASES,
        capability::RENDER_DELTAS,
        capability::TERMINATE,
    ]
    .into_iter()
    .map(|name| CapabilityName::from_str(name).unwrap());
    koru_session_client::SessionClient::connect(
        endpoint,
        product,
        ProtocolOffer {
            versions: ProtocolRange {
                min: MIN_PROTOCOL_VERSION,
                max: MAX_PROTOCOL_VERSION,
            },
            capabilities: CapabilitySet::new(capabilities),
        },
    )
    .unwrap()
}

fn live_session(root: &Path, name: &str) -> koru_session_wire::CreateSession {
    use std::str::FromStr;

    #[cfg(windows)]
    let argv = vec![
        "powershell.exe".into(),
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-Command".into(),
        "Start-Sleep -Seconds 30".into(),
    ];
    #[cfg(unix)]
    let argv = vec!["/bin/sh".into(), "-c".into(), "sleep 30".into()];
    koru_session_wire::CreateSession {
        profile: SessionProfileKey::BuiltIn(BuiltInProfileKey::from_str("platform-shell").unwrap()),
        parent_session_id: None,
        profile_kind: ProfileKind::Shell,
        display_name: name.into(),
        working_directory: root.to_path_buf(),
        mode: SessionMode::RawPty,
        attachment_delivery: koru_session_protocol::AttachmentDeliveryPolicy::Unsupported,
        capabilities: CapabilitySet::default(),
        argv,
        environment: koru_session_wire::EnvironmentSpec::Inherit,
        size: GridSize::new(80, 24).unwrap(),
        project_id: None,
    }
}

// ------------------------------------------------------------------ processes

/// An agent on a root of its own, stopped when the test ends.
struct Agent {
    child: std::process::Child,
    root: tempfile::TempDir,
}

impl Agent {
    /// Start one and wait until it is accepting.
    ///
    /// **Binding and accepting are two moments.** Bind claims the endpoint;
    /// accept is the readiness signal. A test that connected the instant the
    /// process existed would be racing the bind, so this dials until it is
    /// answered — the same wait a shell does.
    fn start() -> Self {
        Self::start_with_options(false, None)
    }

    fn start_with_codex_fixture() -> Self {
        let root = tempfile::tempdir().expect("a temp root");
        let provider_path = codex_fixture_path(root.path());
        Self::start_with_root_options(root, false, Some(&provider_path))
    }

    /// Start an Agent whose shell-launch decision succeeds but deliberately
    /// waits for this test's synthetic Hello. This makes held-route assertions
    /// independent of how quickly the real sibling `koru.exe` starts.
    fn start_holding_shell_launch() -> Self {
        Self::start_with_options(true, None)
    }

    fn start_with_options(hold: bool, provider_path: Option<&Path>) -> Self {
        let root = tempfile::tempdir().expect("a temp root");
        Self::start_with_root_options(root, hold, provider_path)
    }

    fn start_with_root_options(
        root: tempfile::TempDir,
        hold: bool,
        provider_path: Option<&Path>,
    ) -> Self {
        let mut command = std::process::Command::new(agent_exe());
        command
            .arg("--root")
            .arg(root.path())
            // `--console` so a failure prints where the harness can see it
            // rather than into a notification area that is not created anyway.
            .arg("--console")
            .stdin(std::process::Stdio::null());
        if hold {
            command.env("KORU_AGENT_TEST_HOLD_SHELL_LAUNCH", "1");
        }
        if let Some(provider_path) = provider_path {
            let inherited = std::env::var_os("PATH").unwrap_or_default();
            let path = std::env::join_paths(
                std::iter::once(provider_path.to_path_buf())
                    .chain(std::env::split_paths(&inherited)),
            )
            .unwrap();
            command.env("PATH", path);
            command.env(
                "CODEX_FIXTURE_TRACE",
                root.path().join("codex-fixture.trace"),
            );
        }
        let child = command.spawn().expect("the agent starts");
        let agent = Self { child, root };
        agent.wait_until_answering();
        agent
    }

    fn root(&self) -> &Path {
        self.root.path()
    }

    fn endpoint(&self) -> koru_ipc::Endpoint {
        koru_ipc::Endpoint::for_service_root(koru_ipc::AGENT, segment(), self.root())
            .expect("a rooted endpoint")
    }

    fn session_endpoint(&self) -> koru_ipc::Endpoint {
        koru_ipc::Endpoint::for_service_root(koru_ipc::SESSION_HOST, segment(), self.root())
            .expect("a rooted session endpoint")
    }

    fn wait_until_answering(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if koru_ipc::connect(&self.endpoint()).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the agent never accepted a connection on {:?}", self.root());
    }

    /// A shell that has completed the handshake.
    fn shell(&self) -> Shell {
        Shell::greet(&self.endpoint())
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        // Asked politely first, so the agent closes its own endpoint and its
        // databases; killed if it will not, because a leaked agent holds
        // `koru-agent.exe` and the *next* build fails with "Access is denied"
        // several minutes from the cause.
        if let Ok(stream) = koru_ipc::connect(&self.endpoint()) {
            let mut write = &stream;
            let _ = koru_ipc::frame::write_frame(
                &mut write,
                FRAME_VERSION,
                &ShellHello {
                    product: "koru-agent-gate".into(),
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
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut exited = false;
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !exited {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // **Every agent starts a store, so every agent leaks one** — the agent
        // ensures one at start-up rather than only when Search asks. This used
        // to be a guard on the single test that asked for a store explicitly,
        // and that stopped being enough the moment start-up did it too.
        //
        // It is here rather than before the early return that used to be above,
        // because on the ordinary path the agent exits promptly and `return`
        // skipped the whole cleanup.
        stop_gateway_under(self.root.path());
    }
}

/// Stop any gateway still answering on this root, and wait for it to go.
///
/// **Mostly a no-op since Plan 4, and kept because "mostly" is not "always".**
/// A gateway used to outlive the agent that started it; now it is a supervised
/// child in that agent's Job Object and dies with it, so by the time this runs
/// — after the agent's process has been observed to exit — there is usually
/// nothing to ask, and the ask costs one failed connect.
///
/// What it still catches is a gateway this suite's agent did not launch: an
/// older-build sibling, or one a developer left running on the same root. The
/// cost of missing one has not changed. On a temporary root it leaves a daemon
/// holding open SQLite files inside a directory `TempDir` is about to remove,
/// and on Windows that removal fails **silently**, so the root is abandoned
/// with its databases in it — sixteen thousand had accumulated in `%TEMP%`
/// before anybody looked. And a leaked child inherits the test harness's
/// stdout, so `cargo test` never sees EOF and a run in which **every test
/// passed** blocks for ever and reads as a hang.
fn stop_gateway_under(agent_root: &Path) {
    // Twice, because a gateway is started on a worker thread: the first ask can
    // land while the process is still binding, and a gateway that came up after
    // being told to stop is exactly the one that gets left behind.
    for _ in 0..2 {
        let hello = koru_api_discovery::ClientHello {
            product: "koru-agent-gate".into(),
            client_version: "0".into(),
            segment: segment().into(),
            api_version: 1,
            requested_scopes: vec![koru_api_client::CONTROL_SCOPE.into()],
            grant: None,
        };
        if let Ok(found) = koru_api_discovery::connect_at(segment(), agent_root, &hello)
            && let Ok(client) = koru_api_client::Client::from_gateway(found)
        {
            let _ = client.store().shutdown();
        }
        // **Asking is not the same as gone.** `stop` returns when the daemon has
        // been told; the file handles go when the process does, and the removal
        // that follows needs the second. Bounded, because a gateway that will not
        // exit is a finding rather than a reason to hang the suite.
        let deadline = Instant::now() + Duration::from_secs(10);
        while koru_api_discovery::probe_at(segment(), agent_root).is_ok()
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(50));
        }
        if koru_api_discovery::probe_at(segment(), agent_root).is_err() {
            return;
        }
    }
}

// --------------------------------------------------------------- a fake shell

/// One connection, driven by hand.
struct Shell {
    stream: koru_ipc::Stream,
    next_id: u64,
}

impl Shell {
    fn greet(endpoint: &koru_ipc::Endpoint) -> Self {
        let stream = koru_ipc::connect(endpoint).expect("connect");
        koru_ipc::set_timeouts(&stream, Some(READ_DEADLINE));
        let mut shell = Self { stream, next_id: 1 };
        let greeting = shell.hello(PROTOCOL_VERSION, FRAME_VERSION);
        assert!(
            matches!(greeting, Some(AgentGreeting::Welcome(_))),
            "a current shell was not welcomed: {greeting:?}"
        );
        shell
    }

    /// Send a `ShellHello` at the given versions and read whatever comes back.
    /// `None` means the agent closed without a word.
    fn hello(&mut self, protocol: u32, framing: u8) -> Option<AgentGreeting> {
        let mut write = &self.stream;
        koru_ipc::frame::write_frame(
            &mut write,
            framing,
            &ShellHello {
                product: "koru-agent-gate".into(),
                version: "0".into(),
                protocol_version: protocol,
            },
        )
        .ok()?;
        write.flush().ok()?;
        let mut read = &self.stream;
        let (version, body) = koru_ipc::frame::read_raw(&mut read).ok()?;
        assert_eq!(
            version, FRAME_VERSION,
            "the agent replied in a framing we did not agree"
        );
        koru_ipc::frame::decode(&body).ok()
    }

    fn send(&mut self, body: Request) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let mut write = &self.stream;
        koru_ipc::frame::write_frame(&mut write, FRAME_VERSION, &FromShell::Request { id, body })
            .and_then(|()| write.flush())
            .expect("send");
        id
    }

    /// Read frames until the answer to `id` arrives, collecting pushes on the
    /// way. **Pushes are not answers** — the agent speaks unasked-for, which is
    /// what the correlation id exists for.
    fn reply_to(&mut self, id: u64) -> Response {
        for _ in 0..32 {
            match self.next_frame() {
                FromAgent::Response { id: got, body } if got == id => return body,
                _ => continue,
            }
        }
        panic!("no answer to request {id}");
    }

    fn ask(&mut self, body: Request) -> Response {
        let id = self.send(body);
        self.reply_to(id)
    }

    fn next_frame(&mut self) -> FromAgent {
        let mut read = &self.stream;
        let (version, raw) = koru_ipc::frame::read_raw(&mut read).expect("a frame");
        assert_eq!(version, FRAME_VERSION);
        koru_ipc::frame::decode(&raw).expect("a message")
    }

    /// The next push, or `None` if the agent stays quiet for `within`.
    ///
    /// Cached Shortcut and ReminderDiagnostics pushes are skipped: both may
    /// arrive after every Hello and precede whatever a test is actually
    /// waiting for — it is never the delivery under test here.
    fn next_push(&mut self, within: Duration) -> Option<Push> {
        koru_ipc::set_timeouts(&self.stream, Some(within));
        let mut read = &self.stream;
        let out = loop {
            match koru_ipc::frame::read_raw(&mut read) {
                Ok((_, raw)) => match koru_ipc::frame::decode::<FromAgent>(&raw) {
                    Ok(FromAgent::Push {
                        body: Push::Shortcut { .. } | Push::ReminderDiagnostics { .. },
                    }) => continue,
                    Ok(FromAgent::Push { body }) => break Some(body),
                    _ => break None,
                },
                Err(_) => break None,
            }
        };
        koru_ipc::set_timeouts(&self.stream, Some(READ_DEADLINE));
        out
    }

    fn say_hello(&mut self, profile: &str, workspace: &str) -> Response {
        self.ask(Request::Hello {
            profile: profile.into(),
            workspace: workspace.into(),
            profile_name: "Test profile".into(),
            workspace_name: "Test workspace".into(),
            pinned: Vec::new(),
            reopen_claim: None,
        })
    }
}

/// How long a test waits for the agent to answer before calling it wedged.
///
/// **A liveness deadline, not a budget.** It was five seconds, and these tests
/// spawn real `koru` and `koru-store-server` processes while seven other test
/// binaries do the same — so five seconds timed a busy machine rather than a
/// broken agent, and three `lifecycle` tests failed together with
/// `WouldBlock` on one run in four of an otherwise green suite. Nothing here
/// asserts *how fast* a reply arrives, only what it says, so a generous
/// deadline weakens no assertion and a tight one invents failures.
///
/// It matters that this is one constant: `next_push` restores the stream's
/// timeout after its own shorter window, so a number that drifts between the
/// two places is a deadline nobody can find.
const READ_DEADLINE: Duration = Duration::from_secs(30);

const P: &str = "11111111-1111-4111-8111-111111111111";
const W1: &str = "22222222-2222-4222-8222-222222222222";
const W2: &str = "33333333-3333-4333-8333-333333333333";
/// A second profile, and it exists to make "no window of this profile" a state
/// a test can actually be in.
///
/// The two held-target tests below used to reach that state by opening a window
/// on another *workspace*. That stopped being enough when an ordinary open
/// gained a fall-back to the profile's most recently active window
/// ([`Router::open`]): a shell on `(P, W1)` is now a valid destination for a
/// target on `(P, W2)`, so nothing was ever held and both tests failed. The
/// hold path is still there — it is what happens when the profile has no window
/// at all — and this is how a test gets there while still having a socket to
/// speak over.
const P2: &str = "44444444-4444-4444-8444-444444444444";

fn target(profile: &str, workspace: &str) -> Target {
    Target {
        profile: profile.into(),
        workspace: workspace.into(),
        document: None,
        node: None,
        trashed: false,
        match_text: None,
        new_window: false,
    }
}

// ------------------------------------------------------------------- the gate

/// An agent binds, and a second on the same channel *and root* stands down.
///
/// **The bind is the election.** It is what lets the agent hold a machine-wide
/// shortcut without an election of its own, and what a shell's spawn-and-retry
/// depends on being true.
#[test]
fn an_agent_binds_and_a_second_on_the_same_root_stands_down() {
    let agent = Agent::start();

    let second = std::process::Command::new(agent_exe())
        .arg("--root")
        .arg(agent.root())
        .arg("--console")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the second agent runs");

    // **Exit 0, not a failure.** Somebody wanted an agent and there is one; a
    // shell that spawned this and then waited for the endpoint should carry on,
    // because the endpoint is there.
    assert!(
        second.status.success(),
        "a second agent should stand down quietly, not fail: {:?}",
        String::from_utf8_lossy(&second.stderr)
    );
    // And the first is still the one answering.
    assert!(koru_ipc::connect(&agent.endpoint()).is_ok());
}

/// Two real Agents may coexist only because every session-owned namespace is
/// derived from the same explicit root. This pins the whole isolation set in
/// one test: discovery, attachments, cleanup, event measurements, and logs.
#[test]
fn two_rooted_agents_cannot_cross_session_state() {
    let first = Agent::start();
    let second = Agent::start();
    assert_ne!(first.endpoint().name(), second.endpoint().name());
    assert!(first.shell().say_hello(P, W1) == Response::Ok);
    assert!(second.shell().say_hello(P2, W2) == Response::Ok);

    let first_client = session_client(&first.session_endpoint(), "koru-workspace-gate");
    let second_client = session_client(&second.session_endpoint(), "koru-api-gateway-gate");
    let first_session = first_client
        .create(live_session(first.root(), "first-root"))
        .unwrap();
    let second_session = second_client
        .create(live_session(second.root(), "second-root"))
        .unwrap();
    assert_eq!(first_client.list().unwrap().len(), 1);
    assert_eq!(second_client.list().unwrap().len(), 1);
    assert_ne!(first_session.id, second_session.id);

    let (_, render) = first_client
        .attach_live(first_session.id, ViewportGeneration::new(1).unwrap(), 80, 0)
        .unwrap();
    drop(render);
    drop(first_client);
    let replacement = session_client(&first.session_endpoint(), "koru-workspace-gate");
    let first_lease = loop {
        match replacement.attach_live(first_session.id, ViewportGeneration::new(2).unwrap(), 80, 0)
        {
            Ok((lease, _)) => break lease,
            Err(koru_session_client::ClientError::Session(
                koru_session_protocol::SessionFailure {
                    code: koru_session_protocol::ErrorCode::ControllerConflict,
                    ..
                },
            )) => std::thread::sleep(Duration::from_millis(20)),
            Err(error) => panic!("reattach through the rooted Agent failed: {error}"),
        }
    };
    replacement
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: first_session.id,
            controller_lease: first_lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();
    let (second_lease, _) = second_client
        .attach_live(
            second_session.id,
            ViewportGeneration::new(1).unwrap(),
            80,
            0,
        )
        .unwrap();
    second_client
        .command(SessionCommand {
            idempotency_key: IdempotencyKey::mint(),
            session_id: second_session.id,
            controller_lease: second_lease.token,
            action: RawCommand::Terminate,
        })
        .unwrap();

    let first_paths = koru_agent::Paths::for_root(first.root().to_owned());
    let second_paths = koru_agent::Paths::for_root(second.root().to_owned());
    let first_sessions = first_paths.session_paths();
    let second_sessions = second_paths.session_paths();
    assert_ne!(first_sessions.directory(), second_sessions.directory());
    assert_ne!(
        koru_agent::session_endpoint(&first_paths).unwrap().name(),
        koru_agent::session_endpoint(&second_paths).unwrap().name()
    );

    let first_owned = first_sessions.attachment_directory(SessionId::mint());
    let second_owned = second_sessions.attachment_directory(SessionId::mint());
    std::fs::create_dir_all(&first_owned).unwrap();
    std::fs::create_dir_all(&second_owned).unwrap();
    std::fs::write(first_owned.join("first.png"), b"first").unwrap();
    let second_sentinel = second_owned.join("second.png");
    std::fs::write(&second_sentinel, b"second").unwrap();

    assert_eq!(
        std::fs::read(first_owned.join("first.png")).unwrap(),
        b"first"
    );
    assert_eq!(std::fs::read(second_sentinel).unwrap(), b"second");

    let first_events = koru_channel::events_root_beside(&first_paths.store_dir());
    let second_events = koru_channel::events_root_beside(&second_paths.store_dir());
    assert_ne!(first_events, second_events);
    assert!(first_events.starts_with(first.root()));
    assert!(second_events.starts_with(second.root()));

    let first_logs = first_paths.store_dir().join(koru_log::LOG_DIR);
    let second_logs = second_paths.store_dir().join(koru_log::LOG_DIR);
    assert_ne!(first_logs, second_logs);
    assert!(first_logs.starts_with(first.root()));
    assert!(second_logs.starts_with(second.root()));

    let first_endpoint = first.session_endpoint();
    let second_endpoint = second.session_endpoint();
    drop(first);
    drop(second);
    let deadline = Instant::now() + Duration::from_secs(5);
    while (koru_ipc::probe(&first_endpoint) || koru_ipc::probe(&second_endpoint))
        && Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!koru_ipc::probe(&first_endpoint));
    assert!(!koru_ipc::probe(&second_endpoint));
}

/// A shell says what it is showing, and the agent counts it.
///
/// Asserted through `status`, which is what the agent *gives back* — not by
/// reaching into a registry this test cannot see.
#[test]
fn a_shell_that_says_hello_is_in_the_registry() {
    let agent = Agent::start();
    let mut shell = agent.shell();
    assert_eq!(shell.say_hello(P, W1), Response::Ok);

    let Response::Status { shells, .. } = shell.ask(Request::Status) else {
        panic!("not a status");
    };
    assert_eq!(shells, 1, "the shell did not land in the registry");
}

/// **Disconnect is EOF and nothing else** — no heartbeat, no liveness poll. A
/// stale entry would send an `Open` into a closed socket and lose it silently.
#[test]
fn dropping_a_connection_removes_its_entry() {
    let agent = Agent::start();
    {
        let mut going = agent.shell();
        assert_eq!(going.say_hello(P, W1), Response::Ok);
    } // dropped: the socket closes

    let mut watcher = agent.shell();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let Response::Status { shells, .. } = watcher.ask(Request::Status) else {
            panic!("not a status");
        };
        // One is the watcher itself, which never said hello — so the count of
        // *registered* shells has to fall to zero.
        if shells == 0 {
            return;
        }
        assert!(Instant::now() < deadline, "the entry outlived its socket");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A result for a workspace nothing has open is **held**, not dropped, and
/// delivered on the `Hello` that follows.
///
/// This is the case the whole stage exists for: a hit in a workspace no window
/// is showing must still arrive.
#[test]
fn a_target_with_no_window_is_held_and_delivered_on_the_hello_that_follows() {
    let agent = Agent::start_holding_shell_launch();
    let mut opener = agent.shell();
    // **A window of another profile**, so there is a socket to ask over and
    // still nothing this target could be delivered to. A window on `(P, W1)`
    // would now be a valid destination for `(P, W2)`, because an ordinary open
    // falls back to the profile's own most recent window.
    assert_eq!(opener.say_hello(P2, W1), Response::Ok);

    // No window of profile P exists at all, so this cannot be delivered.
    assert_eq!(
        opener.ask(Request::Route {
            target: target(P, W2)
        }),
        Response::Ok,
        "the agent would not take the target"
    );
    // **`Ok` means decided, not delivered.** The target is held against a
    // window that does not exist yet.
    let Response::Status {
        pending_launches, ..
    } = opener.ask(Request::Status)
    else {
        panic!("not a status");
    };
    assert_eq!(pending_launches, 1, "the target was not held");

    // The window arrives and says what it is showing.
    let mut arriving = agent.shell();
    assert_eq!(arriving.say_hello(P, W2), Response::Ok);
    let push = arriving.next_push(Duration::from_secs(5));
    match push {
        Some(Push::Open { target }) => assert_eq!(target.workspace, W2),
        other => panic!("the held target was not delivered: {other:?}"),
    }
}

/// **A headless explicit root touches nothing machine-wide.**
///
/// The shortcut is the one this can ask the agent about directly; the other two
/// are asserted as absences on disk, because a tray icon and a Run-key entry
/// have nothing to answer with.
#[test]
fn a_headless_explicit_root_takes_nothing_from_the_machine() {
    let agent = Agent::start();
    let mut shell = agent.shell();
    let Response::Status {
        owns_the_machine, ..
    } = shell.ask(Request::Status)
    else {
        panic!("not a status");
    };
    assert!(
        !owns_the_machine,
        "a headless rooted agent claimed default machine ownership"
    );

    // `device.json` is under the root, never in %LOCALAPPDATA%.
    let device = agent.root().join("agent").join("device.json");
    assert!(
        device.exists(),
        "the device file did not land under the root: {}",
        device.display()
    );
}

/// An unknown **protocol** version is refused, and the refusal names both
/// numbers — because "incompatible" sends somebody to read two changelogs to
/// find out which end is behind.
#[test]
fn an_unknown_protocol_version_is_refused_naming_both_numbers() {
    let agent = Agent::start();
    let stream = koru_ipc::connect(&agent.endpoint()).expect("connect");
    koru_ipc::set_timeouts(&stream, Some(READ_DEADLINE));
    let mut shell = Shell { stream, next_id: 1 };

    match shell.hello(PROTOCOL_VERSION + 1, FRAME_VERSION) {
        Some(AgentGreeting::Refused {
            what, ours, theirs, ..
        }) => {
            assert_eq!(what, "protocol");
            assert_eq!(ours, PROTOCOL_VERSION);
            assert_eq!(theirs, PROTOCOL_VERSION + 1);
        }
        other => panic!("a future protocol was not refused: {other:?}"),
    }
}

/// An unknown **framing** version gets the connection closed and **no reply
/// frame at all**.
///
/// A refusal is itself a frame, and writing one in a format the peer has just
/// said it does not share is a promise we cannot keep. This is the one case
/// where silence is the correct answer, and the only way to check it is to see
/// nothing come back.
#[test]
fn an_unknown_framing_version_is_closed_without_a_word() {
    let agent = Agent::start();
    let stream = koru_ipc::connect(&agent.endpoint()).expect("connect");
    koru_ipc::set_timeouts(&stream, Some(Duration::from_secs(2)));

    let mut write = &stream;
    koru_ipc::frame::write_frame(
        &mut write,
        FRAME_VERSION + 1,
        &ShellHello {
            product: "koru-agent-gate".into(),
            version: "0".into(),
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .and_then(|()| write.flush())
    .expect("send");

    let mut read = &stream;
    assert!(
        koru_ipc::frame::read_raw(&mut read).is_err(),
        "the agent answered a framing it had just been told it does not share"
    );

    // And it is still serving everyone else.
    let mut fine = agent.shell();
    assert_eq!(fine.say_hello(P, W1), Response::Ok);
}

/// `EnsureService(ApiGateway)` brings a real gateway child and embedded Store
/// worker up **on the agent's root**.
///
/// Since Plan 4 the child is `koru-agent.exe --worker gateway`, so there is no
/// sibling to be missing and nothing to skip.
#[test]
fn ensure_service_starts_a_gateway_and_store_on_this_root() {
    let agent = Agent::start();
    let mut shell = agent.shell();

    // **The reply is queued from a worker**, because `ensure_running` spawns a
    // process and retries while it binds — up to ten seconds — and the agent's
    // event loop may never block. So this is a slow answer, not a fast one.
    koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(30)));
    let answer = shell.ask(Request::EnsureService {
        service: koru_agent_wire::Service::ApiGateway,
    });
    assert_eq!(answer, Response::Ok, "the store did not come up");

    // On *this* root, and no other: the databases are under it.
    //
    // **`store/` inside the agent's root**, because an explicit `--root` is a
    // *channel* root and mirrors the default layout — `store/` and `agent/`
    // beside each other. The gateway receives the channel root and resolves
    // the Store worker's durable root beneath it.
    let store_root = agent.root().join("store");
    let db = store_root.join("catalog.db");
    assert!(
        db.exists(),
        "a store came up somewhere else — {} does not exist",
        db.display()
    );
    // And it did not scatter itself around the agent's own directory.
    assert!(
        !agent.root().join("catalog.db").exists(),
        "the store put its databases in the channel root, beside the agent's"
    );

    // Readiness includes the first successful catalog authorization pass. The
    // gateway supervisor uses a control-only token, while this coordinator
    // needs an ordinary data token; accidentally reusing the former leaves all
    // peers disconnected and produces one read_documents 403 every two
    // seconds. Assert the user-visible outcome rather than inspecting a token.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let Response::Status { sync, .. } = shell.ask(Request::Status) else {
            panic!("not a status");
        };
        if sync.sync_ready {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "sync did not become ready after the gateway started: {:?}",
            sync.sync_error
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    // Stopping it is `Agent::drop`'s, for every test rather than only this one.
}

/// The record the gateway on this root is publishing right now.
///
/// `what` names the moment, because "a gateway is published" three times in one
/// case is a panic nobody can place.
fn gateway_record(root: &Path, what: &str) -> koru_api_discovery::GatewayHello {
    koru_api_discovery::connect_at(segment(), root, &control_hello())
        .unwrap_or_else(|error| panic!("no gateway is published {what}: {error}"))
}

fn control_hello() -> koru_api_discovery::ClientHello {
    koru_api_discovery::ClientHello {
        product: "koru-agent-gate".into(),
        client_version: "0".into(),
        segment: segment().into(),
        api_version: 1,
        requested_scopes: vec![koru_api_client::CONTROL_SCOPE.into()],
        grant: None,
    }
}

/// **A replaced gateway child rebuilds the Agent's private bridge, and the
/// Agent accepts the one it rebuilt.**
///
/// The bridge is the Agent's own edge into the gateway: a capability minted per
/// launch, registered before the spawn and authenticated by the child over
/// `AGENT_GATEWAY_BRIDGE`. When a child goes, that link closes, and the ensure
/// that follows has two ways to be wrong in opposite directions. It can fail to
/// notice, leaving the Agent holding a dead edge; or it can over-correct,
/// stopping and replacing a perfectly good child every time it is asked,
/// because the bridge it is looking at is the *predecessor's*.
///
/// **The third ensure is the assertion.** A serving child is left alone and
/// keeps its launch identity; an ensure that stopped and replaced a healthy
/// child would move it again. Nothing here reaches inside the Agent: the launch
/// id is on the published record, which is where a client would read it too.
///
/// **Since Plan 4 the replacement is the supervisor's**, not an ensure's. The
/// stop over the public API is an exit nobody asked the supervisor for, so it
/// is a crash: the child is reaped, a restart is scheduled inside the budget,
/// and a fresh launch id is minted. The `EnsureService` below waits for that
/// replacement rather than causing it — which is exactly what the deleted
/// adopt-and-stop loop must no longer do.
#[test]
fn a_replaced_gateway_child_rebuilds_the_private_bridge() {
    let agent = Agent::start();
    let mut shell = agent.shell();
    koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(300)));
    assert_eq!(
        shell.ask(Request::EnsureService {
            service: koru_agent_wire::Service::ApiGateway,
        }),
        Response::Ok,
        "the first gateway did not come up"
    );
    let first = gateway_record(agent.root(), "after the first ensure");
    let first_launch = first
        .supervisor_launch_id
        .clone()
        .expect("a supervised child publishes the launch it was started for");

    // Taken away the way the Agent's own quit path takes it away: asked over
    // the public lifecycle API, which is the one thing that closes the private
    // bridge without taking the Agent with it.
    koru_api_client::Client::from_gateway(first)
        .expect("a control client for the running gateway")
        .store()
        .shutdown()
        .expect("the gateway accepted the stop");
    wait_until_gone(agent.root(), &first_launch);

    assert_eq!(
        shell.ask(Request::EnsureService {
            service: koru_agent_wire::Service::ApiGateway,
        }),
        Response::Ok,
        "the gateway was not replaced"
    );
    let second = gateway_record(agent.root(), "after the replacement ensure");
    let second_launch = second
        .supervisor_launch_id
        .clone()
        .expect("the replacement publishes its own launch");
    assert_ne!(
        first_launch, second_launch,
        "the stopped child was still the one being published"
    );

    assert_eq!(
        shell.ask(Request::EnsureService {
            service: koru_agent_wire::Service::ApiGateway,
        }),
        Response::Ok
    );
    let third = gateway_record(agent.root(), "after the third ensure");
    assert_eq!(
        third.supervisor_launch_id.as_deref(),
        Some(second_launch.as_str()),
        "the Agent replaced a child whose private bridge it should have accepted"
    );
    assert_eq!(
        third.instance_id, second.instance_id,
        "the replacement was restarted underneath a bridge that was already live"
    );
}

/// Wait until the child that published `previous` is no longer the one
/// answering: either nothing is published, or something else is.
///
/// **Asking is not the same as gone.** A stop returns when the request has
/// landed; the rendezvous goes when the process does, and an ensure that raced
/// the exit would adopt the dying child and report its launch back. Bounded,
/// because a gateway that will not exit is a finding rather than a reason to
/// hang the suite.
///
/// **Why the bound is minutes rather than seconds, and why it used to be
/// ninety.** A gateway asked to stop over `POST /v1/shutdown` runs axum's
/// graceful shutdown, which waits for in-flight connections — and a live Agent
/// holds one, the reminder worker's `/v1/events` tail, which `koru-api-client`
/// cuts and resumes on a ninety-second `STREAM_DEADLINE`. So the *process*
/// legitimately takes up to about ninety seconds to go, and a ninety-second
/// bound was a coin flip: it was observed passing at 91.0 s and failing at
/// 91.0 s on the same tree. This is pre-existing gateway behaviour and the
/// neighbourhood of techdebt #58; the bound is now comfortably past it.
///
/// **A supervisor-initiated stop is not this slow.** `gateway_host`'s
/// `stop_deadline` kills the child ten seconds after the graceful ask, so quit
/// and the update handoff never wait for the drain. Only a client asking the
/// public route does — which is what this helper is watching.
fn wait_until_gone(root: &Path, previous: &str) {
    let asked = Instant::now();
    let deadline = asked + Duration::from_secs(180);
    loop {
        match koru_api_discovery::connect_at(segment(), root, &control_hello()) {
            Err(koru_api_discovery::DiscoveryError::NotRunning(_)) => break,
            Ok(found) if found.supervisor_launch_id.as_deref() != Some(previous) => break,
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "the gateway that was asked to stop was still answering"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // **The drain, measured rather than argued.** Printed unconditionally
    // under `--nocapture`, because this number is the whole reason the bound
    // above is minutes: it was ~90 s before the shutdown path started closing
    // the event tails, and is a second or two after. A regression here shows
    // up as a number in the transcript before it shows up as a timeout.
    eprintln!(
        "[gateway drain] the stopped gateway was gone {} ms after the request",
        asked.elapsed().as_millis()
    );
}

/// **An unrequested accepted shutdown is an unexpected exit, and the
/// supervision recovers from it without being asked.**
///
/// The idle self-stop is *not* unreachable, and assuming a supervised parent
/// makes it so is the belief this case exists to refuse. Any caller holding
/// `gateway.control` can invoke `POST /v1/shutdown?only_if_idle=true` while the
/// parent is alive, and the handler has no supervised-parent exclusion. So it
/// is asked for here exactly as such a caller would ask, and both answers are
/// legitimate: a decline means ADR-270's idleness rules held, and an
/// acceptance means the child exits.
///
/// **The assertion is what happens next, and that nobody asked for it.** No
/// `EnsureService` is sent: the replacement has to be the supervisor's own
/// bounded recovery, from an exit it did not request. And the Agent keeps
/// answering throughout, because supervision happens on its own thread.
#[test]
fn an_unrequested_shutdown_is_an_unexpected_exit_the_supervisor_recovers_from() {
    let agent = Agent::start();
    let mut shell = agent.shell();
    koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(300)));
    assert_eq!(
        shell.ask(Request::EnsureService {
            service: koru_agent_wire::Service::ApiGateway,
        }),
        Response::Ok,
        "the first gateway did not come up"
    );
    let first = gateway_record(agent.root(), "after the first ensure");
    let first_launch = first
        .supervisor_launch_id
        .clone()
        .expect("a supervised child publishes its launch");

    let client = koru_api_client::Client::from_gateway(first)
        .expect("a control client for the running gateway");
    let stopping = client
        .store()
        .shutdown_if_idle()
        .expect("the gateway answered the idle question");
    if !stopping {
        // ADR-270 declined, which is a correct outcome and not this case. The
        // unconditional form is the same class of request — authorized, and
        // never asked for by the supervisor.
        client
            .store()
            .shutdown()
            .expect("the gateway accepted the stop");
    }
    // Nobody ensures. The supervisor reaps, backs off inside its budget and
    // spawns a fresh child on its own. Bounded generously for the same reason
    // `wait_until_gone` is: the drain of the Agent's own event stream is what
    // the exiting child is waiting on.
    let deadline = Instant::now() + Duration::from_secs(240);
    let replacement = loop {
        if let Ok(found) = koru_api_discovery::connect_at(segment(), agent.root(), &control_hello())
            && let Some(launch) = found.supervisor_launch_id.clone()
            && launch != first_launch
        {
            break found;
        }
        assert!(
            Instant::now() < deadline,
            "an exit nobody requested was not recovered from"
        );
        // The Agent is still answering while this is going on, which is the
        // other half: supervision is not on the thread that answers people.
        let answered = Instant::now();
        assert!(
            matches!(shell.ask(Request::Status), Response::Status { .. }),
            "the Agent stopped answering while its gateway was being replaced"
        );
        assert!(
            answered.elapsed() < Duration::from_secs(5),
            "the Agent's answering thread waited on the gateway"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_ne!(replacement.instance_id, "");
}

/// **The Agent answers while its gateway is still starting**, which is what
/// makes a dictation chord usable during a cold launch.
///
/// Startup creates the supervision and returns; the first child opens Store, AI
/// and events on its own thread. This drives the answering thread through that
/// whole window — a `Status` question and a routed dictation state, the two
/// shapes a hotkey press turns into — and requires every round trip to be
/// prompt while discovery still reports nothing published.
///
/// A refused dictation route is the *correct* answer to a route this test does
/// not own; what is being asserted is that the answer arrived at all, and fast.
#[test]
fn the_agent_answers_while_its_gateway_is_still_starting() {
    let started = Instant::now();
    let agent = Agent::start();
    let accepting = started.elapsed();
    let mut asks = 0u32;
    let mut shell = agent.shell();
    koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(30)));

    let mut answered_before_the_gateway = 0u32;
    let deadline = Instant::now() + Duration::from_secs(60);
    while Instant::now() < deadline {
        let published = koru_api_discovery::probe_at(segment(), agent.root()).is_ok();
        let asked = Instant::now();
        asks += 1;
        assert!(matches!(
            shell.ask(Request::Status),
            Response::Status { .. }
        ));
        let status_took = asked.elapsed();
        let dictation = shell.ask(Request::DictationState {
            route: u64::MAX,
            state: koru_agent_wire::DictationState::Refused,
        });
        assert!(
            matches!(dictation, Response::Failed { .. }),
            "a routed dictation state was not answered: {dictation:?}"
        );
        let round_trip = asked.elapsed();
        assert!(
            round_trip < Duration::from_secs(2),
            "the Agent took {round_trip:?} to answer while its gateway was starting \
             (Status {status_took:?}, published {published}, ask {asks} at {:?}, \
             connectable after {accepting:?})",
            asked - started
        );
        if !published {
            answered_before_the_gateway += 1;
        } else if answered_before_the_gateway > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        answered_before_the_gateway > 0,
        "the gateway was already published before the first question — nothing was proved"
    );
}

/// **Quit ends the gateway on its own terms, and says so.**
///
/// Two things this asserts that were not true before Plan 5, and neither is a
/// latency detail:
///
/// - **The Store is asked, not killed.** The supervisor's stop deadline is ten
///   seconds and the gateway's drain used to be ninety, so *every*
///   supervisor-initiated stop timed out and became a forced kill. With the
///   event tails closed rather than waited on, the child now exits on its own
///   and the quit records `was graceful; it exited 0`. That `0` is the Store's
///   own confirmation: the role writes it only after joining the Store worker
///   and finding the exit clean.
/// - **The record names the operation and the child.** `routing.log` carries
///   the outcome, so a person tracing a quit sees which child ended and how,
///   rather than a line that says the request was sent.
///
/// The agent is asked to shut down exactly as `koru-agent stop` and the tray's
/// Quit ask, over the wire. Reading the log afterwards is the only way in from
/// outside: the supervision is inside the process that has just gone.
/// **The root outlives the agent here**, which the shared `Agent` fixture
/// cannot do: it owns its `TempDir` and takes the directory with it, and what
/// this case reads is a line written *during* that shutdown.
#[test]
fn quit_stops_the_gateway_gracefully_and_records_the_outcome() {
    let root = tempfile::tempdir().expect("a temp root");
    let mut child = std::process::Command::new(agent_exe())
        .arg("--root")
        .arg(root.path())
        .arg("--console")
        .stdin(std::process::Stdio::null())
        .spawn()
        .expect("the agent starts");
    let endpoint = koru_ipc::Endpoint::for_service_root(koru_ipc::AGENT, segment(), root.path())
        .expect("a rooted endpoint");
    let deadline = Instant::now() + Duration::from_secs(10);
    while koru_ipc::connect(&endpoint).is_err() {
        assert!(Instant::now() < deadline, "the agent never accepted");
        std::thread::sleep(Duration::from_millis(50));
    }

    let mut shell = Shell::greet(&endpoint);
    koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(300)));
    assert_eq!(
        shell.ask(Request::EnsureService {
            service: koru_agent_wire::Service::ApiGateway,
        }),
        Response::Ok,
        "the gateway did not come up"
    );

    // Exactly what `koru-agent stop` and the tray's Quit send.
    let mut write = &shell.stream;
    koru_ipc::frame::write_frame(
        &mut write,
        FRAME_VERSION,
        &FromShell::Request {
            id: 99,
            body: Request::Shutdown,
        },
    )
    .expect("the shutdown ask is sent");
    write.flush().expect("flushed");

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait().expect("wait for the agent") {
            Some(_) => break,
            None => assert!(Instant::now() < deadline, "the agent did not quit"),
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let routing_log = root.path().join("agent").join("routing.log");
    let text = std::fs::read_to_string(&routing_log).unwrap_or_default();
    let quit = text
        .lines()
        .find(|line| line.contains("gateway quit"))
        .unwrap_or_else(|| {
            panic!(
                "quit recorded no gateway stop outcome in {}:\n{text}",
                routing_log.display()
            )
        });
    assert!(
        quit.contains("was graceful") && quit.contains("exited 0"),
        "quit did not stop the gateway on its own terms: {quit}"
    );
    stop_gateway_under(root.path());
}

/// **Two rooted Agents each run their own gateway child and never observe each
/// other's.** One root, one child: the rendezvous name carries a digest of the
/// root, so a second child could not claim a root that is taken, and the launch
/// identities say which supervision each belongs to.
#[test]
fn two_rooted_agents_each_supervise_their_own_gateway_child() {
    let first = Agent::start();
    let second = Agent::start();
    let mut first_shell = first.shell();
    let mut second_shell = second.shell();
    koru_ipc::set_timeouts(&first_shell.stream, Some(Duration::from_secs(120)));
    koru_ipc::set_timeouts(&second_shell.stream, Some(Duration::from_secs(120)));
    for shell in [&mut first_shell, &mut second_shell] {
        assert_eq!(
            shell.ask(Request::EnsureService {
                service: koru_agent_wire::Service::ApiGateway,
            }),
            Response::Ok
        );
    }

    let one = gateway_record(first.root(), "on the first root");
    let other = gateway_record(second.root(), "on the second root");
    assert_ne!(
        one.supervisor_launch_id, other.supervisor_launch_id,
        "two roots were served by one child"
    );
    assert_ne!(one.instance_id, other.instance_id);
    assert_ne!(one.api_endpoint, other.api_endpoint);
    // Each opened its Store under its own root, and neither under the other's.
    assert!(first.root().join("store").join("catalog.db").exists());
    assert!(second.root().join("store").join("catalog.db").exists());
}

/// Plan 4.2's public boundary gate. Session operations below know only the
/// discovered loopback URL and scoped bearer; neither private endpoint nor
/// Agent launch authority is copied into the HTTP client.
#[test]
fn public_gateway_controls_one_rooted_session_without_crossing_roots() {
    use base64::Engine as _;
    use koru_session_api::{
        AttachSessionRequest, AttachmentList, CreateSessionRequest, DetachSessionRequest,
        SessionList, SessionStreamItem, SnapshotRequest, SubscribeRequest, TerminateRequest,
        UploadAttachmentRequest, WriteTextRequest,
    };
    use koru_session_protocol::{AttachmentId, GridSize, IdempotencyKey, RowIndex, RowRange};

    fn gateway(agent: &Agent) -> koru_api_discovery::GatewayHello {
        let mut shell = agent.shell();
        koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(30)));
        assert_eq!(
            shell.ask(Request::EnsureService {
                service: koru_agent_wire::Service::ApiGateway,
            }),
            Response::Ok
        );
        let hello = koru_api_discovery::ClientHello {
            product: "public-session-gate".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            segment: segment().into(),
            api_version: koru_api_wire::API_VERSION,
            requested_scopes: koru_session_api::ALL_SCOPES
                .iter()
                .map(|scope| (*scope).to_owned())
                .collect(),
            grant: None,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match koru_api_discovery::connect_at(segment(), agent.root(), &hello) {
                Ok(found) => return found,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(error) => panic!("rooted Gateway discovery failed: {error}"),
            }
        }
    }

    fn url(gateway: &koru_api_discovery::GatewayHello, path: &str) -> String {
        format!("{}{}", gateway.api_endpoint, path)
    }

    fn bearer(
        request: reqwest::blocking::RequestBuilder,
        gateway: &koru_api_discovery::GatewayHello,
    ) -> reqwest::blocking::RequestBuilder {
        request.bearer_auth(gateway.session.expose())
    }

    let first = Agent::start();
    let second = Agent::start();
    let first_gateway = gateway(&first);
    let second_gateway = gateway(&second);
    assert!(
        first_gateway
            .capabilities
            .iter()
            .any(|value| value == "sessions")
    );
    assert!(
        second_gateway
            .capabilities
            .iter()
            .any(|value| value == "sessions")
    );

    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let create_key = IdempotencyKey::mint();
    let create = CreateSessionRequest {
        idempotency_key: create_key,
        profile: SessionProfileKey::BuiltIn("platform-shell".parse::<BuiltInProfileKey>().unwrap()),
        parent_session_id: None,
        mode: koru_session_protocol::SessionMode::RawPty,
        working_directory: first.root().to_string_lossy().into_owned(),
        size: GridSize::new(80, 24).unwrap(),
    };
    let created: koru_session_protocol::SessionSummary = bearer(
        http.post(url(&first_gateway, "/v1/sessions")).json(&create),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    let replayed: koru_session_protocol::SessionSummary = bearer(
        http.post(url(&first_gateway, "/v1/sessions")).json(&create),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    assert_eq!(
        replayed.id, created.id,
        "create retry spawned a second process"
    );

    let foreign: SessionList = bearer(
        http.get(url(&second_gateway, "/v1/sessions")),
        &second_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    assert!(
        foreign.sessions.is_empty(),
        "root B observed root A's session"
    );
    let foreign_get = bearer(
        http.get(url(
            &second_gateway,
            &format!("/v1/sessions/{}", created.id),
        )),
        &second_gateway,
    )
    .send()
    .unwrap();
    assert_eq!(foreign_get.status(), reqwest::StatusCode::NOT_FOUND);

    let attach_request = AttachSessionRequest {
        viewport_generation: ViewportGeneration::new(1).unwrap(),
        columns: 80,
        overscan_rows: 2,
    };
    let lease: koru_session_protocol::ControllerLease = bearer(
        http.post(url(
            &first_gateway,
            &format!("/v1/sessions/{}/controller", created.id),
        ))
        .json(&attach_request),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    let conflict = bearer(
        http.post(url(
            &first_gateway,
            &format!("/v1/sessions/{}/controller", created.id),
        ))
        .json(&attach_request),
        &first_gateway,
    )
    .send()
    .unwrap();
    assert_eq!(conflict.status(), reqwest::StatusCode::CONFLICT);

    bearer(
        http.delete(url(
            &first_gateway,
            &format!("/v1/sessions/{}/controller", created.id),
        ))
        .json(&DetachSessionRequest {
            controller_lease: lease.token,
        }),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap();
    let reattach_request = AttachSessionRequest {
        viewport_generation: ViewportGeneration::new(2).unwrap(),
        ..attach_request
    };
    let lease: koru_session_protocol::ControllerLease = bearer(
        http.post(url(
            &first_gateway,
            &format!("/v1/sessions/{}/controller", created.id),
        ))
        .json(&reattach_request),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();

    let snapshot: koru_session_api::SnapshotResponse = bearer(
        http.post(url(
            &first_gateway,
            &format!("/v1/sessions/{}/snapshot", created.id),
        ))
        .json(&SnapshotRequest {
            viewport_generation: ViewportGeneration::new(1).unwrap(),
            range: RowRange {
                first: RowIndex(0),
                count: 24,
            },
            columns: 80,
        }),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    assert_eq!(snapshot.snapshot.session_id, created.id);

    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let png_base64 = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let attachment_id = AttachmentId::mint();
    let attachment: koru_session_protocol::Attachment = bearer(
        http.post(url(
            &first_gateway,
            &format!("/v1/sessions/{}/attachments", created.id),
        ))
        .json(&UploadAttachmentRequest {
            controller_lease: lease.token,
            attachment_id,
            data_base64: png_base64.clone(),
        }),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    assert_eq!(attachment.id, attachment_id);
    let attachments: AttachmentList = bearer(
        http.get(url(
            &first_gateway,
            &format!("/v1/sessions/{}/attachments", created.id),
        )),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap()
    .json()
    .unwrap();
    assert_eq!(attachments.attachments.len(), 1);

    let subscribe = SubscribeRequest {
        after_event: None,
        after_output: None,
    };
    let mut stream = std::io::BufReader::new(
        bearer(
            http.get(url(
                &first_gateway,
                &format!("/v1/sessions/{}/subscribe", created.id),
            ))
            .query(&subscribe),
            &first_gateway,
        )
        .send()
        .unwrap()
        .error_for_status()
        .unwrap(),
    );

    let write = WriteTextRequest {
        idempotency_key: IdempotencyKey::mint(),
        controller_lease: lease.token,
        text: "echo KORU_PUBLIC_STREAM\n".into(),
    };
    for _ in 0..2 {
        bearer(
            http.post(url(
                &first_gateway,
                &format!("/v1/sessions/{}/input/text", created.id),
            ))
            .json(&write),
            &first_gateway,
        )
        .send()
        .unwrap()
        .error_for_status()
        .unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut line = String::new();
    let mut saw_output = false;
    while Instant::now() < deadline {
        line.clear();
        assert!(
            stream.read_line(&mut line).unwrap() > 0,
            "SSE closed before output"
        );
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let item: SessionStreamItem = serde_json::from_str(data.trim()).unwrap();
        if let SessionStreamItem::Output { chunk } = item
            && String::from_utf8_lossy(&chunk.bytes).contains("KORU_PUBLIC_STREAM")
        {
            saw_output = true;
            break;
        }
    }
    assert!(saw_output, "authorized SSE never delivered process output");
    drop(stream);

    let terminate_key = IdempotencyKey::mint();
    bearer(
        http.delete(url(&first_gateway, &format!("/v1/sessions/{}", created.id)))
            .json(&TerminateRequest {
                idempotency_key: terminate_key,
                controller_lease: lease.token,
            }),
        &first_gateway,
    )
    .send()
    .unwrap()
    .error_for_status()
    .unwrap();

    let logs = first.root().join("store/logs");
    for entry in std::fs::read_dir(logs).unwrap().filter_map(Result::ok) {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        assert!(!text.contains("KORU_PUBLIC_STREAM"));
        assert!(!text.contains(&png_base64));
        assert!(!text.contains(first_gateway.session.expose()));
    }
}

/// Plan 4.3's executable-example gate. This uses only `koru-api-client`'s
/// public namespace: discovery, renewal, route paths, bearer handling, and the
/// private session IPC remain below the boundary.
#[test]
fn public_sessions_client_streams_raw_output_and_delivers_an_image_path() {
    use std::sync::mpsc;

    use koru_api_client::sessions::{
        AttachSessionRequest, AttachmentId, CreateSessionRequest, DeliveryMode, GridSize,
        IdempotencyKey, RowIndex, RowRange, SendPendingInputRequest, SessionStreamItem,
        SnapshotRequest, TerminateRequest, ViewportGeneration, WriteTextRequest,
    };

    let agent = Agent::start();
    let mut shell = agent.shell();
    koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(30)));
    assert_eq!(
        shell.ask(Request::EnsureService {
            service: koru_agent_wire::Service::ApiGateway,
        }),
        Response::Ok
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        match koru_api_client::Client::open_at_with_scopes(
            segment(),
            agent.root(),
            koru_session_api::ALL_SCOPES,
        ) {
            Ok(client) => break client,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("rooted public client discovery failed: {error}"),
        }
    };
    assert!(client.sessions().available());

    let created = client
        .sessions()
        .create(&CreateSessionRequest {
            idempotency_key: IdempotencyKey::mint(),
            profile: SessionProfileKey::BuiltIn(
                if cfg!(windows) { "powershell" } else { "bash" }
                    .parse::<BuiltInProfileKey>()
                    .unwrap(),
            ),
            parent_session_id: None,
            mode: koru_session_protocol::SessionMode::RawPty,
            working_directory: agent.root().to_string_lossy().into_owned(),
            size: GridSize::new(80, 24).unwrap(),
        })
        .unwrap();
    let lease = client
        .sessions()
        .attach(
            created.id,
            &AttachSessionRequest {
                viewport_generation: ViewportGeneration::new(1).unwrap(),
                columns: 80,
                overscan_rows: 2,
            },
        )
        .unwrap();
    let marker = "PUBLIC_CLIENT_STREAM";
    let mut saw_marker = false;
    let mut retained_subscription = None;
    for attempt in 1..=4 {
        let observed = client
            .sessions()
            .snapshot(
                created.id,
                &SnapshotRequest {
                    viewport_generation: ViewportGeneration::new(attempt).unwrap(),
                    range: RowRange {
                        first: RowIndex(0),
                        count: 24,
                    },
                    columns: 80,
                },
            )
            .unwrap();
        assert!(observed.resume.after_event.is_some());
        assert!(observed.resume.after_output.is_some());
        let (sender, receiver) = mpsc::sync_channel(32);
        let subscription = client
            .sessions()
            .subscribe(created.id, observed.resume, move |item| {
                let _ = sender.try_send(item);
            })
            .unwrap();
        client
            .sessions()
            .write_text(
                created.id,
                &WriteTextRequest {
                    idempotency_key: IdempotencyKey::mint(),
                    controller_lease: lease.token,
                    text: format!("echo {marker}\r\n"),
                },
            )
            .unwrap();

        // The echo is typed into a PowerShell that may not have started yet:
        // a cold powershell.exe on a loaded four-core runner took longer than
        // the 5 s this used to allow. The input waits in the PTY, so a long
        // limit costs nothing when the shell is quick.
        let wait = Duration::from_secs(30);
        let deadline = Instant::now() + wait;
        let mut retry_after_gap = false;
        let mut stream_ended = false;
        while Instant::now() < deadline {
            match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(Ok(SessionStreamItem::Output { chunk }))
                    if String::from_utf8_lossy(&chunk.bytes).contains(marker) =>
                {
                    saw_marker = true;
                    break;
                }
                Ok(Ok(SessionStreamItem::Gap { .. })) => {
                    retry_after_gap = true;
                    break;
                }
                Ok(Ok(item @ SessionStreamItem::Closed { .. })) => {
                    panic!("public session stream closed early: {item:?}")
                }
                Ok(Err(error)) => panic!("public session stream failed: {error}"),
                Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    stream_ended = true;
                    break;
                }
            }
        }
        if saw_marker {
            retained_subscription = Some(subscription);
            break;
        }
        subscription.close();
        assert!(
            retry_after_gap,
            "{}",
            if stream_ended {
                "stream ended without output or explicit gap".to_owned()
            } else {
                format!("no output and no gap within {wait:?} of typing the echo")
            }
        );
    }
    assert!(saw_marker, "client stream never delivered process output");

    let image_path = agent.root().join("client-smoke.png");
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(1, 1)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    std::fs::write(&image_path, png.into_inner()).unwrap();
    let attachment = client
        .sessions()
        .upload_image_path(created.id, lease.token, AttachmentId::mint(), &image_path)
        .unwrap();
    let delivery = client
        .sessions()
        .send_pending_input(
            created.id,
            &SendPendingInputRequest {
                idempotency_key: IdempotencyKey::mint(),
                controller_lease: lease.token,
                text: "echo PUBLIC_CLIENT_IMAGE".into(),
                attachment_ids: vec![attachment.id],
            },
        )
        .unwrap();
    assert_eq!(delivery.attachments.len(), 1);
    assert_eq!(delivery.attachments[0].mode, DeliveryMode::QuotedPath);

    client
        .sessions()
        .terminate(
            created.id,
            &TerminateRequest {
                idempotency_key: IdempotencyKey::mint(),
                controller_lease: lease.token,
            },
        )
        .unwrap();
    retained_subscription
        .expect("successful output retains its subscription")
        .close();
}

/// Plan 5.2 acceptance row 21: the caller knows only the public client. The
/// Gateway and private host must preserve managed mode and deliver the rooted
/// image through Codex's structured input without exposing that local path.
#[test]
fn public_client_attaches_an_image_to_a_managed_codex_prompt() {
    use koru_api_client::sessions::{
        AttachSessionRequest, AttachmentId, CreateSessionRequest, DeliveryMode, GridSize,
        IdempotencyKey, SendPendingInputRequest, TerminateRequest, ViewportGeneration,
    };
    use koru_session_protocol::{DeliveryState, ProviderKind};
    use std::str::FromStr as _;

    let agent = Agent::start_with_codex_fixture();
    let mut shell = agent.shell();
    koru_ipc::set_timeouts(&shell.stream, Some(Duration::from_secs(30)));
    assert_eq!(
        shell.ask(Request::EnsureService {
            service: koru_agent_wire::Service::ApiGateway,
        }),
        Response::Ok
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let client = loop {
        match koru_api_client::Client::open_at_with_scopes(
            segment(),
            agent.root(),
            koru_session_api::ALL_SCOPES,
        ) {
            Ok(client) => break client,
            Err(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => panic!("rooted public client discovery failed: {error}"),
        }
    };

    let provider = ProviderKind::from_str("codex").unwrap();
    let created_result = client.sessions().create(&CreateSessionRequest {
        idempotency_key: IdempotencyKey::mint(),
        profile: SessionProfileKey::BuiltIn("codex".parse::<BuiltInProfileKey>().unwrap()),
        parent_session_id: None,
        mode: SessionMode::Managed {
            provider: provider.clone(),
        },
        working_directory: agent.root().to_string_lossy().into_owned(),
        size: GridSize::new(80, 24).unwrap(),
    });
    let created = match created_result {
        Ok(created) => created,
        Err(error) => {
            let trace = std::fs::read_to_string(agent.root().join("codex-fixture.trace"))
                .unwrap_or_else(|_| "fixture was never invoked".into());
            panic!("managed public creation failed: {error}; fixture trace: {trace}");
        }
    };
    assert_eq!(created.mode, SessionMode::Managed { provider });
    let lease = client
        .sessions()
        .attach(
            created.id,
            &AttachSessionRequest {
                viewport_generation: ViewportGeneration::new(1).unwrap(),
                columns: 80,
                overscan_rows: 1,
            },
        )
        .unwrap();

    let image_path = agent.root().join("public-managed-fixture.png");
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgba8(2, 2)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    std::fs::write(&image_path, png.into_inner()).unwrap();
    let attachment = client
        .sessions()
        .upload_image_path(created.id, lease.token, AttachmentId::mint(), &image_path)
        .unwrap();
    assert_eq!(attachment.delivery_mode, DeliveryMode::NativeStructured);
    let delivery = client
        .sessions()
        .send_pending_input(
            created.id,
            &SendPendingInputRequest {
                idempotency_key: IdempotencyKey::mint(),
                controller_lease: lease.token,
                text: "PUBLIC_MANAGED_PROMPT_SENTINEL".into(),
                attachment_ids: vec![attachment.id],
            },
        )
        .unwrap();
    assert_eq!(delivery.attachments.len(), 1);
    assert_eq!(delivery.attachments[0].mode, DeliveryMode::NativeStructured);

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let delivered = client
            .sessions()
            .attachments(created.id)
            .unwrap()
            .attachments
            .into_iter()
            .any(|candidate| {
                candidate.id == attachment.id
                    && matches!(candidate.delivery, DeliveryState::Delivered { .. })
            });
        if delivered {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "public managed image never reached Codex"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    client
        .sessions()
        .terminate(
            created.id,
            &TerminateRequest {
                idempotency_key: IdempotencyKey::mint(),
                controller_lease: lease.token,
            },
        )
        .unwrap();

    for entry in std::fs::read_dir(agent.root().join("store/logs"))
        .unwrap()
        .filter_map(Result::ok)
    {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        assert!(!text.contains("PUBLIC_MANAGED_PROMPT_SENTINEL"));
        assert!(!text.contains(&image_path.to_string_lossy().into_owned()));
    }
}

/// A result for a workspace a window **is** showing goes straight to it.
#[test]
fn a_target_for_an_open_workspace_is_delivered_at_once() {
    let agent = Agent::start();
    let mut showing = agent.shell();
    assert_eq!(showing.say_hello(P, W1), Response::Ok);

    let mut asker = agent.shell();
    assert_eq!(
        asker.ask(Request::Route {
            target: target(P, W1)
        }),
        Response::Ok
    );

    match showing.next_push(Duration::from_secs(5)) {
        Some(Push::Open { target }) => assert_eq!(target.workspace, W1),
        other => panic!("the window showing it did not receive it: {other:?}"),
    }
}

/// **Two results before the window opens replace each other; they do not
/// queue.** Somebody who searched twice changed their mind — they do not want
/// two windows.
#[test]
fn a_second_target_for_the_same_workspace_replaces_the_first() {
    let agent = Agent::start_holding_shell_launch();
    let mut asker = agent.shell();
    // Another profile, for the reason on `P2`: a window of profile P would
    // take delivery of these instead of leaving them held, and what is under
    // test here is the queue rather than the routing.
    assert_eq!(asker.say_hello(P2, W1), Response::Ok);

    let mut first = target(P, W2);
    first.match_text = Some("the older one".into());
    assert_eq!(asker.ask(Request::Route { target: first }), Response::Ok);
    let mut second = target(P, W2);
    second.match_text = Some("the newer one".into());
    assert_eq!(asker.ask(Request::Route { target: second }), Response::Ok);

    let Response::Status {
        pending_launches, ..
    } = asker.ask(Request::Status)
    else {
        panic!("not a status");
    };
    assert_eq!(pending_launches, 1, "two targets were queued, not replaced");

    let mut arriving = agent.shell();
    assert_eq!(arriving.say_hello(P, W2), Response::Ok);
    match arriving.next_push(Duration::from_secs(5)) {
        Some(Push::Open { target }) => {
            assert_eq!(
                target.match_text.as_deref(),
                Some("the newer one"),
                "the older target won"
            );
        }
        other => panic!("nothing was delivered: {other:?}"),
    }
}

/// **The tie-break: the window focused most recently wins**, and no clock
/// appears in any message. The agent stamps the order it observed, which is the
/// only order that survives a laptop resuming with the wrong time.
#[test]
fn the_shell_that_was_active_last_receives_the_target() {
    let agent = Agent::start();
    let mut first = agent.shell();
    assert_eq!(first.say_hello(P, W1), Response::Ok);
    let mut second = agent.shell();
    assert_eq!(second.say_hello(P, W1), Response::Ok);

    // The first one comes to the front *after* the second connected.
    assert_eq!(first.ask(Request::Active), Response::Ok);

    let mut asker = agent.shell();
    assert_eq!(
        asker.ask(Request::Route {
            target: target(P, W1)
        }),
        Response::Ok
    );

    assert!(
        matches!(
            first.next_push(Duration::from_secs(5)),
            Some(Push::Open { .. })
        ),
        "the window that was active last did not get it"
    );
    // `koru_ipc::set_timeouts` deliberately ignores a platform refusing the
    // socket timeout so a usable connection is not rejected. A silence
    // assertion cannot inherit that policy or this gate waits forever on such
    // a platform: keep the blocking read on a helper and bound the assertion
    // with a channel deadline the standard library guarantees.
    let (sent, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // Skip the ambient Shortcut push every Hello earns (protocol 11);
        // silence here means "no *delivery*", not "no frame of any kind".
        loop {
            let frame = second.next_frame();
            if matches!(
                frame,
                FromAgent::Push {
                    body: Push::Shortcut { .. } | Push::ReminderDiagnostics { .. }
                }
            ) {
                continue;
            }
            let _ = sent.send(frame);
            break;
        }
    });
    assert!(
        matches!(
            received.recv_timeout(Duration::from_millis(500)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ),
        "it went to both windows"
    );
}

/// A target whose ids are not uuids is **refused with a code**, never routed
/// somewhere arbitrary.
#[test]
fn a_target_with_an_unreadable_id_is_refused() {
    let agent = Agent::start();
    let mut shell = agent.shell();
    match shell.ask(Request::Route {
        target: target("not-a-uuid", W1),
    }) {
        Response::Failed { code, .. } => assert_eq!(code, "bad_id"),
        other => panic!("an unreadable target was accepted: {other:?}"),
    }
}

/// **The grant's anchor, from outside the process.**
///
/// This gate speaks the protocol exactly as a shell does — a `ShellHello` at
/// the current version and nothing privileged — and that is the point: it is a
/// well-behaved local caller that the Agent did not start. It is welcomed, it
/// may say `Hello`, and it is refused a first-party grant, because the only
/// thing the Agent can honestly say about a caller is whether it launched it.
///
/// A test binary is a fair stand-in for the population the grant exists to
/// stop: an MCP server, a plugin, a script — something that asks politely and
/// would otherwise have nothing say no to it.
#[test]
fn a_caller_the_agent_did_not_launch_is_refused_a_first_party_grant() {
    let agent = Agent::start();
    let mut shell = agent.shell();
    assert!(matches!(
        shell.say_hello(P, W1),
        Response::Ok | Response::Failed { .. }
    ));
    match shell.ask(Request::RequestFirstPartyGrant) {
        Response::Failed { code, .. } => assert_eq!(code, "not_first_party"),
        other => panic!("a grant was minted for a process nobody launched: {other:?}"),
    }
    // And a token nobody minted is not a token: validation answers one bit,
    // and the bit is false.
    match shell.ask(Request::ValidateFirstPartyGrant {
        grant: koru_agent_wire::FirstPartyGrant::new(
            "00000000-0000-4000-8000-000000000000".into(),
            i64::MAX,
        ),
    }) {
        Response::FirstPartyGrantValidity { valid } => {
            assert!(!valid, "an invented grant validated")
        }
        other => panic!("validation answered something else: {other:?}"),
    }
}
